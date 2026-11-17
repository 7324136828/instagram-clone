//! Text, count, time and id helpers that must behave exactly like the JavaScript of the reference
//! web project, because their results are visible (labels, previews) or end up in files that the
//! clone reads again (ids, timestamps, archive names).
//!
//! The differences between JavaScript and Rust that matter here:
//!
//! * `string.length`, `slice()` and every length limit of the clone count UTF-16 code units, not
//!   bytes and not `char`s: an emoji such as "🍋" counts 2. Use `utf16_len` / `truncate_utf16`.
//! * `trim()` and `\s` strip U+FEFF and keep U+0085; Rust's `str::trim` does the opposite. Use
//!   `js_trim` and `is_js_space` for everything that mirrors the clone.
//! * `toFixed(1)` rounds the binary value of a number, with exact ties going up; neither
//!   `format!("{:.1}")` (ties to even) nor decimal integer arithmetic gives the same digits
//!   (`format_count`).
//!
//! Instants are `i64` milliseconds since the Unix epoch, in UTC. The small civil-date code here
//! replaces a date crate; the local time zone comes from one Win32 call (`local_offset_minutes`).
//!
//! Spec references: spec-viewer 8 (counts, relative time, date label, caption preview), spec-edit
//! 3.3-3.4, 7.4 and 13.1 (caption limit, trimming, hashtags and mentions), spec-export A.3, A.5,
//! A.9, C.4 (avatar stem, ISO time, archive stem, ids, timestamps).

use std::cmp::Ordering;
use std::collections::HashSet;
use std::iter::Peekable;
use std::ops::Range;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::manifest::{MAX_CAPTION_CHARS, MAX_ID_CHARS};
use crate::urls;

/// `captionPreview` shows at most this many UTF-16 code units of a caption (Media.tsx
/// `CAPTION_LIMIT`).
pub const CAPTION_PREVIEW_UNITS: usize = 96;

/// A word boundary is only used for the preview cut when it lies after this many UTF-16 code
/// units (0.6 x 96 = 57.6, so index 58 or later).
pub const CAPTION_PREVIEW_MIN_WORD_CUT: usize = 58;

/// Longest avatar file stem and longest id part of a download name, in characters (export.ts
/// `MAX_STEM_CHARS`).
pub const MAX_STEM_CHARS: usize = 64;

/// The placeholder gradients of the clone, in the order `placeholder_index` indexes them
/// (data.ts `PHOTO_KEYS`).
pub const PLACEHOLDER_KEYS: [&str; 12] = [
    "books", "finance", "moon", "floral", "sunset", "ocean", "city", "coffee", "mountains", "cat", "art", "forest",
];

// --- JavaScript whitespace and lengths ------------------------------------------------------

/// True for the characters JavaScript's `trim()` and `\s` treat as white space: U+0009-000D,
/// U+0020, U+00A0, U+1680, U+2000-200A, U+2028, U+2029, U+202F, U+205F, U+3000 and U+FEFF.
/// U+0085 is NOT one of them (spec-export G.1).
pub fn is_js_space(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'..='\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}

/// JavaScript `text.trim()`.
pub fn js_trim(text: &str) -> &str {
    text.trim_matches(is_js_space)
}

/// JavaScript `text.trimStart()`.
pub fn js_trim_start(text: &str) -> &str {
    text.trim_start_matches(is_js_space)
}

/// JavaScript `text.trimEnd()`.
pub fn js_trim_end(text: &str) -> &str {
    text.trim_end_matches(is_js_space)
}

/// JavaScript `text.length`: the number of UTF-16 code units.
pub fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// The longest prefix of `text` that is at most `max_units` UTF-16 code units long and ends on a
/// `char` boundary. It never splits a surrogate pair: a prefix that would end between the two
/// halves of an astral character stops before that character (so the result can be one unit
/// shorter than `max_units`). Returns `text` itself when it already fits.
///
/// Example: `truncate_utf16("ab🍋cd", 3)` is `"ab"`, `truncate_utf16("ab🍋cd", 4)` is `"ab🍋"`.
pub fn truncate_utf16(text: &str, max_units: usize) -> &str {
    let mut units = 0;
    for (index, character) in text.char_indices() {
        units += character.len_utf16();
        if units > max_units {
            return &text[..index];
        }
    }
    text
}

/// Line breaks as a `<textarea>` reports them: every "\r\n" and every lone "\r" becomes "\n"
/// (spec-edit 13.1 rule 2). Text without "\r" is returned unchanged.
pub fn normalize_newlines(text: &str) -> String {
    if !text.contains('\r') {
        return text.to_string();
    }
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Enforces the editor's rules on the raw field text after every change (spec-edit 13.1 rules 1
/// and 2): first `normalize_newlines`, then cut back to `manifest::MAX_CAPTION_CHARS` (10,000)
/// UTF-16 code units with `truncate_utf16`. The text is not trimmed. Returns true when `text`
/// was changed, so the UI knows that the caret may have to move.
pub fn enforce_caption_limit(text: &mut String) -> bool {
    let mut changed = false;
    if text.contains('\r') {
        *text = normalize_newlines(text);
        changed = true;
    }
    let kept_bytes = truncate_utf16(text, MAX_CAPTION_CHARS).len();
    if kept_bytes < text.len() {
        text.truncate(kept_bytes);
        changed = true;
    }
    changed
}

/// The value "Save" stores for the field text: `normalize_newlines`, then `js_trim` (spec-edit
/// 3.4). Inner white space and inner line breaks are kept. May be "".
pub fn saved_caption(field_text: &str) -> String {
    js_trim(&normalize_newlines(field_text)).to_string()
}

/// Whether "Save" is enabled: `saved_caption(field_text) != js_trim(current_caption)` (spec-edit
/// 3.5). Adding only leading or trailing white space is no change; clearing a caption is one.
pub fn caption_changed(field_text: &str, current_caption: &str) -> bool {
    saved_caption(field_text) != js_trim(current_caption)
}

/// The clone's `cleanText` for a string read from post.json (spec-export C.4): remove every
/// U+0000, `js_trim`, then `truncate_utf16` to `max_units`.
pub fn clean_text(value: &str, max_units: usize) -> String {
    let without_nul;
    let text = if value.contains('\0') {
        without_nul = value.replace('\0', "");
        without_nul.as_str()
    } else {
        value
    };
    truncate_utf16(js_trim(text), max_units).to_string()
}

// --- Caption preview ------------------------------------------------------------------------

/// What a feed card shows of a caption.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptionPreview<'a> {
    /// The text to show: the preview when `truncated`, else the whole caption as given.
    pub text: &'a str,
    /// True when the caption was shortened: the card then appends "… " and the "more" button.
    pub truncated: bool,
}

/// The clone's `captionPreview` (Media.tsx:249-272; spec-viewer 8.4, spec-edit 7.1). All indices
/// are UTF-16 code units:
///
/// ```text
/// cut = min(len, 96)
/// if the caption has a second '\n' and its index is < cut: cut = that index   (two lines at most)
/// else if cut < len:
///     space = index of the last is_js_space character in caption[0 ..= cut]   (none: no change)
///     if space >= 58: cut = space                                             (word boundary)
///     if the code unit at cut-1 is a high surrogate: cut -= 1                 (never split an emoji)
/// if cut >= len: not truncated
/// preview = caption[0 .. cut] with js_trim_end
/// truncated only if preview is shorter than js_trim_end(caption)
/// ```
///
/// "last white space" is JavaScript's `slice(0, cut + 1).search(/\s\S*$/)`: the white-space
/// character that is followed only by non-white-space up to index `cut` inclusive.
/// The clone has no unit test for this function: the tests here hold the results of the clone's
/// own function for the boundaries named above (96 units, the second line break, a word
/// boundary at 57 and at 58, an emoji across the cut).
pub fn caption_preview(caption: &str) -> CaptionPreview<'_> {
    let whole = CaptionPreview { text: caption, truncated: false };
    let length = utf16_len(caption);
    let mut cut = length.min(CAPTION_PREVIEW_UNITS);

    // Everything the rules ask about lies in the code units 0 ..= cut.
    let mut index = 0;
    let mut line_breaks = 0;
    let mut second_break = None;
    let mut last_space = None;
    for character in caption.chars() {
        if index > cut {
            break;
        }
        if character == '\n' {
            line_breaks += 1;
            if line_breaks == 2 {
                second_break = Some(index);
            }
        }
        if is_js_space(character) {
            last_space = Some(index);
        }
        index += character.len_utf16();
    }

    match second_break {
        Some(at) if at < cut => cut = at,
        _ if cut < length => {
            // Prefer a word boundary so a #hashtag or @mention is never cut in half.
            if let Some(space) = last_space.filter(|space| *space >= CAPTION_PREVIEW_MIN_WORD_CUT) {
                cut = space;
            }
        }
        _ => return whole,
    }

    // truncate_utf16 stops before an astral character that the cut would split, which is the
    // clone's "cut -= 1" after a high surrogate.
    let preview = js_trim_end(truncate_utf16(caption, cut));
    if preview.len() < js_trim_end(caption).len() {
        CaptionPreview { text: preview, truncated: true }
    } else {
        whole
    }
}

/// The caption line of the auto view (spec-edit 7.3): `None` when `js_trim(caption)` is empty;
/// the trimmed caption when `caption_preview` does not shorten it; otherwise the preview with
/// every trailing white-space character, "." and "…" removed, followed by "…" (U+2026).
pub fn auto_view_caption(caption: &str) -> Option<String> {
    let caption = js_trim(caption);
    if caption.is_empty() {
        return None;
    }
    let preview = caption_preview(caption);
    if !preview.truncated {
        return Some(caption.to_string());
    }
    let stem = preview.text.trim_end_matches(|character| is_js_space(character) || character == '.' || character == '…');
    Some(format!("{stem}…"))
}

// --- Counts and sizes -----------------------------------------------------------------------

/// The clone's compact count, `formatCount` (data.ts:336-340; spec-viewer 8.1):
/// `>= 1_000_000` -> millions with one decimal, a trailing ".0" dropped, + "M"; `>= 1_000` ->
/// thousands likewise + "K"; else the plain digits. There is no promotion from K to M.
///
/// The decimal is what JavaScript's `(n / 1000).toFixed(1)` prints: the quotient is a binary
/// floating-point number, and `toFixed` rounds THAT number, exact ties going up. A quotient that
/// looks like a tie in decimal usually is none in binary: 1150 / 1000 is stored as 1.14999...,
/// so 1150 reads "1.1K", while 1050 / 1000 is stored as 1.05000...04 and reads "1.1K" too. Only
/// quotients ending in .25 or .75 are exact ties (1250 -> "1.3K"). Rounding the decimal quotient
/// half up with integer arithmetic gives a different digit for 3996 of the counts below a
/// million, so the division is done in `f64` here, exactly as the clone does it.
///
/// Vectors: 999 "999", 1000 "1K", 1049 "1K", 1050 "1.1K", 1150 "1.1K", 1250 "1.3K", 8300 "8.3K",
/// 12345 "12.3K", 56789 "56.8K", 263300 "263.3K", 999949 "999.9K", 999950 "1000K", 1000000 "1M",
/// 1250000 "1.3M".
pub fn format_count(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{}M", one_decimal_like_to_fixed(count as f64 / 1_000_000.0))
    } else if count >= 1_000 {
        format!("{}K", one_decimal_like_to_fixed(count as f64 / 1_000.0))
    } else {
        format_count_exact(count)
    }
}

/// JavaScript's `value.toFixed(1).replace(/\.0$/, '')` for a `value` of at least 1 (and below
/// 2^53, which every quotient of `format_count` is): the exact binary value of the number,
/// rounded to one decimal with ties going up.
fn one_decimal_like_to_fixed(value: f64) -> String {
    /// 2^52: a number of at least 1 has no binary digit beyond this many fraction bits.
    const FRACTION_SCALE: f64 = 4_503_599_627_370_496.0;
    const FRACTION_BITS: u32 = 52;

    let whole = value.trunc();
    // Both steps are exact in floating point: the fraction of a number needs no more bits than
    // the number itself, and the scale is a power of two.
    let fraction = ((value - whole) * FRACTION_SCALE) as u128;
    // fraction / 2^52 is the exact fraction; in tenths, rounded half up:
    let tenths_of_fraction = (fraction * 10 + (1_u128 << (FRACTION_BITS - 1))) >> FRACTION_BITS;
    let tenths = whole as u128 * 10 + tenths_of_fraction;
    if tenths.is_multiple_of(10) {
        format!("{}", tenths / 10)
    } else {
        format!("{}.{}", tenths / 10, tenths % 10)
    }
}

/// A whole number with en-US thousands separators, JavaScript's `n.toLocaleString('en-US')`:
/// 0 "0", 999 "999", 1234 "1,234", 10000 "10,000", 1234567 "1,234,567".
pub fn format_count_exact(count: u64) -> String {
    let digits = count.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// The clone's `formatBytes` (limits.ts:93-98): `>= 1 GiB` -> "{x.y} GB", `>= 1 MiB` -> "{x.y} MB"
/// (one decimal, ".0" dropped), `>= 1024` -> "{rounded} KB", else "{n} bytes".
/// 64 MiB is "64 MB", 1024 MiB is "1 GB", 1.5 MiB is "1.5 MB".
pub fn format_bytes(bytes: u64) -> String {
    const KIB: u128 = 1024;
    const MIB: u128 = 1024 * KIB;
    const GIB: u128 = 1024 * MIB;
    let bytes = u128::from(bytes);
    // One decimal, rounded half up, as toFixed(1) does for these quotients.
    let one_decimal = |unit: u128| {
        let tenths = (bytes * 10 + unit / 2) / unit;
        if tenths.is_multiple_of(10) {
            format!("{}", tenths / 10)
        } else {
            format!("{}.{}", tenths / 10, tenths % 10)
        }
    };
    if bytes >= GIB {
        format!("{} GB", one_decimal(GIB))
    } else if bytes >= MIB {
        format!("{} MB", one_decimal(MIB))
    } else if bytes >= KIB {
        format!("{} KB", (bytes + KIB / 2) / KIB)
    } else {
        format!("{bytes} bytes")
    }
}

// --- Time -----------------------------------------------------------------------------------

/// A date and time of day in the proleptic Gregorian calendar, without a time zone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CivilTime {
    /// The year as a plain number: 2026; 0 and negative years are those before year 1.
    pub year: i32,
    /// 1..=12
    pub month: u32,
    /// 1..=31
    pub day: u32,
    /// 0..=23
    pub hour: u32,
    /// 0..=59
    pub minute: u32,
    /// 0..=59
    pub second: u32,
    /// 0..=999
    pub millisecond: u32,
}

const MS_PER_SECOND: i64 = 1000;
const MS_PER_MINUTE: i64 = 60 * MS_PER_SECOND;
const MS_PER_HOUR: i64 = 60 * MS_PER_MINUTE;
const MS_PER_DAY: i64 = 24 * MS_PER_HOUR;

/// The years a timestamp of an export may lie in (manifest.ts `parseTimestamp`): 1970 is what a
/// count mistaken for a date gives, and five-digit years have no ISO form.
const TIMESTAMP_YEARS: std::ops::RangeInclusive<i32> = 1971..=9999;

