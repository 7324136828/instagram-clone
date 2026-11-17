//! HTTP/1.0 and 1.1 over `std::net`, parsed with `httparse` (web brief 4.4).
//!
//! One thread per connection reads requests one after the other (keep-alive), hands each to the
//! route table and writes the answer. It asks its listener for a place (`Gate`) once its first
//! request has fully arrived, and tells it when it works and when it waits. The rules, all
//! chosen to keep the server small and its limits exact:
//!
//! * The request line and the headers together are at most `MAX_HEAD_BYTES` (else 431), the
//!   target at most `MAX_TARGET_BYTES` (else 414), at most `MAX_HEADERS` headers; anything
//!   httparse cannot read is 400 `Malformed request` (the clone's wording).
//! * A body is only read with `Content-Length`. `Transfer-Encoding` is 501 (browsers send a
//!   length for the strings and Blobs of fetch and XHR); two different lengths are 400. The
//!   route checks the length against its own limit BEFORE anything is read (413, and the
//!   connection is closed); a body the route did not read is drained up to
//!   `DRAIN_LIMIT_BYTES`, else the connection is closed. `Expect: 100-continue` is answered
//!   when the route starts to read the body, that is after its checks passed.
//! * Every answer has `Content-Length` (nothing is sent chunked), `Server` and
//!   `X-Content-Type-Options: nosniff`. A connection stays open for HTTP/1.1 unless the client
//!   or an error closed it, `MAX_REQUESTS_PER_CONNECTION` requests were served on it, or it is
//!   older than `Timeouts::connection_life` (a browser opens a new one at no cost; a client that
//!   keeps one connection busy with small requests cannot hold a place for hours).
//! * Timeouts: a new connection must send its first byte within `Timeouts::first_byte`;
//!   `Timeouts::head` bounds a request head once its first byte came (and the first request of a
//!   connection as a whole), `Timeouts::idle` the wait between two requests, `Timeouts::io` every
//!   single read of a body and write of an answer.
//! * A minimum pace on top of that (`Pace`): a body, a drained body and an answer must keep up
//!   with `Timeouts::min_bytes_per_second`, and may fall behind it by at most
//!   `Timeouts::rate_grace`. Bytes that moved faster count as time ahead, but never more than
//!   that same grace: a client that takes 200 MB at once and then one byte a minute is cut off
//!   half a minute later, not hours later. Without it a client that sends or reads one byte
//!   every minute would hold its connection, and with 64 of them the server, for ever.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::messages;

/// The request line and the headers together are at most this long.
pub const MAX_HEAD_BYTES: usize = 32 * 1024;
/// The request target (path and query) is at most this long.
pub const MAX_TARGET_BYTES: usize = 8 * 1024;
/// At most this many headers are read.
pub const MAX_HEADERS: usize = 100;
/// After this many requests a connection is closed (a new one costs a browser nothing).
pub const MAX_REQUESTS_PER_CONNECTION: usize = 100;
/// A body the route did not read is read and thrown away up to this size; a larger one closes
/// the connection.
pub const DRAIN_LIMIT_BYTES: u64 = 64 * 1024;
/// File and body bytes are copied in pieces of this size.
pub const COPY_BUFFER_BYTES: usize = 64 * 1024;
/// A connection closed after an answer is read from for at most this long first (a "lingering
/// close"): closing a socket with unread bytes makes Windows reset the connection, and the
/// client may then never see the answer (413 to an upload that is still being sent).
pub const LINGER_MS: u64 = 2000;
/// ... and at most this many bytes are read and thrown away meanwhile.
pub const LINGER_BYTES: u64 = 8 * 1024 * 1024;

/// How long the server waits for a client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeouts {
    /// For the first byte of a new connection (10 s): a browser that opened a connection sends
    /// its request at once, and an idle one only holds a place.
    pub first_byte: Duration,
    /// For a whole request head once its first byte came, and for the first request of a
    /// connection (30 s).
    pub head: Duration,
    /// Between two requests of one connection (15 s).
    pub idle: Duration,
    /// For each read of a body and each write of an answer (60 s).
    pub io: Duration,
    /// How far a body or an answer may fall behind `min_bytes_per_second`, and the most time
    /// ahead that bytes moved faster can buy (30 s; see `Pace`).
    pub rate_grace: Duration,
    /// The pace a body or an answer must keep (16 KiB/s: a hundredth of what a slow Wi-Fi
    /// carries).
    pub min_bytes_per_second: u64,
    /// A connection older than this is closed after the answer it is sending (5 minutes).
    pub connection_life: Duration,
}

impl Default for Timeouts {
    fn default() -> Timeouts {
        Timeouts {
            first_byte: Duration::from_secs(10),
            head: Duration::from_secs(30),
            idle: Duration::from_secs(15),
            io: Duration::from_secs(60),
            rate_grace: Duration::from_secs(30),
            min_bytes_per_second: 16 * 1024,
            connection_life: Duration::from_secs(5 * 60),
        }
    }
}

