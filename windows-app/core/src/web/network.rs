//! The network side of the browser pages: finding a free port, this computer's names and its
//! LAN addresses (ports of the clone's `run.py available_port` and `backend/network.py`).
//!
//! Port search. The standard library binds without `SO_EXCLUSIVEADDRUSE`, and on Windows a plain
//! bind of `127.0.0.1:p` succeeds while another program listens on `0.0.0.0:p` (verified with
//! the web spike: `cargo run --bin ports`). So before binding, a port is probed by CONNECTING
//! to `127.0.0.1:p` with a short timeout: something answering means the port is taken, by a
//! listener on loopback or on every address. Probing by binding `0.0.0.0` is never done: a
//! listener on a non-loopback address makes the Windows firewall ask, and in local mode it
//! must not.
//!
//! * `AddrInUse` (WSAEADDRINUSE) or `PermissionDenied` (WSAEACCES, a range Hyper-V reserved)
//!   means "taken": the next port is tried when the port came from the defaults or settings.json.
//! * Any other error (`AddrNotAvailable` for an address that is not on this computer) stops at
//!   once: no other port would bind either (clone test
//!   `test_nonlocal_bind_address_fails_without_scanning_more_ports`).
//! * A port the user has just typed in the Browser view sheet is never replaced by another one:
//!   it binds or it is refused (`PortSearch::Exact`).

use std::collections::BTreeSet;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::messages;
use crate::settings::EnvLookup;

/// At most this many ports are tried, from the configured one upward (never past 65535).
pub const PORT_ATTEMPTS: u16 = 100;
/// How long the connect probe of a port waits for an answer. A listening port answers at once;
/// a closed one takes the full time (Windows retries the SYN), so the search runs on a thread
/// of its own.
pub const PORT_PROBE_TIMEOUT_MS: u64 = 150;

/// How a listener's port is chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortSearch {
    /// The port and, while it is taken, the next ones (defaults, settings.json, the command line).
    Upward,
    /// Exactly this port (a port the user typed): taken -> an error naming it.
    Exact,
}

/// Why no listener could be made.
#[derive(Debug)]
pub enum BindError {
    /// The one port that was allowed is used by another program.
    Busy {
        /// The port.
        port: u16,
    },
    /// Every port of the search was taken.
    NoFreePort {
        /// The first port tried.
        first: u16,
        /// The last port tried.
        last: u16,
    },
    /// The address cannot be used at all (not on this computer, or another error).
    Address {
        /// The address and port of the failed bind.
        address: SocketAddrV4,
        /// What the system said.
        error: io::Error,
    },
}

impl BindError {
    /// The sentence for the user.
    pub fn message(&self) -> String {
        match self {
            BindError::Busy { port } => messages::port_in_use(*port),
            BindError::NoFreePort { first, last } => messages::no_free_port(*first, *last),
            BindError::Address { address, error } => messages::address_not_usable(&address.to_string(), &error.to_string()),
        }
    }
}

impl std::fmt::Display for BindError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl std::error::Error for BindError {}

/// True when something accepts a connection on `127.0.0.1:port` within the probe timeout.
///
/// A connection whose own address is the address it connected to is TCP's "self-connect":
/// Windows hands out ephemeral source ports one after the other, so probing the ports of the
/// ephemeral range in a row can pick the very port that is probed as the source, and the
/// connection then answers itself (observed on this machine with ports 64486 and up). That
/// means nobody listens there.
pub fn port_answers(port: u16, timeout: Duration) -> bool {
    let address = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port));
    match TcpStream::connect_timeout(&address, timeout) {
        Ok(stream) => !matches!((stream.local_addr(), stream.peer_addr()), (Ok(local), Ok(peer)) if local == peer),
        Err(_) => false,
    }
}

/// True for the bind errors that mean "this port is taken" (the search goes on).
fn means_taken(error: &io::Error) -> bool {
    matches!(error.kind(), io::ErrorKind::AddrInUse | io::ErrorKind::PermissionDenied)
}

/// One attempt: the probe, then the real bind. `Ok(None)` when the port is taken.
fn try_port(host: Ipv4Addr, port: u16, probe: Duration) -> Result<Option<TcpListener>, BindError> {
    if port_answers(port, probe) {
        return Ok(None);
    }
    let address = SocketAddrV4::new(host, port);
    match TcpListener::bind(address) {
        Ok(listener) => Ok(Some(listener)),
        Err(error) if means_taken(&error) => Ok(None),
        Err(error) => Err(BindError::Address { address, error }),
    }
}

