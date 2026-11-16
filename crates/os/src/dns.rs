//! The DNS servers and the hosts file of the OS. Issue #592 moves the code here from `bare-net`.

use std::net::IpAddr;

/// The DNS server addresses of this machine, in the order the OS lists them. Empty when unknown.
pub fn servers() -> Vec<IpAddr> {
    imp::servers()
}

/// The text of the hosts file. Empty when the file is missing.
pub fn hosts_text() -> String {
    imp::hosts_text()
}

/// The DNS server addresses in the text of `/etc/resolv.conf`: lines like "nameserver 1.1.1.1".
#[cfg(any(unix, test))]
fn nameservers(text: &str) -> Vec<IpAddr> {
    text.lines()
        .filter_map(|line| line.strip_prefix("nameserver"))
        .filter_map(|rest| rest.split_whitespace().next()?.parse().ok())
        .collect()
}

#[cfg(unix)]
mod imp {
    use super::*;

    pub fn servers() -> Vec<IpAddr> {
        nameservers(&std::fs::read_to_string("/etc/resolv.conf").unwrap_or_default())
    }

    pub fn hosts_text() -> String {
        std::fs::read_to_string("/etc/hosts").unwrap_or_default()
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    const AF_INET: u16 = 2;
    const AF_INET6: u16 = 23;
    /// Skip unicast, anycast, multicast and friendly names. Keep the DNS servers.
    const FLAGS: u32 = 0x1 | 0x2 | 0x4 | 0x20;
    const ERROR_BUFFER_OVERFLOW: u32 = 111;
    const NO_ERROR: u32 = 0;
    const OPER_STATUS_UP: i32 = 1;

    /// `IP_ADAPTER_ADDRESSES_LH` up to `OperStatus`. The OS owns the memory; we only read.
    #[repr(C)]
    struct Adapter {
        length: u32,
        if_index: u32,
        next: *const Adapter,
        adapter_name: *const u8,
        first_unicast: *const u8,
        first_anycast: *const u8,
        first_multicast: *const u8,
        first_dns_server: *const DnsServer,
        dns_suffix: *const u16,
        description: *const u16,
        friendly_name: *const u16,
        physical_address: [u8; 8],
        physical_address_length: u32,
        flags: u32,
        mtu: u32,
        if_type: u32,
        oper_status: i32,
    }

    /// `IP_ADAPTER_DNS_SERVER_ADDRESS_XP`.
    #[repr(C)]
    struct DnsServer {
        length: u32,
        reserved: u32,
        next: *const DnsServer,
        sockaddr: *const u8,
        sockaddr_length: i32,
    }

    #[link(name = "iphlpapi")]
    unsafe extern "system" {
        fn GetAdaptersAddresses(family: u32, flags: u32, reserved: *const u8, addrs: *mut Adapter, size: *mut u32) -> u32;
    }

    /// Read an address from a `SOCKADDR_IN` or `SOCKADDR_IN6`.
    ///
    /// # Safety
    /// `p` points to a valid socket address of `len` bytes.
    unsafe fn sockaddr(p: *const u8, len: i32) -> Option<IpAddr> {
        if p.is_null() || len < 2 {
            return None;
        }
        let family = unsafe { (p as *const u16).read_unaligned() };
        if family == AF_INET && len >= 8 {
            let b = unsafe { std::slice::from_raw_parts(p.add(4), 4) };
            Some(IpAddr::V4(Ipv4Addr::new(b[0], b[1], b[2], b[3])))
        } else if family == AF_INET6 && len >= 24 {
            let mut b = [0u8; 16];
            b.copy_from_slice(unsafe { std::slice::from_raw_parts(p.add(8), 16) });
            Some(IpAddr::V6(Ipv6Addr::from(b)))
        } else {
            None
        }
    }

    /// The old site-local placeholders `fec0:0:0:ffff::1`, `::2`, `::3`. Windows lists them when no IPv6 DNS is set.
    fn placeholder(ip: &IpAddr) -> bool {
        matches!(ip, IpAddr::V6(v6) if {
            let s = v6.segments();
            s[0] == 0xfec0 && s[1] == 0 && s[2] == 0 && s[3] == 0xffff && s[4..7] == [0, 0, 0] && (1..=3).contains(&s[7])
        })
    }

    pub fn servers() -> Vec<IpAddr> {
        let mut size: u32 = 16 * 1024;
        // u64 words: the buffer is 8-byte aligned, like `Adapter` needs.
        let mut buf: Vec<u64> = Vec::new();
        for _ in 0..4 {
            buf.resize((size as usize).div_ceil(8), 0);
            let r = unsafe { GetAdaptersAddresses(0, FLAGS, std::ptr::null(), buf.as_mut_ptr() as *mut Adapter, &mut size) };
            if r == NO_ERROR {
                return collect(buf.as_ptr() as *const Adapter);
            }
            if r != ERROR_BUFFER_OVERFLOW {
                return Vec::new();
            }
            // `size` now holds the needed size. The adapter list can grow between calls: try again.
        }
        Vec::new()
    }

    fn collect(first: *const Adapter) -> Vec<IpAddr> {
        let mut out: Vec<IpAddr> = Vec::new();
        let mut a = first;
        while !a.is_null() {
            let adapter = unsafe { &*a };
            if adapter.oper_status == OPER_STATUS_UP {
                let mut d = adapter.first_dns_server;
                while !d.is_null() {
                    let s = unsafe { &*d };
                    if let Some(ip) = unsafe { sockaddr(s.sockaddr, s.sockaddr_length) }
                        && !placeholder(&ip)
                        && !out.contains(&ip)
                    {
                        out.push(ip);
                    }
                    d = s.next;
                }
            }
            a = adapter.next;
        }
        out
    }

    pub fn hosts_text() -> String {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        std::fs::read_to_string(format!(r"{root}\System32\drivers\etc\hosts")).unwrap_or_default()
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use super::*;

    pub fn servers() -> Vec<IpAddr> {
        Vec::new()
    }

    pub fn hosts_text() -> String {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_resolv_conf() {
        let conf = "# comment\nnameserver 127.0.0.53\nnameserver 2606:4700::1111\noptions edns0\n";
        assert_eq!(nameservers(conf), vec!["127.0.0.53".parse::<IpAddr>().unwrap(), "2606:4700::1111".parse().unwrap()]);
    }

    #[test]
    fn hosts_text_does_not_panic() {
        let _ = hosts_text();
    }

    #[cfg(windows)]
    #[test]
    fn windows_has_dns_servers() {
        let s = servers();
        println!("servers: {s:?}");
        assert!(!s.is_empty());
    }
}