/// The pace of one transfer (a body being read or drained, an answer being written), as a
/// deadline: it starts `rate_grace` away; every byte that moves pushes it on by
/// 1 / `min_bytes_per_second` seconds, but never further than `rate_grace` from now. A
/// transfer that keeps the pace never reaches it; one that falls behind by the grace does, and
/// is cut off. The cap is what a cumulative average lacks: there, 200 MB taken in a moment
/// would buy three and a half hours of one byte a minute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pace {
    deadline: Instant,
}

impl Pace {
    /// The pace of a transfer that starts now.
    pub fn start(timeouts: &Timeouts) -> Pace {
        Pace::start_at(timeouts, Instant::now())
    }

    /// The pace of a transfer that starts at `now`.
    pub fn start_at(timeouts: &Timeouts, now: Instant) -> Pace {
        Pace { deadline: now.checked_add(timeouts.rate_grace).unwrap_or(now) }
    }

    /// How long the next read or write may wait: `io`, or less when the transfer would
    /// otherwise fall too far behind; zero when it has.
    pub fn wait(&self, timeouts: &Timeouts) -> Duration {
        self.wait_at(timeouts, Instant::now())
    }

    /// `wait` at the instant `now`.
    pub fn wait_at(&self, timeouts: &Timeouts, now: Instant) -> Duration {
        self.deadline.saturating_duration_since(now).min(timeouts.io)
    }

    /// Counts `bytes` that moved now.
    pub fn moved(&mut self, timeouts: &Timeouts, bytes: u64) {
        self.moved_at(timeouts, bytes, Instant::now());
    }

    /// Counts `bytes` that moved at the instant `now`.
    pub fn moved_at(&mut self, timeouts: &Timeouts, bytes: u64, now: Instant) {
        let rate = timeouts.min_bytes_per_second.max(1) as f64;
        let earned = Duration::from_secs_f64((bytes as f64 / rate).min(1e9));
        let cap = now.checked_add(timeouts.rate_grace).unwrap_or(now);
        self.deadline = self.deadline.checked_add(earned).unwrap_or(cap).min(cap);
    }
}

/// The error of a transfer that fell below the minimum rate.
fn too_slow() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, messages::CLIENT_TOO_SLOW)
}

/// True for the error of a write that waited too long (the pace, or one write's time).
fn timed_out(error: &io::Error) -> bool {
    matches!(error.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock)
}

/// Makes the close of `stream` a reset: an answer that a client stopped reading is dropped with
/// the connection. A normal close would leave the system sending the rest of what it accepted
/// (megabytes of a video) to a client that reads nothing, for as long as that client keeps the
/// connection, and that client would not even see that it was cut off.
fn reset_on_close(stream: &TcpStream) {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawSocket;
        use windows::Win32::Networking::WinSock::{setsockopt, LINGER, SOCKET, SOL_SOCKET, SO_LINGER};
        let linger = LINGER { l_onoff: 1, l_linger: 0 };
        // SAFETY: the bytes are those of a live LINGER value on the stack, read only during the
        // call; the socket handle belongs to `stream`, which outlives the call.
        unsafe {
            let bytes = std::slice::from_raw_parts(std::ptr::from_ref(&linger).cast::<u8>(), std::mem::size_of::<LINGER>());
            setsockopt(SOCKET(stream.as_raw_socket() as usize), SOL_SOCKET, SO_LINGER, Some(bytes));
        }
    }
    #[cfg(not(windows))]
    let _ = stream;
}

/// One request: its line and headers. The body is read through `BodyReader`.
#[derive(Clone, Debug)]
pub struct Request {
    /// The method as sent (methods are case-sensitive).
    pub method: String,
    /// The target as sent: the path and the query.
    pub target: String,
    /// The target without the query.
    pub path: String,
    /// True for HTTP/1.0.
    pub http10: bool,
    /// The headers, names in lower case, in the order sent.
    pub headers: Vec<(String, String)>,
    /// The client's address.
    pub peer: SocketAddr,
    /// The length of the body (`Content-Length`), when one was sent.
    pub content_length: Option<u64>,
    /// The client waits for `100 Continue` before it sends the body.
    pub expects_continue: bool,
}

impl Request {
    /// The first value of the header `name` (lower case).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
    }

    /// True for a request without a body or with an empty one.
    pub fn has_no_body(&self) -> bool {
        self.content_length.unwrap_or(0) == 0
    }
}

/// What an answer sends after its head.
#[derive(Debug)]
pub enum ResponseBody {
    /// Nothing.
    Empty,
    /// Bytes made for this answer.
    Bytes(Vec<u8>),
    /// A file embedded in the program.
    Static(&'static [u8]),
    /// `length` bytes of an open file from `offset` on.
    File {
        /// The open file.
        file: File,
        /// Where the bytes start.
        offset: u64,
        /// How many bytes are sent.
        length: u64,
    },
}

impl ResponseBody {
    /// The number of bytes of the body.
    pub fn len(&self) -> u64 {
        match self {
            ResponseBody::Empty => 0,
            ResponseBody::Bytes(bytes) => bytes.len() as u64,
            ResponseBody::Static(bytes) => bytes.len() as u64,
            ResponseBody::File { length, .. } => *length,
        }
    }

