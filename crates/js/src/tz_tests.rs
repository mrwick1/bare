//! Zone tests. Rules and files made here, then the system files of three zones with known offsets.

use super::*;

const H: i32 = 3600;

/// Seconds since the epoch of a UTC date and time.
fn utc(y: i64, mo: i64, d: i64, h: i64, mi: i64, s: i64) -> i64 {
    days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + s
}

/// A TZif file: version, transitions (instant, type), offsets, footer.
fn tzif(ver: u8, trans: &[(i64, u8)], offs: &[i32], footer: &str) -> Vec<u8> {
    let block = |wide: bool| {
        let mut b = Vec::new();
        b.extend_from_slice(b"TZif");
        b.push(ver);
        b.extend_from_slice(&[0; 15]);
        for n in [0, 0, 0, trans.len(), offs.len(), 4] {
            b.extend_from_slice(&(n as u32).to_be_bytes());
        }
        for &(t, _) in trans {
            if wide { b.extend_from_slice(&t.to_be_bytes()) } else { b.extend_from_slice(&(t as i32).to_be_bytes()) }
        }
        b.extend(trans.iter().map(|x| x.1));
        for &o in offs {
            b.extend_from_slice(&o.to_be_bytes());
            b.extend_from_slice(&[0, 0]);
        }
        b.extend_from_slice(b"ABC\0");
        b
    };
    let mut f = block(false);
    if ver != 0 {
        f.extend(block(true));
        f.extend_from_slice(format!("\n{footer}\n").as_bytes());
    }
    f
}

#[test]
fn rules_follow_posix() {
    let ny = Rule::parse("EST5EDT,M3.2.0,M11.1.0").unwrap();
    // 2024: daylight time from Sunday March 10 02:00 EST (07:00Z) to Sunday November 3 02:00 EDT (06:00Z)
    assert_eq!(ny.offset(utc(2024, 3, 10, 6, 59, 59)), -5 * H);
    assert_eq!(ny.offset(utc(2024, 3, 10, 7, 0, 0)), -4 * H);
    assert_eq!(ny.offset(utc(2024, 11, 3, 5, 59, 59)), -4 * H);
    assert_eq!(ny.offset(utc(2024, 11, 3, 6, 0, 0)), -5 * H);
    // southern: daylight time across the new year, the end at 03:00 local daylight time
    let syd = Rule::parse("AEST-10AEDT,M10.1.0,M4.1.0/3").unwrap();
    assert_eq!(syd.offset(utc(2050, 1, 15, 0, 0, 0)), 11 * H);
    assert_eq!(syd.offset(utc(2050, 7, 15, 0, 0, 0)), 10 * H);
    assert_eq!(syd.offset(utc(2050, 4, 2, 15, 59, 59)), 11 * H); // Sunday April 3 2050 03:00 AEDT is 16:00Z the day before
    assert_eq!(syd.offset(utc(2050, 4, 2, 16, 0, 0)), 10 * H);
    // quoted names, minutes, no daylight time, the week 5 = last, Julian and zero-based days, a time past 24 hours
    assert_eq!(Rule::parse("<+0530>-5:30").unwrap(), Rule { std: 5 * H + 1800, dst: None });
    assert_eq!(Rule::parse("IST-5:30").unwrap().offset(0), 5 * H + 1800);
    let eu = Rule::parse("CET-1CEST,M3.5.0,M10.5.0/3").unwrap();
    assert_eq!(eu.offset(utc(2024, 3, 31, 0, 59, 59)), H); // last Sunday of March 2024 is the 31st, 01:00Z
    assert_eq!(eu.offset(utc(2024, 3, 31, 1, 0, 0)), 2 * H);
    let j = Rule::parse("AAA3BBB,J60/2,300").unwrap(); // J60 is March 1, also in a leap year
    assert_eq!(j.offset(utc(2024, 3, 1, 4, 59, 59)), -3 * H);
    assert_eq!(j.offset(utc(2024, 3, 1, 5, 0, 0)), -2 * H);
    let late = Rule::parse("<-03>3<-02>,M3.5.0/-2,M10.5.0/-1").unwrap(); // negative times (TZif version 3)
    assert_eq!(late.offset(utc(2024, 7, 1, 0, 0, 0)), -2 * H);
    for bad in ["", "AB5", "EST", "EST5EDT", "EST5EDT,M3.2.0", "EST5EDT,M13.1.0,M11.1.0", "EST5EDT,M3.6.0,M11.1.0", "EST25", "EST5EDT,J0,J1", "EST5x", "<A>5"] {
        assert_eq!(Rule::parse(bad), None, "{bad}");
    }
}

