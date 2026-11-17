//! Results of the reference web project's own code, compared with the port in `text`, `urls`
//! and `sniff` through the public interface of the crate.
//!
//! How the vectors were made: the clone's `sniff.ts` and `urls.ts` (types stripped), the link
//! block of its export validator and verbatim copies of its private helpers (`formatCount`,
//! `captionPreview`, the hashtag pattern of `RichText`, `parseTimestamp`) were run in Node 26 on
//! generated inputs: mutated file headers, links assembled from awkward parts, texts drawn from
//! alphabets of troublesome characters. When the port was written, about 190,000 such results
//! were compared. They agreed except where the port is deliberately stricter: a host with
//! characters outside `[A-Za-z0-9._-]` is refused, and so is a date with a space instead of "T"
//! combined with the offset "+24:00", which one browser engine happens to accept. This file
//! keeps a sample of those results, so that a change to a rule shows up here.

use instagram_core::model::MediaKind;
use instagram_core::sniff::{self, MediaFormat};
use instagram_core::text::{self, CaptionLinkKind};
use instagram_core::urls::{self, LinkRejection};

#[test]
fn compact_counts() {
    let vectors: &[(u64, &str)] = &[
        (91050, "91K"),
        (4650, "4.7K"),
        (24250, "24.3K"),
        (54150, "54.1K"),
        (705250, "705.3K"),
        (5416700, "5.4M"),
        (477700, "477.7K"),
        (3432078782950, "3432078.8M"),
        (20382500, "20.4M"),
        (256479881471000, "256479881.5M"),
        (847223850, "847.2M"),
        (46050, "46K"),
        (72910600, "72.9M"),
        (542750, "542.8K"),
        (5259143689200, "5259143.7M"),
        (6500, "6.5K"),
        (6599800, "6.6M"),
        (5950, "6K"),
        (810850, "810.9K"),
        (5414006900740800, "5414006900.7M"),
        (31300, "31.3K"),
        (154000, "154K"),
        (6024298050, "6024.3M"),
        (5795474030077450, "5795474030.1M"),
        (9750, "9.8K"),
        (5729200961050, "5729201M"),
        (5858239300, "5858.2M"),
        (6601336742750, "6601336.7M"),
        (7981140502961350, "7981140503M"),
        (370000, "370K"),
    ];
    for &(count, expected) in vectors {
        assert_eq!(text::format_count(count), expected, "{count}");
    }
}