    /// True when the body has no bytes.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One answer.
#[derive(Debug)]
pub struct Response {
    /// The status code.
    pub status: u16,
    /// Headers besides `Content-Length`, `Server`, `X-Content-Type-Options` and `Connection`,
    /// which the connection writes itself.
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: ResponseBody,
    /// The connection is closed after this answer.
    pub close: bool,
}

impl Response {
    /// An answer with a status and nothing else.
    pub fn empty(status: u16) -> Response {
        Response { status, headers: Vec::new(), body: ResponseBody::Empty, close: false }
    }

    /// A JSON answer of the API: `Content-Type: application/json` and `Cache-Control:
    /// no-store`. The key order of `value` is kept.
    pub fn json(status: u16, value: &Value) -> Response {
        Response::json_bytes(status, value.to_string().into_bytes())
    }

    /// A JSON answer whose body was serialised already.
    pub fn json_bytes(status: u16, bytes: Vec<u8>) -> Response {
        Response::empty(status).with_header("Content-Type", "application/json").with_header("Cache-Control", "no-store").with_body(ResponseBody::Bytes(bytes))
    }

    /// An error of the API: `{"detail": "<sentence>"}` (FastAPI's shape, which the frontend's
    /// `detailOf` reads).
    pub fn detail(status: u16, detail: &str) -> Response {
        let mut body = serde_json::Map::new();
        body.insert("detail".to_owned(), Value::from(detail));
        Response::json(status, &Value::Object(body))
    }

    /// A plain text answer.
    pub fn text(status: u16, text: &str) -> Response {
        Response::empty(status).with_header("Content-Type", "text/plain; charset=utf-8").with_body(ResponseBody::Bytes(text.as_bytes().to_vec()))
    }

    /// The same answer with one more header.
    pub fn with_header(mut self, name: &str, value: impl Into<String>) -> Response {
        self.headers.push((name.to_owned(), value.into()));
        self
    }

    /// The same answer with this body.
    pub fn with_body(mut self, body: ResponseBody) -> Response {
        self.body = body;
        self
    }

    /// The same answer, after which the connection is closed.
    pub fn closing(mut self) -> Response {
        self.close = true;
        self
    }

    /// The first value of the header `name` (compared without regard to case).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.as_str())
    }
}

/// The reason phrase of a status code.
pub fn reason(status: u16) -> &'static str {
    match status {
        100 => "Continue",
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        206 => "Partial Content",
        304 => "Not Modified",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Content Too Large",
        414 => "URI Too Long",
        415 => "Unsupported Media Type",
        416 => "Range Not Satisfiable",
        422 => "Unprocessable Content",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        507 => "Insufficient Storage",
        _ => "Unknown",
    }
}

/// The socket of one connection with the bytes read past the last request head.
pub struct Connection {
    stream: TcpStream,
    buffer: Vec<u8>,
    timeouts: Timeouts,
}

/// Why no request could be read.
enum HeadError {
    /// The client closed the connection, or nothing came in time: close without an answer.
    Gone,
    /// Answer this and close.
    Answer(Response),
}

/// The result of reading a body.
pub struct BodyReader<'a> {
    connection: &'a mut Connection,
    remaining: u64,
    continue_pending: bool,
    continue_sent: bool,
    stop: &'a AtomicBool,
    /// The pace of the body, from when the route started to read (not from the head: a client
    /// that waits for "100 Continue" sends nothing before).
    pace: Option<Pace>,
}

impl BodyReader<'_> {
    /// The bytes of the body that were not read yet.
    pub fn remaining(&self) -> u64 {
        self.remaining
    }

    /// Reads the whole body, which the route checked to be at most `limit` bytes long. Memory
    /// grows with the bytes that really came, not with the length the client announced.
    pub fn read_all(&mut self, limit: u64) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::with_capacity(self.remaining.min(limit).min(COPY_BUFFER_BYTES as u64) as usize);
        self.take(limit).read_to_end(&mut bytes)?;
        Ok(bytes)
    }
}

impl Read for BodyReader<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 || out.is_empty() {
            return Ok(0);
        }
        if self.stop.load(Ordering::SeqCst) {
            return Err(io::Error::new(io::ErrorKind::ConnectionAborted, messages::SERVER_STOPPING));
        }
        if self.continue_pending && !self.continue_sent {
            // The route has looked at the request and wants the body.
            self.continue_sent = true;
            self.connection.stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
        }
        let timeouts = self.connection.timeouts;
        let pace = self.pace.get_or_insert_with(|| Pace::start(&timeouts));
        let wanted = out.len().min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
        let count = if !self.connection.buffer.is_empty() {
            let count = wanted.min(self.connection.buffer.len());
            out[..count].copy_from_slice(&self.connection.buffer[..count]);
            self.connection.buffer.drain(..count);
            count
        } else {
            let wait = pace.wait(&timeouts);
            if wait.is_zero() {
                return Err(too_slow());
            }
            self.connection.stream.set_read_timeout(Some(wait))?;
            let count = self.connection.stream.read(&mut out[..wanted])?;
            if count == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, messages::BODY_CUT_OFF));
            }
            count
        };
        pace.moved(&timeouts, count as u64);
        self.remaining -= count as u64;
        Ok(count)
    }
}

