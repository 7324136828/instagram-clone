//! Links: which ones an export may bring in, what they are called on screen, and which ones may
//! be written back into an export.
//!
//! Two rule sets live here and must not be mixed up:
//!
//! * The READER rules of the clone (frontend/src/import/urls.ts; spec-export C.7): an http(s)
//!   link on any site is accepted; on Instagram's own hosts it must lead to what its label
//!   promises (a post, a profile, the page the export came from). Everything else is dropped with
//!   a warning. These are `sanitize_link`, `sanitize_post_url`, `sanitize_profile_url` and
//!   `sanitize_source_url`.
//! * The VALIDATOR rules (instagram.md section 9, block "links"; spec-export D): https only, one
//!   of the four Instagram hosts, no port, no credentials. `export` writes `url` only when
//!   `is_post_url` holds and `user.profile_url` only when `is_profile_url` holds (spec-export
//!   G.3), so that every archive passes the validator without an ERROR.
//!
//! The app never opens a link that did not pass `sanitize_link`: no `javascript:`, `data:` or
//! `file:` value of a post.json can reach the browser.

use std::net::Ipv6Addr;

use crate::text::{is_js_space, js_trim, utf16_len};

pub use crate::manifest::MAX_URL_CHARS;

/// Instagram's own pages (instagr.am is its short domain). No other subdomain counts:
/// l.instagram.com forwards to any site.
pub const INSTAGRAM_HOSTS: [&str; 4] = ["instagram.com", "www.instagram.com", "m.instagram.com", "instagr.am"];

/// First path segments under which a post lives: `/p/<shortcode>/`, `/reel/<shortcode>/` ...
pub const POST_SEGMENTS: [&str; 4] = ["p", "reel", "reels", "tv"];

/// First path segments of instagram.com that are not usernames.
pub const RESERVED_SEGMENTS: [&str; 23] = [
    "p", "reel", "reels", "tv", "explore", "stories", "accounts", "direct", "about", "developer", "legal", "web",
    "api", "graphql", "share", "oauth", "linkshim", "challenge", "login", "logout", "emails", "session", "ajax",
];

/// Longest Instagram username (`^[A-Za-z0-9._]{1,30}$`).
pub const MAX_USERNAME_LINK_CHARS: usize = 30;

/// Longest post shortcode (`^[A-Za-z0-9_-]{1,64}$`).
pub const MAX_SHORTCODE_CHARS: usize = 64;

/// What `site_label` answers for a link that cannot be read (importedPosts.ts `siteName`).
pub const SITE_LABEL_FALLBACK: &str = "the original";

/// What `site_label` answers for a link on one of Instagram's hosts.
pub const SITE_LABEL_INSTAGRAM: &str = "Instagram";

/// The characters a browser percent-encodes in a path, besides controls, the space and everything
/// outside ASCII (the WHATWG "path percent-encode set" without "#" and "?", which end the path).
const PATH_ENCODE: [char; 7] = ['"', '<', '>', '^', '`', '{', '}'];
/// Likewise in the query of an http(s) link (the "special-query percent-encode set").
const QUERY_ENCODE: [char; 4] = ['"', '<', '>', '\''];
/// Likewise in the fragment (the "fragment percent-encode set").
const FRAGMENT_ENCODE: [char; 4] = ['"', '<', '>', '`'];

/// The parts of an http(s) link, as the WHATWG URL parser of a browser reports them for the
/// links this app accepts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UrlParts {
    /// "http" or "https", lower case.
    pub scheme: String,
    /// Host name in lower case, without credentials and without the port. Never empty.
    pub host: String,
    /// The port when the link names one that is not the scheme's default (80 / 443).
    pub port: Option<u16>,
    /// The path as a browser reports it (`URL.pathname`): it starts with "/" ("/" when the link
    /// has none), "." and ".." segments (also written "%2e") are resolved, and the characters a
    /// path cannot carry are percent-encoded. Resolving matters for safety: the browser opens
    /// "/p/x/../../accounts/logout" as "/accounts/logout", so that is what the rules must see.
    pub path: String,
    /// The query including its "?", or "" when there is none (also for a lone "?").
    pub query: String,
    /// The fragment including its "#", or "" when there is none (also for a lone "#").
    pub fragment: String,
}

impl UrlParts {
    /// The path segments: `path` split on "/", empty parts removed (spec-export C.7).
    pub fn segments(&self) -> Vec<&str> {
        self.path.split('/').filter(|segment| !segment.is_empty()).collect()
    }
}

/// Why a link that was present in post.json was dropped; it selects the warning sentence
/// (`messages::link_rejected`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkRejection {
    /// A proper link to one of Instagram's hosts, but not to the kind of page that is wanted:
    /// "... is an instagram.com link, but not to {target}, and was ignored."
    WrongInstagramPage,
    /// Anything else: "... is not a valid http(s) link and was ignored."
    NotAnHttpLink,
}

/// `text` without `prefix`, compared without regard to ASCII letter case.
fn strip_prefix_ignore_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix).then(|| &text[prefix.len()..])
}

