//! Request checks of both listeners (port of the clone's `backend/app/guard.py`) and the CORS
//! answers of the API listener (Starlette's `CORSMiddleware` as the clone configures it).
//!
//! * Host: a DNS-rebinding page reaches the app under its own domain name, so only host names a
//!   rebinding attacker cannot control are accepted: IP addresses, single-label names
//!   (localhost, the computer's name), `*.localhost`, this computer's own names and the names in
//!   `allowed_hosts` of settings.json (`"*"` allows any).
//! * Writes: a request that changes something (POST, PUT, PATCH, DELETE) must come from the
//!   app's own pages: a browser marks it same-origin (`Sec-Fetch-Site`) or sends an Origin equal
//!   to the address it asked for or one of the trusted (CORS) origins. Clients that are not
//!   browsers send neither header and are not affected.
//! * CORS: only the API listener answers cross-origin requests, and only for the origins of the
//!   app's own pages (the page listener's addresses). The page listener sends no CORS headers.

use std::net::IpAddr;

/// The methods that change nothing (guard.py `SAFE_METHODS`).
pub const SAFE_METHODS: [&str; 4] = ["GET", "HEAD", "OPTIONS", "TRACE"];
/// The methods a preflight may ask for (the clone's `allow_methods`).
pub const CORS_METHODS: [&str; 5] = ["GET", "POST", "PUT", "PATCH", "DELETE"];
/// How long a browser may keep a preflight answer, in seconds (Starlette's default).
pub const CORS_MAX_AGE_SECONDS: u32 = 600;

/// The lower-case name of a Host header (guard.py `host_name`): `"[::1]:80"` -> `"::1"`,
/// `"a.b.:80"` -> `"a.b"`; `None` for an empty name or an unclosed bracket.
pub fn host_name(host: &str) -> Option<String> {
    let value = host.trim();
    let name = if let Some(rest) = value.strip_prefix('[') {
        let end = rest.find(']')?;
        &rest[..end]
    } else if value.matches(':').count() == 1 {
        value.split(':').next().unwrap_or_default()
    } else {
        value
    };
    let name = name.trim_end_matches('.').to_lowercase();
    (!name.is_empty()).then_some(name)
}

/// True when `name` is an IP address (an IPv6 zone such as `%eth0` is ignored).
pub fn is_ip(name: &str) -> bool {
    name.split('%').next().unwrap_or_default().parse::<IpAddr>().is_ok()
}

/// A host name or an origin's host in the form the lists keep: trimmed, without trailing dots,
/// lower case.
fn normalise_name(name: &str) -> String {
    name.trim().trim_end_matches('.').to_lowercase()
}

/// The parts of an origin that are compared: scheme, host and port (the scheme's default port
/// when none is written). `None` for "null" and anything that is not `scheme://host[:port]`
/// (guard.py `_origin_key`, on top of Python's `urlsplit`).
pub fn origin_key(origin: &str) -> Option<(String, String, Option<u16>)> {
    let origin = origin.trim();
    let (scheme, rest) = origin.split_once("://")?;
    if scheme.is_empty() || !scheme.chars().all(|character| character.is_ascii_alphanumeric() || "+-.".contains(character)) {
        return None;
    }
    let scheme = scheme.to_ascii_lowercase();
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    // Credentials are not part of the host.
    let host_port = authority.rsplit('@').next().unwrap_or_default();
    let (host, port_text) = if let Some(bracketed) = host_port.strip_prefix('[') {
        let end = bracketed.find(']')?;
        let after = &bracketed[end + 1..];
        let port = match after.strip_prefix(':') {
            Some(port) => Some(port),
            None if after.is_empty() => None,
            None => return None,
        };
        (bracketed[..end].to_owned(), port)
    } else {
        match host_port.rsplit_once(':') {
            Some((host, port)) => (host.to_owned(), Some(port)),
            None => (host_port.to_owned(), None),
        }
    };
    let host = normalise_name(&host);
    if host.is_empty() {
        return None;
    }
    let port = match port_text {
        // Python: an empty port counts as none; anything else must be a number up to 65535.
        Some("") | None => None,
        Some(text) => Some(text.parse::<u16>().ok().filter(|_| text.chars().all(|character| character.is_ascii_digit()))?),
    };
    let default = match scheme.as_str() {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    };
    Some((scheme, host, port.or(default)))
}

/// The checks of one listener.
#[derive(Clone, Debug, Default)]
pub struct Guard {
    /// `allowed_hosts` of settings.json, normalised.
    extra_hosts: Vec<String>,
    /// This computer's own names (`network::machine_names`).
    machine_names: Vec<String>,
    /// The origins whose writes and CORS requests this listener accepts besides its own.
    trusted: Vec<(String, String, Option<u16>)>,
    /// The origins as written, for the CORS answer (it repeats the request's Origin anyway).
    trusted_text: Vec<String>,
}