/// What a connection hands each request to.
pub trait Handler: Send + Sync {
    /// The answer to `request`; its body (if any) can be read from `body`.
    fn handle(&self, request: &Request, body: &mut BodyReader<'_>) -> Response;
}

/// The listener's answer to a connection whose first request has fully arrived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admitted {
    /// It has a place and is served as usual.
    Yes,
    /// It has a place, but it was closed to make way for a newer connection after its request
    /// had arrived, so its receiving side is shut: that request is answered, and it is its last.
    Last,
    /// It has no place: it gets "Too many connections".
    No,
}

/// What a connection asks of the listener that accepted it.
pub trait Gate {
    /// Its first request has fully arrived: whether it gets a place to be served in. `in_hand`
    /// is true when everything that request needs has been read (its body, if any, came with its
    /// head), so that it can still be answered when its connection was closed to make way since.
    fn admit(&self, in_hand: bool) -> Admitted;
    /// It works on a request (true), or waits (false): for its next request, or while it reads
    /// what the client still sends after an answer. A connection that waits may be closed for a
    /// new one.
    fn busy(&self, busy: bool);
    /// It got no place: the "Too many connections" answer is written elsewhere.
    fn refuse(&self, stream: TcpStream);
}

/// Finds the end of a request head (`\r\n\r\n`).
fn head_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n").map(|position| position + 4)
}

impl Connection {
    /// A connection on `stream`.
    pub fn new(stream: TcpStream, timeouts: Timeouts) -> Connection {
        Connection { stream, buffer: Vec::new(), timeouts }
    }

    /// Reads the next request head. `first` is true for the first request of the connection.
    fn read_head(&mut self, first: bool, peer: SocketAddr) -> Result<Request, HeadError> {
        let started = Instant::now();
        let mut deadline = (first || !self.buffer.is_empty()).then(|| started + self.timeouts.head);
        // A new connection that sends nothing goes sooner than one whose request is on its way.
        let first_byte_deadline = started + self.timeouts.first_byte.min(self.timeouts.head);
        let mut chunk = [0_u8; 8192];
        loop {
            if let Some(end) = head_end(&self.buffer) {
                if end > MAX_HEAD_BYTES {
                    return Err(HeadError::Answer(Response::detail(431, messages::HEADERS_TOO_LARGE).closing()));
                }
                let head: Vec<u8> = self.buffer.drain(..end).collect();
                return parse_head(&head, peer).map_err(HeadError::Answer);
            }
            if self.buffer.len() > MAX_HEAD_BYTES {
                return Err(HeadError::Answer(Response::detail(431, messages::HEADERS_TOO_LARGE).closing()));
            }
            let wait = match deadline {
                Some(_) if first && self.buffer.is_empty() => first_byte_deadline.saturating_duration_since(Instant::now()),
                Some(deadline) => deadline.saturating_duration_since(Instant::now()),
                None => self.timeouts.idle,
            };
            if wait.is_zero() {
                return Err(HeadError::Gone);
            }
            if self.stream.set_read_timeout(Some(wait)).is_err() {
                return Err(HeadError::Gone);
            }
            match self.stream.read(&mut chunk) {
                Ok(0) | Err(_) => return Err(HeadError::Gone),
                Ok(count) => {
                    self.buffer.extend_from_slice(&chunk[..count]);
                    // The head of a request must arrive within its own time once it started.
                    deadline.get_or_insert_with(|| Instant::now() + self.timeouts.head);
                }
            }
        }
    }

    /// Writes `bytes` in pieces, each with the time the pace of the answer still allows.
    fn send(&mut self, bytes: &[u8], pace: &mut Pace) -> io::Result<()> {
        for piece in bytes.chunks(COPY_BUFFER_BYTES) {
            let wait = pace.wait(&self.timeouts);
            if wait.is_zero() {
                return Err(too_slow());
            }
            self.stream.set_write_timeout(Some(wait))?;
            self.stream.write_all(piece)?;
            pace.moved(&self.timeouts, piece.len() as u64);
        }
        Ok(())
    }

