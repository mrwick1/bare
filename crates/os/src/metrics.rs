//! Current-process resource measurements. No sampler or collection is started.
//!
//! Linux reads `/proc/self/status` and `/proc/self/stat`. These reads and the
//! allocator query are separate operations, so a snapshot is not atomic.
//! Procfs RSS and peak RSS are approximate kernel counters.

/// A best-effort measurement of the current process, across all its threads.
/// Missing, malformed, overflowing or unsupported measurements are `None`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// Current resident memory in bytes (`VmRSS` on Linux).
    pub rss_bytes: Option<u64>,
    /// Peak resident memory in bytes (`VmHWM` on Linux).
    pub peak_rss_bytes: Option<u64>,
    /// Cumulative user CPU microseconds, including guest time, excluding children.
    pub cpu_user_us: Option<u64>,
    /// Cumulative system CPU microseconds, excluding children.
    pub cpu_system_us: Option<u64>,
    /// Current number of threads in this process.
    pub thread_count: Option<u64>,
    /// glibc `mallinfo2.uordblks + hblkhd`, in bytes (requires glibc >= 2.33).
    /// Includes allocator overhead and mmap blocks. Cached freed blocks can
    /// remain counted. Excludes other allocators and direct application mmap.
    /// This is allocator accounting, not RSS or engine/JS live-object bytes.
    pub allocator_live_bytes: Option<u64>,
}

/// Read available process counters. Non-Linux platforms return all `None`.
/// CPU time has clock-tick resolution, truncated to whole microseconds.
#[cfg(target_os = "linux")]
pub fn snapshot() -> Snapshot {
    let mut result = std::fs::read_to_string("/proc/self/status")
        .map(|s| parse_status(&s))
        .unwrap_or_default();
    if let Ok(stat) = std::fs::read_to_string("/proc/self/stat") {
        unsafe extern "C" {
            fn sysconf(name: std::ffi::c_int) -> std::ffi::c_long;
        }
        // Linux glibc bits/confname.h and musl unistd.h define this selector as 2.
        // This is an API selector, not a clock frequency.
        const SC_CLK_TCK: std::ffi::c_int = 2;
        // SAFETY: sysconf takes an integer selector and returns -1 on failure.
        let hz = u64::try_from(unsafe { sysconf(SC_CLK_TCK) }).ok().filter(|&v| v > 0);
        (result.cpu_user_us, result.cpu_system_us) = parse_cpu(&stat, hz);
    }
    result.allocator_live_bytes = allocator_live_bytes();
    result
}

#[cfg(not(target_os = "linux"))]
pub fn snapshot() -> Snapshot {
    Snapshot::default()
}

#[cfg(target_os = "linux")]
fn parse_status(input: &str) -> Snapshot {
    let mut result = Snapshot::default();
    for line in input.lines() {
        let Some((key, value)) = line.split_once(':') else { continue };
        let mut words = value.split_ascii_whitespace();
        let number = words.next().and_then(|v| v.parse::<u64>().ok());
        match key {
            "VmRSS" | "VmHWM" => {
                let bytes = if words.next() == Some("kB") && words.next().is_none() {
                    number.and_then(|v| v.checked_mul(1024))
                } else {
                    None
                };
                if key == "VmRSS" {
                    result.rss_bytes = bytes
                } else {
                    result.peak_rss_bytes = bytes
                }
            }
            "Threads" => {
                result.thread_count = number.filter(|&v| v > 0 && words.next().is_none());
            }
            _ => {}
        }
    }
    result
}

#[cfg(target_os = "linux")]
fn parse_cpu(input: &str, hz: Option<u64>) -> (Option<u64>, Option<u64>) {
    let Some((pid, rest)) = input.split_once(" (") else {
        return (None, None);
    };
    if !pid.parse::<u64>().is_ok_and(|v| v > 0) {
        return (None, None);
    }
    // comm can contain spaces, newlines and parentheses. Only its final ')'
    // delimits the name; later stat fields contain no parentheses.
    let Some((_, fields)) = rest.rsplit_once(") ") else {
        return (None, None);
    };
    let mut fields = fields.split_ascii_whitespace();
    if !matches!(
        fields.next(),
        Some("R" | "S" | "D" | "Z" | "T" | "t" | "W" | "X" | "x" | "K" | "P" | "I")
    ) {
        return (None, None);
    }
    // Skip fields 4..13 to reach utime (14) and stime (15).
    let user = fields.nth(10).and_then(|v| v.parse().ok()).and_then(|v| ticks_to_us(v, hz));
    let system = fields.next().and_then(|v| v.parse().ok()).and_then(|v| ticks_to_us(v, hz));
    (user, system)
}