/// The largest instant a JavaScript `Date` can hold, in milliseconds (8.64e15).
const MAX_DATE_MS: f64 = 8_640_000_000_000_000.0;

/// en-US month names in upper case, for `date_label`.
const MONTH_LABELS: [&str; 12] = [
    "JANUARY", "FEBRUARY", "MARCH", "APRIL", "MAY", "JUNE", "JULY", "AUGUST", "SEPTEMBER", "OCTOBER", "NOVEMBER",
    "DECEMBER",
];

/// Days in `month` (1..=12) of `year`; 0 for a month outside that range.
pub fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        _ => 0,
    }
}

/// The UTC date and time of an instant given in epoch milliseconds (also before 1970).
pub fn civil_from_epoch_ms(epoch_ms: i64) -> CivilTime {
    let days = epoch_ms.div_euclid(MS_PER_DAY);
    let ms_of_day = epoch_ms.rem_euclid(MS_PER_DAY);

    // Days since 1970-01-01 to a civil date, after Howard Hinnant's "civil_from_days".
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);

    CivilTime {
        year: year as i32,
        month: month as u32,
        day: day as u32,
        hour: (ms_of_day / MS_PER_HOUR) as u32,
        minute: (ms_of_day % MS_PER_HOUR / MS_PER_MINUTE) as u32,
        second: (ms_of_day % MS_PER_MINUTE / MS_PER_SECOND) as u32,
        millisecond: (ms_of_day % MS_PER_SECOND) as u32,
    }
}

/// The instant, in epoch milliseconds, of a civil date and time read as UTC. `None` when a field
/// is out of range: month 13, day 31 in a 30-day month, 29 February in a common year, hour 24,
/// minute or second 60, millisecond 1000 (`parse_timestamp_text` handles the one spelling of
/// hour 24 that ISO 8601 allows, "24:00:00", itself).
pub fn epoch_ms_from_civil(civil: &CivilTime) -> Option<i64> {
    if civil.day < 1 || civil.day > days_in_month(civil.year, civil.month) {
        return None;
    }
    if civil.hour > 23 || civil.minute > 59 || civil.second > 59 || civil.millisecond > 999 {
        return None;
    }

    // A civil date to days since 1970-01-01, after Howard Hinnant's "days_from_civil".
    let year = i64::from(civil.year) - i64::from(civil.month <= 2);
    let month = i64::from(civil.month);
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(civil.day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;

    let ms_of_day = i64::from(civil.hour) * MS_PER_HOUR
        + i64::from(civil.minute) * MS_PER_MINUTE
        + i64::from(civil.second) * MS_PER_SECOND
        + i64::from(civil.millisecond);
    // A year far outside the calendar range of an i64 of milliseconds is not an instant.
    days.checked_mul(MS_PER_DAY)?.checked_add(ms_of_day)
}

/// The current instant in epoch milliseconds (UTC).
pub fn now_ms() -> i64 {
    epoch_ms_from_system_time(SystemTime::now())
}

/// A file time or wall-clock time as epoch milliseconds (UTC); negative before 1970.
pub fn epoch_ms_from_system_time(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(after) => i64::try_from(after.as_millis()).unwrap_or(i64::MAX),
        Err(before) => i64::try_from(before.duration().as_millis()).map(|ms| -ms).unwrap_or(i64::MIN),
    }
}

/// The offset of the computer's local time zone from UTC at the instant `utc_ms`, in minutes
/// east of UTC (Berlin in winter: 60; New York in summer: -240). Daylight saving time is taken
/// into account for that instant with the zone's current rules. 0 when the zone cannot be asked
/// (an instant outside the years 1601..=30827) and on platforms other than Windows.
///
/// This is the one place that asks the operating system about time zones: `date_label` and
/// `export::ExportOptions` take the offset as a parameter, so tests stay deterministic.
#[cfg(windows)]
pub fn local_offset_minutes(utc_ms: i64) -> i32 {
    use windows::Win32::Foundation::SYSTEMTIME;
    use windows::Win32::System::Time::SystemTimeToTzSpecificLocalTime;

    let utc = civil_from_epoch_ms(utc_ms);
    if !(1601..=30827).contains(&utc.year) {
        return 0;
    }
    let universal = SYSTEMTIME {
        wYear: utc.year as u16,
        wMonth: utc.month as u16,
        wDayOfWeek: 0,
        wDay: utc.day as u16,
        wHour: utc.hour as u16,
        wMinute: utc.minute as u16,
        wSecond: utc.second as u16,
        wMilliseconds: utc.millisecond as u16,
    };
    let mut local = SYSTEMTIME::default();
    // SAFETY: both pointers refer to SYSTEMTIME values that live for the whole call, and a null
    // time zone pointer (None) selects the currently active time zone, as documented.
    let converted = unsafe { SystemTimeToTzSpecificLocalTime(None, &universal, &mut local) };
    if converted.is_err() {
        return 0;
    }
    let local = CivilTime {
        year: i32::from(local.wYear),
        month: u32::from(local.wMonth),
        day: u32::from(local.wDay),
        hour: u32::from(local.wHour),
        minute: u32::from(local.wMinute),
        second: u32::from(local.wSecond),
        millisecond: u32::from(local.wMilliseconds),
    };
    match epoch_ms_from_civil(&local) {
        Some(local_ms) => (local_ms - utc_ms).div_euclid(MS_PER_MINUTE) as i32,
        None => 0,
    }
}

/// See the Windows variant: other platforms have no implementation and report UTC.
#[cfg(not(windows))]
pub fn local_offset_minutes(_utc_ms: i64) -> i32 {
    0
}

/// `YYYY-MM-DDTHH:MM:SS[.mmm]Z` for an instant whose UTC year has four digits.
fn iso_text(epoch_ms: i64, always_milliseconds: bool) -> Option<String> {
    let time = civil_from_epoch_ms(epoch_ms);
    if !(1..=9999).contains(&time.year) {
        return None;
    }
    let mut text = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        time.year, time.month, time.day, time.hour, time.minute, time.second
    );
    if always_milliseconds || time.millisecond != 0 {
        text.push_str(&format!(".{:03}", time.millisecond));
    }
    text.push('Z');
    Some(text)
}

/// The clone's `isoTime` (export.ts:148-154; spec-export A.5), used for `exported_at`, `taken_at`
/// and `comments[].taken_at`: UTC, `YYYY-MM-DDTHH:MM:SSZ` when the milliseconds are 0, otherwise
/// `YYYY-MM-DDTHH:MM:SS.mmmZ` with exactly three digits.
///
/// `None` when the UTC year is outside 0001..=9999: such a value has no four-digit ISO form (the
/// clone's validator requires one), and the field is then omitted.
///
/// Vectors: 1793545445000 -> "2026-11-01T15:04:05Z"; 1793545445417 -> "2026-11-01T15:04:05.417Z".
pub fn iso_time(epoch_ms: i64) -> Option<String> {
    iso_text(epoch_ms, false)
}

/// Like `iso_time`, but always with milliseconds (`YYYY-MM-DDTHH:MM:SS.mmmZ`), which is
/// JavaScript's `toISOString()` and what the derived post id hashes (`derived_post_id`).
pub fn iso_time_millis(epoch_ms: i64) -> Option<String> {
    iso_text(epoch_ms, true)
}

/// JavaScript's `Math.round`: the nearest whole number, a tie going towards +infinity
/// (2.5 -> 3, -2.5 -> -2), where `f64::round` moves a tie away from zero. The difference to the
/// floor is exact, so no value just below a half is rounded up by accident. A value that is not
/// finite is returned as it is.
pub fn js_round(value: f64) -> f64 {
    let floor = value.floor();
    if value - floor >= 0.5 {
        floor + 1.0
    } else {
        floor
    }
}

/// True when the UTC year of an instant is one a timestamp of an export may have.
fn in_timestamp_years(epoch_ms: i64) -> bool {
    TIMESTAMP_YEARS.contains(&civil_from_epoch_ms(epoch_ms).year)
}

/// A number from post.json as an instant (spec-export C.4 "epoch scaling", manifest.ts
/// `epochToMs`): not finite or `<= 0` -> `None`; `< 1e11` seconds; `< 1e14` milliseconds;
/// `< 1e17` microseconds; else nanoseconds. The result is rounded to the nearest millisecond the
/// way `Math.round` does (1793545445.9996 seconds is 1793545446000), and its UTC year must be
/// 1971..=9999.
///
/// Vectors: 1793545445, 1793545445000 and 1793545445000000 all give 1793545445000; 0 and -10
/// give `None`.
pub fn epoch_number_to_ms(value: f64) -> Option<i64> {
    if !value.is_finite() || value <= 0.0 {
        return None;
    }
    let ms = if value < 1e11 {
        js_round(value * 1000.0)
    } else if value < 1e14 {
        js_round(value)
    } else if value < 1e17 {
        js_round(value / 1e3)
    } else {
        js_round(value / 1e6)
    };
    if ms > MAX_DATE_MS {
        return None;
    }
    let ms = ms as i64;
    in_timestamp_years(ms).then_some(ms)
}

/// `^\d+(?:\.\d+)?$` with ASCII digits: the spelling of an epoch number in a string.
fn is_epoch_number_text(text: &str) -> bool {
    let all_digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    match text.split_once('.') {
        Some((whole, fraction)) => all_digits(whole) && all_digits(fraction),
        None => all_digits(text),
    }
}

/// `count` ASCII digits at `position` as a number; `None` when there are fewer.
fn fixed_digits(bytes: &[u8], position: usize, count: usize) -> Option<u32> {
    let digits = bytes.get(position..position + count)?;
    digits.iter().try_fold(0_u32, |value, byte| byte.is_ascii_digit().then(|| value * 10 + u32::from(byte - b'0')))
}

/// The ISO 8601 calendar forms `parse_timestamp_text` documents, as an instant.
fn parse_iso_date_time(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    let separator = |position: usize, expected: u8| (bytes.get(position) == Some(&expected)).then_some(());

    let year = fixed_digits(bytes, 0, 4)?;
    separator(4, b'-')?;
    let month = fixed_digits(bytes, 5, 2)?;
    separator(7, b'-')?;
    let day = fixed_digits(bytes, 8, 2)?;
    let mut position = 10;

    let mut time = CivilTime { year: year as i32, month, day, hour: 0, minute: 0, second: 0, millisecond: 0 };
    let mut offset_minutes = 0_i64;
    if position < bytes.len() {
        if !matches!(bytes[position], b'T' | b't' | b' ') {
            return None;
        }
        time.hour = fixed_digits(bytes, position + 1, 2)?;
        separator(position + 3, b':')?;
        time.minute = fixed_digits(bytes, position + 4, 2)?;
        position += 6;

        if bytes.get(position) == Some(&b':') {
            time.second = fixed_digits(bytes, position + 1, 2)?;
            position += 3;
            if bytes.get(position) == Some(&b'.') {
                position += 1;
                let digits = bytes[position..].iter().take_while(|byte| byte.is_ascii_digit()).count();
                if !(1..=9).contains(&digits) {
                    return None;
                }
                // Digits beyond the milliseconds are cut, not rounded, as browsers do.
                for place in 0..3 {
                    let digit = if place < digits { u32::from(bytes[position + place] - b'0') } else { 0 };
                    time.millisecond = time.millisecond * 10 + digit;
                }
                position += digits;
            }
        }

        match bytes.get(position) {
            None => {}
            Some(b'Z' | b'z') => position += 1,
            Some(sign @ (b'+' | b'-')) => {
                let hours = fixed_digits(bytes, position + 1, 2)?;
                position += 3;
                if bytes.get(position) == Some(&b':') {
                    position += 1;
                }
                let minutes = fixed_digits(bytes, position, 2)?;
                position += 2;
                if hours > 23 || minutes > 59 {
                    return None;
                }
                let east = i64::from(hours * 60 + minutes);
                offset_minutes = if *sign == b'+' { east } else { -east };
            }
            Some(_) => return None,
        }
    }
    if position != bytes.len() {
        return None;
    }

    // ISO 8601 lets a day end at 24:00:00, which is midnight of the next day.
    let mut extra_days = 0;
    if time.hour == 24 {
        if time.minute != 0 || time.second != 0 || time.millisecond != 0 {
            return None;
        }
        time.hour = 0;
        extra_days = 1;
    }
    let local = epoch_ms_from_civil(&time)?;
    Some(local + extra_days * MS_PER_DAY - offset_minutes * MS_PER_MINUTE)
}

/// A timestamp string from post.json (spec-export C.4, `parseTimestamp`), or from
/// descriptions.json. `js_trim` first; empty -> `None`. Accepted, and nothing else:
///
/// * digits with an optional fraction (`^\d+(?:\.\d+)?$`): an epoch number, see
///   `epoch_number_to_ms`;
/// * ISO 8601 calendar forms, which is the part of JavaScript's `Date.parse` that every engine
///   shares: `YYYY-MM-DD` (UTC midnight), `YYYY-MM-DDTHH:MM`, `YYYY-MM-DDTHH:MM:SS`,
///   `YYYY-MM-DDTHH:MM:SS.f` (1 to 9 fraction digits, cut to milliseconds), each date-time form
///   followed by `Z` / `z`, by an offset `+HH:MM`, `-HH:MM`, `+HHMM`, `-HHMM`, or by nothing. `T`
///   may be `t` or one space. The time `24:00:00` (with nothing but zeros after it) is the
///   midnight that ends the day.
///
/// Native decision (the browser would use the local zone): a date-time without `Z` or an offset
/// is read as UTC, so that a file means the same on every computer. Fields out of range are
/// rejected (`2026-13-45T99:99:99Z`, and also a day the month does not have, such as 30
/// February, which some browsers roll over into March), and the resulting UTC year must be
/// 1971..=9999.
///
/// Vectors: "1793545445" and "2026-11-01T16:04:05+01:00" -> 1793545445000; "2026-11-01" ->
/// 1793491200000; `None` for "-5", "2 days ago", "not a date", "2026-13-45T99:99:99Z".
pub fn parse_timestamp_text(text: &str) -> Option<i64> {
    let text = js_trim(text);
    if text.is_empty() {
        return None;
    }
    if is_epoch_number_text(text) {
        // Rust parses decimal digits to the nearest f64, as JavaScript's Number() does.
        return epoch_number_to_ms(text.parse::<f64>().ok()?);
    }
    let ms = parse_iso_date_time(text)?;
    in_timestamp_years(ms).then_some(ms)
}

/// The clone's `parseTimestamp` for any JSON value: a number -> `epoch_number_to_ms`, a string
/// -> `parse_timestamp_text`, anything else (true, null, {}, []) -> `None`.
pub fn parse_timestamp(value: &serde_json::Value) -> Option<i64> {
    match value {
        serde_json::Value::Number(number) => epoch_number_to_ms(number.as_f64()?),
        serde_json::Value::String(text) => parse_timestamp_text(text),
        _ => None,
    }
}