    /// Writes `response` (its body only when `with_body`).
    fn write(&mut self, response: &mut Response, with_body: bool, keep_alive: bool, server: &str) -> io::Result<()> {
        let mut pace = Pace::start(&self.timeouts);
        let mut head = format!("HTTP/1.1 {} {}\r\n", response.status, reason(response.status));
        let mut has_nosniff = false;
        for (name, value) in &response.headers {
            has_nosniff |= name.eq_ignore_ascii_case("x-content-type-options");
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        // A 1xx, 204 and 304 answer has no body and no length (RFC 9110 8.6); every other one has a length.
        if !matches!(response.status, 204 | 304) {
            head.push_str(&format!("Content-Length: {}\r\n", response.body.len()));
        }
        head.push_str(&format!("Server: {server}\r\n"));
        if !has_nosniff {
            head.push_str("X-Content-Type-Options: nosniff\r\n");
        }
        head.push_str(if keep_alive { "Connection: keep-alive\r\n\r\n" } else { "Connection: close\r\n\r\n" });
        self.send(head.as_bytes(), &mut pace)?;
        if with_body && !matches!(response.status, 204 | 304) {
            match &mut response.body {
                ResponseBody::Empty => {}
                ResponseBody::Bytes(bytes) => self.send(bytes, &mut pace)?,
                ResponseBody::Static(bytes) => self.send(bytes, &mut pace)?,
                ResponseBody::File { file, offset, length } => {
                    file.seek(SeekFrom::Start(*offset))?;
                    let mut left = *length;
                    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
                    while left > 0 {
                        let want = buffer.len().min(usize::try_from(left).unwrap_or(usize::MAX));
                        let count = file.read(&mut buffer[..want])?;
                        if count == 0 {
                            // The file got shorter while it was sent: the client sees a cut body.
                            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, messages::FILE_GOT_SHORTER));
                        }
                        self.send(&buffer[..count], &mut pace)?;
                        left -= count as u64;
                    }
                }
            }
        }
        self.stream.flush()
    }

    /// Serves requests until the client goes, an answer closes the connection, the server
    /// stops, `MAX_REQUESTS_PER_CONNECTION` were served or the connection is older than
    /// `Timeouts::connection_life`. The `gate` is asked for a place once the first request has
    /// arrived, and told when a request is being worked on (so that a stopping server, or a new
    /// request that needs the place, leaves it alone) and when the connection waits. The socket
    /// when it is still to be closed (`None` when it went to `Gate::refuse`): its owner closes it
    /// once the listener forgot the connection.
    pub fn serve(mut self, peer: SocketAddr, handler: &dyn Handler, stop: &AtomicBool, server: &str, gate: &dyn Gate) -> Option<TcpStream> {
        if self.serve_requests(peer, handler, stop, server, gate) {
            Some(self.stream)
        } else {
            gate.refuse(self.stream);
            None
        }
    }

    /// `serve`; false when the first request got no place.
    fn serve_requests(&mut self, peer: SocketAddr, handler: &dyn Handler, stop: &AtomicBool, server: &str, gate: &dyn Gate) -> bool {
        let opened = Instant::now();
        for served in 0..MAX_REQUESTS_PER_CONNECTION {
            if stop.load(Ordering::SeqCst) {
                return true;
            }
            let request = match self.read_head(served == 0, peer) {
                Ok(request) => request,
                Err(HeadError::Gone) => return true,
                Err(HeadError::Answer(mut response)) => {
                    gate.busy(true);
                    match self.write(&mut response, true, false, server) {
                        Ok(()) => {
                            gate.busy(false);
                            self.linger();
                        }
                        Err(error) if timed_out(&error) => reset_on_close(&self.stream),
                        Err(_) => {}
                    }
                    return true;
                }
            };
            // A connection holds a place from its first whole request on: one that sends
            // nothing, or half a request, takes none. Its request has been read from the socket
            // by now, so a newer connection may have closed this one to make way meanwhile; a
            // request that needs nothing more from the socket is answered all the same, as the
            // connection's last (`Admitted::Last`).
            let mut last = false;
            if served == 0 {
                let in_hand = request.content_length.unwrap_or(0) <= self.buffer.len() as u64;
                match gate.admit(in_hand) {
                    Admitted::Yes => {}
                    Admitted::Last => last = true,
                    Admitted::No => return false,
                }
            }
            gate.busy(true);
            let head_only = request.method == "HEAD";
            let content_length = request.content_length.unwrap_or(0);
            let (mut response, unread, continue_skipped) = {
                let mut body = BodyReader { connection: self, remaining: content_length, continue_pending: request.expects_continue, continue_sent: false, stop, pace: None };
                let response = handler.handle(&request, &mut body);
                (response, body.remaining, body.continue_pending && !body.continue_sent)
            };
            let mut keep_alive = !last
                && !request.http10
                && !response.close
                && request.header("connection").is_none_or(|value| !value.eq_ignore_ascii_case("close"))
                && served + 1 < MAX_REQUESTS_PER_CONNECTION
                && opened.elapsed() < self.timeouts.connection_life
                && !stop.load(Ordering::SeqCst);
            // A body that was not read is thrown away, unless it is large or the client still
            // waits for "100 Continue" (it would send it later or never).
            if unread > 0 && (continue_skipped || unread > DRAIN_LIMIT_BYTES) {
                keep_alive = false;
            }
            if let Err(error) = self.write(&mut response, !head_only, keep_alive, server) {
                if timed_out(&error) {
                    reset_on_close(&self.stream);
                }
                return true;
            }
            // The answer is out: the connection waits (for its next request, or while it reads
            // what the client still sends), so bytes a client holds back keep no place. A next
            // request that is here already (sent right behind this one) is not waited for: the
            // place is not given away in between.
            if !keep_alive || unread > 0 || head_end(&self.buffer).is_none() {
                gate.busy(false);
            }
            if !keep_alive {
                self.linger();
                return true;
            }
            if unread > 0 && !self.drain(unread) {
                return true;
            }
        }
        true
    }

    /// Closes the sending side and reads what the client still sends, for a moment, so that the
    /// answer just written reaches it (`LINGER_MS`).
    fn linger(&mut self) {
        if self.stream.shutdown(Shutdown::Write).is_err() {
            return;
        }
        let deadline = Instant::now() + Duration::from_millis(LINGER_MS);
        let mut chunk = [0_u8; 8192];
        let mut read: u64 = 0;
        while read < LINGER_BYTES {
            let wait = deadline.saturating_duration_since(Instant::now());
            if wait.is_zero() || self.stream.set_read_timeout(Some(wait)).is_err() {
                return;
            }
            match self.stream.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(count) => read += count as u64,
            }
        }
    }

    /// Reads and drops `count` bytes of an unread body (at the minimum rate, like a body that is
    /// read). False when that failed.
    fn drain(&mut self, mut count: u64) -> bool {
        let from_buffer = self.buffer.len().min(usize::try_from(count).unwrap_or(usize::MAX));
        self.buffer.drain(..from_buffer);
        count -= from_buffer as u64;
        let mut pace = Pace::start(&self.timeouts);
        let mut chunk = [0_u8; 8192];
        while count > 0 {
            let wait = pace.wait(&self.timeouts);
            if wait.is_zero() || self.stream.set_read_timeout(Some(wait)).is_err() {
                return false;
            }
            let want = chunk.len().min(usize::try_from(count).unwrap_or(usize::MAX));
            match self.stream.read(&mut chunk[..want]) {
                Ok(0) | Err(_) => return false,
                Ok(read) => {
                    count -= read as u64;
                    pace.moved(&self.timeouts, read as u64);
                }
            }
        }
        true
    }
}

