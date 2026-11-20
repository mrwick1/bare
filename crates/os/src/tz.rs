//! The time zone of the machine. Zone data is not here: it is in `bare-tzdata`, the same on every OS.
//!
//! Unix (Linux and macOS): the part of the target of the `/etc/localtime` link after `zoneinfo/`. A regular file is not read.
//! Windows: `GetDynamicTimeZoneInformation` gives the key of the zone (`Pacific Standard Time`), and the CLDR row for territory
//! 001 gives the IANA name. A key with no row gets a fixed offset from the bias of the same call (`+05:30`).
//! The caller reads `TZ` first. This function does not.

/// The zone that the OS is set to: an IANA name, or a fixed offset (`+05:30`), or `None` when the OS gives none.
pub fn local_zone() -> Option<String> {
    #[cfg(unix)]
    {
        let target = std::fs::read_link("/etc/localtime").ok()?;
        target.to_str()?.split_once("zoneinfo/").map(|x| x.1.to_string())
    }
    #[cfg(windows)]
    {
        windows()
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

/// The name for a Windows zone key: the CLDR row, else the fixed offset of `east_minutes` (`UTC` for 0).
#[cfg(any(windows, test))]
fn from_windows_key(key: &str, east_minutes: i32) -> String {
    if let Some(name) = bare_tzdata::windows_zone(key) {
        return name.to_string();
    }
    if east_minutes == 0 {
        return "UTC".to_string();
    }
    let a = east_minutes.abs();
    format!("{}{:02}:{:02}", if east_minutes < 0 { '-' } else { '+' }, a / 60, a % 60)
}

#[cfg(windows)]
fn windows() -> Option<String> {
    #[repr(C)]
    struct SystemTime([u16; 8]);
    #[repr(C)]
    struct DynamicTimeZoneInformation {
        bias: i32,
        standard_name: [u16; 32],
        standard_date: SystemTime,
        standard_bias: i32,
        daylight_name: [u16; 32],
        daylight_date: SystemTime,
        daylight_bias: i32,
        time_zone_key_name: [u16; 128],
        dynamic_daylight_time_disabled: u8,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetDynamicTimeZoneInformation(info: *mut DynamicTimeZoneInformation) -> u32;
    }
    const TIME_ZONE_ID_INVALID: u32 = 0xFFFF_FFFF;
    const TIME_ZONE_ID_DAYLIGHT: u32 = 2;

    // SAFETY: the structure is plain integers, so all-zero is valid. The call writes only inside it.
    let mut info: DynamicTimeZoneInformation = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is a valid, exclusive `DYNAMIC_TIME_ZONE_INFORMATION`.
    let id = unsafe { GetDynamicTimeZoneInformation(&mut info) };
    if id == TIME_ZONE_ID_INVALID {
        return None;
    }
    let key = &info.time_zone_key_name;
    let key = String::from_utf16_lossy(&key[..key.iter().position(|&c| c == 0).unwrap_or(key.len())]);
    let active = if id == TIME_ZONE_ID_DAYLIGHT { info.daylight_bias } else { info.standard_bias };
    Some(from_windows_key(&key, -(info.bias + active)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_with_a_row_gives_its_name_and_one_without_gives_the_offset() {
        assert_eq!(from_windows_key("Pacific Standard Time", 0), "America/Los_Angeles");
        assert_eq!(from_windows_key("Mid-Atlantic Standard Time", -60), "-01:00");
        assert_eq!(from_windows_key("Kamchatka Standard Time", 12 * 60), "+12:00");
        assert_eq!(from_windows_key("Nowhere Standard Time", 5 * 60 + 30), "+05:30");
        assert_eq!(from_windows_key("Nowhere Standard Time", -(9 * 60 + 30)), "-09:30");
        assert_eq!(from_windows_key("", 0), "UTC");
    }

    /// On Windows the machine always gives a zone. Elsewhere there is no rule for the answer (a container has no link).
    #[cfg(windows)]
    #[test]
    fn the_local_zone_of_windows_has_data_or_is_an_offset() {
        let z = local_zone().expect("a Windows zone");
        assert!(bare_tzdata::find(&z).is_some() || z == "UTC" || z.starts_with(['+', '-']), "{z}");
    }
}