/// The clone's `relativeTime` (importedPosts.ts:43-54; spec-viewer 8.2):
/// `m = floor((now - then) / 60_000)`; `m < 1` -> "now" (also every future instant); `m < 60` ->
/// "{m}m"; `h = m / 60 < 24` -> "{h}h"; `d = h / 24 < 7` -> "{d}d"; else "{d / 7}w". There are no
/// months or years: three years read "156w". A post without a time has no label at all: the
/// caller shows nothing for `taken_at == None`.
pub fn relative_time_label(now_ms: i64, then_ms: i64) -> String {
    // i128: the difference of two arbitrary i64 instants does not fit an i64.
    let minutes = (i128::from(now_ms) - i128::from(then_ms)).div_euclid(i128::from(MS_PER_MINUTE));
    if minutes < 1 {
        return "now".to_string();
    }
    if minutes < 60 {
        return format!("{minutes}m");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h");
    }
    let days = hours / 24;
    if days < 7 {
        return format!("{days}d");
    }
    format!("{}w", days / 7)
}

/// The clone's `dateLabel` (importedPosts.ts:57-67; spec-viewer 8.3): the date of `then_ms` in
/// LOCAL time, en-US, upper-cased: "NOVEMBER 1" when its local year equals the local year of
/// `now_ms`, else "NOVEMBER 1, 2024".
///
/// `offset_minutes(utc_ms)` gives the local offset from UTC at an instant, in minutes east of
/// UTC. The UI passes `&local_offset_minutes`; tests pass a fixed offset such as `&|_| 60`. It is
/// asked once for `then_ms` and once for `now_ms` (the two can differ by daylight saving time).
pub fn date_label(now_ms: i64, then_ms: i64, offset_minutes: &dyn Fn(i64) -> i32) -> String {
    let local = |utc_ms: i64| {
        civil_from_epoch_ms(utc_ms.saturating_add(i64::from(offset_minutes(utc_ms)) * MS_PER_MINUTE))
    };
    let then = local(then_ms);
    let now = local(now_ms);
    let month = MONTH_LABELS[(then.month - 1) as usize];
    if then.year == now.year {
        format!("{month} {}", then.day)
    } else {
        format!("{month} {}, {}", then.day, then.year)
    }
}

// --- Ids and names --------------------------------------------------------------------------

/// True for the characters ids, avatar stems and download names keep: `[A-Za-z0-9._-]`.
fn is_name_safe(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
}

/// JavaScript's `text.replace(/[^A-Za-z0-9._-]+/g, '_')`: every run of other characters becomes
/// one "_". The result is ASCII.
fn replace_unsafe_runs(text: &str) -> String {
    let mut replaced = String::with_capacity(text.len());
    let mut in_run = false;
    for character in text.chars() {
        if is_name_safe(character) {
            replaced.push(character);
            in_run = false;
        } else if !in_run {
            replaced.push('_');
            in_run = true;
        }
    }
    replaced
}

/// The clone's `cleanId` (spec-export C.4), also used for the id of a loose picture (its file
/// stem; spec-viewer 14.2): `js_trim`, every run of characters outside `[A-Za-z0-9._-]` becomes
/// one "_", then the first `manifest::MAX_ID_CHARS` (100) characters. `None` when the result
/// holds no `[A-Za-z0-9]`.
///
/// Vectors: `weird id/with <chars>` -> "weird_id_with_chars_"; "12345" -> "12345";
/// "Sunset at the pier (final)" -> "Sunset_at_the_pier_final_"; "###" and "" -> `None`.
pub fn clean_id(raw: &str) -> Option<String> {
    let mut id = replace_unsafe_runs(js_trim(raw));
    // ASCII only, so bytes, characters and UTF-16 code units are the same count.
    id.truncate(MAX_ID_CHARS);
    id.bytes().any(|byte| byte.is_ascii_alphanumeric()).then_some(id)
}

/// The clone's `hash53` (cyrb53, manifest.ts:393-404) over the UTF-16 code units of `text`, with
/// all multiplications wrapping at 32 bits:
///
/// ```text
/// h1 = 0xdeadbeef; h2 = 0x41c6ce57
/// for each code unit c: h1 = (h1 ^ c) * 2654435761; h2 = (h2 ^ c) * 1597334677
/// h1 = ((h1 ^ (h1 >> 16)) * 2246822507) ^ ((h2 ^ (h2 >> 13)) * 3266489909)
/// h2 = ((h2 ^ (h2 >> 16)) * 2246822507) ^ ((h1 ^ (h1 >> 13)) * 3266489909)     (uses the new h1)
/// result = ((h2 & 0x1FFFFF) << 32) | h1                                        (53 bits)
/// ```
pub fn hash53(text: &str) -> u64 {
    let mut h1: u32 = 0xdead_beef;
    let mut h2: u32 = 0x41c6_ce57;
    for unit in text.encode_utf16() {
        let code = u32::from(unit);
        h1 = (h1 ^ code).wrapping_mul(2_654_435_761);
        h2 = (h2 ^ code).wrapping_mul(1_597_334_677);
    }
    h1 = (h1 ^ (h1 >> 16)).wrapping_mul(2_246_822_507) ^ (h2 ^ (h2 >> 13)).wrapping_mul(3_266_489_909);
    h2 = (h2 ^ (h2 >> 16)).wrapping_mul(2_246_822_507) ^ (h1 ^ (h1 >> 13)).wrapping_mul(3_266_489_909);
    (u64::from(h2 & 0x1F_FFFF) << 32) | u64::from(h1)
}

/// The 64-bit FNV-1a hash of `bytes` (offset basis 0xcbf29ce484222325, prime 0x100000001b3). It
/// names things the app derives from a path (the folder of an archive in the archive cache), so
/// it must stay the same between runs and builds; it is not meant to be hard to collide.
/// Vectors of the FNV reference: "" -> 0xcbf29ce484222325, "a" -> 0xaf63dc4c8601ec8c.
pub fn fnv1a_64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    bytes.iter().fold(OFFSET_BASIS, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(PRIME))
}

/// A number in base 36 with lower-case digits, JavaScript's `value.toString(36)`: 0 -> "0",
/// 35 -> "z", 36 -> "10".
pub fn to_base36(value: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if value == 0 {
        return "0".to_string();
    }
    let mut digits = Vec::new();
    let mut rest = value;
    while rest > 0 {
        digits.push(DIGITS[(rest % 36) as usize]);
        rest /= 36;
    }
    digits.iter().rev().map(|digit| char::from(*digit)).collect()
}

/// The id of an export entry that names none (spec-export C.4):
/// `"post-" + to_base36(hash53(username + "\n" + taken_at_iso + "\n" + media_paths.join("\n")))`
/// where `username` is the entry's cleaned username or "", `taken_at_iso` is
/// `iso_time_millis(taken_at)` or "" and `media_paths` are the normalised media paths in order.
pub fn derived_post_id(username: &str, taken_at: Option<i64>, media_paths: &[String]) -> String {
    let taken_at_iso = taken_at.and_then(iso_time_millis).unwrap_or_default();
    let fingerprint = format!("{username}\n{taken_at_iso}\n{}", media_paths.join("\n"));
    format!("post-{}", to_base36(hash53(&fingerprint)))
}

/// The id of a loose picture whose file stem gives no id (`clean_id` is `None`):
/// `"post-" + to_base36(hash53(file_name))` (spec-viewer 14.2).
pub fn derived_file_id(file_name: &str) -> String {
    format!("post-{}", to_base36(hash53(file_name)))
}

/// Makes an id unique, as the clone does on import and on export: `id` when it is not in `used`,
/// else the first free of `{id}-2`, `{id}-3`, ... The returned id is added to `used`.
/// Comparison is exact (case-sensitive).
pub fn unique_id(id: &str, used: &mut HashSet<String>) -> String {
    let mut candidate = id.to_string();
    let mut number = 2_u64;
    while used.contains(&candidate) {
        candidate = format!("{id}-{number}");
        number += 1;
    }
    used.insert(candidate.clone());
    candidate
}

/// export.ts `WINDOWS_RESERVED`: con, prn, aux, nul, com1-com9, lpt1-lpt9 in any letter case.
fn is_windows_device_name(stem: &str) -> bool {
    let lower = stem.to_ascii_lowercase();
    if matches!(lower.as_str(), "con" | "prn" | "aux" | "nul") {
        return true;
    }
    let numbered = lower.strip_prefix("com").or_else(|| lower.strip_prefix("lpt"));
    numbered.is_some_and(|number| number.len() == 1 && matches!(number.as_bytes()[0], b'1'..=b'9'))
}

/// The clone's `avatarStem` (export.ts:156-164; spec-export A.3): the file stem of a profile
/// picture in an archive.
///
/// ```text
/// 1. every run of characters outside [A-Za-z0-9._-] becomes one "_"
/// 2. all leading dots and all trailing dots are removed
/// 3. the first 64 characters are kept (MAX_STEM_CHARS)
/// 4. empty -> "user"
/// 5. a Windows device name (con, prn, aux, nul, com1-com9, lpt1-lpt9; any letter case) gets "_" appended
/// then: candidate = stem; while `taken` holds lowercase(candidate): candidate = stem + "-" + n (n = 2, 3, ...)
/// ```
///
/// The lower-cased result is added to `taken`, which the caller keeps per archive part.
/// Vector: "../CON" -> "_CON" (the path is then "avatars/_CON.jpg").
pub fn avatar_stem(username: &str, taken: &mut HashSet<String>) -> String {
    let replaced = replace_unsafe_runs(username);
    let mut stem = replaced.trim_matches('.').to_string();
    stem.truncate(MAX_STEM_CHARS);
    if stem.is_empty() {
        stem.push_str("user");
    }
    if is_windows_device_name(&stem) {
        stem.push('_');
    }
    let mut candidate = stem.clone();
    let mut number = 2_u64;
    while taken.contains(&candidate.to_ascii_lowercase()) {
        candidate = format!("{stem}-{number}");
        number += 1;
    }
    taken.insert(candidate.to_ascii_lowercase());
    candidate
}

/// The id part of a single-post download name (App.tsx:949-954; spec-export A.9): every run of
/// characters outside `[A-Za-z0-9._-]` becomes one "_", then the first 64 characters. No other
/// sanitising. "FXTbasic004" stays as it is.
pub fn archive_stem(id: &str) -> String {
    let mut stem = replace_unsafe_runs(id);
    stem.truncate(MAX_STEM_CHARS);
    stem
}

/// Takes a run of ASCII digits off `characters` and returns it without leading zeros (so "007"
/// and "7" give the same text, and "000" gives "").
fn take_number<I: Iterator<Item = char>>(characters: &mut Peekable<I>) -> String {
    let mut digits = String::new();
    while let Some(digit) = characters.peek().copied().filter(char::is_ascii_digit) {
        if !(digits.is_empty() && digit == '0') {
            digits.push(digit);
        }
        characters.next();
    }
    digits
}

/// The order of loose pictures: Explorer's order of file names. Case is ignored (compare the
/// lower-cased characters), and where both names have a run of ASCII digits at the same place
/// the runs are compared as numbers of any length (leading zeros ignored), so "img_1.png" <
/// "IMG_2.jpg" < "IMG_10.jpg". Names that are equal under these rules are ordered by a plain
/// comparison of the original text, so the order is total and stable between runs.
pub fn natural_cmp(left: &str, right: &str) -> Ordering {
    let mut left_characters = left.chars().flat_map(char::to_lowercase).peekable();
    let mut right_characters = right.chars().flat_map(char::to_lowercase).peekable();
    loop {
        match (left_characters.peek().copied(), right_characters.peek().copied()) {
            (None, None) => break,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(a), Some(b)) if a.is_ascii_digit() && b.is_ascii_digit() => {
                let left_number = take_number(&mut left_characters);
                let right_number = take_number(&mut right_characters);
                // Without leading zeros the longer number is the larger one.
                let order = left_number.len().cmp(&right_number.len()).then_with(|| left_number.cmp(&right_number));
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(a), Some(b)) => {
                if a != b {
                    return a.cmp(&b);
                }
                left_characters.next();
                right_characters.next();
            }
        }
    }
    left.cmp(right)
}

// --- Hashtags and mentions ------------------------------------------------------------------

/// What a token in a caption links to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptionLinkKind {
    /// `#tag`
    Hashtag,
    /// `@name`
    Mention,
}

/// One `#hashtag` or `@mention` found in a caption or comment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptionLink {
    /// Byte range of the token in the scanned text, including the leading "#" or "@".
    pub range: Range<usize>,
    /// A hashtag or a mention.
    pub kind: CaptionLinkKind,
    /// The tag or user name without the sign.
    pub name: String,
}

impl CaptionLink {
    /// Where the token leads: `urls::hashtag_url` or `urls::mention_url` of `name`.
    pub fn url(&self) -> String {
        match self.kind {
            CaptionLinkKind::Hashtag => urls::hashtag_url(&self.name),
            CaptionLinkKind::Mention => urls::mention_url(&self.name),
        }
    }
}

/// A hashtag has at most this many characters (code points) after the "#".
const MAX_HASHTAG_CHARS: usize = 100;
/// A mention has at most this many characters after the "@".
const MAX_MENTION_CHARS: usize = 30;