/// Parses a complete request head.
fn parse_head(head: &[u8], peer: SocketAddr) -> Result<Request, Response> {
    let malformed = || Response::detail(400, messages::MALFORMED_REQUEST).closing();
    let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
    let mut parsed = httparse::Request::new(&mut headers);
    match parsed.parse(head) {
        Ok(httparse::Status::Complete(_)) => {}
        Ok(httparse::Status::Partial) => return Err(malformed()),
        Err(httparse::Error::TooManyHeaders) => return Err(Response::detail(431, messages::HEADERS_TOO_LARGE).closing()),
        Err(_) => return Err(malformed()),
    }
    let method = parsed.method.unwrap_or_default().to_owned();
    let target = parsed.path.unwrap_or_default().to_owned();
    if target.len() > MAX_TARGET_BYTES {
        return Err(Response::detail(414, messages::TARGET_TOO_LONG).closing());
    }
    if !target.starts_with('/') {
        return Err(malformed());
    }
    let http10 = parsed.version == Some(0);
    let mut list: Vec<(String, String)> = Vec::with_capacity(parsed.headers.len());
    for header in parsed.headers.iter() {
        let Ok(value) = std::str::from_utf8(header.value) else {
            return Err(malformed());
        };
        list.push((header.name.to_ascii_lowercase(), value.trim().to_owned()));
    }
    let request_header = |name: &str| list.iter().filter(|(key, _)| key == name).map(|(_, value)| value.clone()).collect::<Vec<String>>();
    if !request_header("transfer-encoding").is_empty() {
        return Err(Response::detail(501, messages::CHUNKED_NOT_SUPPORTED).closing());
    }
    let lengths = request_header("content-length");
    let mut content_length: Option<u64> = None;
    for value in &lengths {
        // "12, 12" (a proxy's repeat) is the same length twice.
        for part in value.split(',') {
            let part = part.trim();
            if part.is_empty() || !part.chars().all(|character| character.is_ascii_digit()) {
                return Err(malformed());
            }
            let length: u64 = part.parse().map_err(|_| malformed())?;
            if content_length.is_some_and(|known| known != length) {
                return Err(Response::detail(400, messages::CONFLICTING_LENGTHS).closing());
            }
            content_length = Some(length);
        }
    }
    let expects_continue = request_header("expect").iter().any(|value| value.eq_ignore_ascii_case("100-continue"));
    let path = target.split('?').next().unwrap_or_default().to_owned();
    Ok(Request { method, target, path, http10, headers: list, peer, content_length, expects_continue })
}

/// The value of one hexadecimal digit.
fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Percent-decodes one path segment. A `%` that is not followed by two hexadecimal digits stays
/// as it is (as Python's `unquote` leaves it); `None` when the result is not UTF-8.
pub fn percent_decode(segment: &str) -> Option<String> {
    let bytes = segment.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex_value(bytes[index + 1]), hex_value(bytes[index + 2])) {
                decoded.push(high * 16 + low);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8(decoded).ok()
}