#[test]
fn caption_previews() {
    // (caption, captionPreview, the caption line of the auto view)
    let vectors: &[(&str, Option<&str>, Option<&str>)] = &[
        (
            "\u{E9},word #tag \u{FEFF}\u{2028}\nthe  \u{FEFF}\u{85} \u{2026},\u{9}b\u{2028} .\u{FEFF}a\u{FEFF}\u{85}\n\u{E9}  \u{671D}\u{2028}a,\u{FEFF}\u{0}\u{2026}\u{1F30A}\u{2026}\u{E9}thethe \u{A0}\u{E9}.\u{E9},\u{0}\u{E9}the#tag  ,\u{2026} \u{2028}\n",
            Some("\u{E9},word #tag \u{FEFF}\u{2028}\nthe  \u{FEFF}\u{85} \u{2026},\u{9}b\u{2028} .\u{FEFF}a\u{FEFF}\u{85}"),
            Some("\u{E9},word #tag \u{FEFF}\u{2028}\nthe  \u{FEFF}\u{85} \u{2026},\u{9}b\u{2028} .\u{FEFF}a\u{FEFF}\u{85}\u{2026}"),
        ),
        (
            "\u{1F30A}\u{D}\n\u{D}\n\u{1F30A}@me \u{A0} \u{FEFF}\u{9}\u{9}\u{1F34B}\u{85}\n\u{85}\u{FEFF}\u{D}\nb\u{D}\n\u{9}\u{2028}#tag\u{E9}#tag\u{9}\u{1F34B} \u{9}\u{1F30A}\u{1F30A}  #tagthe    .#tag\u{85}wordwordthe  \u{1F30A}\u{D}\n.\u{1F34B}\u{85}\u{FEFF}\u{2028}\u{1F34B}\u{2026},b\u{2028},",
            Some("\u{1F30A}"),
            Some("\u{1F30A}\u{2026}"),
        ),
        (
            "\n\n.word\u{0}  . #tag\u{9}\u{85}\u{D}\n.\u{85}. \u{A0}\u{FEFF} \u{2028}\n, .#tagathethe#tag\u{9}\u{1F30A}the\u{FEFF}\u{D}\nwordbthe\u{671D}b  @me \u{671D}word\u{85}#tagbword\n#tag#tag  .",
            Some(""),
            Some(".word\u{0}  . #tag\u{9}\u{85}\u{D}\n.\u{85}\u{2026}"),
        ),
        (
            "\u{2026}\u{0}\u{2028}bb\u{FEFF}\u{9} \u{1F30A}\u{85}\u{E9}\u{9}\u{A0}  word\u{A0}the\u{FEFF}#tag#tag\u{2026}\u{2026}\u{2028}\u{9}\u{2026}\u{671D}\u{9}word#tag\u{E9}#tagword\u{671D}@me\u{2028}b\u{FEFF}\u{671D}#tag\u{2026}the\u{1F34B}\u{2026}\u{671D}",
            None,
            Some("\u{2026}\u{0}\u{2028}bb\u{FEFF}\u{9} \u{1F30A}\u{85}\u{E9}\u{9}\u{A0}  word\u{A0}the\u{FEFF}#tag#tag\u{2026}\u{2026}\u{2028}\u{9}\u{2026}\u{671D}\u{9}word#tag\u{E9}#tagword\u{671D}@me\u{2028}b\u{FEFF}\u{671D}#tag\u{2026}the\u{1F34B}\u{2026}\u{671D}"),
        ),
        (
            "\u{A0}\u{9}#tag,\u{0}theword\u{2028}\u{2028}\u{FEFF}@meb\u{FEFF}\u{85}\u{A0}\u{FEFF}\u{671D}word\u{671D}the\u{671D}\u{E9}@me  word\u{1F30A}#tag#tagb\u{671D}   \u{FEFF}  \u{E9}\u{2028}\u{85}    a  \u{1F30A}  @me\u{671D},\u{1F30A}.wordwordthe",
            Some("\u{A0}\u{9}#tag,\u{0}theword\u{2028}\u{2028}\u{FEFF}@meb\u{FEFF}\u{85}\u{A0}\u{FEFF}\u{671D}word\u{671D}the\u{671D}\u{E9}@me  word\u{1F30A}#tag#tagb\u{671D}   \u{FEFF}  \u{E9}\u{2028}\u{85}    a  \u{1F30A}"),
            Some("#tag,\u{0}theword\u{2028}\u{2028}\u{FEFF}@meb\u{FEFF}\u{85}\u{A0}\u{FEFF}\u{671D}word\u{671D}the\u{671D}\u{E9}@me  word\u{1F30A}#tag#tagb\u{671D}   \u{FEFF}  \u{E9}\u{2028}\u{85}    a  \u{1F30A}  @me\u{671D},\u{1F30A}.wordwordthe"),
        ),
    ];
    for &(caption, preview, auto_view) in vectors {
        let ours = text::caption_preview(caption);
        assert_eq!(ours.truncated.then_some(ours.text), preview, "{caption:?}");
        if !ours.truncated {
            assert_eq!(ours.text, caption);
        }
        assert_eq!(text::auto_view_caption(caption).as_deref(), auto_view, "{caption:?}");
    }
}