impl Guard {
    /// A guard with the configured host names, this computer's names and the trusted origins
    /// (none for the page listener, the page listener's addresses for the API listener).
    pub fn new(allowed_hosts: &[String], machine_names: &[String], trusted_origins: &[String]) -> Guard {
        let extra_hosts = allowed_hosts.iter().map(|name| normalise_name(name)).filter(|name| !name.is_empty()).collect();
        let trusted_text: Vec<String> = trusted_origins.iter().map(|origin| origin.trim().to_owned()).filter(|origin| !origin.is_empty()).collect();
        let trusted = trusted_text.iter().filter_map(|origin| origin_key(origin)).collect();
        Guard { extra_hosts, machine_names: machine_names.iter().map(|name| normalise_name(name)).collect(), trusted, trusted_text }
    }

    /// The trusted origins as they were given.
    pub fn trusted_origins(&self) -> &[String] {
        &self.trusted_text
    }

    /// guard.py `host_allowed` for a name taken from a Host header.
    pub fn host_allowed(&self, name: Option<&str>) -> bool {
        let Some(name) = name else {
            return false;
        };
        if self.extra_hosts.iter().any(|extra| extra == "*" || extra == name) {
            return true;
        }
        if is_ip(name) || !name.contains('.') || name.ends_with(".localhost") {
            return true;
        }
        self.machine_names.iter().any(|own| own == name)
    }

    /// The Host check of every request: a request without Host (HTTP/1.0; no browser sends
    /// that) passes.
    pub fn host_ok(&self, host: Option<&str>) -> bool {
        match host {
            None => true,
            Some(host) => self.host_allowed(host_name(host).as_deref()),
        }
    }

    /// guard.py `_write_ok`: whether a request that changes something comes from the app's own
    /// pages (or from a client that is not a browser).
    pub fn write_ok(&self, sec_fetch_site: Option<&str>, origin: Option<&str>, host: Option<&str>) -> bool {
        if matches!(sec_fetch_site, Some("same-origin" | "none")) {
            return true;
        }
        let Some(origin) = origin else {
            // Browsers always send Origin with a cross-site write; other clients send neither.
            return sec_fetch_site.is_none();
        };
        let Some(key) = origin_key(origin) else {
            // "null" (sandboxed frames, file: pages) and garbage.
            return false;
        };
        if self.trusted.contains(&key) {
            return true;
        }
        let own = host.and_then(|host| origin_key(&format!("http://{host}")));
        own.is_some_and(|own| own == key)
    }

    /// True when a CORS request from `origin` is answered with the CORS headers.
    pub fn cors_allowed(&self, origin: &str) -> bool {
        origin_key(origin).is_some_and(|key| self.trusted.contains(&key))
    }
}

/// True for a method that changes nothing.
pub fn is_safe_method(method: &str) -> bool {
    SAFE_METHODS.contains(&method)
}

/// True when the peer is this computer: its address is a loopback address. There is no proxy
/// in front of this server, so `X-Forwarded-For` is never trusted.
pub fn is_local_peer(peer: IpAddr) -> bool {
    match peer {
        IpAddr::V4(address) => address.is_loopback(),
        IpAddr::V6(address) => address.is_loopback() || address.to_ipv4_mapped().is_some_and(|mapped| mapped.is_loopback()),
    }
}

/// True when an absolute Windows path starts at `index`: a drive (`C:\`, `C:/`) or a share
/// (`\\server\...`), not inside a word (so `http://` is no path).
fn path_starts_at(chars: &[char], index: usize) -> bool {
    if index > 0 && chars[index - 1].is_alphanumeric() {
        return false;
    }
    let drive = chars[index].is_ascii_alphabetic() && chars.get(index + 1) == Some(&':') && matches!(chars.get(index + 2), Some('\\' | '/'));
    let share = chars[index] == '\\' && chars.get(index + 1) == Some(&'\\') && chars.get(index + 2).is_some_and(|next| !next.is_whitespace() && *next != '\\');
    drive || share
}

/// The length of the path that starts at `index`: up to the closing quote when it stands in
/// quotes, else up to `": "` (a reason follows), a quote, a line end or the end of the text,
/// without a full stop that ends the sentence.
fn path_length(chars: &[char], index: usize) -> usize {
    let closing = match index.checked_sub(1).map(|before| chars[before]) {
        Some('"') => Some('"'),
        Some('\'') => Some('\''),
        Some('\u{201C}') => Some('\u{201D}'),
        _ => None,
    };
    let mut end = index;
    while end < chars.len() {
        let character = chars[end];
        if Some(character) == closing || character == '\n' || character == '\r' {
            break;
        }
        if closing.is_none() && (character == '"' || (character == ':' && end > index + 2 && matches!(chars.get(end + 1), None | Some(' ')))) {
            break;
        }
        end += 1;
    }
    if closing.is_none() && end > index + 3 && chars[end - 1] == '.' {
        end -= 1;
    }
    end - index
}