/// A listener on `host` at `port`, or (with `PortSearch::Upward`) at the first free port after
/// it, skipping `skip` (the other listener's port). Port 0 means "any free port" (tests).
pub fn bind_listener(host: Ipv4Addr, port: u16, skip: Option<u16>, search: PortSearch) -> Result<TcpListener, BindError> {
    bind_listener_with(host, port, skip, search, Duration::from_millis(PORT_PROBE_TIMEOUT_MS))
}

/// `bind_listener` with the probe timeout as a parameter.
pub fn bind_listener_with(host: Ipv4Addr, port: u16, skip: Option<u16>, search: PortSearch, probe: Duration) -> Result<TcpListener, BindError> {
    if port == 0 {
        let address = SocketAddrV4::new(host, 0);
        return TcpListener::bind(address).map_err(|error| BindError::Address { address, error });
    }
    if search == PortSearch::Exact {
        if skip == Some(port) {
            return Err(BindError::Busy { port });
        }
        return try_port(host, port, probe)?.ok_or(BindError::Busy { port });
    }
    let last = port.saturating_add(PORT_ATTEMPTS - 1);
    for candidate in port..=last {
        if Some(candidate) == skip {
            continue;
        }
        if let Some(listener) = try_port(host, candidate, probe)? {
            return Ok(listener);
        }
    }
    Err(BindError::NoFreePort { first: port, last })
}

/// True for an IPv4 address that another computer could reach: not loopback, not "every
/// address", not multicast, not the broadcast address (network.py `_lan_address`).
pub fn is_lan_address(address: Ipv4Addr) -> bool {
    !(address.is_loopback() || address.is_unspecified() || address.is_multicast() || address.is_broadcast())
}

/// This computer's IPv4 LAN addresses, sorted and distinct: the addresses of its own name
/// (`COMPUTERNAME`), as the operating system resolves it from the adapters (no outside lookup).
pub fn discover_lan_ipv4(env: &dyn EnvLookup) -> Vec<Ipv4Addr> {
    let Some(name) = env.var("COMPUTERNAME").filter(|name| !name.is_empty()) else {
        return Vec::new();
    };
    let Ok(resolved) = (name.as_str(), 0_u16).to_socket_addrs() else {
        return Vec::new();
    };
    let found: BTreeSet<Ipv4Addr> = resolved
        .filter_map(|address| match address.ip() {
            IpAddr::V4(address) if is_lan_address(address) => Some(address),
            _ => None,
        })
        .collect();
    found.into_iter().collect()
}

/// The LAN addresses a listener bound to `host` serves (network.py `lan_urls_for_host`): all of
/// them for `0.0.0.0`, the address itself for one LAN address, none for loopback.
pub fn lan_addresses_for(host: Ipv4Addr, discovered: &[Ipv4Addr]) -> Vec<Ipv4Addr> {
    if host.is_unspecified() {
        discovered.to_vec()
    } else if is_lan_address(host) {
        vec![host]
    } else {
        Vec::new()
    }
}

/// This computer's own names in lower case (guard.py `machine_names`): `COMPUTERNAME`,
/// `COMPUTERNAME.local` (mDNS) and `COMPUTERNAME.USERDNSDOMAIN` when the domain is set.
pub fn machine_names(env: &dyn EnvLookup) -> Vec<String> {
    let Some(name) = env.var("COMPUTERNAME").map(|name| name.trim().trim_end_matches('.').to_lowercase()).filter(|name| !name.is_empty()) else {
        return Vec::new();
    };
    let mut names = vec![name.clone(), format!("{name}.local")];
    if let Some(domain) = env.var("USERDNSDOMAIN").map(|domain| domain.trim().trim_end_matches('.').to_lowercase()).filter(|domain| !domain.is_empty()) {
        names.push(format!("{name}.{domain}"));
    }
    names
}