/// The code points of the Unicode general category M (Mn, Mc, Me: combining marks), as inclusive
/// ranges in ascending order. Unicode 17.0, listed from the regular expression engine the clone
/// runs on (`/\p{M}/u`). std has no test for this category, and a hashtag in Thai, Hindi or
/// Arabic is cut short without it (tone marks, viramas and vowel signs are marks).
const COMBINING_MARK_RANGES: [(u32, u32); 327] = [
    (0x0300, 0x036F), (0x0483, 0x0489), (0x0591, 0x05BD), (0x05BF, 0x05BF), (0x05C1, 0x05C2), (0x05C4, 0x05C5),
    (0x05C7, 0x05C7), (0x0610, 0x061A), (0x064B, 0x065F), (0x0670, 0x0670), (0x06D6, 0x06DC), (0x06DF, 0x06E4),
    (0x06E7, 0x06E8), (0x06EA, 0x06ED), (0x0711, 0x0711), (0x0730, 0x074A), (0x07A6, 0x07B0), (0x07EB, 0x07F3),
    (0x07FD, 0x07FD), (0x0816, 0x0819), (0x081B, 0x0823), (0x0825, 0x0827), (0x0829, 0x082D), (0x0859, 0x085B),
    (0x0897, 0x089F), (0x08CA, 0x08E1), (0x08E3, 0x0903), (0x093A, 0x093C), (0x093E, 0x094F), (0x0951, 0x0957),
    (0x0962, 0x0963), (0x0981, 0x0983), (0x09BC, 0x09BC), (0x09BE, 0x09C4), (0x09C7, 0x09C8), (0x09CB, 0x09CD),
    (0x09D7, 0x09D7), (0x09E2, 0x09E3), (0x09FE, 0x09FE), (0x0A01, 0x0A03), (0x0A3C, 0x0A3C), (0x0A3E, 0x0A42),
    (0x0A47, 0x0A48), (0x0A4B, 0x0A4D), (0x0A51, 0x0A51), (0x0A70, 0x0A71), (0x0A75, 0x0A75), (0x0A81, 0x0A83),
    (0x0ABC, 0x0ABC), (0x0ABE, 0x0AC5), (0x0AC7, 0x0AC9), (0x0ACB, 0x0ACD), (0x0AE2, 0x0AE3), (0x0AFA, 0x0AFF),
    (0x0B01, 0x0B03), (0x0B3C, 0x0B3C), (0x0B3E, 0x0B44), (0x0B47, 0x0B48), (0x0B4B, 0x0B4D), (0x0B55, 0x0B57),
    (0x0B62, 0x0B63), (0x0B82, 0x0B82), (0x0BBE, 0x0BC2), (0x0BC6, 0x0BC8), (0x0BCA, 0x0BCD), (0x0BD7, 0x0BD7),
    (0x0C00, 0x0C04), (0x0C3C, 0x0C3C), (0x0C3E, 0x0C44), (0x0C46, 0x0C48), (0x0C4A, 0x0C4D), (0x0C55, 0x0C56),
    (0x0C62, 0x0C63), (0x0C81, 0x0C83), (0x0CBC, 0x0CBC), (0x0CBE, 0x0CC4), (0x0CC6, 0x0CC8), (0x0CCA, 0x0CCD),
    (0x0CD5, 0x0CD6), (0x0CE2, 0x0CE3), (0x0CF3, 0x0CF3), (0x0D00, 0x0D03), (0x0D3B, 0x0D3C), (0x0D3E, 0x0D44),
    (0x0D46, 0x0D48), (0x0D4A, 0x0D4D), (0x0D57, 0x0D57), (0x0D62, 0x0D63), (0x0D81, 0x0D83), (0x0DCA, 0x0DCA),
    (0x0DCF, 0x0DD4), (0x0DD6, 0x0DD6), (0x0DD8, 0x0DDF), (0x0DF2, 0x0DF3), (0x0E31, 0x0E31), (0x0E34, 0x0E3A),
    (0x0E47, 0x0E4E), (0x0EB1, 0x0EB1), (0x0EB4, 0x0EBC), (0x0EC8, 0x0ECE), (0x0F18, 0x0F19), (0x0F35, 0x0F35),
    (0x0F37, 0x0F37), (0x0F39, 0x0F39), (0x0F3E, 0x0F3F), (0x0F71, 0x0F84), (0x0F86, 0x0F87), (0x0F8D, 0x0F97),
    (0x0F99, 0x0FBC), (0x0FC6, 0x0FC6), (0x102B, 0x103E), (0x1056, 0x1059), (0x105E, 0x1060), (0x1062, 0x1064),
    (0x1067, 0x106D), (0x1071, 0x1074), (0x1082, 0x108D), (0x108F, 0x108F), (0x109A, 0x109D), (0x135D, 0x135F),
    (0x1712, 0x1715), (0x1732, 0x1734), (0x1752, 0x1753), (0x1772, 0x1773), (0x17B4, 0x17D3), (0x17DD, 0x17DD),
    (0x180B, 0x180D), (0x180F, 0x180F), (0x1885, 0x1886), (0x18A9, 0x18A9), (0x1920, 0x192B), (0x1930, 0x193B),
    (0x1A17, 0x1A1B), (0x1A55, 0x1A5E), (0x1A60, 0x1A7C), (0x1A7F, 0x1A7F), (0x1AB0, 0x1ADD), (0x1AE0, 0x1AEB),
    (0x1B00, 0x1B04), (0x1B34, 0x1B44), (0x1B6B, 0x1B73), (0x1B80, 0x1B82), (0x1BA1, 0x1BAD), (0x1BE6, 0x1BF3),
    (0x1C24, 0x1C37), (0x1CD0, 0x1CD2), (0x1CD4, 0x1CE8), (0x1CED, 0x1CED), (0x1CF4, 0x1CF4), (0x1CF7, 0x1CF9),
    (0x1DC0, 0x1DFF), (0x20D0, 0x20F0), (0x2CEF, 0x2CF1), (0x2D7F, 0x2D7F), (0x2DE0, 0x2DFF), (0x302A, 0x302F),
    (0x3099, 0x309A), (0xA66F, 0xA672), (0xA674, 0xA67D), (0xA69E, 0xA69F), (0xA6F0, 0xA6F1), (0xA802, 0xA802),
    (0xA806, 0xA806), (0xA80B, 0xA80B), (0xA823, 0xA827), (0xA82C, 0xA82C), (0xA880, 0xA881), (0xA8B4, 0xA8C5),
    (0xA8E0, 0xA8F1), (0xA8FF, 0xA8FF), (0xA926, 0xA92D), (0xA947, 0xA953), (0xA980, 0xA983), (0xA9B3, 0xA9C0),
    (0xA9E5, 0xA9E5), (0xAA29, 0xAA36), (0xAA43, 0xAA43), (0xAA4C, 0xAA4D), (0xAA7B, 0xAA7D), (0xAAB0, 0xAAB0),
    (0xAAB2, 0xAAB4), (0xAAB7, 0xAAB8), (0xAABE, 0xAABF), (0xAAC1, 0xAAC1), (0xAAEB, 0xAAEF), (0xAAF5, 0xAAF6),
    (0xABE3, 0xABEA), (0xABEC, 0xABED), (0xFB1E, 0xFB1E), (0xFE00, 0xFE0F), (0xFE20, 0xFE2F), (0x101FD, 0x101FD),
    (0x102E0, 0x102E0), (0x10376, 0x1037A), (0x10A01, 0x10A03), (0x10A05, 0x10A06), (0x10A0C, 0x10A0F),
    (0x10A38, 0x10A3A), (0x10A3F, 0x10A3F), (0x10AE5, 0x10AE6), (0x10D24, 0x10D27), (0x10D69, 0x10D6D),
    (0x10EAB, 0x10EAC), (0x10EFA, 0x10EFF), (0x10F46, 0x10F50), (0x10F82, 0x10F85), (0x11000, 0x11002),
    (0x11038, 0x11046), (0x11070, 0x11070), (0x11073, 0x11074), (0x1107F, 0x11082), (0x110B0, 0x110BA),
    (0x110C2, 0x110C2), (0x11100, 0x11102), (0x11127, 0x11134), (0x11145, 0x11146), (0x11173, 0x11173),
    (0x11180, 0x11182), (0x111B3, 0x111C0), (0x111C9, 0x111CC), (0x111CE, 0x111CF), (0x1122C, 0x11237),
    (0x1123E, 0x1123E), (0x11241, 0x11241), (0x112DF, 0x112EA), (0x11300, 0x11303), (0x1133B, 0x1133C),
    (0x1133E, 0x11344), (0x11347, 0x11348), (0x1134B, 0x1134D), (0x11357, 0x11357), (0x11362, 0x11363),
    (0x11366, 0x1136C), (0x11370, 0x11374), (0x113B8, 0x113C0), (0x113C2, 0x113C2), (0x113C5, 0x113C5),
    (0x113C7, 0x113CA), (0x113CC, 0x113D0), (0x113D2, 0x113D2), (0x113E1, 0x113E2), (0x11435, 0x11446),
    (0x1145E, 0x1145E), (0x114B0, 0x114C3), (0x115AF, 0x115B5), (0x115B8, 0x115C0), (0x115DC, 0x115DD),
    (0x11630, 0x11640), (0x116AB, 0x116B7), (0x1171D, 0x1172B), (0x1182C, 0x1183A), (0x11930, 0x11935),
    (0x11937, 0x11938), (0x1193B, 0x1193E), (0x11940, 0x11940), (0x11942, 0x11943), (0x119D1, 0x119D7),
    (0x119DA, 0x119E0), (0x119E4, 0x119E4), (0x11A01, 0x11A0A), (0x11A33, 0x11A39), (0x11A3B, 0x11A3E),
    (0x11A47, 0x11A47), (0x11A51, 0x11A5B), (0x11A8A, 0x11A99), (0x11B60, 0x11B67), (0x11C2F, 0x11C36),
    (0x11C38, 0x11C3F), (0x11C92, 0x11CA7), (0x11CA9, 0x11CB6), (0x11D31, 0x11D36), (0x11D3A, 0x11D3A),
    (0x11D3C, 0x11D3D), (0x11D3F, 0x11D45), (0x11D47, 0x11D47), (0x11D8A, 0x11D8E), (0x11D90, 0x11D91),
    (0x11D93, 0x11D97), (0x11EF3, 0x11EF6), (0x11F00, 0x11F01), (0x11F03, 0x11F03), (0x11F34, 0x11F3A),
    (0x11F3E, 0x11F42), (0x11F5A, 0x11F5A), (0x13440, 0x13440), (0x13447, 0x13455), (0x1611E, 0x1612F),
    (0x16AF0, 0x16AF4), (0x16B30, 0x16B36), (0x16F4F, 0x16F4F), (0x16F51, 0x16F87), (0x16F8F, 0x16F92),
    (0x16FE4, 0x16FE4), (0x16FF0, 0x16FF1), (0x1BC9D, 0x1BC9E), (0x1CF00, 0x1CF2D), (0x1CF30, 0x1CF46),
    (0x1D165, 0x1D169), (0x1D16D, 0x1D172), (0x1D17B, 0x1D182), (0x1D185, 0x1D18B), (0x1D1AA, 0x1D1AD),
    (0x1D242, 0x1D244), (0x1DA00, 0x1DA36), (0x1DA3B, 0x1DA6C), (0x1DA75, 0x1DA75), (0x1DA84, 0x1DA84),
    (0x1DA9B, 0x1DA9F), (0x1DAA1, 0x1DAAF), (0x1E000, 0x1E006), (0x1E008, 0x1E018), (0x1E01B, 0x1E021),
    (0x1E023, 0x1E024), (0x1E026, 0x1E02A), (0x1E08F, 0x1E08F), (0x1E130, 0x1E136), (0x1E2AE, 0x1E2AE),
    (0x1E2EC, 0x1E2EF), (0x1E4EC, 0x1E4EF), (0x1E5EE, 0x1E5EF), (0x1E6E3, 0x1E6E3), (0x1E6E6, 0x1E6E6),
    (0x1E6EE, 0x1E6EF), (0x1E6F5, 0x1E6F5), (0x1E8D0, 0x1E8D6), (0x1E944, 0x1E94A), (0xE0100, 0xE01EF),
];

/// True for a combining mark (`\p{M}`): an accent, a vowel sign, a tone mark, a variation
/// selector. See `COMBINING_MARK_RANGES`.
pub fn is_combining_mark(character: char) -> bool {
    let code = u32::from(character);
    // Nothing below U+0300 is a mark; this keeps plain text off the table.
    if code < COMBINING_MARK_RANGES[0].0 {
        return false;
    }
    COMBINING_MARK_RANGES
        .binary_search_by(|&(first, last)| {
            if last < code {
                Ordering::Less
            } else if first > code {
                Ordering::Greater
            } else {
                Ordering::Equal
            }
        })
        .is_ok()
}

/// The circled and squared Latin letters (Ⓐ, 🄰 ...): Unicode calls them alphabetic, but their
/// general category is "symbol", so they are no letters for the token pattern.
fn is_letter_like_symbol(character: char) -> bool {
    matches!(
        character,
        '\u{24B6}'..='\u{24E9}' | '\u{1F130}'..='\u{1F149}' | '\u{1F150}'..='\u{1F169}' | '\u{1F170}'..='\u{1F189}'
    )
}

/// True for a character that continues a word in the sense of the clone's token pattern:
/// `\p{L}`, `\p{N}` or "_".
///
/// std offers `char::is_alphabetic` (letters, letter numbers, and the marks and symbols with the
/// property Other_Alphabetic) and `char::is_numeric` (`\p{N}`), but no test for the general
/// category "letter". Taking the combining marks and the letter-like symbols away from the
/// alphabetic characters leaves exactly the letters and letter numbers.
pub fn is_word_char(character: char) -> bool {
    if character.is_ascii() {
        return character.is_ascii_alphanumeric() || character == '_';
    }
    (character.is_alphabetic() || character.is_numeric())
        && !is_combining_mark(character)
        && !is_letter_like_symbol(character)
}

/// A character of a hashtag: `[\p{L}\p{M}\p{N}_]`.
fn is_hashtag_char(character: char) -> bool {
    is_word_char(character) || is_combining_mark(character)
}

/// A byte of a mention: `[A-Za-z0-9_.]`.
fn is_mention_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'.'
}

/// A character a token may follow: anything but a letter, a digit and `_ @ # & / .`.
fn is_token_prefix(character: char) -> bool {
    !is_word_char(character) && !matches!(character, '@' | '#' | '&' | '/' | '.')
}

/// The lookahead of the pattern, `(?![\p{L}\p{N}_])`, at byte `end` of `text`.
fn token_may_end_at(text: &str, end: usize) -> bool {
    !text[end..].chars().next().is_some_and(is_word_char)
}

/// `#([\p{L}\p{M}\p{N}_]{1,100})` at byte `start` of `text`, where the "#" is.
fn hashtag_at(text: &str, start: usize) -> Option<CaptionLink> {
    let name_start = start + 1;
    // The byte offsets at which a tag of 1, 2, ... characters would end.
    let mut ends = Vec::new();
    for (offset, character) in text[name_start..].char_indices() {
        if ends.len() == MAX_HASHTAG_CHARS || !is_hashtag_char(character) {
            break;
        }
        ends.push(name_start + offset + character.len_utf8());
    }
    // Greedy first; like the regular expression, give characters back until the tag is not
    // followed by a letter, a digit or "_" (it can then only end before a combining mark).
    let end = ends.into_iter().rev().find(|end| token_may_end_at(text, *end))?;
    Some(CaptionLink { range: start..end, kind: CaptionLinkKind::Hashtag, name: text[name_start..end].to_string() })
}

/// `@([A-Za-z0-9_](?:[A-Za-z0-9_.]{0,28}[A-Za-z0-9_])?)` at byte `start` of `text`, where the
/// "@" is.
fn mention_at(text: &str, start: usize) -> Option<CaptionLink> {
    let name_start = start + 1;
    let name = &text.as_bytes()[name_start..];
    let run = name.iter().take(MAX_MENTION_CHARS).take_while(|byte| is_mention_byte(**byte)).count();
    if run == 0 || name[0] == b'.' {
        return None;
    }
    // Longest first: a name does not end with "." and is not followed by a letter, a digit or "_".
    // The run is ASCII, so every length is a character boundary.
    let length = (1..=run).rev().find(|length| name[length - 1] != b'.' && token_may_end_at(text, name_start + length))?;
    let end = name_start + length;
    Some(CaptionLink { range: start..end, kind: CaptionLinkKind::Mention, name: text[name_start..end].to_string() })
}

/// The token that starts at byte `start` of `text`, if there is a "#" or "@" with a valid name.
fn token_at(text: &str, start: usize) -> Option<CaptionLink> {
    match text.as_bytes().get(start) {
        Some(b'#') => hashtag_at(text, start),
        Some(b'@') => mention_at(text, start),
        _ => None,
    }
}