/// A text for another computer (web brief 4.5): every absolute Windows path in it is replaced by
/// its last name, so that no folder of this computer, and with it the Windows account name of
/// its user, leaves it. `"C:\Users\jane\...\descriptions.json" could not be read` becomes
/// `"descriptions.json" could not be read`.
pub fn without_full_paths(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut cleaned = String::with_capacity(text.len());
    let mut index = 0;
    while index < chars.len() {
        if !path_starts_at(&chars, index) {
            cleaned.push(chars[index]);
            index += 1;
            continue;
        }
        let length = path_length(&chars, index);
        let path: String = chars[index..index + length].iter().collect();
        let trimmed = path.trim_end_matches(['\\', '/']);
        let name = trimmed.rsplit(['\\', '/']).next().unwrap_or_default();
        // A drive alone ("C:\") names no folder of anybody.
        cleaned.push_str(if name.is_empty() || name.ends_with(':') { trimmed } else { name });
        index += length;
    }
    cleaned
}

/// The origins of the app's own pages, trusted by the API listener (run.py
/// `merge_cors_origins`): `http://127.0.0.1:<page port>`, `http://localhost:<page port>`,
/// `http://<computer name>:<page port>` and one per LAN address the pages are served on.
pub fn page_origins(page_port: u16, machine_names: &[String], lan: &[std::net::Ipv4Addr]) -> Vec<String> {
    let mut origins = vec![format!("http://127.0.0.1:{page_port}"), format!("http://localhost:{page_port}")];
    if let Some(name) = machine_names.first() {
        origins.push(format!("http://{name}:{page_port}"));
    }
    origins.extend(lan.iter().map(|address| format!("http://{address}:{page_port}")));
    origins
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guard() -> Guard {
        Guard::new(&[], &["desk-01".to_owned(), "desk-01.local".to_owned()], &[])
    }

    #[test]
    fn other_computers_are_told_file_names_instead_of_full_paths() {
        assert_eq!(
            without_full_paths("The saved descriptions in \"C:\\Users\\Jane Doe\\AppData\\Roaming\\instagram-windows-app\\data\\descriptions.json\" could not be read: The process cannot access the file because it is being used by another process. (os error 32)"),
            "The saved descriptions in \"descriptions.json\" could not be read: The process cannot access the file because it is being used by another process. (os error 32)"
        );
        assert_eq!(without_full_paths("Could not update C:\\Users\\Jane Doe\\data\\descriptions.json: Access is denied. (os error 5)"), "Could not update descriptions.json: Access is denied. (os error 5)");
        assert_eq!(without_full_paths("The export in D:/Exports/Trip 2025/post.json is damaged."), "The export in post.json is damaged.");
        assert_eq!(without_full_paths("\\\\nas\\photos\\jane\\post.json could not be read"), "post.json could not be read");
        assert_eq!(without_full_paths("Two: 'C:\\a\\one.jpg' and \u{201C}C:\\b\\two.jpg\u{201D}."), "Two: 'one.jpg' and \u{201C}two.jpg\u{201D}.");
        assert_eq!(without_full_paths("on the drive C:\\"), "on the drive C:");
        // Text without a path stays as it is, addresses and times included.
        for text in ["Open http://127.0.0.1:3127/ at 10:30.", "No such upload; start a new one.", "a:b c:d"] {
            assert_eq!(without_full_paths(text), text);
        }
    }

    #[test]
    fn host_names_are_taken_from_the_header_as_the_clone_does() {
        // test_library_safety.py test_host_names
        assert_eq!(host_name("localhost:8000").as_deref(), Some("localhost"));
        assert_eq!(host_name("[::1]:8000").as_deref(), Some("::1"));
        assert_eq!(host_name("Example.COM.").as_deref(), Some("example.com"));
        assert_eq!(host_name("example.com.:80").as_deref(), Some("example.com"));
        assert_eq!(host_name("::1").as_deref(), Some("::1"));
        assert_eq!(host_name("[::1"), None);
        assert_eq!(host_name(""), None);
        assert_eq!(host_name(":80"), None);
    }

    #[test]
    fn dns_rebound_host_names_are_refused() {
        let guard = guard();
        for host in ["evil.example", "evil.example:3127", "127.0.0.1.nip.io", "attacker.localhost.evil.com"] {
            assert!(!guard.host_ok(Some(host)), "{host}");
        }
    }

    #[test]
    fn addresses_local_names_and_configured_hosts_are_accepted() {
        let guard = guard();
        for host in ["127.0.0.1:3127", "192.168.1.20:3127", "[::1]:8317", "localhost:3127", "desk-01:3127", "DESK-01.local", "app.localhost"] {
            assert!(guard.host_ok(Some(host)), "{host}");
        }
        assert!(guard.host_ok(None), "HTTP/1.0 without Host");
        let configured = Guard::new(&["Photos.Example.".to_owned()], &[], &[]);
        assert!(configured.host_ok(Some("photos.example:3127")));
        assert!(!configured.host_ok(Some("other.example")));
        assert!(Guard::new(&["*".to_owned()], &[], &[]).host_ok(Some("anything.example")));
    }

    #[test]
    fn cross_site_writes_are_refused() {
        let guard = guard();
        assert!(!guard.write_ok(Some("cross-site"), Some("http://evil.example"), Some("127.0.0.1:3127")));
        assert!(!guard.write_ok(None, Some("http://evil.example"), Some("127.0.0.1:3127")));
        assert!(!guard.write_ok(None, Some("null"), Some("127.0.0.1:3127")));
        assert!(!guard.write_ok(Some("same-site"), None, Some("127.0.0.1:3127")), "a browser without Origin");
        // Another port of the same host is another origin.
        assert!(!guard.write_ok(None, Some("http://127.0.0.1:9999"), Some("127.0.0.1:3127")));
    }

    #[test]
    fn the_apps_own_pages_and_other_clients_can_write() {
        let guard = guard();
        assert!(guard.write_ok(Some("same-origin"), Some("http://127.0.0.1:3127"), Some("127.0.0.1:3127")));
        assert!(guard.write_ok(Some("none"), None, None));
        assert!(guard.write_ok(None, None, Some("127.0.0.1:3127")), "curl sends neither header");
        // A LAN page gets no Sec-Fetch-Site over plain http; its Origin is the address it asked.
        assert!(guard.write_ok(None, Some("http://192.168.1.20:3127"), Some("192.168.1.20:3127")));
        assert!(guard.write_ok(None, Some("http://localhost"), Some("localhost:80")), "the default port");
        let api = Guard::new(&[], &[], &page_origins(3127, &["desk-01".to_owned()], &[]));
        assert!(api.write_ok(Some("same-site"), Some("http://127.0.0.1:3127"), Some("127.0.0.1:8317")));
        assert!(api.write_ok(None, Some("http://desk-01:3127"), Some("desk-01:8317")));
        assert!(!api.write_ok(Some("cross-site"), Some("http://127.0.0.1:3128"), Some("127.0.0.1:8317")));
        assert!(api.cors_allowed("http://localhost:3127"));
        assert!(!api.cors_allowed("http://evil.example"));
        assert!(!guard.cors_allowed("http://127.0.0.1:3127"), "the page listener trusts no other origin");
    }

    #[test]
    fn origins_are_compared_by_scheme_host_and_port() {
        assert_eq!(origin_key("HTTP://LocalHost:3127"), Some(("http".to_owned(), "localhost".to_owned(), Some(3127))));
        assert_eq!(origin_key("https://example.com"), Some(("https".to_owned(), "example.com".to_owned(), Some(443))));
        assert_eq!(origin_key("http://[::1]:80"), Some(("http".to_owned(), "::1".to_owned(), Some(80))));
        assert_eq!(origin_key("http://user:secret@example.com:81/path"), Some(("http".to_owned(), "example.com".to_owned(), Some(81))));
        assert_eq!(origin_key("null"), None);
        assert_eq!(origin_key("http://example.com:99999"), None);
        assert_eq!(origin_key("http://:80"), None);
        assert_eq!(origin_key("example.com"), None);
    }

    #[test]
    fn only_loopback_peers_are_local() {
        assert!(is_local_peer("127.0.0.1".parse().expect("address")));
        assert!(is_local_peer("::1".parse().expect("address")));
        assert!(is_local_peer("::ffff:127.0.0.1".parse().expect("address")));
        assert!(!is_local_peer("192.168.1.30".parse().expect("address")));
    }

    #[test]
    fn the_page_origins_follow_run_py() {
        let lan = ["192.168.1.20".parse().expect("address")];
        assert_eq!(page_origins(3127, &["desk-01".to_owned()], &lan), ["http://127.0.0.1:3127", "http://localhost:3127", "http://desk-01:3127", "http://192.168.1.20:3127"]);
        assert_eq!(page_origins(3127, &[], &[]), ["http://127.0.0.1:3127", "http://localhost:3127"]);
    }
}