#[test]
fn hashtags_and_mentions() {
    // (text, [(start byte, end byte, sign, name)])
    type Links = [(usize, usize, char, &'static str)];
    let vectors: &[(&str, &Links)] = &[
        ("\u{E48}\u{E9}", &[]),
        ("-a&", &[]),
        ("\u{1F34B} #1\u{BD}", &[(5, 9, '#', "1\u{BD}")]),
        ("\u{1F34B}\u{E48}\nxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxa\u{2019} a\n@#a", &[]),
        ("\u{24B6}@a&a 1\n\u{DF}aZ \u{671D}\n\u{2019} a#@\n\u{E9}\u{2019}.1\u{2019}\u{1F34B}Z#@b-,-\u{671D}Z", &[(3, 5, '@', "a")]),
        ("Z\n#b\n\u{E9}\u{BD} @\u{671D}&\u{DF}#\u{E9}/@\u{24B6}", &[(2, 4, '#', "b")]),
        ("1xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx/a.\u{2019} a1# @\u{2019},#Z \u{24B6}\u{E9}", &[(48, 50, '#', "Z")]),
        ("\u{1F34B}@Z @\u{2019}", &[(4, 6, '@', "Z")]),
        ("@1\u{301}#.", &[(0, 2, '@', "1")]),
        (
            "\u{E48}@a@_\u{1F34B}xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\u{E48}/& a- .\u{2019}@1",
            &[(3, 5, '@', "a"), (55, 57, '@', "1")],
        ),
        (
            "#\u{E48}1  xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\u{E9}\u{DF} @Z\u{24B6}-",
            &[(0, 5, '#', "\u{E48}1"), (43, 45, '@', "Z")],
        ),
        (
            "#\u{E9}\u{1F34B}\n\u{E48}\u{BD}\u{301}\u{24B6}_\n\u{2019}_@\u{E48}#\u{E48}.a../  ",
            &[(0, 3, '#', "\u{E9}"), (28, 32, '#', "\u{E48}")],
        ),
        ("@_b\n\u{BD}\u{E9}\u{671D}-\u{24B6}\n#Z", &[(0, 3, '@', "_b"), (16, 18, '#', "Z")]),
        (
            "Z\u{E48}#\u{E9}-#&\u{24B6} \u{1F34B}#xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx \u{BD},",
            &[(4, 7, '#', "\u{E9}"), (18, 50, '#', "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx")],
        ),
        ("\u{BD}\u{1F34B}.b\u{24B6} @\u{301}@1\u{1F34B}#a \u{24B6}/_", &[(15, 17, '@', "1"), (21, 23, '#', "a")]),
        (
            "#\u{671D}-_\u{E9},\u{E9} \u{301}a #\u{E48}\u{301}Z1@Z.\u{E9}@.@",
            &[(0, 4, '#', "\u{671D}"), (16, 24, '#', "\u{E48}\u{301}Z1")],
        ),
        (
            "#bb.,,#\u{E48}@#,b\u{BD}\u{2019}@a#\u{BD}#b @",
            &[(0, 3, '#', "bb"), (6, 10, '#', "\u{E48}"), (19, 21, '@', "a")],
        ),
    ];
    for &(input, expected) in vectors {
        let found = text::scan_caption_links(input);
        let ours: Vec<(usize, usize, char, &str)> = found
            .iter()
            .map(|link| {
                let sign = if link.kind == CaptionLinkKind::Hashtag { '#' } else { '@' };
                (link.range.start, link.range.end, sign, link.name.as_str())
            })
            .collect();
        assert_eq!(ours, expected, "{input:?}");
    }
}

/// What the clone does with a link under one of its three labels.
#[derive(Clone, Copy, Debug)]
enum Labelled {
    /// Kept, as this text.
    K(&'static str),
    /// Dropped: "... is an instagram.com link, but not to ...".
    W,
    /// Dropped: "... is not a valid http(s) link ...".
    N,
}

impl Labelled {
    fn result(self) -> Result<String, LinkRejection> {
        match self {
            Labelled::K(link) => Ok(link.to_string()),
            Labelled::W => Err(LinkRejection::WrongInstagramPage),
            Labelled::N => Err(LinkRejection::NotAnHttpLink),
        }
    }
}

#[test]
fn links() {
    use Labelled::{K, N, W};
    // (input, sanitizeWebUrl, as post link, as profile link, as source link,
    //  the validator's isPostUrl, isProfileUrl, isSourceUrl)
    type Row = (&'static str, Option<&'static str>, Labelled, Labelled, Labelled, bool, bool, bool);
    let vectors: &[Row] = &[
        ("http://www.instagram.com:0080/@alice?", Some("https://www.instagram.com/@alice"), W, W, W, false, false, false),
        (
            "HTTPS://example.com:8080/logout?next=/p/abc/#top",
            Some("https://example.com:8080/logout?next=/p/abc/#top"),
            K("https://example.com:8080/logout?next=/p/abc/#top"),
            K("https://example.com:8080/logout?next=/p/abc/#top"),
            K("https://example.com:8080/logout?next=/p/abc/#top"),
            false,
            false,
            false,
        ),
        (
            "https://www.instagram.com/tags/alice.real",
            Some("https://www.instagram.com/tags/alice.real"),
            W,
            W,
            K("https://www.instagram.com/tags/alice.real"),
            false,
            false,
            true,
        ),
        ("https:/m.instagram.com:80/#a#b", None, N, N, N, false, false, false),
        (
            "HTTPS://www.instagram.com/reel/share/p?x=1#",
            Some("https://www.instagram.com/reel/share/p?x=1"),
            K("https://www.instagram.com/reel/share/p?x=1"),
            W,
            K("https://www.instagram.com/reel/share/p?x=1"),
            false,
            false,
            false,
        ),
        (
            "https://a@b@www.instagram.com.:80/",
            Some("https://www.instagram.com.:80/"),
            K("https://www.instagram.com.:80/"),
            K("https://www.instagram.com.:80/"),
            K("https://www.instagram.com.:80/"),
            false,
            false,
            false,
        ),
        (
            "https://www.instagram.com/logout/tv/a`b/reel?x=1",
            Some("https://www.instagram.com/logout/tv/a%60b/reel?x=1"),
            W,
            W,
            W,
            false,
            false,
            false,
        ),
        ("//WWW.Instagram.com:0/?a?b", None, N, N, N, false, false, false),
        ("https://instagr.am", Some("https://instagr.am/"), W, W, K("https://instagr.am/"), false, false, true),
        (
            "https://instagram.com/alice.real/reels/saved/a|b/a|b/",
            Some("https://instagram.com/alice.real/reels/saved/a|b/a|b/"),
            K("https://instagram.com/alice.real/reels/saved/a|b/a|b/"),
            W,
            K("https://instagram.com/alice.real/reels/saved/a|b/a|b/"),
            true,
            false,
            true,
        ),
        (
            "https://instagram.com/saved#a#b",
            Some("https://instagram.com/saved#a#b"),
            W,
            K("https://instagram.com/saved#a#b"),
            K("https://instagram.com/saved#a#b"),
            false,
            true,
            true,
        ),
        (
            "https://m.instagram.com/saved/p/share",
            Some("https://m.instagram.com/saved/p/share"),
            K("https://m.instagram.com/saved/p/share"),
            W,
            K("https://m.instagram.com/saved/p/share"),
            true,
            false,
            true,
        ),
        (
            " Http://a@b@instagram.com:443/?a?b#top",
            Some("https://instagram.com/?a?b#top"),
            W,
            W,
            K("https://instagram.com/?a?b#top"),
            false,
            false,
            false,
        ),
        (
            "https://instagr.am/explore/.%2E/?x=1#top",
            Some("https://instagr.am/?x=1#top"),
            W,
            W,
            K("https://instagr.am/?x=1#top"),
            false,
            false,
            false,
        ),
        (
            "https://m.instagram.com/P/./saved",
            Some("https://m.instagram.com/P/saved"),
            K("https://m.instagram.com/P/saved"),
            W,
            K("https://m.instagram.com/P/saved"),
            false,
            false,
            false,
        ),
        (
            "https://www.instagram.com/alice.real/",
            Some("https://www.instagram.com/alice.real/"),
            W,
            K("https://www.instagram.com/alice.real/"),
            K("https://www.instagram.com/alice.real/"),
            false,
            true,
            true,
        ),
        (
            "HTTPS://www.instagram.com:443/natgeo",
            Some("https://www.instagram.com/natgeo"),
            W,
            K("https://www.instagram.com/natgeo"),
            K("https://www.instagram.com/natgeo"),
            false,
            false,
            false,
        ),
        (
            "https://www.instagram.com/tags/%2e#top",
            Some("https://www.instagram.com/tags/#top"),
            W,
            K("https://www.instagram.com/tags/#top"),
            K("https://www.instagram.com/tags/#top"),
            false,
            false,
            false,
        ),
    ];
    for &(input, web, post, profile, source, is_post, is_profile, is_source) in vectors {
        assert_eq!(urls::sanitize_link(input).as_deref(), web, "{input:?}");
        assert_eq!(urls::sanitize_post_url(input), post.result(), "{input:?} as a post link");
        assert_eq!(urls::sanitize_profile_url(input), profile.result(), "{input:?} as a profile link");
        assert_eq!(urls::sanitize_source_url(input), source.result(), "{input:?} as a source link");
        assert_eq!(urls::is_post_url(input), is_post, "{input:?}");
        assert_eq!(urls::is_profile_url(input), is_profile, "{input:?}");
        assert_eq!(urls::is_source_url(input), is_source, "{input:?}");
        // A sanitised link stays as it is when it is read again.
        if let Some(link) = web {
            assert_eq!(urls::sanitize_link(link).as_deref(), Some(link));
        }
    }
}

#[test]
fn timestamp_texts() {
    let vectors: &[(&str, Option<i64>)] = &[
        ("1970-08-02T05:26+0900", None),
        ("2141-02-04T11:45+24:00", None),
        ("1971-06-25t09:39:30-19:15", Some(46760070000)),
        ("2024-04-19t02:05:29Z", Some(1713492329000)),
        ("1970-08-04t02:05z", None),
        ("1800693655x", None),
        ("9999-04-01T14:07:41.86-05:60", None),
        ("2026-12-15T12:16:28.5869772Z", Some(1797336988586)),
        ("1970-03-27t16:34:01+09:22", None),
        ("1795369862000000.5", Some(1795369862000)),
        ("1969-02-23", None),
        ("1796811629000.5", Some(1796811629001)),
        ("1794518129000000.25", Some(1794518129000)),
        ("1999-04-20t00:11+13:15", Some(924519360000)),
        ("1971-10-22T09:09:43.541934231Z", Some(56970583541)),
        ("1794462195", Some(1794462195000)),
        ("1794680502000000.", None),
        ("3587-04-19T19:45:22.73z", Some(51036983122730)),
        ("1796412761000000", Some(1796412761000)),
        ("2024-11-12T18:13:14.865330404+0645", Some(1731410894865)),
        ("1798817795000.", None),
        ("1970-07-00 14:57:53.56+0700", None),
        ("8142-11-27t06:21:41.6z", Some(194798067701600)),
        ("9999-11-04T08:44Z", Some(253397321040000)),
        ("1970-07-09t25:21:22Z", None),
        ("1999-10-06t15:52:01.26200-05:60", None),
        ("2026-10-13 08:29:57.5-19:33", Some(1791950577500)),
        ("1801286417000", Some(1801286417000)),
        ("2024-06-05T00:03:32.01292842z", Some(1717545812012)),
        ("1970-02-14T08:47:20.7017-05:60", None),
        ("-5", None),
        ("1969-07-22t20:17:42+0530", None),
        ("1798920717.5", Some(1798920717500)),
        ("1999-03-18 22:60:49.8777305Z", None),
        ("2024-02-11t25:29:49.49855759+24:00", None),
        ("1970-06-99T13:25:05.994Z", None),
        ("2024-01-02T14:38+0730", Some(1704179280000)),
        ("9999-03-27T12:12:17.4661-11:55", Some(253378195637466)),
        ("1971-01-23t13:08:24.913060+0900", Some(33451704913)),
        ("1969-00-07 00:00:25.742+20:55", None),
    ];
    for &(input, expected) in vectors {
        assert_eq!(text::parse_timestamp_text(input), expected, "{input:?}");
        assert_eq!(text::parse_timestamp(&serde_json::Value::from(input)), expected, "{input:?}");
    }
}

#[test]
fn timestamp_numbers() {
    let vectors: &[(f64, Option<i64>)] = &[
        (1796384619000000000.0, Some(1796384619000)),
        (1801807894000.0, Some(1801807894000)),
        (0.5, None),
        (1801901098.1792588, Some(1801901098179)),
        (1801889256000000.2, Some(1801889256000)),
        (1794825842.0, Some(1794825842000)),
        (1802253376000000.0, Some(1802253376000)),
        (31535999.5, None),
        (10000000000.0, Some(10000000000000)),
        (10000000000000.0, Some(10000000000000)),
        (253402300799.0, Some(253402300799)),
        (100000000000000000000.0, Some(100000000000000)),
        (100000000000000.0, Some(100000000000)),
        (100000000000000000.0, Some(100000000000)),
        (1797527643000.3035, Some(1797527643000)),
        (10000000000000000.0, Some(10000000000000)),
        (0.0, None),
        (-1.0, None),
        (31536000.0, Some(31536000000)),
    ];
    for &(input, expected) in vectors {
        assert_eq!(text::epoch_number_to_ms(input), expected, "{input}");
        assert_eq!(text::parse_timestamp(&serde_json::Value::from(input)), expected, "{input}");
    }
}

fn bytes_of(hex: &str) -> Vec<u8> {
    (0..hex.len()).step_by(2).map(|index| u8::from_str_radix(&hex[index..index + 2], 16).unwrap()).collect()
}

/// The clone's name of a format (`SniffedMedia['format']`).
fn format_name(format: MediaFormat) -> &'static str {
    match format {
        MediaFormat::Jpeg => "jpeg",
        // Every other format is exported under the clone's own name for it.
        other => other.extension(),
    }
}

#[test]
fn sniffed_file_headers() {
    // (leading bytes in hexadecimal, sniffMedia format, its MIME type, describeUnsupportedMedia)
    type Row = (&'static str, Option<&'static str>, Option<&'static str>, Option<&'static str>);
    let vectors: &[Row] = &[
        ("1a45dfa385bf8c17ec820000", None, None, Some("a Matroska (MKV) video")),
        ("0000000066747970686976730000000020202020686569734d34412071742020", Some("heic"), Some("image/heic"), None),
        ("36676753", None, None, None),
        ("1a45dfa3", None, None, None),
        ("1a45dfa3400eec8200004282847765626d5f91e3000094bc", Some("webm"), Some("video/webm"), None),
        ("0000000866747970646173680000", Some("mp4"), Some("video/mp4"), None),
        ("4fdc6753", None, None, None),
        ("474946383761", Some("gif"), Some("image/gif"), None),
        ("00000008667479704d34422000000001", None, None, Some("an audio file (M4A)")),
        ("524946460102030457415645590000fc", None, None, None),
        ("00000010667479707174202000000000617669666865766d", Some("mov"), Some("video/quicktime"), None),
        ("000001b3", None, None, Some("an MPEG video")),
        ("00000014667479b04d345020000000006865696d61627f6468657678", None, None, None),
        ("89504e470d891a0a", None, None, None),
        ("00000014667479706176697300000000", Some("avif"), Some("image/avif"), None),
        ("000001ba", None, None, Some("an MPEG video")),
        ("620001b3", None, None, None),
        ("00000014667479706d7034320000000020202020", Some("mp4"), Some("video/mp4"), None),
        ("524946460102030441564920", None, None, Some("an AVI video")),
        ("89504e470d0a1a0a00000000", Some("png"), Some("image/png"), None),
        ("0000000c6674797061627f64000000006176696664617368686576636865697868657678", Some("avif"), Some("image/avif"), None),
        ("3026b2753166cf11d7000000", None, None, None),
        ("424d01020304000001003600000028000000", None, None, None),
        ("024c5601", None, None, None),
        ("474946383861", None, None, None),
        ("ffd8ffe00c00dd00", Some("jpeg"), Some("image/jpeg"), None),
        ("464c560100002600", None, None, Some("a Flash (FLV) video")),
        ("4f67675300000000", None, None, Some("an Ogg file")),
        ("6d4c560100000000", None, None, None),
        ("524946460102030457454250", Some("webp"), Some("image/webp"), None),
        ("00000000667479703367703400000000", Some("mp4"), Some("video/mp4"), None),
        ("3c73766720786d6c6e73", None, None, None),
        ("4f673f53", None, None, None),
        ("000001b0480010000003ac000000006f000024790050a900008d0000d4a5009700007a4400000000ad000000", None, None, None),
        ("000000106674797061766973000000006d7366316d696631463441204d344120", Some("avif"), Some("image/avif"), None),
        ("00000010667479706865", None, None, None),
        ("3c735e6720786d6c6e730000000e", None, None, None),
        ("47000000000000000000000000000000000000000000", None, None, None),
        ("46b95601", None, None, None),
        ("ffd895e0", None, None, None),
        ("3026b2758e66cf110000", None, None, Some("a Windows Media (WMV/ASF) video")),
        ("0000000c667479706461736800000001", Some("mp4"), Some("video/mp4"), None),
        ("00000000", None, None, None),
        ("000000ff66", None, None, None),
        ("4749db383861", None, None, None),
        ("00000010667479706865766d00000001686569f5", Some("heic"), Some("image/heic"), None),
        ("524946c8010203", None, None, None),
        ("000000186674797061766973000000024d34562061766966", Some("avif"), Some("image/avif"), None),
        ("424d01020304000000003600000038000000", Some("bmp"), Some("image/bmp"), None),
        ("ffd8ff11", Some("jpeg"), Some("image/jpeg"), None),
        ("0000012c6674797064", None, None, None),
        ("0000012c6674797061766966000000006d6966316176697368657673", Some("avif"), Some("image/avif"), None),
        ("3c73a26720", None, None, None),
        ("0000002066", None, None, None),
        ("89504e250d0a1a0a", None, None, None),
        ("5b00009a", None, None, None),
        ("464c56a3", None, None, None),
        ("ba6d917ccaac00ff000000f700007a3c003bcf00800000000000e67cee00000032000000002800fd", None, None, None),
        ("0000001c66", None, None, None),
        ("476946383861", None, None, None),
    ];
    for &(hex, format, mime, unsupported) in vectors {
        let bytes = bytes_of(hex);
        let sniffed = sniff::sniff_media(&bytes);
        assert_eq!(sniffed.map(|sniffed| format_name(sniffed.format)), format, "{hex}");
        assert_eq!(sniffed.map(|sniffed| sniffed.mime), mime, "{hex}");
        if let Some(sniffed) = sniffed {
            let is_video = sniffed.mime.starts_with("video/");
            assert_eq!(sniffed.kind == MediaKind::Video, is_video, "{hex}");
            assert_eq!(sniffed.kind, sniffed.format.kind(), "{hex}");
        }
        assert_eq!(sniff::describe_unsupported_media(&bytes), unsupported, "{hex}");
    }
}