/// The hashtags and mentions of a caption, in text order, never overlapping (common.tsx:297-327;
/// spec-edit 7.4). The UI shows the text between the ranges as plain text; plain URLs are not
/// links. This is a hand-written scanner for the pattern
///
/// ```text
/// (^|[^\p{L}\p{N}_@#&/.])(?:#([\p{L}\p{M}\p{N}_]{1,100})|@([A-Za-z0-9_](?:[A-Za-z0-9_.]{0,28}[A-Za-z0-9_])?))(?![\p{L}\p{N}_])
/// ```
///
/// * A token counts only at the start of the text or after a character that is not a letter, a
///   digit or one of `_ @ # & / .` (so not inside words, URLs or e-mail addresses). That
///   character belongs to the match: it cannot be the end of the token before it, which is why
///   "#a#b" links only "#a".
/// * `#tag`: 1 to 100 characters, each a letter, a combining mark, a digit or "_", matched
///   greedily. Letters and digits are `is_word_char`, combining marks `is_combining_mark`.
/// * `@name`: 1 to 30 characters of `[A-Za-z0-9_.]`, matched greedily, that neither start nor
///   end with "." (a trailing "." is given back: "@alice." links "@alice").
/// * The token must not be followed by a letter, a digit or "_"; when the greedy match is, the
///   scanner backtracks like the regular expression does (shorter matches are tried).
///
/// Example: "Baked with @fixture_tidewater. #sourdough #朝ごはん" gives the mention
/// "fixture_tidewater" and the hashtags "sourdough" and "朝ごはん"; "mail@example.com" and
/// "https://x.example/#top" give nothing.
pub fn scan_caption_links(text: &str) -> Vec<CaptionLink> {
    let mut links = Vec::new();
    if !text.contains(['#', '@']) {
        return links;
    }
    // Where the next match may begin: at its prefix character or, at the very start, at the token.
    let mut position = 0;
    while let Some(character) = text[position..].chars().next() {
        let mut found = if position == 0 { token_at(text, 0) } else { None };
        if found.is_none() && is_token_prefix(character) {
            found = token_at(text, position + character.len_utf8());
        }
        match found {
            Some(link) => {
                position = link.range.end;
                links.push(link);
            }
            None => position += character.len_utf8(),
        }
    }
    links
}

// --- Avatar placeholder ---------------------------------------------------------------------

