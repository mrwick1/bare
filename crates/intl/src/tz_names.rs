//! English time zone names for `timeZoneName` of `Intl.DateTimeFormat`. Pure functions: the formatting is in `intl.rs`, the
//! offsets in `tz.rs`.
//!
//! A small table gives the names of the common zones (ICU has them for every zone). A zone that is not in the table gets
//! the offset form (`GMT+5:30` for the short kinds, `GMT+05:30` for the long kinds), as ICU does for a zone that has no
//! abbreviation in the locale. Known deviations: the long and the generic names of a zone that is not in the table are
//! offsets (ICU writes `Nepal Time`); `shortGeneric` has names for `en` and `en-US` only.

use crate::packed::pack_table;

/// The kinds, in the order of the `timeZoneName` option values.
pub const SHORT: u8 = 0;
pub const LONG: u8 = 1;
pub const LONG_OFFSET: u8 = 3;
pub const SHORT_GENERIC: u8 = 4;
pub const LONG_GENERIC: u8 = 5;
pub const KINDS: [&str; 6] = ["short", "long", "shortOffset", "longOffset", "shortGeneric", "longGeneric"];

/// A locale bit, by index in `intl::LOCALES` (`en-US`, `en`, `en-CA`, `en-IN`, `en-GB`).
const US: u8 = 0b00011;
const CA: u8 = 0b00100;
const IN: u8 = 0b01000;
const GB: u8 = 0b10000;

struct Entry {
    names: &'static [&'static str],
    /// The standard offset in seconds: another offset is daylight time.
    std: i32,
    /// The long names: standard, daylight, generic.
    long: [&'static str; 3],
    /// The abbreviations of standard and daylight time, and the locales that use them.
    short: [&'static str; 2],
    abbr: u8,
    /// The `shortGeneric` name of `en` and `en-US`.
    generic: &'static str,
    /// The long daylight name of `en-CA` has "Daylight Saving Time".
    saving: bool,
}

const fn e(names: &'static [&'static str], std: i32, long: [&'static str; 3], short: [&'static str; 2], abbr: u8, generic: &'static str, saving: bool) -> Entry {
    Entry { names, std, long, short, abbr, generic, saving }
}

const H: i32 = 3600;
const TABLE: [Entry; 14] = [
    e(&["America/New_York", "US/Eastern"], -5 * H, ["Eastern Standard Time", "Eastern Daylight Time", "Eastern Time"], ["EST", "EDT"], US | CA, "ET", true),
    e(&["America/Chicago", "US/Central"], -6 * H, ["Central Standard Time", "Central Daylight Time", "Central Time"], ["CST", "CDT"], US | CA, "CT", true),
    e(&["America/Denver", "US/Mountain"], -7 * H, ["Mountain Standard Time", "Mountain Daylight Time", "Mountain Time"], ["MST", "MDT"], US | CA, "MT", false),
    e(&["America/Los_Angeles", "US/Pacific"], -8 * H, ["Pacific Standard Time", "Pacific Daylight Time", "Pacific Time"], ["PST", "PDT"], US | CA, "PT", false),
    e(&["America/Phoenix", "US/Arizona"], -7 * H, ["Mountain Standard Time"; 3], ["MST", "MST"], US | CA, "MST", false),
    e(&["America/Anchorage", "US/Alaska"], -9 * H, ["Alaska Standard Time", "Alaska Daylight Time", "Alaska Time"], ["AKST", "AKDT"], US | CA, "AKT", true),
    e(&["Pacific/Honolulu", "US/Hawaii"], -10 * H, ["Hawaii-Aleutian Standard Time"; 3], ["HST", "HST"], US | CA, "HST", false),
    e(&["Europe/London"], 0, ["Greenwich Mean Time", "British Summer Time", "United Kingdom Time"], ["GMT", "BST"], GB, "United Kingdom Time", false),
    e(&["Asia/Kolkata", "Asia/Calcutta"], 19800, ["India Standard Time"; 3], ["IST", "IST"], IN | CA, "India Time", false),
    e(&["Australia/Sydney"], 10 * H, ["Australian Eastern Standard Time", "Australian Eastern Daylight Time", "Australian Eastern Time"], ["AEST", "AEDT"], CA, "Sydney Time", false),
    e(&["Europe/Paris"], H, ["Central European Standard Time", "Central European Summer Time", "Central European Time"], ["CET", "CEST"], GB, "France Time", false),
    e(&["Europe/Berlin"], H, ["Central European Standard Time", "Central European Summer Time", "Central European Time"], ["CET", "CEST"], GB, "Germany Time", false),
    e(&["Asia/Tokyo"], 9 * H, ["Japan Standard Time"; 3], ["", ""], 0, "Japan Time", false),
    e(&["Asia/Seoul"], 9 * H, ["Korean Standard Time"; 3], ["", ""], 0, "South Korea Time", false),
];