/// `http://<address>:<port>`, the form the log, About and the CORS origins use.
pub fn http_url(address: impl std::fmt::Display, port: u16) -> String {
    format!("http://{address}:{port}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(key, value)| ((*key).to_owned(), (*value).to_owned())).collect()
    }

    #[test]
    fn a_taken_port_is_skipped_and_the_next_one_is_used() {
        let taken = TcpListener::bind("127.0.0.1:0").expect("a listener");
        let port = taken.local_addr().expect("address").port();
        let listener = bind_listener_with(Ipv4Addr::LOCALHOST, port, None, PortSearch::Upward, Duration::from_millis(50)).expect("the next port");
        assert_ne!(listener.local_addr().expect("address").port(), port);
    }

    #[test]
    fn a_typed_port_that_is_taken_is_refused_with_its_number() {
        let taken = TcpListener::bind("127.0.0.1:0").expect("a listener");
        let port = taken.local_addr().expect("address").port();
        let error = bind_listener_with(Ipv4Addr::LOCALHOST, port, None, PortSearch::Exact, Duration::from_millis(50)).expect_err("taken");
        assert!(matches!(error, BindError::Busy { port: busy } if busy == port));
        assert!(error.message().contains(&port.to_string()), "{}", error.message());
    }

    #[test]
    fn a_listener_on_every_address_is_found_by_the_probe() {
        // A plain bind of 127.0.0.1:p succeeds beside 0.0.0.0:p on Windows; the probe sees it.
        let taken = TcpListener::bind("0.0.0.0:0").expect("a listener on every address");
        let port = taken.local_addr().expect("address").port();
        assert!(port_answers(port, Duration::from_millis(500)));
        let listener = bind_listener_with(Ipv4Addr::LOCALHOST, port, None, PortSearch::Upward, Duration::from_millis(50)).expect("another port");
        assert_ne!(listener.local_addr().expect("address").port(), port);
    }

    #[test]
    fn the_other_listeners_port_is_skipped() {
        let probe = TcpListener::bind("127.0.0.1:0").expect("a free port");
        let port = probe.local_addr().expect("address").port();
        drop(probe);
        let listener = bind_listener_with(Ipv4Addr::LOCALHOST, port, Some(port), PortSearch::Upward, Duration::from_millis(50)).expect("bound");
        assert_ne!(listener.local_addr().expect("address").port(), port);
        assert!(matches!(bind_listener_with(Ipv4Addr::LOCALHOST, port, Some(port), PortSearch::Exact, Duration::from_millis(50)), Err(BindError::Busy { .. })));
    }

    #[test]
    fn an_address_of_another_computer_fails_without_scanning() {
        // 192.0.2.0/24 is TEST-NET-1: never an address of this computer.
        let started = std::time::Instant::now();
        let error = bind_listener_with(Ipv4Addr::new(192, 0, 2, 77), 40_000, None, PortSearch::Upward, Duration::from_millis(20)).expect_err("not local");
        assert!(matches!(error, BindError::Address { .. }), "{error:?}");
        assert!(started.elapsed() < Duration::from_secs(2), "it stopped at the first port");
    }

    #[test]
    fn the_search_never_passes_the_last_port() {
        assert_eq!(65_530_u16.saturating_add(PORT_ATTEMPTS - 1), 65_535);
    }

    #[test]
    fn lan_addresses_leave_out_what_no_other_computer_can_reach() {
        assert!(!is_lan_address(Ipv4Addr::LOCALHOST));
        assert!(!is_lan_address(Ipv4Addr::UNSPECIFIED));
        assert!(!is_lan_address(Ipv4Addr::new(224, 0, 0, 1)));
        assert!(!is_lan_address(Ipv4Addr::BROADCAST));
        assert!(is_lan_address(Ipv4Addr::new(192, 168, 1, 20)));
        let discovered = [Ipv4Addr::new(10, 0, 0, 5), Ipv4Addr::new(192, 168, 1, 20)];
        assert_eq!(lan_addresses_for(Ipv4Addr::UNSPECIFIED, &discovered), discovered.to_vec());
        assert_eq!(lan_addresses_for(Ipv4Addr::new(192, 168, 1, 20), &discovered), vec![Ipv4Addr::new(192, 168, 1, 20)]);
        assert!(lan_addresses_for(Ipv4Addr::LOCALHOST, &discovered).is_empty());
    }

    #[test]
    fn the_machine_names_are_lower_case_with_their_local_and_domain_forms() {
        assert_eq!(machine_names(&env(&[("COMPUTERNAME", "DESK-01")])), ["desk-01", "desk-01.local"]);
        assert_eq!(machine_names(&env(&[("COMPUTERNAME", "DESK-01"), ("USERDNSDOMAIN", "CORP.EXAMPLE")])), ["desk-01", "desk-01.local", "desk-01.corp.example"]);
        assert!(machine_names(&env(&[])).is_empty());
        assert!(discover_lan_ipv4(&env(&[])).is_empty());
    }
}