#[cfg(target_os = "linux")]
fn ticks_to_us(ticks: u64, hz: Option<u64>) -> Option<u64> {
    let hz = hz.filter(|&v| v > 0)?;
    u64::try_from(u128::from(ticks) * 1_000_000 / u128::from(hz)).ok()
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn allocator_live_bytes() -> Option<u64> {
    #[repr(C)]
    struct MallInfo2 {
        arena: usize,
        ordblks: usize,
        smblks: usize,
        hblks: usize,
        hblkhd: usize,
        usmblks: usize,
        fsmblks: usize,
        uordblks: usize,
        fordblks: usize,
        keepcost: usize,
    }
    unsafe extern "C" {
        fn mallinfo2() -> MallInfo2;
    }
    // SAFETY: the C layout matches glibc's malloc.h. No pointers are passed.
    let info = unsafe { mallinfo2() };
    u64::try_from(info.uordblks.checked_add(info.hblkhd)?).ok()
}

#[cfg(all(target_os = "linux", not(target_env = "gnu")))]
fn allocator_live_bytes() -> Option<u64> {
    None
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn status_units_and_missing_fields() {
        let s = parse_status("Name:\ttest\nVmRSS:\t12 kB\nVmHWM: 20 kB\nThreads: 3\n");
        assert_eq!(s.rss_bytes, Some(12 * 1024));
        assert_eq!(s.peak_rss_bytes, Some(20 * 1024));
        assert_eq!(s.thread_count, Some(3));
        assert_eq!(parse_status("").rss_bytes, None);
        assert_eq!(parse_status("Name: test\n").thread_count, None);
        assert_eq!(parse_status("VmRSS: 0 kB\n").rss_bytes, Some(0));
    }

    #[test]
    fn status_rejects_malformed_truncated_and_overflowing_values() {
        for value in [
            "",
            "12",
            "-1 kB",
            "1 MB",
            "x kB",
            "1 kB extra",
            "18446744073709551615 kB",
            "18446744073709551616 kB",
        ] {
            assert_eq!(parse_status(&format!("VmRSS: {value}\n")).rss_bytes, None, "{value}");
        }
        for value in ["", "x", "-1", "0", "2 extra", "18446744073709551616"] {
            assert_eq!(parse_status(&format!("Threads: {value}\n")).thread_count, None, "{value}");
        }
        assert_eq!(parse_status("VmRSS: 12 kB\nVmHWM: bad\nThreads: 2\n").peak_rss_bytes, None);
    }

    fn stat(name: &str, user: &str, system: &str) -> String {
        // Fields 3..13 precede utime (14) and stime (15).
        format!("123 ({name}) R 1 2 3 4 5 6 7 8 9 10 {user} {system} 0 0 0\n")
    }

    #[test]
    fn cpu_fields_allow_spaces_and_parentheses_in_names() {
        for name in ["normal", "with spaces", "a ) b ( c)", "a\n)b"] {
            assert_eq!(parse_cpu(&stat(name, "125", "42"), Some(100)), (Some(1_250_000), Some(420_000)));
        }
    }

    #[test]
    fn cpu_rejects_bad_headers_missing_fields_and_invalid_values() {
        for input in [
            "",
            "123",
            "123 name R",
            "123 (name",
            "123 (name) R",
            "bad (name) R 1 2",
            "123 name) R 1 2",
        ] {
            assert_eq!(parse_cpu(input, Some(100)), (None, None), "{input}");
        }
        assert_eq!(parse_cpu(&stat("test", "bad", "2"), Some(100)), (None, Some(20_000)));
        assert_eq!(parse_cpu(&stat("test", "1", "-2"), Some(100)), (Some(10_000), None));
        assert_eq!(parse_cpu(&stat("test", "18446744073709551616", "0"), Some(100)), (None, Some(0)));
        assert_eq!(parse_cpu("123 (test) R 1 2 3 4 5 6 7 8 9 10 1", Some(100)), (Some(10_000), None));
    }

    #[test]
    fn cpu_conversion_checks_units_frequency_and_overflow() {
        assert_eq!(ticks_to_us(1, Some(3)), Some(333_333));
        assert_eq!(ticks_to_us(u64::MAX, Some(1_000_000)), Some(u64::MAX));
        assert_eq!(ticks_to_us(u64::MAX, Some(1)), None);
        assert_eq!(ticks_to_us(0, Some(100)), Some(0));
        assert_eq!(ticks_to_us(1, None), None);
        assert_eq!(ticks_to_us(1, Some(0)), None);
    }

    #[test]
    fn current_process_smoke() {
        let s = snapshot();
        assert!(s.rss_bytes.is_some_and(|v| v > 0));
        assert!(s.peak_rss_bytes.is_some_and(|v| v > 0));
        assert!(s.thread_count.is_some_and(|v| v > 0));
        assert!(s.cpu_user_us.is_some());
        assert!(s.cpu_system_us.is_some());
        #[cfg(target_env = "gnu")]
        assert!(s.allocator_live_bytes.is_some());
        #[cfg(not(target_env = "gnu"))]
        assert_eq!(s.allocator_live_bytes, None);
    }
}

#[cfg(all(test, not(target_os = "linux")))]
mod tests {
    #[test]
    fn unsupported_platform_returns_unavailable() {
        assert_eq!(super::snapshot(), super::Snapshot::default());
    }
}