#[test]
fn files_give_transitions_then_the_footer() {
    let at = utc(2000, 1, 1, 0, 0, 0);
    for ver in [0, b'2', b'3', b'4'] {
        let z = Zone::parse(&tzif(ver, &[(at, 1)], &[H, 2 * H], "XXX-3")).unwrap();
        assert_eq!(z.offset(at - 1), H, "type 0 before the first transition");
        assert_eq!(z.offset(at), 2 * H);
        // after the last transition: version 1 keeps the last type, later versions use the footer
        assert_eq!(z.offset(at + 1000), if ver == 0 { 2 * H } else { 3 * H });
    }
    let slim = Zone::parse(&tzif(b'2', &[], &[H], "XXX-3")).unwrap();
    assert_eq!(slim.offset(at), 3 * H, "no transitions: the footer for all time");
    let empty_footer = Zone::parse(&tzif(b'2', &[(at, 1)], &[H, 2 * H], "")).unwrap();
    assert_eq!(empty_footer.offset(at + 1000), 2 * H);
    // refused: a bad magic, a cut file, a type out of range, unsorted transitions, leap seconds
    let good = tzif(b'2', &[(at, 1)], &[H, 2 * H], "XXX-3");
    let mut magic = good.clone();
    magic[0] = b'X';
    assert_eq!(Zone::parse(&magic), None);
    assert_eq!(Zone::parse(&good[..good.len() - 3]), None);
    assert_eq!(Zone::parse(&tzif(b'2', &[(at, 2)], &[H, 2 * H], "XXX-3")), None);
    assert_eq!(Zone::parse(&tzif(b'2', &[(at, 0), (at - 1, 1)], &[H, 2 * H], "XXX-3")), None);
    let mut leap = good.clone();
    leap[28..32].copy_from_slice(&1u32.to_be_bytes());
    assert_eq!(Zone::parse(&leap), None);
}

#[test]
fn names() {
    assert_eq!(parse_offset_name("+05:30"), Some(5 * H + 1800));
    assert_eq!(parse_offset_name("-0530"), Some(-(5 * H + 1800)));
    assert_eq!(parse_offset_name("+05"), Some(5 * H));
    for bad in ["05:30", "+5:30", "+05:30:00", "+24", "+05:60", "+05-30", "+"] {
        assert_eq!(parse_offset_name(bad), None, "{bad}");
    }
    assert!(is_utc_name("utc") && is_utc_name("Etc/GMT") && is_utc_name("Zulu"));
    assert!(!is_utc_name("Etc/GMT+5"));
    assert!(is_file_name("America/Argentina/Buenos_Aires") && is_file_name("Etc/GMT+5"));
    for bad in ["", "../etc/passwd", "Asia/../UTC", "/etc/passwd", "Asia//Kolkata", "posix/Asia/Kolkata", "right/UTC", "a.b", "posixrules", "Asia/Kolkata\0"] {
        assert!(!is_file_name(bad), "{bad}");
    }
}

#[test]
fn system_files() {
    let dir = std::env::var("TZDIR").unwrap_or_else(|_| "/usr/share/zoneinfo".into());
    let load = |n: &str| std::fs::read(format!("{dir}/{n}")).ok().map(|b| Zone::parse(&b).expect(n));
    let Some(ny) = load("America/New_York") else {
        eprintln!("no system tzdata in {dir}: skipped");
        return;
    };
    assert_eq!(ny.offset(utc(2024, 1, 15, 12, 0, 0)), -5 * H);
    assert_eq!(ny.offset(utc(2024, 7, 1, 12, 0, 0)), -4 * H);
    assert_eq!(ny.offset(utc(2024, 3, 10, 7, 0, 0)), -4 * H);
    assert_eq!(ny.offset(utc(2060, 7, 1, 12, 0, 0)), -4 * H, "the footer after 2037");
    assert_eq!(ny.offset(utc(1850, 1, 1, 0, 0, 0)), -(4 * H + 56 * 60 + 2), "local mean time before 1883");
    let kol = load("Asia/Kolkata").unwrap();
    assert_eq!(kol.offset(utc(2024, 1, 5, 0, 0, 0)), 5 * H + 1800);
    assert_eq!(kol.offset(utc(1943, 1, 1, 0, 0, 0)), 6 * H + 1800, "war time");
    let syd = load("Australia/Sydney").unwrap();
    assert_eq!(syd.offset(utc(2024, 1, 15, 0, 0, 0)), 11 * H);
    assert_eq!(syd.offset(utc(2024, 7, 15, 0, 0, 0)), 10 * H);
}