/// `%XX` for every UTF-8 byte of the characters outside ASCII and of the ASCII characters in
/// `encode`; everything else as it is.
fn percent_encode(text: &str, encode: &[char]) -> String {
    let mut encoded = String::with_capacity(text.len());
    let mut utf8 = [0_u8; 4];
    for character in text.chars() {
        if character.is_ascii() && !encode.contains(&character) {
            encoded.push(character);
        } else {
            for byte in character.encode_utf8(&mut utf8).bytes() {
                encoded.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    encoded
}

/// "." and its percent-encoded spelling: a path segment that means "this folder".
fn is_single_dot(segment: &str) -> bool {
    segment == "." || segment.eq_ignore_ascii_case("%2e")
}

/// ".." in its four spellings: a path segment that means "the folder above".
fn is_double_dot(segment: &str) -> bool {
    segment == ".."
        || segment.eq_ignore_ascii_case(".%2e")
        || segment.eq_ignore_ascii_case("%2e.")
        || segment.eq_ignore_ascii_case("%2e%2e")
}

/// The path of a link as the browser's URL parser stores it (its "path state"): `raw` is empty or
/// starts with "/". Dot segments are resolved, empty segments are kept ("//a" stays "//a").
fn normalize_path(raw: &str) -> String {
    let pieces: Vec<&str> = raw.strip_prefix('/').unwrap_or(raw).split('/').collect();
    let mut segments: Vec<String> = Vec::with_capacity(pieces.len());
    for (index, piece) in pieces.iter().enumerate() {
        let is_last = index + 1 == pieces.len();
        if is_double_dot(piece) {
            segments.pop();
            // "/a/.." names the folder "/", with its trailing slash.
            if is_last {
                segments.push(String::new());
            }
        } else if is_single_dot(piece) {
            if is_last {
                segments.push(String::new());
            }
        } else {
            segments.push(percent_encode(piece, &PATH_ENCODE));
        }
    }
    format!("/{}", segments.join("/"))
}

/// An IPv6 address the way a browser writes it: lower-case hexadecimal groups, the longest run
/// of two or more zero groups (the first of equally long ones) written as "::", and never the
/// dotted form for the last 32 bits.
fn ipv6_text(address: &Ipv6Addr) -> String {
    let groups = address.segments();
    let mut longest: Option<(usize, usize)> = None;
    let mut index = 0;
    while index < groups.len() {
        if groups[index] != 0 {
            index += 1;
            continue;
        }
        let start = index;
        while index < groups.len() && groups[index] == 0 {
            index += 1;
        }
        let length = index - start;
        if length >= 2 && longest.is_none_or(|(_, best)| length > best) {
            longest = Some((start, length));
        }
    }

    let mut text = String::new();
    let mut index = 0;
    while index < groups.len() {
        if let Some((start, length)) = longest.filter(|(start, _)| *start == index) {
            text.push_str(if start == 0 { "::" } else { ":" });
            index += length;
            continue;
        }
        text.push_str(&format!("{:x}", groups[index]));
        if index + 1 < groups.len() {
            text.push(':');
        }
        index += 1;
    }
    text
}

/// True when the last label of a host is a number in the browser's sense (decimal digits, or
/// "0x" with hexadecimal digits): the browser then reads the whole host as an IPv4 address.
fn ends_in_a_number(host: &str) -> bool {
    let host = host.strip_suffix('.').unwrap_or(host);
    let last = host.rsplit('.').next().unwrap_or(host);
    if last.is_empty() {
        return false;
    }
    if last.bytes().all(|byte| byte.is_ascii_digit()) {
        return true;
    }
    last.strip_prefix("0x").is_some_and(|hex| hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

/// Four decimal numbers 0..=255 without leading zeros, separated by dots: the one spelling of an
/// IPv4 address that every parser reads the same way.
fn is_canonical_ipv4(host: &str) -> bool {
    let parts: Vec<&str> = host.split('.').collect();
    parts.len() == 4
        && parts.iter().all(|part| {
            let plain = !part.is_empty() && part.len() <= 3 && part.bytes().all(|byte| byte.is_ascii_digit());
            plain && (*part == "0" || !part.starts_with('0')) && part.parse::<u16>().is_ok_and(|value| value <= 255)
        })
}

/// The host of an http(s) link in lower case, or `None` when this parser does not take it.
fn parse_host(host: &str) -> Option<String> {
    if let Some(inner) = host.strip_prefix('[') {
        let address: Ipv6Addr = inner.strip_suffix(']')?.parse().ok()?;
        return Some(format!("[{}]", ipv6_text(&address)));
    }
    let allowed = |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_');
    if host.is_empty() || !host.bytes().all(allowed) {
        return None;
    }
    let host = host.to_ascii_lowercase();
    // A browser turns "0x7f.1" or "1.2.3" into 127.0.0.1 and 1.2.0.3. Rather than follow every
    // such spelling, only the plain one is taken.
    if ends_in_a_number(&host) && !is_canonical_ipv4(&host) {
        return None;
    }
    Some(host)
}

/// The port after the ":" of an authority: `Some(None)` for none, an empty one or the default
/// port of the scheme; `None` when it is not a number up to 65535.
fn parse_port(port: &str, scheme: &str) -> Option<Option<u16>> {
    if port.is_empty() {
        return Some(None);
    }
    if !port.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let digits = port.trim_start_matches('0');
    let number: u16 = if digits.is_empty() { 0 } else { digits.parse().ok()? };
    let default = if scheme == "https" { 443 } else { 80 };
    Some((number != default).then_some(number))
}

/// Reads an http(s) link the way the clone's sanitisers do before they look at the host
/// (urls.ts; spec-export C.7): `js_trim`; non-empty and at most `MAX_URL_CHARS` (2048) UTF-16
/// code units; starts with `http://` or `https://` in any letter case, followed by a host
/// character (not "/", "?" or "#"); contains no character U+0000-U+0020, U+007F-U+009F and no
/// backslash. Credentials (`user:pass@`) are dropped, the host is lower-cased, a default port is
/// dropped, the path is normalised as described at `UrlParts::path`. `None` for everything else
/// (`javascript:alert(1)`, `data:text/html,x`, `ftp://example.com/alice`, an empty host, a port
/// that is not a number up to 65535).
///
/// This is a deliberately small parser, not a full WHATWG implementation. Where it differs from
/// a browser it refuses the link; it never reads one differently:
///
/// * a host may only contain ASCII letters, digits, "-", "." and "_" (no IDNA mapping of other
///   scripts, no percent-encoded host), or be an IPv6 literal in "[...]";
/// * a host that a browser would read as an IPv4 address must be written as four plain decimal
///   numbers (`127.0.0.1`; not `0x7f.1`, `127.1` or `0127.0.0.1`);
/// * the clone's Instagram check lets "https:///www.instagram.com/" pass, because a browser
///   skips the extra slash; here a host must follow the two slashes directly.
pub fn parse_http_url(value: &str) -> Option<UrlParts> {
    let raw = js_trim(value);
    if raw.is_empty() || utf16_len(raw) > MAX_URL_CHARS {
        return None;
    }
    // Whitespace, control characters and backslashes are parser-differential bait.
    let forbidden = |character: char| character <= ' ' || ('\u{7F}'..='\u{9F}').contains(&character) || character == '\\';
    if raw.chars().any(forbidden) {
        return None;
    }
    let (scheme, rest) = if let Some(rest) = strip_prefix_ignore_case(raw, "https://") {
        ("https", rest)
    } else {
        ("http", strip_prefix_ignore_case(raw, "http://")?)
    };

    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, after_authority) = rest.split_at(authority_end);
    // Everything up to the last "@" is credentials, which are dropped.
    let host_and_port = authority.rsplit_once('@').map_or(authority, |(_, host_and_port)| host_and_port);
    let (host, port) = if host_and_port.starts_with('[') {
        let (literal, after_literal) = host_and_port.split_at(host_and_port.find(']')? + 1);
        (literal, if after_literal.is_empty() { "" } else { after_literal.strip_prefix(':')? })
    } else {
        host_and_port.split_once(':').unwrap_or((host_and_port, ""))
    };
    let host = parse_host(host)?;
    let port = parse_port(port, scheme)?;

    let path_end = after_authority.find(['?', '#']).unwrap_or(after_authority.len());
    let (path, after_path) = after_authority.split_at(path_end);
    let query_end = if after_path.starts_with('?') { after_path.find('#').unwrap_or(after_path.len()) } else { 0 };
    let (query, fragment) = after_path.split_at(query_end);
    // A lone "?" or "#" is no query and no fragment (URL.search and URL.hash are then empty).
    let query = if query.len() > 1 { percent_encode(query, &QUERY_ENCODE) } else { String::new() };
    let fragment = if fragment.len() > 1 { format!("#{}", percent_encode(&fragment[1..], &FRAGMENT_ENCODE)) } else { String::new() };

    Some(UrlParts { scheme: scheme.to_string(), host, port, path: normalize_path(path), query, fragment })
}

/// True when `host` (any letter case) is one of `INSTAGRAM_HOSTS`.
pub fn is_instagram_host(host: &str) -> bool {
    INSTAGRAM_HOSTS.iter().any(|known| host.eq_ignore_ascii_case(known))
}

/// `^[A-Za-z0-9._]{1,30}$`: what an Instagram username looks like.
pub fn is_username(text: &str) -> bool {
    (1..=MAX_USERNAME_LINK_CHARS).contains(&text.len())
        && text.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'_')
}

/// `^[A-Za-z0-9_-]{1,64}$`: what a post shortcode looks like.
pub fn is_shortcode(text: &str) -> bool {
    (1..=MAX_SHORTCODE_CHARS).contains(&text.len())
        && text.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// True when `segment` (any letter case) is one of `names`, which are lower case.
fn is_one_of(names: &[&str], segment: &str) -> bool {
    names.iter().any(|name| segment.eq_ignore_ascii_case(name))
}

/// The clone's `isProfileSegment` and the validator's `isName`: `is_username` and, lower-cased,
/// not one of `RESERVED_SEGMENTS`.
pub fn is_profile_segment(segment: &str) -> bool {
    is_username(segment) && !is_one_of(&RESERVED_SEGMENTS, segment)
}

/// The clone's and the validator's `isPostPath` over path segments:
/// `at = 1` when there are at least 3 segments and the first is a profile segment or "share"
/// (any case), else 0; true iff there are at least `at + 2` segments, segment `at` (lower-cased)
/// is one of `POST_SEGMENTS` and segment `at + 1` is a shortcode. Trailing segments are allowed.
/// True for /p/X/, /reel/X/, /natgeo/p/X/, /share/reel/X/; false for /alice/ and /explore/.
pub fn is_post_path(segments: &[&str]) -> bool {
    let below_a_name =
        segments.len() >= 3 && (is_profile_segment(segments[0]) || segments[0].eq_ignore_ascii_case("share"));
    let at = usize::from(below_a_name);
    segments.len() >= at + 2 && is_one_of(&POST_SEGMENTS, segments[at]) && is_shortcode(segments[at + 1])
}

/// What a source link on Instagram may lead to: the home feed (no segments), a post, an explore
/// page, or a profile and its tabs. Shared by the reader's and the validator's source rule.
fn is_source_path(segments: &[&str]) -> bool {
    match segments.first() {
        None => true,
        Some(first) => is_post_path(segments) || first.eq_ignore_ascii_case("explore") || is_profile_segment(first),
    }
}

/// A link as text, the way the clone rebuilds it from the parsed parts.
fn link_text(parts: &UrlParts) -> String {
    if is_instagram_host(&parts.host) {
        // Instagram's own pages: https only, no port.
        return format!("https://{}{}{}{}", parts.host, parts.path, parts.query, parts.fragment);
    }
    let port = parts.port.map(|port| format!(":{port}")).unwrap_or_default();
    format!("{}://{}{port}{}{}{}", parts.scheme, parts.host, parts.path, parts.query, parts.fragment)
}

/// The clone's `sanitizeWebUrl`: a link the app may show and open. `parse_http_url`, then:
/// on an Instagram host the result is `https://{host}{path}{query}{fragment}` (http upgraded,
/// port dropped); on any other host `{scheme}://{host}[:{port}]{path}{query}{fragment}`.
///
/// Vectors: `HTTP://Example.COM:81/x` -> `http://example.com:81/x`; `https://a:b@example.com/x`
/// -> `https://example.com/x`; `http://instagram.com/p/FXTlen002/` ->
/// `https://instagram.com/p/FXTlen002/`; `javascript:alert(1)` -> `None`.
pub fn sanitize_link(value: &str) -> Option<String> {
    parse_http_url(value).map(|parts| link_text(&parts))
}

/// The clone's `sanitizeInstagramUrl`: `sanitize_link`, but `None` unless the host is one of
/// Instagram's. Used to choose between the two rejection sentences.
pub fn sanitize_instagram_url(value: &str) -> Option<String> {
    parse_http_url(value).filter(|parts| is_instagram_host(&parts.host)).map(|parts| link_text(&parts))
}

/// The pattern of the three labelled sanitisers: any http(s) link passes, except that a link on
/// one of Instagram's hosts must have a path `fits` accepts.
fn sanitize_labelled(value: &str, fits: fn(&[&str]) -> bool) -> Result<String, LinkRejection> {
    let parts = parse_http_url(value).ok_or(LinkRejection::NotAnHttpLink)?;
    if is_instagram_host(&parts.host) && !fits(&parts.segments()) {
        return Err(LinkRejection::WrongInstagramPage);
    }
    Ok(link_text(&parts))
}

/// The clone's `sanitizePostUrl` for the `url` of an entry: `sanitize_link`; on an Instagram
/// host the path must be a post path (`is_post_path`); links on other sites are accepted as
/// they are. `Err` says which warning the reader gives.
///
/// Kept: `https://www.instagram.com/alice/reel/Cabc123/?igsh=1`, `https://mastodon.social/@alice/1123`,
/// `https://www.instagram.com.evil.example/p/X/` (an ordinary other-site link).
/// `WrongInstagramPage`: `https://www.instagram.com/accounts/logout/`, `https://www.instagram.com/alice/`.
/// `NotAnHttpLink`: `javascript:alert(1)`, `data:text/html,x`, `ftp://example.com/alice`.
pub fn sanitize_post_url(value: &str) -> Result<String, LinkRejection> {
    sanitize_labelled(value, is_post_path)
}

/// The clone's `sanitizeProfileUrl` for `user.profile_url`: `sanitize_link`; on an Instagram
/// host the path must be exactly one profile segment.
pub fn sanitize_profile_url(value: &str) -> Result<String, LinkRejection> {
    sanitize_labelled(value, |segments| segments.len() == 1 && is_profile_segment(segments[0]))
}

/// The clone's `sanitizeSourceUrl` for the export's `source.url`: `sanitize_link`; on an
/// Instagram host: no segments (the home feed), or a post path, or a first segment "explore"
/// (any case), or a first segment that is a profile segment (a profile and its tabs).
pub fn sanitize_source_url(value: &str) -> Result<String, LinkRejection> {
    sanitize_labelled(value, is_source_path)
}

/// The name of the site a link leads to, for "View on {site}" and "View profile on {site}"
/// (importedPosts.ts `siteName`; spec-edit 2): `SITE_LABEL_INSTAGRAM` for a link on an Instagram
/// host, else the host without a leading "www." (any case), else `SITE_LABEL_FALLBACK` when the
/// link cannot be parsed. `https://example.com/p/abc/` -> "example.com".
pub fn site_label(url: &str) -> String {
    let Some(parts) = parse_http_url(url) else {
        return SITE_LABEL_FALLBACK.to_string();
    };
    if is_instagram_host(&parts.host) {
        return SITE_LABEL_INSTAGRAM.to_string();
    }
    // The host is lower case already.
    let site = parts.host.strip_prefix("www.").unwrap_or(&parts.host);
    if site.is_empty() {
        SITE_LABEL_FALLBACK.to_string()
    } else {
        site.to_string()
    }
}

/// `https://www.instagram.com/{username}/` when `is_username(username)`, else `None`.
pub fn profile_url_for(username: &str) -> Option<String> {
    is_username(username).then(|| format!("https://www.instagram.com/{username}/"))
}

/// `https://www.instagram.com/p/{shortcode}/` when `is_shortcode(shortcode)`, else `None`.
pub fn post_url_for(shortcode: &str) -> Option<String> {
    is_shortcode(shortcode).then(|| format!("https://www.instagram.com/p/{shortcode}/"))
}

/// The clone's "fromInstagram" test for an entry (spec-export C.4): true when the entry has no
/// permalink (`None` or ""), or its (sanitised) permalink is on an Instagram host.
pub fn is_from_instagram(permalink: Option<&str>) -> bool {
    match permalink {
        None | Some("") => true,
        Some(link) => parse_http_url(link).is_some_and(|parts| is_instagram_host(&parts.host)),
    }
}

/// The profile link the reader invents for an entry of an EXPORT whose `profile_url` is absent
/// or was rejected (manifest.ts:483-485): `profile_url_for(username)` when
/// `is_from_instagram(permalink)`, else `None` (the author of a post on another site gets no
/// Instagram profile).
///
/// When NOT to call this (spec-viewer 14.4): for a loose picture. Its author is the default
/// username, which is not an Instagram account; `catalog` leaves `profile_url` empty, so the
/// sheet shows no "View profile on Instagram" and nothing is exported.
pub fn guessed_profile_url(username: &str, permalink: Option<&str>) -> Option<String> {
    if is_from_instagram(permalink) {
        profile_url_for(username)
    } else {
        None
    }
}

/// The shortcode in the path of a (sanitised) post link on an Instagram host, the clone's
/// `shortcodeFromUrl`: the first segment that is a shortcode and directly follows a segment that
/// is one of `POST_SEGMENTS` (any case) (spec-export C.4 "givenId"). `None` for a link on
/// another site, whose path says nothing about Instagram.
pub fn shortcode_from_permalink(permalink: &str) -> Option<String> {
    let parts = parse_http_url(permalink).filter(|parts| is_instagram_host(&parts.host))?;
    let segments = parts.segments();
    segments
        .windows(2)
        .find(|pair| is_one_of(&POST_SEGMENTS, pair[0]) && is_shortcode(pair[1]))
        .map(|pair| pair[1].to_string())
}

/// The clone's `usernameFromProfileUrl`: the username of a (sanitised) profile link on an
/// Instagram host, i.e. its only path segment when that is a profile segment. The reader uses it
/// for the default user of an export whose `source.url` is a profile. `None` for any other link.
pub fn username_from_profile_url(url: &str) -> Option<String> {
    let parts = parse_http_url(url).filter(|parts| is_instagram_host(&parts.host))?;
    match parts.segments().as_slice() {
        [only] if is_profile_segment(only) => Some((*only).to_string()),
        _ => None,
    }
}

// --- The validator's rules (what may be written into an export) ------------------------------

/// The validator's `igPath`: the path segments of an https link on instagram.com, or `None` for
/// anything else. The link must be at most 2048 UTF-16 code units long and match
///
/// ```text
/// ^https:\/\/([A-Za-z0-9.-]+)([/?#][^\s\\\x00-\x1f\x7f-\x9f]*)?$
/// ```
///
/// (lower-case "https" exactly, no port, no credentials; `\s` is `text::is_js_space`), its host
/// (lower-cased) must be one of `INSTAGRAM_HOSTS`, and the segments are the part of group 2
/// before the first "?" or "#", split on "/", empty parts removed. `None` when any segment is
/// ".", "..", "%2e" or a mix of those up to two (`^(?:\.|%2e){1,2}$`, any letter case).
///
/// The value is taken as it is: unlike the reader's rules, nothing is trimmed or normalised.
pub fn validator_instagram_path(value: &str) -> Option<Vec<String>> {
    if utf16_len(value) > MAX_URL_CHARS {
        return None;
    }
    let rest = value.strip_prefix("https://")?;
    let host_end = rest
        .find(|character: char| !(character.is_ascii_alphanumeric() || character == '.' || character == '-'))
        .unwrap_or(rest.len());
    let (host, tail) = rest.split_at(host_end);
    if host.is_empty() || !is_instagram_host(host) {
        return None;
    }
    // After the host: nothing, or "/", "?" or "#" and then no white space, backslash or control.
    if !tail.is_empty() && !tail.starts_with(['/', '?', '#']) {
        return None;
    }
    let forbidden = |character: char| {
        is_js_space(character) || character == '\\' || character < ' ' || ('\u{7F}'..='\u{9F}').contains(&character)
    };
    if tail.chars().any(forbidden) {
        return None;
    }
    let path = &tail[..tail.find(['?', '#']).unwrap_or(tail.len())];
    let segments: Vec<&str> = path.split('/').filter(|segment| !segment.is_empty()).collect();
    if segments.iter().any(|segment| is_single_dot(segment) || is_double_dot(segment)) {
        return None;
    }
    Some(segments.into_iter().map(str::to_string).collect())
}

/// Runs one of the validator's path rules over the segments of `value`.
fn validator_accepts(value: &str, fits: impl Fn(&[&str]) -> bool) -> bool {
    validator_instagram_path(value).is_some_and(|segments| {
        let segments: Vec<&str> = segments.iter().map(String::as_str).collect();
        fits(&segments)
    })
}

/// The validator's `isPostUrl`: `validator_instagram_path` and `is_post_path`. `export` writes
/// the `url` of an entry only when this holds for the post's permalink.
pub fn is_post_url(value: &str) -> bool {
    validator_accepts(value, is_post_path)
}

/// The validator's `isProfileUrl`: `validator_instagram_path` with exactly one segment that is
/// a profile segment. `export` writes `user.profile_url` only when this holds.
pub fn is_profile_url(value: &str) -> bool {
    validator_accepts(value, |segments| segments.len() == 1 && is_profile_segment(segments[0]))
}

/// The validator's `isSourceUrl`: `validator_instagram_path` with no segments, a post path, a
/// first segment "explore" (any case) or a first segment that is a profile segment. The app
/// never writes `source.url` (the export's source is always `{"type": "posts"}`); the function
/// exists so tests can pin the three validator rules together.
pub fn is_source_url(value: &str) -> bool {
    validator_accepts(value, is_source_path)
}

// --- Links of hashtags and mentions ---------------------------------------------------------

/// JavaScript's `encodeURIComponent`: every UTF-8 byte is written as `%XX` (upper-case hex)
/// except the unreserved characters `A-Z a-z 0-9 - _ . ! ~ * ' ( )`.
pub fn encode_uri_component(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// `https://www.instagram.com/explore/tags/{encode_uri_component(tag)}/` (spec-edit 7.4).
pub fn hashtag_url(tag: &str) -> String {
    format!("https://www.instagram.com/explore/tags/{}/", encode_uri_component(tag))
}

/// `https://www.instagram.com/{encode_uri_component(name)}/` (spec-edit 7.4).
pub fn mention_url(name: &str) -> String {
    format!("https://www.instagram.com/{}/", encode_uri_component(name))
}

#[cfg(test)]
mod tests {
    //! The vectors of the clone's urls.test.ts and of the link tests in importedPosts.test.ts and
    //! manifest.test.ts, and the link rules of the validator (instagram.md section 9).

    use super::*;

    const WRONG_PAGE: Result<String, LinkRejection> = Err(LinkRejection::WrongInstagramPage);
    const NO_LINK: Result<String, LinkRejection> = Err(LinkRejection::NotAnHttpLink);

    fn kept(link: &str) -> Result<String, LinkRejection> {
        Ok(link.to_string())
    }

    // --- sanitizeInstagramUrl -------------------------------------------------------------------

    #[test]
    fn instagram_links_are_rebuilt_as_https_without_credentials_and_port() {
        let accepted = [
            ("https://www.instagram.com/p/DBx1abcDEF/", "https://www.instagram.com/p/DBx1abcDEF/"),
            ("https://instagram.com/natgeo/", "https://instagram.com/natgeo/"),
            ("http://www.instagram.com/natgeo/", "https://www.instagram.com/natgeo/"),
            ("  https://www.instagram.com/reel/Cabc123/  ", "https://www.instagram.com/reel/Cabc123/"),
            ("HTTPS://WWW.INSTAGRAM.COM/p/AbC/", "https://www.instagram.com/p/AbC/"),
            ("https://m.instagram.com/natgeo/", "https://m.instagram.com/natgeo/"),
            ("https://instagr.am/p/abc/", "https://instagr.am/p/abc/"),
            ("https://www.instagram.com/p/abc/?img_index=2#x", "https://www.instagram.com/p/abc/?img_index=2#x"),
            // Credentials and ports are dropped rather than passed on.
            ("https://user:secret@www.instagram.com/p/abc/", "https://www.instagram.com/p/abc/"),
            ("https://www.instagram.com:8443/p/abc/", "https://www.instagram.com/p/abc/"),
            ("http://www.instagram.com:80/p/abc/", "https://www.instagram.com/p/abc/"),
            ("http://www.instagram.com/p/abc/", "https://www.instagram.com/p/abc/"),
            ("https://a:b@m.instagram.com:444/x?y=1", "https://m.instagram.com/x?y=1"),
            ("https://www.instagram.com", "https://www.instagram.com/"),
        ];
        for (input, expected) in accepted {
            assert_eq!(sanitize_instagram_url(input).as_deref(), Some(expected), "{input}");
            // sanitizeWebUrl returns the Instagram result when there is one.
            assert_eq!(sanitize_link(input).as_deref(), Some(expected), "{input}");
        }
    }

    #[test]
    fn everything_else_is_no_instagram_link() {
        let long = format!("https://www.instagram.com/{}", "a".repeat(3000));
        let rejected = [
            "javascript:alert(1)",
            "JaVaScRiPt:alert(document.cookie)",
            "javascript://www.instagram.com/%0aalert(1)",
            "data:text/html,<script>alert(1)</script>",
            "vbscript:msgbox(1)",
            "blob:https://www.instagram.com/123",
            "file:///etc/passwd",
            "ftp://www.instagram.com/p/abc/",
            "//www.instagram.com/p/abc/",
            "//evil.example/p/abc/",
            "/p/abc/",
            "www.instagram.com/p/abc/",
            "instagram.com",
            "https://instagram.com.evil.tld/p/abc/",
            "https://evil-instagram.com/p/abc/",
            "https://evilinstagram.com/",
            "https://instagram.com.evil.tld",
            // Subdomains are other services: an open redirector, OAuth consent pages, the help centre.
            "https://l.instagram.com/?u=https%3A%2F%2Fevil.example%2Fphish&e=AT0signature",
            "https://L.Instagram.com/?u=https%3A%2F%2Fevil.example",
            "https://api.instagram.com/oauth/authorize?client_id=1&redirect_uri=https://evil.example",
            "https://help.instagram.com/123",
            "https://evil.instagram.com/x",
            "https://www.www.instagram.com/p/abc/",
            "https://www.instagr.am/p/abc/",
            "https://www.instagram.com@evil.tld/p/abc/",
            "https://www.instagram.com:pass@evil.tld/",
            "https://evil.tld/www.instagram.com/p/abc/",
            "https://evil.tld/?next=https://www.instagram.com/",
            "https://evil.tld#https://www.instagram.com/",
            "https://www.instagram.com\\@evil.tld/",
            "https://evil.tld\\.instagram.com/",
            "https://instagram.com%2eevil.tld/",
            "https://www.instagram.com.",
            "https://instagr.am.evil.tld/",
            "https://xinstagr.am/",
            "https://www.instagr.am.evil/",
            "https://instagr\u{0430}m.com/p/abc/", // Cyrillic "а"
            "https://www.instagram.com/p/abc/\njavascript:alert(1)",
            "java\tscript:alert(1)",
            "https://www.instagram.com/ p/abc/",
            "https://",
            "https:///p/abc",
            "https://[::1]/",
            "https://127.0.0.1/",
            "",
            "   ",
            long.as_str(),
        ];
        for input in rejected {
            assert_eq!(sanitize_instagram_url(input), None, "{input}");
        }
    }

    // --- sanitizePostUrl ------------------------------------------------------------------------

    #[test]
    fn post_links_on_instagram_must_lead_to_a_post() {
        let accepted = [
            ("https://www.instagram.com/p/DBx1abcDEF/", "https://www.instagram.com/p/DBx1abcDEF/"),
            ("http://instagram.com/p/DBx1abcDEF", "https://instagram.com/p/DBx1abcDEF"),
            ("https://www.instagram.com/reel/Cxyz_-9/?igsh=1", "https://www.instagram.com/reel/Cxyz_-9/?igsh=1"),
            ("https://www.instagram.com/reels/Cxyz/", "https://www.instagram.com/reels/Cxyz/"),
            ("https://www.instagram.com/tv/Babc/", "https://www.instagram.com/tv/Babc/"),
            ("https://www.instagram.com/natgeo/p/DBx1abcDEF/", "https://www.instagram.com/natgeo/p/DBx1abcDEF/"),
            ("https://www.instagram.com/share/reel/BAxyz/", "https://www.instagram.com/share/reel/BAxyz/"),
            ("https://www.instagram.com/p/DBx1abcDEF/?img_index=2", "https://www.instagram.com/p/DBx1abcDEF/?img_index=2"),
            ("https://instagr.am/p/abc/", "https://instagr.am/p/abc/"),
            // manifest.test.ts and importedPosts.test.ts.
            ("http://instagram.com/p/FXTlen002/", "https://instagram.com/p/FXTlen002/"),
            ("https://www.instagram.com/alice/reel/Cabc123/?igsh=1", "https://www.instagram.com/alice/reel/Cabc123/?igsh=1"),
            ("http://user:pw@www.instagram.com:8443/p/DBx1abcDEF/", "https://www.instagram.com/p/DBx1abcDEF/"),
        ];
        for (input, expected) in accepted {
            assert_eq!(sanitize_post_url(input), kept(expected), "{input}");
        }

        let wrong_page = [
            "https://www.instagram.com/",
            "https://www.instagram.com/natgeo/",
            "https://www.instagram.com/alice/",
            "https://www.instagram.com/p/",
            "https://www.instagram.com/p/has%20space/",
            "https://www.instagram.com/accounts/logout/",
            "https://www.instagram.com/accounts/p/abc/",
            "https://www.instagram.com/accounts/login/?next=/p/abc/",
            "https://www.instagram.com/accounts/login/?next=%2Fp%2FDBx1abcDEF%2F",
            "https://www.instagram.com/oauth/authorize?client_id=1",
            "https://www.instagram.com/linkshim/?u=https%3A%2F%2Fevil.example",
            "https://www.instagram.com/explore/tags/p/abc/",
            "https://www.instagram.com/natgeo/../accounts/logout/",
        ];
        for input in wrong_page {
            assert_eq!(sanitize_post_url(input), WRONG_PAGE, "{input}");
        }

        for input in [
            "javascript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "data:text/html,x",
            "ftp://example.com/alice",
            "//example.com/post/1",
            "//www.instagram.com/p/DBx1abcDEF/",
            "/p/DBx1abcDEF/",
            "not a link",
            "",
        ] {
            assert_eq!(sanitize_post_url(input), NO_LINK, "{input}");
        }
    }

    #[test]
    fn post_links_on_other_sites_are_kept() {
        let accepted = [
            ("https://example.com/posts/42", "https://example.com/posts/42"),
            ("https://Mastodon.Social/@alice/1123?x=1#c", "https://mastodon.social/@alice/1123?x=1#c"),
            ("http://blog.example.org:8080/2024/hello", "http://blog.example.org:8080/2024/hello"),
            ("https://user:secret@example.com/p/abc/", "https://example.com/p/abc/"),
            // Not Instagram's own pages, so the Instagram shape rules do not apply to them.
            ("https://l.instagram.com/p/abc/", "https://l.instagram.com/p/abc/"),
            ("https://evil.tld/p/abc/", "https://evil.tld/p/abc/"),
            ("https://mastodon.social/@alice/1123", "https://mastodon.social/@alice/1123"),
            ("https://l.instagram.com/?u=https%3A%2F%2Fexample.com%2F", "https://l.instagram.com/?u=https%3A%2F%2Fexample.com%2F"),
            ("http://localhost:3000/post/1", "http://localhost:3000/post/1"),
            // A look-alike host is an ordinary link to another site (spec-export C.7).
            ("https://www.instagram.com.evil.example/p/X/", "https://www.instagram.com.evil.example/p/X/"),
        ];
        for (input, expected) in accepted {
            assert_eq!(sanitize_post_url(input), kept(expected), "{input}");
            // The same link is fine as a profile link and as a source link.
            assert_eq!(sanitize_profile_url(input), kept(expected), "{input}");
            assert_eq!(sanitize_source_url(input), kept(expected), "{input}");
        }
    }

    #[test]
    fn dot_segments_are_resolved_before_the_page_is_judged() {
        // What the browser would open decides, not what the text looks like.
        assert_eq!(sanitize_post_url("https://www.instagram.com/p/abc/../../accounts/logout/"), WRONG_PAGE);
        assert_eq!(sanitize_post_url("https://www.instagram.com/p/abc/%2e%2E/%2e%2e/accounts/logout"), WRONG_PAGE);
        assert_eq!(sanitize_profile_url("https://www.instagram.com/natgeo/.."), WRONG_PAGE);
        assert_eq!(sanitize_post_url("https://www.instagram.com/accounts/../p/abc/"), kept("https://www.instagram.com/p/abc/"));
        assert_eq!(sanitize_profile_url("https://www.instagram.com/./natgeo/."), kept("https://www.instagram.com/natgeo/"));
        assert_eq!(sanitize_link("https://example.com/a/../b/./c/..").as_deref(), Some("https://example.com/b/"));
        assert_eq!(sanitize_link("https://example.com/a/%2e%2E/b/%2e").as_deref(), Some("https://example.com/b/"));
        assert_eq!(sanitize_link("https://example.com/../..").as_deref(), Some("https://example.com/"));
    }

    // --- sanitizeProfileUrl and sanitizeSourceUrl -----------------------------------------------

    #[test]
    fn profile_links_on_instagram_are_profile_pages_only() {
        assert_eq!(sanitize_profile_url("http://instagram.com/alice.real/"), kept("https://instagram.com/alice.real/"));
        assert_eq!(sanitize_profile_url("https://www.instagram.com/natgeo"), kept("https://www.instagram.com/natgeo"));
        assert_eq!(
            sanitize_profile_url("https://www.instagram.com/natgeo/?hl=en"),
            kept("https://www.instagram.com/natgeo/?hl=en")
        );
        for input in [
            "https://www.instagram.com/",
            "https://www.instagram.com/p/abc/",
            "https://www.instagram.com/reel/DBx1abcDEF/",
            "https://www.instagram.com/accounts/logout/",
            "https://www.instagram.com/accounts/",
            "https://www.instagram.com/logout/",
            "https://www.instagram.com/oauth/",
            "https://www.instagram.com/natgeo/saved/",
            "https://www.instagram.com/accounts/login/?next=%2Fp%2FDBx1abcDEF%2F",
            "https://www.instagram.com/EXPLORE/",
            "https://www.instagram.com/name-with-dash/",
        ] {
            assert_eq!(sanitize_profile_url(input), WRONG_PAGE, "{input}");
        }
        assert_eq!(sanitize_profile_url("javascript:alert(1)"), NO_LINK);
        assert_eq!(sanitize_profile_url("natgeo"), NO_LINK);
        // A profile on another site.
        assert_eq!(sanitize_profile_url("https://mastodon.social/@alice"), kept("https://mastodon.social/@alice"));
        assert_eq!(sanitize_profile_url("https://evil.tld/natgeo/"), kept("https://evil.tld/natgeo/"));
    }

    #[test]
    fn source_links_are_the_pages_an_export_is_taken_from() {
        for input in [
            "https://www.instagram.com/",
            "https://www.instagram.com/natgeo/",
            "https://www.instagram.com/alice/",
            "https://www.instagram.com/natgeo/saved/",
            "https://www.instagram.com/natgeo/tagged/",
            "https://www.instagram.com/p/DBx1abcDEF/",
            "https://www.instagram.com/reel/DBx1abcDEF/",
            "https://www.instagram.com/explore/tags/sourdough/",
            "https://www.instagram.com/explore/tags/sunset/",
            "https://www.instagram.com/explore/locations/213385402/paris-france/",
            "https://www.instagram.com/fixture.sunrise.bakery/",
        ] {
            assert_eq!(sanitize_source_url(input), kept(input), "{input}");
        }
        for input in [
            "https://www.instagram.com/accounts/logout/",
            "https://www.instagram.com/accounts/edit/",
            "https://www.instagram.com/oauth/authorize?client_id=1",
            "https://www.instagram.com/linkshim/?u=https%3A%2F%2Fevil.example",
            "https://www.instagram.com/direct/inbox/",
            "https://www.instagram.com/accounts/login/?next=%2Fp%2FDBx1abcDEF%2F",
        ] {
            assert_eq!(sanitize_source_url(input), WRONG_PAGE, "{input}");
        }
        assert_eq!(sanitize_source_url("javascript:alert(1)"), NO_LINK);
        assert_eq!(sanitize_source_url("feed"), NO_LINK);
        // A page on another site.
        for input in ["https://example.com/", "https://www.reddit.com/r/pics/", "http://localhost:3000/feed"] {
            assert_eq!(sanitize_source_url(input), kept(input), "{input}");
        }
    }

    // --- sanitizeWebUrl -------------------------------------------------------------------------

    #[test]
    fn links_on_any_site_keep_scheme_and_port_and_lose_credentials() {
        assert_eq!(sanitize_link("https://example.com/a?b=1#c").as_deref(), Some("https://example.com/a?b=1#c"));
        assert_eq!(sanitize_link("HTTP://Example.COM:81/x").as_deref(), Some("http://example.com:81/x"));
        assert_eq!(sanitize_link("https://a:b@example.com/x").as_deref(), Some("https://example.com/x"));
        assert_eq!(sanitize_link("https://127.0.0.1:5000/x").as_deref(), Some("https://127.0.0.1:5000/x"));
        // Instagram's own hosts are still normalised exactly as before.
        assert_eq!(sanitize_link("http://www.instagram.com:80/p/abc/").as_deref(), Some("https://www.instagram.com/p/abc/"));
        assert_eq!(sanitize_link("http://instagram.com/p/FXTlen002/").as_deref(), Some("https://instagram.com/p/FXTlen002/"));

        let long = format!("https://example.com/{}", "a".repeat(3000));
        for input in [
            "javascript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "file:///etc/passwd",
            "ftp://example.com/x",
            "//example.com/x",
            "/relative/path",
            "example.com/x",
            "https://",
            "https:///x",
            "https://example.com/a b",
            "https://example.com\\@evil.tld/",
            "https://example.com/a\nb",
            "https://example.com/a\u{0085}b",
            "https://example.com/a\u{007F}b",
            "",
            long.as_str(),
        ] {
            assert_eq!(sanitize_link(input), None, "{input:?}");
        }
    }

    #[test]
    fn the_length_limit_counts_utf16_code_units_after_trimming() {
        let prefix = "https://example.com/";
        let fits = format!("{prefix}{}", "a".repeat(MAX_URL_CHARS - prefix.len()));
        assert_eq!(sanitize_link(&fits).as_deref(), Some(fits.as_str()));
        assert_eq!(sanitize_link(&format!("  {fits}\n")).as_deref(), Some(fits.as_str()));
        assert_eq!(sanitize_link(&format!("{fits}a")), None);
        // A lemon is two code units: 1014 of them fill the 2028 units that are left, 1015 do not.
        assert!(sanitize_link(&format!("{prefix}{}", "🍋".repeat(1014))).is_some());
        assert!(sanitize_link(&format!("{prefix}{}", "🍋".repeat(1015))).is_none());
    }

    #[test]
    fn urls_are_parsed_like_a_browser_does() {
        // Each expectation is what `new URL(input)` reports in Node (protocol, host, pathname, search, hash).
        let parts = |input: &str| -> String {
            let parts = parse_http_url(input).unwrap_or_else(|| panic!("{input} was not parsed"));
            let port = parts.port.map(|port| format!(":{port}")).unwrap_or_default();
            format!("{} {}{port} {} {} {}", parts.scheme, parts.host, parts.path, parts.query, parts.fragment)
        };
        assert_eq!(parts("https://example.com"), "https example.com /  ");
        assert_eq!(parts("https://example.com?x"), "https example.com / ?x ");
        assert_eq!(parts("https://example.com#"), "https example.com /  ");
        assert_eq!(parts("https://example.com/?"), "https example.com /  ");
        assert_eq!(parts("https://example.com//a//"), "https example.com //a//  ");
        assert_eq!(parts("https://EXAMPLE.com:0443/x"), "https example.com /x  ");
        assert_eq!(parts("https://example.com:/x"), "https example.com /x  ");
        assert_eq!(parts("http://example.com:80/x"), "http example.com /x  ");
        assert_eq!(parts("http://example.com:443/x"), "http example.com:443 /x  ");
        assert_eq!(parts("https://a..b/"), "https a..b /  ");
        assert_eq!(parts("https://www.instagram.com./p/x/"), "https www.instagram.com. /p/x/  ");
        assert_eq!(parts("https://a_b.example.com/"), "https a_b.example.com /  ");
        assert_eq!(parts("https://-a.b-/"), "https -a.b- /  ");
        assert_eq!(parts("https://xn--bcher-kva.example/"), "https xn--bcher-kva.example /  ");
        assert_eq!(parts("https://user@example.com/"), "https example.com /  ");
        assert_eq!(parts("https://a@b@example.com/"), "https example.com /  ");
        assert_eq!(parts("https://1.2.3.4:80/"), "https 1.2.3.4:80 /  ");
        assert_eq!(parts("https://example.com/a?b?c#d#e"), "https example.com /a ?b?c #d#e");
        assert_eq!(parts("https://example.com/%zz%"), "https example.com /%zz%  ");
        assert_eq!(parts("https://example.com/~!$&()*+,;=:@[]"), "https example.com /~!$&()*+,;=:@[]  ");
        assert_eq!(parts("https://example.com/é/ü?ä#ö"), "https example.com /%C3%A9/%C3%BC ?%C3%A4 #%C3%B6");
        assert_eq!(parts("https://example.com/a\u{00A0}b"), "https example.com /a%C2%A0b  ");
        assert_eq!(
            parts("https://example.com/a^b{c}|d`e\"f<g>h'i?q^{}|`\"<>'#f^{}|`\"<>'"),
            "https example.com /a%5Eb%7Bc%7D|d%60e%22f%3Cg%3Eh'i ?q^{}|`%22%3C%3E%27 #f^{}|%60%22%3C%3E'"
        );
        assert_eq!(parts("https://[::1]:8080/x"), "https [::1]:8080 /x  ");
        assert_eq!(parts("https://[2001:DB8:0:0:1:0:0:1]/"), "https [2001:db8::1:0:0:1] /  ");
        assert_eq!(parts("https://[::ffff:1.2.3.4]/"), "https [::ffff:102:304] /  ");
        assert_eq!(parts("https://[::]/"), "https [::] /  ");
        assert_eq!(parts("https://[1:0:0:2:0:0:0:3]/"), "https [1:0:0:2::3] /  ");
        assert_eq!(parts("https://[1:0:2:0:3:0:4:0]/"), "https [1:0:2:0:3:0:4:0] /  ");
    }

    #[test]
    fn links_a_browser_refuses_or_reads_in_a_surprising_way_are_refused() {
        for input in [
            // Refused by the browser as well.
            "https://example.com:65536/",
            "https://example.com:8a/",
            "https://example.com:80:80/",
            "https://1.2.3.4.5/",
            "https://foo.123/",
            "https://256.1.1.1/",
            "https://a%b/",
            "https://[::1/",
            "https://[::1]x/",
            "https://[zz::1]/",
            "https://exa mple.com/",
            "https://@/",
            "https://:80/",
            // Read by the browser as another host than the text shows: not taken here.
            "https://1.2.3/",
            "https://0x7f.1/",
            "https://1.2.3.4./",
            "https://01.2.3.4/",
            "https://2130706433/",
            "https://exa%6dple.com/",
            "https://b\u{00FC}cher.example/",
            "https://example.com\u{3002}evil/",
        ] {
            assert_eq!(parse_http_url(input), None, "{input:?}");
        }
    }

    // --- Small rules ----------------------------------------------------------------------------

    #[test]
    fn hosts_usernames_and_shortcodes() {
        for host in ["instagram.com", "www.instagram.com", "m.instagram.com", "instagr.am", "WWW.Instagram.COM"] {
            assert!(is_instagram_host(host), "{host}");
        }
        for host in ["l.instagram.com", "instagram.com.", "www.instagr.am", "example.com", "", "instagram.com.evil.tld"] {
            assert!(!is_instagram_host(host), "{host}");
        }

        assert!(is_username("natgeo"));
        assert!(is_username("a.b_c9"));
        assert!(is_username(&"x".repeat(30)));
        let too_long = "x".repeat(31);
        for name in [too_long.as_str(), "", "has space", "../../evil", "evil.tld/x", "name?x=1", "ünïcode", "a-b", "natgeo\n"] {
            assert!(!is_username(name), "{name:?}");
        }

        assert!(is_shortcode("DBx1abc-_F"));
        assert!(is_shortcode(&"x".repeat(64)));
        let too_long = "x".repeat(65);
        for code in [too_long.as_str(), "", "a/b", "a.b", "has%20space", "ü"] {
            assert!(!is_shortcode(code), "{code:?}");
        }

        assert!(is_profile_segment("natgeo"));
        assert!(is_profile_segment("explorer"));
        for segment in RESERVED_SEGMENTS {
            assert!(!is_profile_segment(segment), "{segment}");
            assert!(!is_profile_segment(&segment.to_uppercase()), "{segment}");
        }
    }

    #[test]
    fn post_paths() {
        let post_paths: &[&[&str]] = &[
            &["p", "X"],
            &["reel", "X"],
            &["reels", "X"],
            &["tv", "X"],
            &["P", "X"],
            &["natgeo", "p", "X"],
            &["share", "reel", "X"],
            &["SHARE", "Reel", "X"],
            &["p", "X", "liked_by"],
            &["natgeo", "p", "X", "embed"],
            // Two segments are never "below a name": this is /p/<shortcode "p">/.
            &["p", "p"],
            // With three segments the first one is skipped only when it is a name or "share".
            &["p", "X", "p"],
        ];
        for path in post_paths {
            assert!(is_post_path(path), "{path:?}");
        }
        let other_paths: &[&[&str]] = &[
            &[],
            &["p"],
            &["alice"],
            &["explore"],
            &["alice", "p"],
            &["accounts", "p", "X"],
            &["explore", "tags", "p", "X"],
            &["natgeo", "saved", "X"],
            &["p", "has%20space"],
            &["stories", "p", "X"],
        ];
        for path in other_paths {
            assert!(!is_post_path(path), "{path:?}");
        }
    }

    #[test]
    fn names_the_site_a_link_leads_to() {
        assert_eq!(site_label("https://www.instagram.com/p/abc/"), "Instagram");
        assert_eq!(site_label("https://instagr.am/p/abc/"), "Instagram");
        assert_eq!(site_label("https://www.example.com/posts/1"), "example.com");
        assert_eq!(site_label("https://WWW.Example.com/posts/1"), "example.com");
        assert_eq!(site_label("https://example.com/p/abc/"), "example.com");
        assert_eq!(site_label("https://mastodon.social/@alice"), "mastodon.social");
        assert_eq!(site_label("http://localhost:3000/"), "localhost");
        assert_eq!(site_label("https://l.instagram.com/?u=x"), "l.instagram.com");
        assert_eq!(site_label("https://www./"), "the original");
        assert_eq!(site_label("nonsense"), "the original");
        assert_eq!(site_label(""), "the original");
    }

    #[test]
    fn derives_instagram_links_only_from_plausible_names() {
        assert_eq!(profile_url_for("natgeo").as_deref(), Some("https://www.instagram.com/natgeo/"));
        assert_eq!(profile_url_for("a.b_c9").as_deref(), Some("https://www.instagram.com/a.b_c9/"));
        assert!(profile_url_for(&"x".repeat(30)).is_some());
        let too_long = "x".repeat(31);
        for name in [too_long.as_str(), "", "has space", "../../evil", "evil.tld/x", "name?x=1", "ünïcode"] {
            assert_eq!(profile_url_for(name), None, "{name:?}");
        }
        assert_eq!(post_url_for("DBx1abc-_F").as_deref(), Some("https://www.instagram.com/p/DBx1abc-_F/"));
        assert_eq!(post_url_for("a/b"), None);
        assert_eq!(post_url_for(""), None);
    }

    #[test]
    fn tells_whether_an_entry_is_from_instagram() {
        assert!(is_from_instagram(None));
        assert!(is_from_instagram(Some("")));
        assert!(is_from_instagram(Some("https://www.instagram.com/p/abc/")));
        assert!(is_from_instagram(Some("https://instagr.am/p/abc/")));
        assert!(!is_from_instagram(Some("https://l.instagram.com/?u=x")));
        assert!(!is_from_instagram(Some("https://example.com/")));
        assert!(!is_from_instagram(Some("nonsense")));

        // manifest.test.ts: a profile link is guessed for Instagram posts only.
        assert_eq!(guessed_profile_url("alice", None).as_deref(), Some("https://www.instagram.com/alice/"));
        assert_eq!(
            guessed_profile_url("alice", Some("https://www.instagram.com/p/Cabc123/")).as_deref(),
            Some("https://www.instagram.com/alice/")
        );
        assert_eq!(guessed_profile_url("alice", Some("https://example.com/p/abc/")), None);
        assert_eq!(guessed_profile_url("alice", Some("https://mastodon.social/@alice/1123")), None);
        assert_eq!(guessed_profile_url("not a name", None), None);
    }

    #[test]
    fn extracts_the_shortcode_from_post_permalinks() {
        assert_eq!(shortcode_from_permalink("https://www.instagram.com/p/DBx1abcDEF/").as_deref(), Some("DBx1abcDEF"));
        assert_eq!(shortcode_from_permalink("https://www.instagram.com/reel/Cxyz_-9/?igsh=1").as_deref(), Some("Cxyz_-9"));
        assert_eq!(shortcode_from_permalink("https://www.instagram.com/tv/Babc/").as_deref(), Some("Babc"));
        assert_eq!(shortcode_from_permalink("https://www.instagram.com/REELS/Babc/").as_deref(), Some("Babc"));
        assert_eq!(
            shortcode_from_permalink("https://www.instagram.com/natgeo/p/DBx1abcDEF/").as_deref(),
            Some("DBx1abcDEF")
        );
        // The clone looks at every pair of neighbouring segments, not only at the first "p".
        assert_eq!(shortcode_from_permalink("https://www.instagram.com/p/has%20space/p/abc/").as_deref(), Some("abc"));
        assert_eq!(shortcode_from_permalink("https://www.instagram.com/natgeo/"), None);
        assert_eq!(shortcode_from_permalink("https://www.instagram.com/p/"), None);
        assert_eq!(shortcode_from_permalink("nonsense"), None);
        // A post on another site gives no id (manifest.test.ts:589).
        assert_eq!(shortcode_from_permalink("https://example.com/p/abc/"), None);
    }

    #[test]
    fn extracts_the_username_from_profile_links_only() {
        assert_eq!(username_from_profile_url("https://www.instagram.com/natgeo/").as_deref(), Some("natgeo"));
        assert_eq!(
            username_from_profile_url("https://www.instagram.com/fixture.sunrise.bakery/").as_deref(),
            Some("fixture.sunrise.bakery")
        );
        assert_eq!(username_from_profile_url("https://www.instagram.com/explore/"), None);
        assert_eq!(username_from_profile_url("https://www.instagram.com/p/abc/"), None);
        assert_eq!(username_from_profile_url("https://www.instagram.com/"), None);
        assert_eq!(username_from_profile_url("https://mastodon.social/@alice"), None);
        assert_eq!(username_from_profile_url("https://example.com/natgeo/"), None);
    }

    // --- The validator --------------------------------------------------------------------------

    fn segments(value: &str) -> Option<Vec<String>> {
        validator_instagram_path(value)
    }

    #[test]
    fn the_validator_reads_https_links_on_instagram_only() {
        let path = |parts: &[&str]| Some(parts.iter().map(|part| part.to_string()).collect::<Vec<String>>());
        assert_eq!(segments("https://www.instagram.com/p/DBx1abcDEF/"), path(&["p", "DBx1abcDEF"]));
        assert_eq!(segments("https://instagram.com/natgeo"), path(&["natgeo"]));
        assert_eq!(segments("https://M.Instagram.com//a//b/?x=/c#/d"), path(&["a", "b"]));
        assert_eq!(segments("https://instagr.am"), path(&[]));
        assert_eq!(segments("https://www.instagram.com/"), path(&[]));
        assert_eq!(segments("https://www.instagram.com?x=1"), path(&[]));
        assert_eq!(segments("https://www.instagram.com#top"), path(&[]));
        assert_eq!(segments("https://www.instagram.com/a/...//%2e%2e%2e"), path(&["a", "...", "%2e%2e%2e"]));

        let long = format!("https://www.instagram.com/{}", "a".repeat(2048));
        let just_fits = format!("https://www.instagram.com/{}", "a".repeat(2048 - 26));
        assert!(segments(&just_fits).is_some());
        for value in [
            "http://www.instagram.com/p/abc/",
            "HTTPS://www.instagram.com/p/abc/",
            " https://www.instagram.com/p/abc/",
            "https://www.instagram.com/p/abc/ ",
            "https://www.instagram.com/p/abc/\n",
            "https://www.instagram.com:443/p/abc/",
            "https://user@www.instagram.com/p/abc/",
            "https://l.instagram.com/p/abc/",
            "https://www.instagram.com.evil.example/p/abc/",
            "https://www.instagram.com./p/abc/",
            "https://example.com/p/abc/",
            "https:///p/abc/",
            "https://",
            "//www.instagram.com/p/abc/",
            "javascript:alert(1)",
            "",
            "https://www.instagram.com/p/a b/",
            "https://www.instagram.com/p/a\u{00A0}b/",
            "https://www.instagram.com/p/a\u{FEFF}b/",
            "https://www.instagram.com/p/a\\b/",
            "https://www.instagram.com/p/a\tb/",
            "https://www.instagram.com/p/a\u{0085}b/",
            "https://www.instagram.com/p/abc/?next=a b",
            "https://www.instagram.com/p/abc/../../accounts/logout/",
            "https://www.instagram.com/./p/abc/",
            "https://www.instagram.com/p/abc/%2E%2e/",
            "https://www.instagram.com/p/abc/.%2e",
            "https://www.instagram.com/%2e/p/abc/",
            long.as_str(),
        ] {
            assert_eq!(segments(value), None, "{value:?}");
        }
    }

    #[test]
    fn the_validator_judges_post_profile_and_source_links() {
        for value in [
            "https://www.instagram.com/p/DBx1abcDEF/",
            "https://www.instagram.com/p/DBx1abcDEF",
            "https://instagram.com/reel/Cxyz_-9/?igsh=1",
            "https://m.instagram.com/reels/Cxyz/",
            "https://instagr.am/tv/Babc/",
            "https://www.instagram.com/natgeo/p/DBx1abcDEF/",
            "https://www.instagram.com/share/reel/BAxyz/",
            "https://www.instagram.com/p/DBx1abcDEF/?img_index=2#x",
        ] {
            assert!(is_post_url(value), "{value}");
            assert!(is_source_url(value), "{value}");
            assert!(!is_profile_url(value), "{value}");
        }
        for value in [
            "https://www.instagram.com/natgeo/",
            "https://www.instagram.com/natgeo",
            "https://www.instagram.com/fixture.sunrise.bakery/",
            "https://instagram.com/unknown/?hl=en",
        ] {
            assert!(is_profile_url(value), "{value}");
            assert!(is_source_url(value), "{value}");
            assert!(!is_post_url(value), "{value}");
        }
        for value in [
            "https://www.instagram.com/",
            "https://www.instagram.com",
            "https://www.instagram.com/natgeo/saved/",
            "https://www.instagram.com/explore/tags/sourdough/",
            "https://www.instagram.com/Explore/",
        ] {
            assert!(is_source_url(value), "{value}");
            assert!(!is_post_url(value), "{value}");
            assert!(!is_profile_url(value), "{value}");
        }
        for value in [
            "https://www.instagram.com/accounts/logout/",
            "https://www.instagram.com/oauth/authorize?client_id=1",
            "https://www.instagram.com/direct/inbox/",
            "https://www.instagram.com/p/",
            "https://www.instagram.com/name-with-dash/",
            // What the reader keeps but an export must not carry (spec-export G.3).
            "http://www.instagram.com/p/DBx1abcDEF/",
            "https://example.com/p/abc/",
            "https://mastodon.social/@alice/1123",
            "https://www.instagram.com.evil.example/p/X/",
            "http://localhost:3000/post/1",
            "",
        ] {
            assert!(!is_post_url(value), "{value}");
            assert!(!is_profile_url(value), "{value}");
            assert!(!is_source_url(value), "{value}");
        }
    }

    #[test]
    fn what_the_reader_accepts_on_instagram_passes_the_validator() {
        // A link the reader sanitised is what the app keeps in memory and what `export` tests.
        for input in [
            "http://instagram.com/p/FXTlen002/",
            "  https://www.instagram.com/reel/Cabc123/  ",
            "https://user:secret@www.instagram.com:8443/p/abc/?img_index=2#x",
            "https://www.instagram.com/accounts/../p/abc/",
        ] {
            let link = sanitize_post_url(input).unwrap();
            assert!(is_post_url(&link), "{input} -> {link}");
        }
        let profile = sanitize_profile_url("http://instagram.com/alice.real/").unwrap();
        assert!(is_profile_url(&profile), "{profile}");
        assert!(is_profile_url(&profile_url_for("unknown").unwrap()));
        assert!(is_post_url(&post_url_for("DBx1abc-_F").unwrap()));
    }

    // --- Hashtag and mention links --------------------------------------------------------------

    #[test]
    fn components_are_encoded_like_encode_uri_component() {
        assert_eq!(
            encode_uri_component("a b&c/d?e=f#g!~*'()-_.朝ごはん🍋é%"),
            "a%20b%26c%2Fd%3Fe%3Df%23g!~*'()-_.%E6%9C%9D%E3%81%94%E3%81%AF%E3%82%93%F0%9F%8D%8B%C3%A9%25"
        );
        assert_eq!(encode_uri_component(""), "");
        assert_eq!(encode_uri_component("AZaz09"), "AZaz09");
        assert_eq!(encode_uri_component("\n\"<>\\^`{|}"), "%0A%22%3C%3E%5C%5E%60%7B%7C%7D");
    }

    #[test]
    fn hashtags_and_mentions_link_to_instagram() {
        assert_eq!(hashtag_url("sourdough"), "https://www.instagram.com/explore/tags/sourdough/");
        assert_eq!(
            hashtag_url("朝ごはん"),
            "https://www.instagram.com/explore/tags/%E6%9C%9D%E3%81%94%E3%81%AF%E3%82%93/"
        );
        assert_eq!(mention_url("fixture_tidewater"), "https://www.instagram.com/fixture_tidewater/");
        assert_eq!(mention_url("a.b"), "https://www.instagram.com/a.b/");
        // Both are links the app itself would open.
        assert_eq!(sanitize_link(&hashtag_url("朝ごはん")), Some(hashtag_url("朝ごはん")));
        assert!(is_profile_url(&mention_url("fixture_tidewater")));
    }
}