/// `GMT`, `GMT+5`, `GMT+5:30` (short) or `GMT+05:30` (long) for an offset in seconds.
fn offset_name(off: i32, long: bool) -> String {
    let m = off.abs() / 60;
    let sign = if off < 0 { '-' } else { '+' };
    match (m, long) {
        (0, _) => "GMT".into(),
        (_, true) => format!("GMT{sign}{:02}:{:02}", m / 60, m % 60),
        (_, false) if m % 60 == 0 => format!("GMT{sign}{}", m / 60),
        _ => format!("GMT{sign}{}:{:02}", m / 60, m % 60),
    }
}

/// The name of kind `kind` for zone `zone` (as given) at offset `off` seconds, in locale `loc` (an index in `intl::LOCALES`).
pub fn name(zone: &str, off: i32, kind: u8, loc: u8) -> String {
    let long_kind = matches!(kind, LONG | LONG_OFFSET | LONG_GENERIC);
    if is_utc_name(zone) {
        return match kind {
            SHORT => "UTC".into(),
            LONG => "Coordinated Universal Time".into(),
            _ => "GMT".into(),
        };
    }
    let bit = 1u8 << loc;
    let zones = pack_table!(TABLE, 8, (i32, u8, bool), |i| {
        let t = &TABLE[i];
        assert!(!t.names.is_empty() && t.names.len() <= 2);
        let alias = if t.names.len() == 2 { t.names[1] } else { "" };
        let [std_name, dst_name, long_generic] = t.long;
        let [std_abbr, dst_abbr] = t.short;
        ([t.names[0], alias, std_name, dst_name, long_generic, std_abbr, dst_abbr, t.generic], (t.std, t.abbr, t.saving))
    });
    if let Some(i) = (0..zones.len()).find(|&i| zones.text(i, 0) == zone || (!zone.is_empty() && zones.text(i, 1) == zone)) {
        let (std, abbr, saving) = zones.meta(i);
        let dst = off != std;
        match kind {
            SHORT if abbr & bit != 0 => return zones.text(i, 5 + dst as usize).into(),
            LONG => {
                let n = zones.text(i, 2 + dst as usize);
                return if dst && saving && loc == 2 { n.replace("Daylight Time", "Daylight Saving Time") } else { n.into() };
            }
            SHORT_GENERIC if loc < 2 => return zones.text(i, 7).into(),
            LONG_GENERIC => return zones.text(i, 4).into(),
            _ => {}
        }
    }
    offset_name(off, long_kind)
}

pub fn is_utc_name(s: &str) -> bool {
    const UTC: [&str; 18] = [
        "UTC", "Etc/UTC", "Etc/GMT", "GMT", "Etc/UCT", "UCT", "Etc/Universal", "Universal", "Etc/Zulu", "Zulu", "Etc/GMT0", "GMT0", "Etc/GMT+0",
        "Etc/GMT-0", "GMT+0", "GMT-0", "Etc/Greenwich", "Greenwich",
    ];
    UTC.iter().any(|u| u.eq_ignore_ascii_case(s))
}