/// Which of the twelve placeholder gradients (`PLACEHOLDER_KEYS`) a username gets: FNV-1a over
/// its UTF-16 code units, modulo 12 (importedPosts.ts `placeholderKey`; spec-viewer 0):
/// `hash = 0x811c9dc5; for each unit c: hash ^= c; hash = hash * 0x01000193 (wrapping, 32 bit)`.
pub fn placeholder_index(username: &str) -> usize {
    let mut hash: u32 = 0x811c_9dc5;
    for unit in username.encode_utf16() {
        hash ^= u32::from(unit);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash as usize % PLACEHOLDER_KEYS.len()
}

/// The letter of a placeholder avatar: the first `[A-Za-z0-9]` character of the username in
/// upper case, or '?' when it has none (common.tsx:8-66).
pub fn avatar_initial(username: &str) -> char {
    username.chars().find(char::is_ascii_alphanumeric).map_or('?', |character| character.to_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn js_trim_strips_the_byte_order_mark_and_keeps_next_line() {
        assert_eq!(js_trim("\u{FEFF}  text\u{00A0}\u{3000}\n"), "text");
        assert_eq!(js_trim("\u{0085}text\u{0085}"), "\u{0085}text\u{0085}");
        assert_eq!(js_trim_start(" \t a "), "a ");
        assert_eq!(js_trim_end(" a \u{2028}"), " a");
    }

    #[test]
    fn lengths_count_utf16_code_units() {
        // The counter example of the clone's tests: 38 units, the emoji counts 2.
        assert_eq!(utf16_len("  Low tide again 🌊\nThe second line.  "), 38);
        assert_eq!(utf16_len("🍋"), 2);
    }

    #[test]
    fn truncation_never_splits_a_character() {
        assert_eq!(truncate_utf16("ab🍋cd", 3), "ab");
        assert_eq!(truncate_utf16("ab🍋cd", 4), "ab🍋");
        assert_eq!(truncate_utf16("abc", 3), "abc");
        assert_eq!(truncate_utf16("abc", 0), "");
        assert_eq!(truncate_utf16("", 5), "");
    }

    #[test]
    fn exact_counts_use_en_us_separators() {
        assert_eq!(format_count_exact(0), "0");
        assert_eq!(format_count_exact(999), "999");
        assert_eq!(format_count_exact(1234), "1,234");
        assert_eq!(format_count_exact(10_000), "10,000");
        assert_eq!(format_count_exact(1_234_567), "1,234,567");
    }

    #[test]
    fn byte_sizes_read_like_the_clone() {
        const MIB: u64 = 1024 * 1024;
        assert_eq!(format_bytes(64 * MIB), "64 MB");
        assert_eq!(format_bytes(1024 * MIB), "1 GB");
        assert_eq!(format_bytes(4096 * MIB), "4 GB");
        assert_eq!(format_bytes(32 * MIB), "32 MB");
        assert_eq!(format_bytes(MIB + MIB / 2), "1.5 MB");
        assert_eq!(format_bytes(1536), "2 KB");
        assert_eq!(format_bytes(1023), "1023 bytes");
    }

    #[test]
    fn civil_time_round_trips() {
        let civil = civil_from_epoch_ms(1_793_545_445_417);
        let expected =
            CivilTime { year: 2026, month: 11, day: 1, hour: 15, minute: 4, second: 5, millisecond: 417 };
        assert_eq!(civil, expected);
        assert_eq!(epoch_ms_from_civil(&civil), Some(1_793_545_445_417));

        assert_eq!(civil_from_epoch_ms(0).year, 1970);
        let before = civil_from_epoch_ms(-1);
        assert_eq!((before.year, before.month, before.day, before.millisecond), (1969, 12, 31, 999));
        let leap = CivilTime { year: 2024, month: 2, day: 29, hour: 0, minute: 0, second: 0, millisecond: 0 };
        assert_eq!(civil_from_epoch_ms(epoch_ms_from_civil(&leap).unwrap()), leap);
    }

    #[test]
    fn impossible_civil_times_are_rejected() {
        let valid = CivilTime { year: 2026, month: 11, day: 1, hour: 0, minute: 0, second: 0, millisecond: 0 };
        assert!(epoch_ms_from_civil(&valid).is_some());
        assert!(epoch_ms_from_civil(&CivilTime { month: 13, ..valid }).is_none());
        assert!(epoch_ms_from_civil(&CivilTime { day: 31, ..valid }).is_none());
        assert!(epoch_ms_from_civil(&CivilTime { day: 0, ..valid }).is_none());
        assert!(epoch_ms_from_civil(&CivilTime { hour: 24, ..valid }).is_none());
        assert!(epoch_ms_from_civil(&CivilTime { year: 2026, month: 2, day: 29, ..valid }).is_none());
    }

    #[test]
    fn the_local_offset_is_a_plausible_number_of_minutes() {
        let offset = local_offset_minutes(1_793_545_445_000);
        assert!((-14 * 60..=14 * 60).contains(&offset), "offset {offset}");
        // Far outside what the operating system can convert: reported as UTC, never a panic.
        assert_eq!(local_offset_minutes(i64::MAX), 0);
    }

    // --- The editor's text rules ---------------------------------------------------------------

    #[test]
    fn line_breaks_become_line_feeds() {
        assert_eq!(normalize_newlines("one\r\ntwo\rthree\nfour"), "one\ntwo\nthree\nfour");
        assert_eq!(normalize_newlines("\r\r\n\n\r"), "\n\n\n\n");
        assert_eq!(normalize_newlines("no carriage return"), "no carriage return");
        assert_eq!(normalize_newlines(""), "");
    }

    #[test]
    fn the_caption_limit_counts_utf16_code_units_of_the_raw_text() {
        let mut short = String::from("  Low tide again 🌊\nThe second line.  ");
        assert!(!enforce_caption_limit(&mut short));
        assert_eq!(short, "  Low tide again 🌊\nThe second line.  ");

        let mut exact = "x".repeat(10_000);
        assert!(!enforce_caption_limit(&mut exact));
        assert_eq!(exact.len(), 10_000);

        let mut long = "x".repeat(10_001);
        assert!(enforce_caption_limit(&mut long));
        assert_eq!(long.len(), 10_000);

        // 5,000 lemons are exactly 10,000 code units; one more does not fit.
        let mut lemons = "🍋".repeat(5_000);
        assert!(!enforce_caption_limit(&mut lemons));
        let mut lemons = "🍋".repeat(5_001);
        assert!(enforce_caption_limit(&mut lemons));
        assert_eq!(utf16_len(&lemons), 10_000);

        // A lemon that would end at unit 10,001 is dropped whole: 9,999 units remain.
        let mut split = format!("{}🍋", "x".repeat(9_999));
        assert!(enforce_caption_limit(&mut split));
        assert_eq!(split, "x".repeat(9_999));

        // White space counts; nothing is trimmed.
        let mut spaces = " ".repeat(10_002);
        assert!(enforce_caption_limit(&mut spaces));
        assert_eq!(spaces.len(), 10_000);
    }

    #[test]
    fn pasted_windows_line_breaks_are_normalised_before_counting() {
        let mut pasted = String::from("one\r\ntwo\r\n");
        assert!(enforce_caption_limit(&mut pasted));
        assert_eq!(pasted, "one\ntwo\n");

        // 10,000 units once every "\r\n" is one unit: nothing is cut.
        let mut pasted = "ab\r\n".repeat(3_333) + "c";
        assert!(enforce_caption_limit(&mut pasted));
        assert_eq!(pasted, "ab\n".repeat(3_333) + "c");
        assert_eq!(utf16_len(&pasted), 10_000);
    }

    #[test]
    fn save_stores_the_trimmed_text() {
        // library.spec.ts:647 and test_library.py:551-556.
        assert_eq!(saved_caption("  Low tide again 🌊\nThe second line.  "), "Low tide again 🌊\nThe second line.");
        assert_eq!(saved_caption("  New text 🍋\nsecond line  "), "New text 🍋\nsecond line");
        assert_eq!(saved_caption("\u{FEFF} a  b \r\n c \u{00A0}\r\n"), "a  b \n c");
        assert_eq!(saved_caption(" \n\t "), "");
        assert_eq!(saved_caption(""), "");
        // U+0085 is no white space for JavaScript.
        assert_eq!(saved_caption("\u{0085}a\u{0085}"), "\u{0085}a\u{0085}");
    }

    #[test]
    fn save_is_enabled_only_for_a_real_change() {
        assert!(!caption_changed("Croissants", "Croissants"));
        assert!(!caption_changed("  Croissants \n", "Croissants"));
        assert!(!caption_changed("Croissants", "  Croissants  "));
        assert!(!caption_changed("", "   "));
        assert!(caption_changed("", "Croissants"));
        assert!(caption_changed("Croissants 🥐", "Croissants"));
        assert!(caption_changed("Crois sants", "Croissants"));
        assert!(caption_changed("croissants", "Croissants"));
    }

    #[test]
    fn manifest_text_is_cleaned_like_the_clone() {
        assert_eq!(clean_text("  hello\0 world \n", 100), "hello world");
        assert_eq!(clean_text("\0\0", 100), "");
        assert_eq!(clean_text("\u{FEFF}caption\u{2029}", 100), "caption");
        assert_eq!(clean_text("abcdef", 3), "abc");
        // The cut never leaves half an emoji behind.
        assert_eq!(clean_text("ab🍋cd", 3), "ab");
        assert_eq!(clean_text("ab🍋cd", 4), "ab🍋");
        // Trimming happens before the cut, so white space inside the limit stays.
        assert_eq!(clean_text("  ab cd  ", 3), "ab ");
        assert_eq!(utf16_len(&clean_text(&"x".repeat(10_050), MAX_CAPTION_CHARS)), 10_000);
    }

    // --- Caption preview (results of the clone's captionPreview, taken with Node) ---------------

    fn preview(caption: &str) -> Option<&str> {
        let preview = caption_preview(caption);
        if preview.truncated {
            Some(preview.text)
        } else {
            assert_eq!(preview.text, caption, "an untruncated preview is the caption as given");
            None
        }
    }

    #[test]
    fn short_captions_are_shown_whole() {
        assert_eq!(preview("A short caption."), None);
        assert_eq!(preview(""), None);
        assert_eq!(preview("   "), None);
        assert_eq!(preview("  hello  "), None);
        assert_eq!(preview(&"x".repeat(96)), None);
        // Only white space lies beyond the limit: nothing would be hidden.
        assert_eq!(preview(&format!("{}   ", "x".repeat(96))), None);
    }

    #[test]
    fn long_captions_are_cut_at_96_code_units() {
        let x97 = "x".repeat(97);
        assert_eq!(preview(&x97), Some("x".repeat(96).as_str()));
        let sentence = "Low tide again 🌊 and the whole pier smelled of salt and diesel. We stayed until the light \
                        went out, then walked home #sea #evening";
        assert_eq!(
            preview(sentence),
            Some("Low tide again 🌊 and the whole pier smelled of salt and diesel. We stayed until the light went")
        );
    }

    #[test]
    fn at_most_two_lines_are_previewed() {
        assert_eq!(preview("one\ntwo\nthree"), Some("one\ntwo"));
        assert_eq!(preview("one\ntwo"), None);
        assert_eq!(preview("one\ntwo\n"), None);
        assert_eq!(preview("one\ntwo\n   "), None);
        assert_eq!(preview("one\n\nthree"), Some("one"));

        // A second line break at index 96 is not "before the cut": the word-boundary rule finds it.
        let at_96 = format!("{}\n{}\ntail", "a".repeat(40), "b".repeat(55));
        assert_eq!(preview(&at_96), Some(format!("{}\n{}", "a".repeat(40), "b".repeat(55)).as_str()));
        let at_95 = format!("{}\n{}\ntail", "a".repeat(40), "b".repeat(54));
        assert_eq!(preview(&at_95), Some(format!("{}\n{}", "a".repeat(40), "b".repeat(54)).as_str()));
    }

    #[test]
    fn a_word_boundary_is_used_from_index_58() {
        let at_57 = format!("{} {}", "a".repeat(57), "b".repeat(60));
        assert_eq!(preview(&at_57), Some(format!("{} {}", "a".repeat(57), "b".repeat(38)).as_str()));
        let at_58 = format!("{} {}", "a".repeat(58), "b".repeat(60));
        assert_eq!(preview(&at_58), Some("a".repeat(58).as_str()));

        // The white space may sit at index 96 itself, but not behind it.
        let at_95 = format!("{} {}", "a".repeat(95), "b".repeat(10));
        assert_eq!(preview(&at_95), Some("a".repeat(95).as_str()));
        let at_96 = format!("{} {}", "a".repeat(96), "b".repeat(10));
        assert_eq!(preview(&at_96), Some("a".repeat(96).as_str()));
        let at_97 = format!("{} {}", "a".repeat(97), "b".repeat(10));
        assert_eq!(preview(&at_97), Some("a".repeat(96).as_str()));

        // The last white space of a run is the boundary; the rest of the run is trimmed.
        let run = format!("{}     {}", "a".repeat(60), "b".repeat(60));
        assert_eq!(preview(&run), Some("a".repeat(60).as_str()));
    }

    #[test]
    fn the_preview_uses_the_javascript_white_space_set() {
        let no_break_space = format!("{}\u{00A0}{}", "a".repeat(70), "b".repeat(40));
        assert_eq!(preview(&no_break_space), Some("a".repeat(70).as_str()));
        // U+0085 is no white space: the cut stays at 96.
        let next_line = format!("{}\u{0085}{}", "a".repeat(70), "b".repeat(40));
        assert_eq!(preview(&next_line), Some(format!("{}\u{0085}{}", "a".repeat(70), "b".repeat(25)).as_str()));
    }

    #[test]
    fn an_emoji_is_never_split() {
        // The lemon occupies the units 95 and 96: the cut moves in front of it.
        let across = format!("{}🍋tail", "a".repeat(95));
        assert_eq!(preview(&across), Some("a".repeat(95).as_str()));
        // Here it ends exactly at unit 96 and stays.
        let before = format!("{}🍋tail", "a".repeat(94));
        assert_eq!(preview(&before), Some(format!("{}🍋", "a".repeat(94)).as_str()));
        assert_eq!(preview(&"🍋".repeat(60)), Some("🍋".repeat(48).as_str()));
    }

    #[test]
    fn the_auto_view_caption_ends_with_an_ellipsis_when_shortened() {
        assert_eq!(auto_view_caption(""), None);
        assert_eq!(auto_view_caption(" \n\u{FEFF} "), None);
        assert_eq!(auto_view_caption("  hello  ").as_deref(), Some("hello"));
        assert_eq!(auto_view_caption("A short caption.").as_deref(), Some("A short caption."));
        assert_eq!(auto_view_caption("one\ntwo\n").as_deref(), Some("one\ntwo"));
        assert_eq!(auto_view_caption("one\ntwo\nthree").as_deref(), Some("one\ntwo…"));
        assert_eq!(auto_view_caption("one\n\nthree").as_deref(), Some("one…"));
        assert_eq!(auto_view_caption(&"x".repeat(97)), Some(format!("{}…", "x".repeat(96))));

        let dots = format!("Wait for it... {}long. And more text after that to cut", "really ".repeat(12));
        assert_eq!(
            auto_view_caption(&dots).as_deref(),
            Some("Wait for it... really really really really really really really really really really really…")
        );
        // Trailing full stops and an ellipsis of the caption itself make way for the one "…".
        let full_stop = format!("{}. {}", "a".repeat(60), "b".repeat(60));
        assert_eq!(auto_view_caption(&full_stop), Some(format!("{}…", "a".repeat(60))));
        let ellipsis = format!("{}… . {}", "a".repeat(60), "b".repeat(60));
        assert_eq!(auto_view_caption(&ellipsis), Some(format!("{}…", "a".repeat(60))));
    }

    // --- Counts ---------------------------------------------------------------------------------

    #[test]
    fn compact_counts_match_the_javascript_results() {
        // Every pair is the output of the clone's formatCount in Node.
        let vectors: &[(u64, &str)] = &[
            (0, "0"), (1, "1"), (999, "999"), (1000, "1K"), (1049, "1K"), (1050, "1.1K"), (1051, "1.1K"),
            (1149, "1.1K"), (1150, "1.1K"), (1151, "1.2K"), (1250, "1.3K"), (1350, "1.4K"), (1450, "1.4K"),
            (1550, "1.6K"), (1650, "1.6K"), (1750, "1.8K"), (1850, "1.9K"), (1950, "1.9K"), (2050, "2K"),
            (2150, "2.1K"), (2250, "2.3K"), (2350, "2.4K"), (2450, "2.5K"), (2550, "2.5K"), (8300, "8.3K"),
            (8350, "8.3K"), (8450, "8.4K"), (12345, "12.3K"), (12350, "12.3K"), (12450, "12.4K"), (56789, "56.8K"),
            (99949, "99.9K"), (99950, "100K"), (100050, "100K"), (100150, "100.2K"), (263300, "263.3K"),
            (263350, "263.4K"), (999949, "999.9K"), (999950, "1000K"), (999999, "1000K"), (1000000, "1M"),
            (1049999, "1M"), (1050000, "1.1M"), (1150000, "1.1M"), (1250000, "1.3M"), (1350000, "1.4M"),
            (1450000, "1.4M"), (1750000, "1.8M"), (2250000, "2.3M"), (2350000, "2.4M"), (12350000, "12.3M"),
            (999949999, "999.9M"), (999950000, "1000M"), (1000000000, "1000M"), (1234567890, "1234.6M"),
            (9007199254740991, "9007199254.7M"),
        ];
        assert_eq!(vectors.len(), 56);
        for &(count, expected) in vectors {
            assert_eq!(format_count(count), expected, "{count}");
        }
        // Far beyond what JavaScript can count exactly: still a sentence-safe text, no panic.
        assert_eq!(format_count(u64::MAX), "18446744073709.6M");
    }

    #[test]
    fn compact_counts_round_the_binary_quotient_with_ties_up() {
        // An independent statement of toFixed(1): Rust's "{:.1}" rounds the same binary value and
        // differs only for an exact tie, which it sends to the even digit. A quotient is an exact
        // tie only when it ends in .25 or .75, and only .25 has an even digit below it.
        let expected = |count: u64, unit: u64, suffix: &str| {
            let mut digits = format!("{:.1}", count as f64 / unit as f64);
            if count % unit == unit / 4 {
                digits = format!("{}.3", count / unit);
            }
            format!("{}{suffix}", digits.strip_suffix(".0").unwrap_or(&digits))
        };
        for count in 1_000..=250_000_u64 {
            assert_eq!(format_count(count), expected(count, 1_000, "K"), "{count}");
        }
        for count in (990_000..1_000_000_u64).chain((1_000_000..=40_000_000).step_by(1_250)) {
            let (unit, suffix) = if count >= 1_000_000 { (1_000_000, "M") } else { (1_000, "K") };
            assert_eq!(format_count(count), expected(count, unit, suffix), "{count}");
        }
    }

    // --- Time -----------------------------------------------------------------------------------

    fn instant(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> i64 {
        epoch_ms_from_civil(&CivilTime { year, month, day, hour, minute, second: 0, millisecond: 0 }).unwrap()
    }

    #[test]
    fn iso_times_carry_milliseconds_only_when_there_are_any() {
        assert_eq!(iso_time(1_793_545_445_000).as_deref(), Some("2026-11-01T15:04:05Z"));
        assert_eq!(iso_time(1_793_545_445_417).as_deref(), Some("2026-11-01T15:04:05.417Z"));
        assert_eq!(iso_time(1_793_545_445_007).as_deref(), Some("2026-11-01T15:04:05.007Z"));
        assert_eq!(iso_time(instant(2026, 11, 11, 10, 0)).as_deref(), Some("2026-11-11T10:00:00Z"));
        assert_eq!(iso_time(0).as_deref(), Some("1970-01-01T00:00:00Z"));
        assert_eq!(iso_time(-1).as_deref(), Some("1969-12-31T23:59:59.999Z"));
        assert_eq!(iso_time(instant(987, 6, 5, 4, 3)).as_deref(), Some("0987-06-05T04:03:00Z"));

        assert_eq!(iso_time_millis(1_793_545_445_000).as_deref(), Some("2026-11-01T15:04:05.000Z"));
        assert_eq!(iso_time_millis(1_793_545_445_417).as_deref(), Some("2026-11-01T15:04:05.417Z"));
    }

    #[test]
    fn instants_without_a_four_digit_year_have_no_iso_text() {
        let last = instant(9999, 12, 31, 23, 59);
        assert_eq!(iso_time(last).as_deref(), Some("9999-12-31T23:59:00Z"));
        assert_eq!(iso_time(last + MS_PER_MINUTE), None);
        assert_eq!(iso_time(instant(1, 1, 1, 0, 0)).as_deref(), Some("0001-01-01T00:00:00Z"));
        assert_eq!(iso_time(instant(1, 1, 1, 0, 0) - 1), None);
        assert_eq!(iso_time(i64::MAX), None);
        assert_eq!(iso_time(i64::MIN), None);
        assert_eq!(iso_time_millis(i64::MAX), None);
    }

    #[test]
    fn epoch_numbers_are_scaled_by_their_size() {
        // manifest.test.ts: seconds, milliseconds and microseconds of the same instant.
        assert_eq!(epoch_number_to_ms(1_793_545_445.0), Some(1_793_545_445_000));
        assert_eq!(epoch_number_to_ms(1_793_545_445_000.0), Some(1_793_545_445_000));
        assert_eq!(epoch_number_to_ms(1_793_545_445_000_000.0), Some(1_793_545_445_000));
        assert_eq!(epoch_number_to_ms(1_793_545_445_000_000_000.0), Some(1_793_545_445_000));
        // The borders between the units (results of the clone's epochToMs).
        assert_eq!(epoch_number_to_ms(99_999_999_999.0), Some(99_999_999_999_000));
        assert_eq!(epoch_number_to_ms(100_000_000_000.0), Some(100_000_000_000));
        assert_eq!(epoch_number_to_ms(1e14), Some(100_000_000_000));
        assert_eq!(epoch_number_to_ms(1e17), Some(100_000_000_000));
        assert_eq!(epoch_number_to_ms(253_402_300_799.0), Some(253_402_300_799));
    }

    #[test]
    fn rounding_moves_a_half_up_like_math_round() {
        for (value, rounded) in
            [(2.5, 3.0), (-2.5, -2.0), (2.4, 2.0), (-2.6, -3.0), (0.499_999_999_999_999_94, 0.0), (-0.5, 0.0), (7.0, 7.0)]
        {
            assert_eq!(js_round(value), rounded, "{value}");
        }
        assert!(js_round(f64::NAN).is_nan());
        assert_eq!(js_round(f64::INFINITY), f64::INFINITY);
    }

    #[test]
    fn epoch_numbers_are_rounded_like_math_round() {
        assert_eq!(epoch_number_to_ms(1_793_545_445.999_6), Some(1_793_545_446_000));
        assert_eq!(epoch_number_to_ms(1_793_545_445.499_4), Some(1_793_545_445_499));
        assert_eq!(epoch_number_to_ms(1_793_545_445_000.5), Some(1_793_545_445_001));
        assert_eq!(epoch_number_to_ms(31_535_999.999_6), Some(31_536_000_000));
    }

    #[test]
    fn epoch_numbers_outside_1971_to_9999_are_no_timestamps() {
        for value in [0.0, -10.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.4, 5e-324, 31_535_999.0] {
            assert_eq!(epoch_number_to_ms(value), None, "{value}");
        }
        assert_eq!(epoch_number_to_ms(31_536_000.0), Some(31_536_000_000));
        // Nanoseconds of the year 10000 and a number no Date can hold.
        assert_eq!(epoch_number_to_ms(253_402_300_800_000_000_000.0), None);
        assert_eq!(epoch_number_to_ms(1e300), None);
        assert_eq!(epoch_number_to_ms(f64::MAX), None);
    }

    #[test]
    fn timestamp_texts_of_the_clones_tests() {
        // manifest.test.ts "parseTimestamp": every accepted input is 2026-11-01T15:04:05.000Z.
        assert_eq!(parse_timestamp_text("2026-11-01T15:04:05Z"), Some(1_793_545_445_000));
        assert_eq!(parse_timestamp_text("2026-11-01T16:04:05+01:00"), Some(1_793_545_445_000));
        assert_eq!(parse_timestamp_text("2026-11-01"), Some(1_793_491_200_000));
        assert_eq!(parse_timestamp_text("1793545445"), Some(1_793_545_445_000));
        assert_eq!(parse_timestamp_text("1793545445000"), Some(1_793_545_445_000));
        for rejected in ["", "   ", "not a date", "-5", "2 days ago", "2026-13-45T99:99:99Z"] {
            assert_eq!(parse_timestamp_text(rejected), None, "{rejected:?}");
        }
    }

    #[test]
    fn timestamp_texts_in_every_accepted_form() {
        // Results of Date.parse in Node for the same strings.
        let vectors = [
            ("2026-11-01T15:04Z", 1_793_545_440_000),
            ("2026-11-01T15:04:05.1Z", 1_793_545_445_100),
            ("2026-11-01T15:04:05.123456789Z", 1_793_545_445_123),
            ("2026-11-01T15:04:05.9999Z", 1_793_545_445_999),
            ("2026-11-01 15:04:05Z", 1_793_545_445_000),
            ("2026-11-01t15:04:05z", 1_793_545_445_000),
            ("2026-11-01T15:04:05+0100", 1_793_541_845_000),
            ("2026-11-01T15:04:05-23:59", 1_793_631_785_000),
            ("2026-11-01T24:00:00Z", 1_793_577_600_000),
            ("2024-02-29", 1_709_164_800_000),
            ("  2026-11-01T15:04:05Z\u{FEFF}\n", 1_793_545_445_000),
            ("1793545445.5", 1_793_545_445_500),
            ("0001793545445", 1_793_545_445_000),
        ];
        for (text, expected) in vectors {
            assert_eq!(parse_timestamp_text(text), Some(expected), "{text:?}");
        }
        // Native decision: a date-time without a zone is UTC on every computer.
        assert_eq!(parse_timestamp_text("2026-11-01T15:04:05"), Some(1_793_545_445_000));
        assert_eq!(parse_timestamp_text("2026-11-01 15:04"), Some(1_793_545_440_000));
    }

    #[test]
    fn timestamp_texts_that_are_rejected() {
        let rejected = [
            "2026-11-01T24:00:01Z",
            "2026-11-01T24:01:00Z",
            "2026-11-01T15:04:05+01",
            "2026-11-01T15:04:05+24:00",
            "2026-11-01T15:04:05+01:60",
            "2026-11-01T15:04:60Z",
            "2026-11-01T15:60:00Z",
            "2026-11-01T15Z",
            "2026-11-01T15:04:05,5Z",
            "2026-11-01T15:04:05.Z",
            "2026-11-01T15:04:05.1234567890Z",
            "2026-11-01T15:04:05 Z",
            "2026-11-01T15:04:05Zjunk",
            "2026-11-01T",
            "2026-11-01Z",
            "2026-11",
            "2026-1-1",
            "2026/11/01",
            "20261101",
            "2026-02-29",
            "2026-04-31T00:00:00Z",
            "2026-00-10",
            "2026-11-00",
            "November 1, 2026",
            "+002026-11-01T00:00:00Z",
            "１７９３５４５４４５",
            "1793545445.",
            ".5",
            "1e9",
            "0x10",
            "0",
            "0.0",
            // Years outside 1971..=9999.
            "1970-12-31T23:59:59Z",
            "1969-07-20",
            "0000-01-01",
            "31535999",
            // A zone offset that moves the instant out of the range.
            "1971-01-01T00:00:00+00:01",
            "9999-12-31T23:59:59-00:01",
        ];
        for text in rejected {
            assert_eq!(parse_timestamp_text(text), None, "{text:?}");
        }
        assert_eq!(parse_timestamp_text("1971-01-01T00:00:00Z"), Some(31_536_000_000));
        assert_eq!(parse_timestamp_text("1971-01-01T00:01:00+00:01"), Some(31_536_000_000));
        assert_eq!(parse_timestamp_text("9999-12-31T23:59:59.999Z"), Some(253_402_300_799_999));
        // A digit string too long for any number is rejected, not a panic.
        assert_eq!(parse_timestamp_text(&"9".repeat(400)), None);
    }

    #[test]
    fn timestamps_of_any_json_value() {
        assert_eq!(parse_timestamp(&json!(1793545445)), Some(1_793_545_445_000));
        assert_eq!(parse_timestamp(&json!(1793545445000_u64)), Some(1_793_545_445_000));
        assert_eq!(parse_timestamp(&json!(1793545445000000_u64)), Some(1_793_545_445_000));
        assert_eq!(parse_timestamp(&json!(1793545445.5)), Some(1_793_545_445_500));
        assert_eq!(parse_timestamp(&json!("1793545445")), Some(1_793_545_445_000));
        assert_eq!(parse_timestamp(&json!("2026-11-01T16:04:05+01:00")), Some(1_793_545_445_000));
        for rejected in [json!(null), json!(true), json!({}), json!([]), json!(0), json!(-10), json!(""), json!("-5")] {
            assert_eq!(parse_timestamp(&rejected), None, "{rejected}");
        }
        // What the reader hands on is what the writer prints (spec-export F.7).
        let read = parse_timestamp(&json!("2026-11-01T16:04:05+01:00")).unwrap();
        assert_eq!(iso_time_millis(read).as_deref(), Some("2026-11-01T15:04:05.000Z"));
        assert_eq!(iso_time(read).as_deref(), Some("2026-11-01T15:04:05Z"));
    }

    #[test]
    fn relative_times_have_no_months_or_years() {
        const MINUTE: i64 = 60_000;
        const HOUR: i64 = 60 * MINUTE;
        const DAY: i64 = 24 * HOUR;
        let now = 1_793_545_445_000;
        let label = |age: i64| relative_time_label(now, now - age);
        assert_eq!(label(0), "now");
        assert_eq!(label(MINUTE - 1), "now");
        assert_eq!(label(MINUTE), "1m");
        assert_eq!(label(35 * MINUTE), "35m");
        assert_eq!(label(HOUR - 1), "59m");
        assert_eq!(label(HOUR), "1h");
        assert_eq!(label(7 * HOUR + 59 * MINUTE), "7h");
        assert_eq!(label(DAY - 1), "23h");
        assert_eq!(label(DAY), "1d");
        assert_eq!(label(2 * DAY), "2d");
        assert_eq!(label(7 * DAY - 1), "6d");
        assert_eq!(label(7 * DAY), "1w");
        assert_eq!(label(20 * DAY), "2w");
        assert_eq!(label(3 * 365 * DAY), "156w");
        // The future, also the far future, reads "now".
        assert_eq!(label(-1), "now");
        assert_eq!(label(-10 * DAY), "now");
        assert_eq!(relative_time_label(i64::MIN, i64::MAX), "now");
        assert!(relative_time_label(i64::MAX, i64::MIN).ends_with('w'));
    }

    #[test]
    fn date_labels_are_upper_case_en_us_dates() {
        let now = instant(2026, 11, 12, 12, 0);
        let utc: &dyn Fn(i64) -> i32 = &|_| 0;
        assert_eq!(date_label(now, 1_793_545_445_000, utc), "NOVEMBER 1");
        assert_eq!(date_label(now, instant(2026, 1, 31, 0, 0), utc), "JANUARY 31");
        assert_eq!(date_label(now, instant(2024, 2, 29, 9, 30), utc), "FEBRUARY 29, 2024");
        assert_eq!(date_label(now, instant(2027, 5, 9, 9, 30), utc), "MAY 9, 2027");
        let months = [
            "JANUARY", "FEBRUARY", "MARCH", "APRIL", "MAY", "JUNE", "JULY", "AUGUST", "SEPTEMBER", "OCTOBER",
            "NOVEMBER", "DECEMBER",
        ];
        for (index, name) in months.iter().enumerate() {
            assert_eq!(date_label(now, instant(2026, index as u32 + 1, 15, 12, 0), utc), format!("{name} 15"));
        }
    }

    #[test]
    fn date_labels_use_local_time() {
        let now = instant(2026, 11, 12, 12, 0);
        let late = instant(2026, 11, 1, 23, 30);
        assert_eq!(date_label(now, late, &|_| 0), "NOVEMBER 1");
        assert_eq!(date_label(now, late, &|_| 60), "NOVEMBER 2");
        assert_eq!(date_label(now, instant(2026, 11, 1, 0, 30), &|_| -300), "OCTOBER 31");

        // The year is the local year too, for the picture and for "now".
        let new_year = instant(2026, 1, 1, 2, 0);
        assert_eq!(date_label(now, new_year, &|_| 0), "JANUARY 1");
        assert_eq!(date_label(now, new_year, &|_| -300), "DECEMBER 31, 2025");
        let just_after_midnight = instant(2026, 1, 1, 0, 30);
        assert_eq!(date_label(just_after_midnight, instant(2025, 12, 31, 12, 0), &|_| 0), "DECEMBER 31, 2025");
        assert_eq!(date_label(just_after_midnight, instant(2025, 12, 31, 12, 0), &|_| -60), "DECEMBER 31");

        // The offset is asked for each instant: summer time for the picture, winter time for now.
        let switch = instant(2026, 10, 25, 1, 0);
        let berlin = move |utc_ms: i64| if utc_ms < switch { 120 } else { 60 };
        assert_eq!(date_label(now, instant(2026, 7, 31, 22, 30), &berlin), "AUGUST 1");
        assert_eq!(date_label(now, instant(2026, 10, 31, 22, 30), &berlin), "OCTOBER 31");
        // Extreme instants give some label, never a panic.
        assert!(!date_label(i64::MAX, i64::MIN, &|_| 840).is_empty());
    }

    // --- Ids and names --------------------------------------------------------------------------

    #[test]
    fn ids_are_cleaned_like_the_clone() {
        assert_eq!(clean_id("weird id/with <chars>").as_deref(), Some("weird_id_with_chars_"));
        assert_eq!(clean_id("12345").as_deref(), Some("12345"));
        assert_eq!(clean_id("Sunset at the pier (final)").as_deref(), Some("Sunset_at_the_pier_final_"));
        assert_eq!(clean_id("  C8ZmP4RvNd7 ").as_deref(), Some("C8ZmP4RvNd7"));
        assert_eq!(clean_id("\u{FEFF}IMG_0042\u{00A0}").as_deref(), Some("IMG_0042"));
        assert_eq!(clean_id("🍋a").as_deref(), Some("_a"));
        assert_eq!(clean_id("ä1").as_deref(), Some("_1"));
        assert_eq!(clean_id(&"a".repeat(120)), Some("a".repeat(100)));
        for rejected in ["###", "", "   ", "🍋", "._-", "朝ごはん"] {
            assert_eq!(clean_id(rejected), None, "{rejected:?}");
        }
    }

    #[test]
    fn the_hash_matches_the_clones_cyrb53() {
        // Results of the clone's hash53 in Node: the 53-bit value and its base-36 text.
        let vectors: [(&str, u64, &str); 6] = [
            ("", 3_338_908_027_751_811_u64, "wvjl67o803"),
            ("a", 7_929_297_801_672_961, "262p94epp1d"),
            ("hello", 4_625_896_200_565_286, "19jqrdkps12"),
            ("alice\n2026-11-01T15:04:05.000Z\nposts/a.jpg", 7_050_785_890_582_802, "1xfamblctg2"),
            ("🍋 (final).jpg", 1_416_413_452_220_747, "dy2rcuqnuz"),
            ("朝ごはん.png", 7_405_348_264_842_521, "20wz610gljt"),
        ];
        for (text, hash, base36) in vectors {
            assert_eq!(hash53(text), hash, "{text:?}");
            assert_eq!(to_base36(hash), base36, "{text:?}");
            assert!(hash < 1 << 53);
        }
    }

    #[test]
    fn fnv1a_matches_the_reference_vectors() {
        // From the FNV reference implementation's test suite.
        assert_eq!(fnv1a_64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a_64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a_64(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn base_36_uses_lower_case_digits() {
        assert_eq!(to_base36(0), "0");
        assert_eq!(to_base36(35), "z");
        assert_eq!(to_base36(36), "10");
        assert_eq!(to_base36(1295), "zz");
        assert_eq!(to_base36(1296), "100");
        assert_eq!(to_base36(9_007_199_254_740_991), "2gosa7pa2gv");
        assert_eq!(to_base36(9_007_199_254_740_992), "2gosa7pa2gw");
        assert_eq!(to_base36(u64::MAX), "3w5e11264sgsf");
    }

    #[test]
    fn derived_ids_fingerprint_author_time_and_files() {
        let one = ["posts/a.jpg".to_string()];
        let two = ["posts/a.jpg".to_string(), "posts/b.jpg".to_string()];
        assert_eq!(derived_post_id("alice", Some(1_793_545_445_000), &one), "post-1xfamblctg2");
        assert_eq!(derived_post_id("alice", None, &two), "post-h56u4rj7vg");
        assert_eq!(derived_post_id("", None, &["posts/1.jpg".to_string()]), "post-2ehez72ylty");
        // A time without an ISO form counts as no time.
        assert_eq!(derived_post_id("alice", Some(i64::MAX), &two), "post-h56u4rj7vg");
        // Each part matters.
        assert_ne!(derived_post_id("bob", None, &two), derived_post_id("alice", None, &two));
        assert_ne!(derived_post_id("alice", None, &one), derived_post_id("alice", None, &two));
        assert_ne!(derived_post_id("alice", Some(1), &one), derived_post_id("alice", Some(2), &one));

        assert_eq!(derived_file_id("🍋 (final).jpg"), "post-dy2rcuqnuz");
        assert_eq!(derived_file_id("朝ごはん.png"), "post-20wz610gljt");
        assert_eq!(derived_file_id("###.jpg"), "post-tmfxxkmcjm");
    }

    #[test]
    fn duplicate_ids_get_a_running_number() {
        let mut used = HashSet::new();
        assert_eq!(unique_id("C8ZmP4RvNd7", &mut used), "C8ZmP4RvNd7");
        assert_eq!(unique_id("C8ZmP4RvNd7", &mut used), "C8ZmP4RvNd7-2");
        assert_eq!(unique_id("C8ZmP4RvNd7", &mut used), "C8ZmP4RvNd7-3");
        // An id that is itself taken by an earlier duplicate moves on as well.
        assert_eq!(unique_id("C8ZmP4RvNd7-2", &mut used), "C8ZmP4RvNd7-2-2");
        // Letter case makes a different id.
        assert_eq!(unique_id("c8zmp4rvnd7", &mut used), "c8zmp4rvnd7");
        assert_eq!(used.len(), 5);
        // A number that is already used is skipped.
        let mut used: HashSet<String> = ["a".to_string(), "a-2".to_string()].into();
        assert_eq!(unique_id("a", &mut used), "a-3");
    }

    #[test]
    fn avatar_stems_match_the_clone() {
        // The same calls in the same order against the clone's avatarStem, sharing one set.
        let vectors: Vec<(String, &str)> = vec![
            ("../CON".to_string(), "_CON"),
            ("fixture.sunrise.bakery".to_string(), "fixture.sunrise.bakery"),
            ("Alice".to_string(), "Alice"),
            ("alice".to_string(), "alice-2"),
            ("ALICE".to_string(), "ALICE-3"),
            ("...".to_string(), "user"),
            (String::new(), "user-2"),
            ("🍋🍋".to_string(), "_"),
            ("com1".to_string(), "com1_"),
            ("COM0".to_string(), "COM0"),
            ("lpt9".to_string(), "lpt9_"),
            ("nul.".to_string(), "nul_"),
            ("a b  c".to_string(), "a_b_c"),
            ("ünï".to_string(), "_n_"),
            (".hidden.".to_string(), "hidden"),
            ("x".repeat(70), "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"),
            ("x".repeat(63) + ".y", "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx."),
            ("aux".to_string(), "aux_"),
            ("AUX".to_string(), "AUX_-2"),
            ("con.txt".to_string(), "con.txt"),
            ("_".to_string(), "_-2"),
            ("user".to_string(), "user-3"),
        ];
        let mut taken = HashSet::new();
        for (username, expected) in &vectors {
            assert_eq!(avatar_stem(username, &mut taken), *expected, "{username:?}");
        }
        assert!(taken.contains("alice-3"), "the set holds lower-cased stems");
        // A fresh set (a new archive part) starts again.
        assert_eq!(avatar_stem("alice", &mut HashSet::new()), "alice");
    }

    #[test]
    fn every_windows_device_name_is_escaped() {
        for name in ["con", "PRN", "Aux", "nul", "com1", "COM9", "lpt1", "LPT9"] {
            assert_eq!(avatar_stem(name, &mut HashSet::new()), format!("{name}_"));
        }
        for name in ["com", "com0", "com10", "lpt0", "lptx", "console", "nul1"] {
            assert_eq!(avatar_stem(name, &mut HashSet::new()), name);
        }
    }

    #[test]
    fn download_names_keep_only_safe_characters() {
        assert_eq!(archive_stem("FXTbasic004"), "FXTbasic004");
        assert_eq!(archive_stem("post 1/2 (final)🍋"), "post_1_2_final_");
        assert_eq!(archive_stem(&"a".repeat(70)), "a".repeat(64));
        // No other sanitising: dots and device names stay.
        assert_eq!(archive_stem("..con"), "..con");
        assert_eq!(archive_stem(""), "");
    }

    #[test]
    fn file_names_sort_like_explorer() {
        let mut names = vec![
            "IMG_10.jpg", "img_1.png", "IMG_2.jpg", "Sunset at the pier (final).webp", "portrait.heic", "broken.jpg",
            "notes.txt",
        ];
        names.sort_by(|left, right| natural_cmp(left, right));
        assert_eq!(
            names,
            ["broken.jpg", "img_1.png", "IMG_2.jpg", "IMG_10.jpg", "notes.txt", "portrait.heic", "Sunset at the pier (final).webp"]
        );
    }

    #[test]
    fn digit_runs_compare_as_numbers() {
        assert_eq!(natural_cmp("a2", "a10"), Ordering::Less);
        assert_eq!(natural_cmp("a10", "a2"), Ordering::Greater);
        assert_eq!(natural_cmp("a2b", "a2a"), Ordering::Greater);
        assert_eq!(natural_cmp("1-2", "1-10"), Ordering::Less);
        assert_eq!(natural_cmp("x9", "x10y"), Ordering::Less);
        // Longer than any integer type.
        assert_eq!(natural_cmp("n99999999999999999999999998", "n99999999999999999999999999"), Ordering::Less);
        assert_eq!(natural_cmp("n100000000000000000000000000", "n99999999999999999999999999"), Ordering::Greater);
        // Leading zeros do not change the number; equal names fall back to the plain text.
        assert_eq!(natural_cmp("a007", "a8"), Ordering::Less);
        assert_eq!(natural_cmp("a007", "a7"), Ordering::Less);
        assert_eq!(natural_cmp("a7", "a007"), Ordering::Greater);
        assert_eq!(natural_cmp("a0", "a000"), Ordering::Less);
        assert_eq!(natural_cmp("a007b", "a7a"), Ordering::Greater);
    }

    #[test]
    fn the_order_ignores_letter_case_and_is_total() {
        assert_eq!(natural_cmp("abc", "ABD"), Ordering::Less);
        assert_eq!(natural_cmp("ABC", "abd"), Ordering::Less);
        assert_eq!(natural_cmp("Zebra", "apple"), Ordering::Greater);
        assert_eq!(natural_cmp("ÄRGER", "ärger2"), Ordering::Less);
        assert_eq!(natural_cmp("a", "a"), Ordering::Equal);
        assert_eq!(natural_cmp("", ""), Ordering::Equal);
        assert_eq!(natural_cmp("", "a"), Ordering::Less);
        assert_eq!(natural_cmp("a", "ab"), Ordering::Less);
        // Same name in another letter case: never Equal, and opposite when swapped.
        assert_eq!(natural_cmp("IMG.JPG", "img.jpg"), Ordering::Less);
        assert_eq!(natural_cmp("img.jpg", "IMG.JPG"), Ordering::Greater);

        let mut names = vec!["b1", "B01", "a", "A", "b001", "B1", "10", "9", "09", "_", "é", "E"];
        names.sort_by(|left, right| natural_cmp(left, right));
        assert_eq!(names, ["09", "9", "10", "_", "A", "a", "B01", "B1", "b001", "b1", "E", "é"]);
        // Sorting again from another start order gives the same list.
        let mut again = names.clone();
        again.reverse();
        again.sort_by(|left, right| natural_cmp(left, right));
        assert_eq!(again, names);
    }

    // --- Hashtags and mentions (results of the clone's TOKEN_RE, taken with Node) ---------------

    /// (start byte, end byte, sign, name) of every link, the way the Node script printed them.
    fn links(text: &str) -> Vec<(usize, usize, char, String)> {
        scan_caption_links(text)
            .into_iter()
            .map(|link| {
                let sign = match link.kind {
                    CaptionLinkKind::Hashtag => '#',
                    CaptionLinkKind::Mention => '@',
                };
                assert_eq!(&text[link.range.clone()], format!("{sign}{}", link.name), "range and name agree");
                (link.range.start, link.range.end, sign, link.name)
            })
            .collect()
    }

    fn link(start: usize, end: usize, sign: char, name: &str) -> (usize, usize, char, String) {
        (start, end, sign, name.to_string())
    }

    #[test]
    fn finds_hashtags_and_mentions() {
        assert_eq!(
            links("Baked with @fixture_tidewater. #sourdough #朝ごはん"),
            [link(11, 29, '@', "fixture_tidewater"), link(31, 41, '#', "sourdough"), link(42, 55, '#', "朝ごはん")]
        );
        assert_eq!(links("#a #b"), [link(0, 2, '#', "a"), link(3, 5, '#', "b")]);
        assert_eq!(links("(#tag)"), [link(1, 5, '#', "tag")]);
        assert_eq!(links("#tag!"), [link(0, 4, '#', "tag")]);
        assert_eq!(links("#1"), [link(0, 2, '#', "1")]);
        assert_eq!(links("#_"), [link(0, 2, '#', "_")]);
        assert_eq!(links("@_"), [link(0, 2, '@', "_")]);
        assert_eq!(links("@a"), [link(0, 2, '@', "a")]);
        assert_eq!(links("\n#b\n@c"), [link(1, 3, '#', "b"), link(4, 6, '@', "c")]);
        assert_eq!(links("#tag @user"), [link(0, 4, '#', "tag"), link(5, 10, '@', "user")]);
        assert_eq!(
            links("see #one,#two;#three"),
            [link(4, 8, '#', "one"), link(9, 13, '#', "two"), link(14, 20, '#', "three")]
        );
        assert_eq!(links("@alice’s #tag’s"), [link(0, 6, '@', "alice"), link(11, 15, '#', "tag")]);
        assert_eq!(links("🍋#a"), [link(4, 6, '#', "a")]);
        assert_eq!(links("#a🍋"), [link(0, 2, '#', "a")]);
        assert_eq!(links("#a_b-c"), [link(0, 4, '#', "a_b")]);
        assert_eq!(links("@a.b_c-d"), [link(0, 6, '@', "a.b_c")]);
        assert_eq!(links("@abc-def"), [link(0, 4, '@', "abc")]);
    }

    #[test]
    fn tokens_inside_words_addresses_and_urls_are_no_links() {
        let nothing: [(usize, usize, char, String); 0] = [];
        for text in [
            "mail@example.com", "https://x.example/#top", "x#tag", "#", "@", "# tag", "&#x27;", "a/#b", "a.#b", "@@a",
            "##a", "#@a", "@#a", "@.alice", "#🍋", "＃tag", "@abcé", "plain text", "",
        ] {
            assert_eq!(links(text), nothing, "{text:?}");
        }
        assert_eq!(links("a,#b"), [link(2, 4, '#', "b")]);
    }

    #[test]
    fn the_character_before_a_token_belongs_to_the_match() {
        // The second token has no character of its own in front of it.
        assert_eq!(links("#a#b"), [link(0, 2, '#', "a")]);
        assert_eq!(links("@a@b"), [link(0, 2, '@', "a")]);
        assert_eq!(links("#a@b"), [link(0, 2, '#', "a")]);
        assert_eq!(links("#tag.@user"), [link(0, 4, '#', "tag")]);
        // A combining mark ends the first tag; it is consumed and cannot introduce the next one.
        assert_eq!(links("#a\u{0301}#b"), [link(0, 4, '#', "a\u{0301}")]);
        assert_eq!(
            links("#caf\u{00E9}\u{0301} #e\u{0301}#x"),
            [link(0, 8, '#', "caf\u{00E9}\u{0301}"), link(9, 13, '#', "e\u{0301}")]
        );
        assert_eq!(links("#tag\u{0E48}"), [link(0, 7, '#', "tag\u{0E48}")]);
        // A mark after a mention is not "a letter after the name".
        assert_eq!(links("@name\u{0301}"), [link(0, 5, '@', "name")]);
        // A zero-width joiner is an ordinary character in front of a token.
        assert_eq!(links("#x\u{200D}#y"), [link(0, 2, '#', "x"), link(5, 7, '#', "y")]);
        assert_eq!(links("#a/#b"), [link(0, 2, '#', "a")]);
        assert_eq!(links("@a/@b"), [link(0, 2, '@', "a")]);
        assert_eq!(links("(@a)(#b)"), [link(1, 3, '@', "a"), link(5, 7, '#', "b")]);
        assert_eq!(links("a@b #c"), [link(4, 6, '#', "c")]);
        assert_eq!(
            links("ok @a_b.c, #x-y and @z!"),
            [link(3, 9, '@', "a_b.c"), link(11, 13, '#', "x"), link(20, 22, '@', "z")]
        );
    }

    #[test]
    fn mentions_follow_the_username_shape() {
        assert_eq!(links("@alice."), [link(0, 6, '@', "alice")]);
        assert_eq!(links("@alice.bob."), [link(0, 10, '@', "alice.bob")]);
        assert_eq!(links("@alice..bob"), [link(0, 11, '@', "alice..bob")]);
        assert_eq!(links("@a."), [link(0, 2, '@', "a")]);
        assert_eq!(links("@a.."), [link(0, 2, '@', "a")]);
        assert_eq!(links("@a.b"), [link(0, 4, '@', "a.b")]);
        assert_eq!(links("@ab."), [link(0, 3, '@', "ab")]);

        let thirty = "a".repeat(30);
        assert_eq!(links(&format!("@{thirty}")), [link(0, 31, '@', &thirty)]);
        assert_eq!(links(&format!("@{thirty}.")), [link(0, 31, '@', &thirty)]);
        assert!(links(&format!("@{}", "a".repeat(31))).is_empty());
        let twenty_nine = "a".repeat(29);
        assert_eq!(links(&format!("@{twenty_nine}.")), [link(0, 30, '@', &twenty_nine)]);
        // Too long as a whole, but the part before the dot is a name that is not followed by a letter.
        assert_eq!(links(&format!("@{}.{}", "a".repeat(10), "b".repeat(25))), [link(0, 11, '@', &"a".repeat(10))]);
        // The 30 characters end with a dot, which is given back: 29 remain.
        let dotted = "a.".repeat(40);
        assert_eq!(links(&format!(" @{dotted}")), [link(1, 31, '@', &dotted[..29])]);
        assert_eq!(links("x @A.B. y"), [link(2, 6, '@', "A.B")]);
    }

    #[test]
    fn hashtags_take_at_most_100_characters() {
        let hundred = "a".repeat(100);
        assert_eq!(links(&format!("#{hundred}")), [link(0, 101, '#', &hundred)]);
        assert!(links(&format!("#{}", "a".repeat(101))).is_empty());
        // 100 letters followed by a combining mark: the mark is not "a letter after the tag".
        assert_eq!(links(&format!("#{hundred}\u{0301}")), [link(0, 101, '#', &hundred)]);
        // Too long, but the pattern may give characters back to just before a combining mark.
        let fifty = "a".repeat(50);
        assert_eq!(links(&format!("#{fifty}\u{0301}{}", "a".repeat(60))), [link(0, 51, '#', &fifty)]);
    }

    #[test]
    fn hashtags_in_other_scripts_keep_their_marks() {
        assert_eq!(links("#ไทยแลนด์ สวัสดี"), [link(0, 25, '#', "ไทยแลนด์")]);
        assert_eq!(links("#हिन्दी test"), [link(0, 19, '#', "हिन्दी")]);
        assert_eq!(links("#١٢٣"), [link(0, 7, '#', "١٢٣")]);
        assert_eq!(links("#Москва и #Київ"), [link(0, 13, '#', "Москва"), link(17, 26, '#', "Київ")]);
        assert_eq!(
            links("#ǅ #ʰ #½ #Ⅷ"),
            [link(0, 3, '#', "ǅ"), link(4, 7, '#', "ʰ"), link(8, 11, '#', "½"), link(12, 16, '#', "Ⅷ")]
        );
        // A circled letter is a symbol, not a letter.
        assert!(links("#Ⓐ").is_empty());
    }

    #[test]
    fn links_lead_to_instagram() {
        let found = scan_caption_links("Baked with @fixture_tidewater. #sourdough #朝ごはん");
        let urls: Vec<String> = found.iter().map(CaptionLink::url).collect();
        assert_eq!(
            urls,
            [
                "https://www.instagram.com/fixture_tidewater/",
                "https://www.instagram.com/explore/tags/sourdough/",
                "https://www.instagram.com/explore/tags/%E6%9C%9D%E3%81%94%E3%81%AF%E3%82%93/",
            ]
        );
    }

    #[test]
    fn the_scanner_is_quick_on_hostile_text() {
        // Many candidate starts, each with a long run that fails at its end.
        let hostile = "#".repeat(5_000) + &" #".repeat(2_000) + &format!(" @{}", "a".repeat(5_000));
        assert!(scan_caption_links(&hostile).is_empty());
        let long_tags = format!(" #{}", "a".repeat(150)).repeat(500);
        assert!(scan_caption_links(&long_tags).is_empty());
        let long_names = format!(" @{}", "a".repeat(40)).repeat(500);
        assert!(scan_caption_links(&long_names).is_empty());
    }

    #[test]
    fn combining_marks_and_word_characters() {
        // The table is sorted and its ranges do not touch, which the binary search relies on.
        for pair in COMBINING_MARK_RANGES.windows(2) {
            assert!(pair[0].0 <= pair[0].1 && pair[0].1 + 1 < pair[1].0, "{pair:X?}");
        }
        for mark in ['\u{0300}', '\u{036F}', '\u{0E48}', '\u{094D}', '\u{064B}', '\u{3099}', '\u{FE0F}', '\u{E0100}', '\u{E01EF}'] {
            assert!(is_combining_mark(mark), "U+{:04X}", u32::from(mark));
            assert!(!is_word_char(mark), "U+{:04X}", u32::from(mark));
        }
        for other in ['a', '_', ' ', '\u{02FF}', '\u{0370}', 'é', '朝', '🍋', '\u{E01F0}', '\u{10FFFF}'] {
            assert!(!is_combining_mark(other), "U+{:04X}", u32::from(other));
        }
        for word in ['a', 'Z', '0', '9', '_', 'é', 'ß', '朝', 'ご', 'ไ', 'ह', '١', 'Ⅷ', '½', 'ǅ', 'ʰ'] {
            assert!(is_word_char(word), "{word}");
        }
        for other in [' ', '-', '.', '#', '@', '🍋', 'Ⓐ', '🄰', '’', '\u{200D}', '＃', '€', '\n'] {
            assert!(!is_word_char(other), "{other:?}");
        }
    }

    #[test]
    fn the_character_classes_equal_the_regular_expression_classes() {
        // Counts and code point sums of /\p{M}/u and /[\p{L}\p{N}_]/u in Node (Unicode 17.0).
        let (mut marks, mut mark_sum, mut words, mut word_sum) = (0_u64, 0_u64, 0_u64, 0_u64);
        for character in '\0'..=char::MAX {
            if is_combining_mark(character) {
                marks += 1;
                mark_sum += u64::from(u32::from(character));
            }
            if is_word_char(character) {
                words += 1;
                word_sum += u64::from(u32::from(character));
            }
        }
        assert_eq!((marks, mark_sum), (2_543, 318_299_529));
        // The letters come from the standard library, whose tables follow the compiler's Unicode
        // version: the comparison only means something for the version the numbers were taken from.
        if char::UNICODE_VERSION == (17, 0, 0) {
            assert_eq!((words, word_sum), (147_597, 16_280_671_712));
        }
    }

    // --- Avatar placeholder ---------------------------------------------------------------------

    #[test]
    fn placeholder_gradients_match_the_clones_fnv_hash() {
        // Results of the clone's placeholderKey in Node.
        let vectors = [
            ("", "finance"),
            ("unknown", "finance"),
            ("alice", "forest"),
            ("fixture.sunrise.bakery", "floral"),
            ("fixture_tidewater", "art"),
            ("🍋lemon", "coffee"),
            ("natgeo", "cat"),
        ];
        for (username, key) in vectors {
            assert_eq!(PLACEHOLDER_KEYS[placeholder_index(username)], key, "{username:?}");
        }
    }

    #[test]
    fn the_avatar_letter_is_the_first_ascii_letter_or_digit() {
        assert_eq!(avatar_initial("alice"), 'A');
        assert_eq!(avatar_initial("_bob"), 'B');
        assert_eq!(avatar_initial(".x"), 'X');
        assert_eq!(avatar_initial("9lives"), '9');
        assert_eq!(avatar_initial("éa"), 'A');
        assert_eq!(avatar_initial("🍋"), '?');
        assert_eq!(avatar_initial(""), '?');
    }
}