/// The decoded segments of a request path (the part after the first `/`), or `Err` with the
/// 400 answer `Invalid path` when a segment decodes to `.` or `..`, holds NUL, `\` or `/`, or is
/// not UTF-8 (clone `packaged.py` `has_dot_segment`; the decoded `/` would make another path).
pub fn path_segments(path: &str) -> Result<Vec<String>, Response> {
    let invalid = || Response::detail(400, messages::INVALID_PATH);
    let rest = path.strip_prefix('/').unwrap_or(path);
    if rest.is_empty() {
        return Ok(Vec::new());
    }
    let mut segments = Vec::new();
    for raw in rest.split('/') {
        let segment = percent_decode(raw).ok_or_else(invalid)?;
        if segment == "." || segment == ".." || segment.contains(['\0', '\\', '/']) {
            return Err(invalid());
        }
        segments.push(segment);
    }
    Ok(segments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_are_decoded_one_by_one() {
        assert_eq!(path_segments("/api/library/files/posts/a%20b.jpg").ok(), Some(vec!["api".to_owned(), "library".to_owned(), "files".to_owned(), "posts".to_owned(), "a b.jpg".to_owned()]));
        assert_eq!(path_segments("/").ok(), Some(Vec::new()));
        assert_eq!(path_segments("/assets/").ok(), Some(vec!["assets".to_owned(), String::new()]));
        // A lone or short percent sign stays.
        assert_eq!(percent_decode("100%").as_deref(), Some("100%"));
        assert_eq!(percent_decode("%4").as_deref(), Some("%4"));
        assert_eq!(percent_decode("%zz").as_deref(), Some("%zz"));
        assert_eq!(percent_decode("%41%42").as_deref(), Some("AB"));
        assert_eq!(percent_decode("%e2%82%ac").as_deref(), Some("\u{20ac}"));
    }

    #[test]
    fn traversal_and_hidden_separators_are_an_invalid_path() {
        for path in ["/api/library/files/posts/../../Windows/win.ini", "/api/library/files/%2e%2e/x", "/api/library/files/..%2f..%2fWindows%2fwin.ini", "/a/%5cb", "/a/b%00", "/./x", "/a/%ff"] {
            let refused = path_segments(path).expect_err(path);
            assert_eq!(refused.status, 400, "{path}");
            assert!(matches!(&refused.body, ResponseBody::Bytes(bytes) if String::from_utf8_lossy(bytes).contains("Invalid path")), "{path}");
        }
    }

    #[test]
    fn a_transfer_earns_time_with_the_bytes_it_moves_but_never_more_than_the_grace() {
        let timeouts = Timeouts { rate_grace: Duration::from_secs(30), min_bytes_per_second: 1000, io: Duration::from_secs(60), ..Timeouts::default() };
        let start = Instant::now();
        let at = |seconds: u64| start + Duration::from_secs(seconds);
        let mut pace = Pace::start_at(&timeouts, start);
        assert_eq!(pace.wait_at(&timeouts, start), Duration::from_secs(30), "the grace at the start");
        // 1000 bytes a second for 100 s: always the grace ahead.
        for second in 1..=100 {
            pace.moved_at(&timeouts, 1000, at(second));
        }
        assert_eq!(pace.wait_at(&timeouts, at(100)), Duration::from_secs(30), "a steady transfer never runs out");
        // 690 bytes a second falls behind by the grace after about 97 s.
        let mut slow = Pace::start_at(&timeouts, start);
        let mut cut = None;
        for second in 1..=200 {
            if slow.wait_at(&timeouts, at(second)).is_zero() {
                cut = Some(second);
                break;
            }
            slow.moved_at(&timeouts, 690, at(second));
        }
        assert!(cut.is_some_and(|second| (90..=100).contains(&second)), "{cut:?}");
        // 200 MB at once buys the grace and nothing more: one byte a minute afterwards is cut
        // off half a minute later, not 58 hours later as a cumulative average would allow.
        let mut front_loaded = Pace::start_at(&timeouts, start);
        front_loaded.moved_at(&timeouts, 200 * 1024 * 1024, at(1));
        assert_eq!(front_loaded.wait_at(&timeouts, at(1)), Duration::from_secs(30), "never more than the grace ahead");
        front_loaded.moved_at(&timeouts, 1, at(20));
        assert!(front_loaded.wait_at(&timeouts, at(31)) < Duration::from_secs(1));
        assert_eq!(front_loaded.wait_at(&timeouts, at(32)), Duration::ZERO, "cut off once it is the grace behind");
        // One read or write never waits longer than `io`.
        let patient = Timeouts { io: Duration::from_secs(10), ..timeouts };
        assert_eq!(Pace::start_at(&patient, start).wait_at(&patient, start), Duration::from_secs(10));
    }

    #[test]
    fn heads_are_parsed_with_their_limits() {
        let peer: SocketAddr = "127.0.0.1:5000".parse().expect("address");
        let request = parse_head(b"PUT /api/x?y=1 HTTP/1.1\r\nHost: a\r\nContent-Length: 12\r\nExpect: 100-continue\r\n\r\n", peer).expect("parsed");
        assert_eq!((request.method.as_str(), request.path.as_str(), request.target.as_str()), ("PUT", "/api/x", "/api/x?y=1"));
        assert_eq!(request.content_length, Some(12));
        assert!(request.expects_continue && !request.http10);
        assert_eq!(request.header("host"), Some("a"));
        assert_eq!(parse_head(b"GET / HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\n", peer).expect_err("two lengths").status, 400);
        assert_eq!(parse_head(b"GET / HTTP/1.1\r\nContent-Length: 2, 2\r\n\r\n", peer).expect("the same length twice").content_length, Some(2));
        assert_eq!(parse_head(b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n", peer).expect_err("chunked").status, 501);
        assert_eq!(parse_head(b"GET / HTTP/1.1\r\nContent-Length: -1\r\n\r\n", peer).expect_err("negative").status, 400);
        assert_eq!(parse_head(b"GARBAGE\r\n\r\n", peer).expect_err("garbage").status, 400);
        assert_eq!(parse_head(b"GET http://a/ HTTP/1.1\r\n\r\n", peer).expect_err("absolute form").status, 400);
        let long = format!("GET /{} HTTP/1.1\r\n\r\n", "a".repeat(MAX_TARGET_BYTES));
        assert_eq!(parse_head(long.as_bytes(), peer).expect_err("long target").status, 414);
        let many: String = (0..=MAX_HEADERS).map(|index| format!("X-{index}: 1\r\n")).collect();
        assert_eq!(parse_head(format!("GET / HTTP/1.1\r\n{many}\r\n").as_bytes(), peer).expect_err("too many headers").status, 431);
        assert!(parse_head(b"GET / HTTP/1.0\r\n\r\n", peer).expect("1.0").http10);
    }

    /// A listener that closes the connection to make way between the read of its first request
    /// and the question for a place (E2E4-1), and answers `answer`.
    struct ClosingGate {
        stream: TcpStream,
        answer: Admitted,
        asked: std::sync::Mutex<Vec<bool>>,
    }

    impl Gate for ClosingGate {
        fn admit(&self, in_hand: bool) -> Admitted {
            // What `Handle::close` does: the receiving side is shut.
            let _ = self.stream.shutdown(Shutdown::Read);
            self.asked.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(in_hand);
            if in_hand {
                self.answer
            } else {
                Admitted::No
            }
        }

        fn busy(&self, _busy: bool) {}

        fn refuse(&self, _stream: TcpStream) {}
    }

    /// Answers every request with "hello", after reading its body.
    struct Hello;

    impl Handler for Hello {
        fn handle(&self, _request: &Request, body: &mut BodyReader<'_>) -> Response {
            let mut text = String::new();
            if body.read_to_string(&mut text).is_err() {
                return Response::text(400, "body");
            }
            Response::text(200, &format!("hello{text}"))
        }
    }

    /// Serves one connection whose client sent `request` with a `ClosingGate`; the client's view:
    /// whether it was served, what the gate was told, and the whole answer.
    fn served_after_making_way(request: &[u8]) -> (bool, Vec<bool>, String) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a listener");
        let mut client = TcpStream::connect(listener.local_addr().expect("address")).expect("connect");
        client.write_all(request).expect("the request");
        let (server, peer) = listener.accept().expect("accept");
        let gate = ClosingGate { stream: server.try_clone().expect("a second handle"), answer: Admitted::Last, asked: std::sync::Mutex::default() };
        let left = Connection::new(server, Timeouts::default()).serve(peer, &Hello, &AtomicBool::new(false), "test", &gate);
        let served = left.is_some();
        // Both handles of the server's socket close, as its owner closes it.
        drop(left);
        let asked = gate.asked.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
        drop(gate);
        client.set_read_timeout(Some(Duration::from_secs(5))).expect("a timeout");
        let mut answer = Vec::new();
        let _ = client.read_to_end(&mut answer);
        (served, asked, String::from_utf8_lossy(&answer).into_owned())
    }

    #[test]
    fn a_request_in_hand_is_answered_after_its_connection_made_way() {
        // No body: answered, and the connection closes after it.
        let (served, asked, answer) = served_after_making_way(b"GET /x HTTP/1.1\r\nHost: a\r\n\r\n");
        assert!(served && asked == [true], "{asked:?}");
        assert!(answer.starts_with("HTTP/1.1 200 ") && answer.contains("Connection: close\r\n") && answer.ends_with("\r\n\r\nhello"), "{answer}");
        // A body that came with its head is in hand too.
        let (served, asked, answer) = served_after_making_way(b"PUT /x HTTP/1.1\r\nHost: a\r\nContent-Length: 3\r\n\r\nabc");
        assert!(served && asked == [true] && answer.ends_with("\r\n\r\nhelloabc"), "{asked:?} {answer}");
        // One whose body is still on its way is not: it gets no place.
        let (served, asked, answer) = served_after_making_way(b"PUT /x HTTP/1.1\r\nHost: a\r\nContent-Length: 30\r\n\r\nabc");
        assert!(!served && asked == [false] && answer.is_empty(), "{asked:?} {answer}");
    }
}
