//! Module 1c: a DNS client. It turns a name like "example.com" into IP addresses.
//!
//! We do not ask the OS. We do every step by hand:
//!
//! 1. "localhost" is always this machine. It never goes to the network.
//! 2. Look in our cache of earlier answers. Each answer says how long we may keep it. The cache comes
//!    first: it is a memory lookup (9 µs), and reading `/etc/hosts` is a file read (47 µs).
//! 3. Look in `/etc/hosts`, a local list of names and addresses. A name that is already in the cache
//!    keeps its cached address until the TTL ends, even if someone adds it to this file.
//! 4. Ask the DNS server listed in `/etc/resolv.conf`. The question is a small message
//!    sent over UDP (quick, no connection). We ask twice at the same time: once for an IPv4 address
//!    (an "A" record) and once for an IPv6 address (an "AAAA" record).
//! 5. If the answer is too big for one UDP message, the server says "truncated".
//!    Then we ask again over TCP, which has no size limit.

use std::collections::HashMap;
use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream, UdpSocket};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

const TYPE_A: u16 = 1; // an IPv4 address
const TYPE_CNAME: u16 = 5; // an alias: "this name is really that name"
const TYPE_AAAA: u16 = 28; // an IPv6 address
const CLASS_IN: u16 = 1; // "Internet". DNS was designed for other networks too.
const TIMEOUT: Duration = Duration::from_secs(2);
const MAX_ALIASES: usize = 8; // stop alias chains that never end

/// The answer to one question.
#[derive(Debug, PartialEq)]
struct Answer {
    addrs: Vec<IpAddr>,
    alias: Option<String>,
    // The last alias (CNAME) in the answer. We need it when the server gives
    // the alias but not the address.
    ttl: u32,
    // How many seconds we may keep this answer: the smallest TTL of the records we used.
}

static CACHE: LazyLock<Mutex<HashMap<String, (Vec<IpAddr>, Instant)>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
// One cache for the whole program: name -> (addresses, time it runs out).
// `static` is a global. JS: a variable at the top of a file.
// `HashMap` is like a JS object used as a lookup table.
// `LazyLock` creates the map the first time it is used.
// `Mutex` is a lock: only one piece of code can use the map at a time. Rust requires it for
// a global that can change, so two parts of the program never change it at once.

/// Find the IP addresses for `host`. IPv4 addresses come first.
pub fn resolve(host: &str) -> Result<Vec<IpAddr>, String> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![ip]);
        // Already an address, like "127.0.0.1" or "::1". Nothing to look up.
        // `vec![..]` makes a `Vec` from a list. JS: `[ip]`.
    }
    let name = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
    // "example.com." with a final dot is the same name. The dot means "from the root".

    if name == "localhost" || name.ends_with(".localhost") {
        return Ok(vec![IpAddr::V4(Ipv4Addr::LOCALHOST), IpAddr::V6(Ipv6Addr::LOCALHOST)]);
        // These names always mean this machine. Browsers never ask a DNS server about them,
        // so no server can point "localhost" somewhere else.
    }

    if let Some((addrs, expiry)) = CACHE.lock().unwrap().get(&name) {
        // `.lock()` waits until no one else uses the map, then gives it to us.
        // Rust opens the lock again by itself after this `if let`.
        if Instant::now() < *expiry {
            return Ok(addrs.clone());
        }
    }

    let hosts = std::fs::read_to_string("/etc/hosts").unwrap_or_default();
    // A missing file is fine. `unwrap_or_default()` gives "" then.
    let addrs = hosts_lookup(&hosts, &name);
    if !addrs.is_empty() {
        return Ok(addrs);
    }

    let conf = std::fs::read_to_string("/etc/resolv.conf").unwrap_or_default();
    let mut servers = nameservers(&conf);
    if servers.is_empty() {
        servers.push(IpAddr::V4(Ipv4Addr::LOCALHOST));
        // No "nameserver" line: ask this machine, like other Linux programs do.
    }

    let (addrs, ttl) = lookup(&servers, &name)?;
    CACHE.lock().unwrap().insert(name, (addrs.clone(), Instant::now() + Duration::from_secs(ttl.into())));
    Ok(addrs)
}

/// Ask the servers for the A and AAAA records of `name`. Follow aliases when needed.
fn lookup(servers: &[IpAddr], name: &str) -> Result<(Vec<IpAddr>, u32), String> {
    let mut name = name.to_string();
    for _ in 0..MAX_ALIASES {
        let mut addrs = Vec::new();
        let mut ttl = u32::MAX;
        let mut alias = None;
        // Ask both questions at the same time: the wait is one round trip, not two.
        // JS: like `Promise.all([ask(A), ask(AAAA)])`. `scope` runs a thread, and waits for it before it returns.
        let (a, aaaa) = std::thread::scope(|scope| {
            let aaaa = scope.spawn(|| ask(servers, &name, TYPE_AAAA));
            (ask(servers, &name, TYPE_A), aaaa.join().unwrap_or_else(|_| Err("DNS thread failed".to_string())))
        });
        for answer in [a, aaaa] {
            let answer = answer?.ok_or_else(|| format!("no such host: {name}"))?;
            // `Ok(None)` means the server said the name does not exist.
            addrs.extend(answer.addrs);
            ttl = ttl.min(answer.ttl);
            alias = alias.or(answer.alias);
        }
        if !addrs.is_empty() {
            return Ok((addrs, ttl));
        }
        match alias {
            Some(target) => name = target,
            // The server gave an alias but no address. Ask again about the alias.
            None => return Err(format!("no addresses for {name}")),
        }
    }
    Err(format!("too many aliases for {name}"))
}

/// Ask each server in turn, two tries each, until one answers.
/// `Ok(None)` means the name does not exist.
fn ask(servers: &[IpAddr], name: &str, qtype: u16) -> Result<Option<Answer>, String> {
    let mut error = String::from("no DNS servers");
    for &server in servers {
        for _ in 0..2 {
            match query(SocketAddr::new(server, 53), name, qtype) {
                Ok(answer) => return Ok(answer),
                Err(e) => error = e,
                // A lost packet or a slow server. Try again.
            }
        }
    }
    Err(error)
}

/// Send one question to one server and read the answer.
fn query(server: SocketAddr, name: &str, qtype: u16) -> Result<Option<Answer>, String> {
    let id = RandomState::new().hash_one(0u8) as u16;
    // A random ID for this question. The answer must carry the same ID.
    // Someone who wants to send us a fake answer must guess it.
    // Rust's standard library has no simple random function, so we borrow the randomness
    // that `HashMap` uses inside. You can read this line as `Math.random()`.
    let packet = build_query(id, name, qtype)?;
    let err = |e: std::io::Error| format!("DNS query to {server} failed: {e}");
    // A small arrow function that turns a network error into our error text.

    let local = if server.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" };
    let udp = UdpSocket::bind(local).map_err(err)?;
    // Port 0 means "any free port". The OS picks a random one.
    udp.connect(server).map_err(err)?;
    // UDP has no real connection. Here `connect` only means "talk to this server",
    // and ignore messages from anyone else.
    udp.set_read_timeout(Some(TIMEOUT)).map_err(err)?;
    udp.send(&packet).map_err(err)?;
    let mut buf = vec![0; 4096];
    let n = udp.recv(&mut buf).map_err(err)?;
    let reply = &buf[..n];

    if reply.len() > 2 && reply[2] & 0x02 != 0 {
        // The "truncated" flag is on: the answer did not fit. Ask again over TCP.
        return query_tcp(server, &packet, id, name, qtype).map_err(err)?;
    }
    parse_response(reply, id, name, qtype)
}

/// Ask over TCP. TCP is one long stream of bytes, so each message starts with its length.
/// Then the reader knows where the message ends.
fn query_tcp(server: SocketAddr, packet: &[u8], id: u16, name: &str, qtype: u16) -> std::io::Result<Result<Option<Answer>, String>> {
    let mut tcp = TcpStream::connect_timeout(&server, TIMEOUT)?;
    tcp.set_read_timeout(Some(TIMEOUT))?;
    tcp.write_all(&(packet.len() as u16).to_be_bytes())?;
    // `to_be_bytes` turns a number into bytes, biggest part first.
    // Networks always use this order.
    tcp.write_all(packet)?;
    let mut len = [0u8; 2];
    tcp.read_exact(&mut len)?;
    let mut reply = vec![0; u16::from_be_bytes(len).into()];
    tcp.read_exact(&mut reply)?;
    Ok(parse_response(&reply, id, name, qtype))
}

/// Build a question packet.
fn build_query(id: u16, name: &str, qtype: u16) -> Result<Vec<u8>, String> {
    let mut p = Vec::new();
    p.extend(id.to_be_bytes());
    p.extend([0x01, 0x00]);
    // Settings. Only one is on: "please find the full answer for me".
    p.extend([0, 1, 0, 0, 0, 0, 0, 0]);
    // How many items follow: 1 question, and no answers or other records.
    for label in name.split('.') {
        // A name is sent as labels, each with its length first:
        // "example.com" becomes [7]example[3]com[0].
        if label.is_empty() || label.len() > 63 {
            return Err(format!("name cannot be sent to DNS: {name}"));
            // DNS allows at most 63 characters per label. Browsers allow longer labels in
            // URLs, but DNS cannot send them.
        }
        p.push(label.len() as u8);
        p.extend(label.as_bytes());
    }
    p.push(0); // a length of 0 ends the name
    if p.len() - 12 > 255 {
        return Err(format!("name too long for DNS: {name}"));
    }
    p.extend(qtype.to_be_bytes());
    p.extend(CLASS_IN.to_be_bytes());
    Ok(p)
}

/// Read an answer packet. Check that it answers our question, then collect the addresses.
fn parse_response(p: &[u8], id: u16, name: &str, qtype: u16) -> Result<Option<Answer>, String> {
    if read_u16(p, 0)? != id {
        return Err("DNS answer has the wrong ID".to_string());
    }
    let flags = read_u16(p, 2)?;
    if flags & 0x8000 == 0 {
        return Err("DNS packet is not an answer".to_string());
        // One flag says if this is a question or an answer.
    }
    match flags & 0x000F {
        // The result code: 0 is OK, 3 is "no such name", anything else is an error.
        0 => {}
        3 => return Ok(None), // the name does not exist
        code => return Err(format!("DNS server error, code {code}")),
    }
    if read_u16(p, 4)? != 1 {
        return Err("DNS answer must repeat exactly one question".to_string());
    }
    let answer_count = read_u16(p, 6)?;

    let (question, mut pos) = read_name(p, 12)?;
    if !question.eq_ignore_ascii_case(name) || read_u16(p, pos)? != qtype {
        return Err("DNS answer is for a different question".to_string());
        // Another check against fake answers.
    }
    pos += 4; // skip the type and class of the question

    let mut answer = Answer { addrs: Vec::new(), alias: None, ttl: u32::MAX };
    let mut owner = name.to_string();
    // The name whose records we want. An alias record changes it:
    // "www.example.com is example.com" means we now want records for "example.com".
    for _ in 0..answer_count {
        // Each record: name, type, class, how long to keep it, data length, data.
        let (record_name, next) = read_name(p, pos)?;
        let rtype = read_u16(p, next)?;
        let class = read_u16(p, next + 2)?;
        let ttl = read_u32(p, next + 4)?;
        let len = read_u16(p, next + 8)? as usize;
        let start = next + 10;
        let data = p.get(start..start + len).ok_or("DNS packet too short")?;
        pos = start + len;

        if class != CLASS_IN || !record_name.eq_ignore_ascii_case(&owner) {
            continue; // not part of the chain from our question to the address
        }
        match rtype {
            TYPE_A if qtype == TYPE_A && len == 4 => {
                answer.addrs.push(IpAddr::V4(Ipv4Addr::new(data[0], data[1], data[2], data[3])));
            }
            TYPE_AAAA if qtype == TYPE_AAAA && len == 16 => {
                let bytes: [u8; 16] = data.try_into().map_err(|_| "bad AAAA record")?;
                // `try_into` turns the bytes into a list of exactly 16, if the length is right.
                answer.addrs.push(IpAddr::V6(Ipv6Addr::from(bytes)));
            }
            TYPE_CNAME => {
                let (target, _) = read_name(p, start)?;
                owner = target.clone();
                answer.alias = Some(target);
            }
            _ => continue, // a record type we do not need
        }
        answer.ttl = answer.ttl.min(ttl);
    }
    Ok(Some(answer))
}

/// Read a name at `start`. Give the name and the position just after it.
///
/// Names can be shortened: instead of repeating a name, the packet can say "the rest of
/// the name is at position N". This saves space, because one name often appears many times.
fn read_name(p: &[u8], start: usize) -> Result<(String, usize), String> {
    let mut labels = Vec::new();
    let mut pos = start;
    let mut end = None; // where the name ends in the packet, after the first pointer
    let mut jumps = 0;
    loop {
        let len = *p.get(pos).ok_or("DNS packet too short")? as usize;
        // `.get(i)` gives `None` past the end, instead of crashing. Every read in this file
        // is checked, because the packet comes from the network and may contain anything.
        if len == 0 {
            return Ok((labels.join("."), end.unwrap_or(pos + 1)));
        } else if len & 0xC0 == 0xC0 {
            end = end.or(Some(pos + 2));
            jumps += 1;
            if jumps > 32 {
                return Err("DNS name has a pointer loop".to_string());
                // A fake packet can point a name at itself. Without this limit we would loop forever.
            }
            pos = (read_u16(p, pos)? & 0x3FFF) as usize;
            // Read the position to jump to.
        } else if len <= 63 {
            let label = p.get(pos + 1..pos + 1 + len).ok_or("DNS packet too short")?;
            labels.push(String::from_utf8_lossy(label).into_owned());
            pos += 1 + len;
        } else {
            return Err("bad DNS label".to_string());
        }
    }
}

fn read_u16(p: &[u8], i: usize) -> Result<u16, String> {
    let b = p.get(i..i + 2).ok_or("DNS packet too short")?;
    Ok(u16::from_be_bytes([b[0], b[1]]))
}

fn read_u32(p: &[u8], i: usize) -> Result<u32, String> {
    let b = p.get(i..i + 4).ok_or("DNS packet too short")?;
    Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// The addresses for `name` in the text of `/etc/hosts`. Each line: address, then names.
fn hosts_lookup(text: &str, name: &str) -> Vec<IpAddr> {
    let mut addrs = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("");
        // Text after "#" is a comment.
        let mut words = line.split_whitespace();
        let Some(ip) = words.next().and_then(|w| w.parse::<IpAddr>().ok()) else {
            continue;
        };
        if words.any(|w| w.eq_ignore_ascii_case(name)) && !addrs.contains(&ip) {
            addrs.push(ip);
        }
    }
    addrs
}

/// The DNS server addresses in the text of `/etc/resolv.conf`: lines like "nameserver 1.1.1.1".
fn nameservers(text: &str) -> Vec<IpAddr> {
    text.lines()
        .filter_map(|line| line.strip_prefix("nameserver"))
        .filter_map(|rest| rest.split_whitespace().next()?.parse().ok())
        .collect()
    // `filter_map` is `map` and `filter` in one: it keeps only the `Some` results.
    // JS: `.map(f).filter(x => x !== undefined)`.
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// Build an answer to `question` with the given records, like a server would.
    fn reply(question: &[u8], flags: u8, records: &[u8], count: u16) -> Vec<u8> {
        let mut r = question.to_vec();
        r[2] |= 0x80 | flags; // mark it as an answer, plus any extra flags
        r[6..8].copy_from_slice(&count.to_be_bytes()); // the answer count
        r.extend(records);
        r
    }

    /// One record. A name of `0xC0, 12` means "the name at position 12": the question's name.
    fn record(name: &[u8], rtype: u16, ttl: u32, data: &[u8]) -> Vec<u8> {
        let mut r = name.to_vec();
        r.extend(rtype.to_be_bytes());
        r.extend(CLASS_IN.to_be_bytes());
        r.extend(ttl.to_be_bytes());
        r.extend((data.len() as u16).to_be_bytes());
        r.extend(data);
        r
    }

    #[test]
    fn builds_query() {
        let q = build_query(0xABCD, "example.com", TYPE_A).unwrap();
        let mut expected = vec![0xAB, 0xCD, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
        expected.extend(b"\x07example\x03com\x00\x00\x01\x00\x01");
        // `b"..."` is a byte string. `\x07` is the byte 7: the length of "example".
        assert_eq!(q, expected);
        assert!(build_query(1, &"a".repeat(64), TYPE_A).is_err());
        assert!(build_query(1, "a..b", TYPE_A).is_err());
    }

    #[test]
    fn follows_alias_chain() {
        let q = build_query(7, "www.example.com", TYPE_A).unwrap();
        let mut records = record(&[0xC0, 12], TYPE_CNAME, 600, &[0xC0, 16]);
        // "www.example.com is example.com". Position 16 is "example.com" inside the question.
        records.extend(record(&[0xC0, 16], TYPE_A, 300, &[93, 184, 215, 14]));
        records.extend(record(b"\x05other\x00", TYPE_A, 300, &[6, 6, 6, 6]));
        // A record for a name outside our chain. It must be ignored.
        let answer = parse_response(&reply(&q, 0, &records, 3), 7, "www.example.com", TYPE_A).unwrap();
        assert_eq!(
            answer,
            Some(Answer { addrs: vec!["93.184.215.14".parse().unwrap()], alias: Some("example.com".into()), ttl: 300 })
        );
    }

    #[test]
    fn reads_ipv6_and_not_found() {
        let q = build_query(9, "a.com", TYPE_AAAA).unwrap();
        let ip: Ipv6Addr = "2001:db8::1".parse().unwrap();
        let r = reply(&q, 0, &record(&[0xC0, 12], TYPE_AAAA, 60, &ip.octets()), 1);
        assert_eq!(parse_response(&r, 9, "a.com", TYPE_AAAA).unwrap().unwrap().addrs, vec![IpAddr::V6(ip)]);

        let mut nx = reply(&q, 0, &[], 0);
        nx[3] |= 3; // result code 3: no such name
        assert_eq!(parse_response(&nx, 9, "a.com", TYPE_AAAA), Ok(None));
    }

    #[test]
    fn rejects_bad_answers() {
        let q = build_query(5, "a.com", TYPE_A).unwrap();
        let good = reply(&q, 0, &record(&[0xC0, 12], TYPE_A, 60, &[1, 2, 3, 4]), 1);
        assert!(parse_response(&good, 6, "a.com", TYPE_A).is_err()); // wrong ID
        assert!(parse_response(&good, 5, "b.com", TYPE_A).is_err()); // different question
        assert!(parse_response(&q, 5, "a.com", TYPE_A).is_err()); // a question, not an answer
        for cut in 0..good.len() {
            assert!(parse_response(&good[..cut], 5, "a.com", TYPE_A).is_err());
            // Every shorter packet gives an error, never a crash.
        }
        let looped = reply(&q, 0, &record(&[0xC0, 23], TYPE_A, 60, &[1, 2, 3, 4]), 1);
        // The record starts at position 23 (12 header + 7 name + 4 type and class). Its name
        // is a pointer to position 23: itself, forever.
        assert!(parse_response(&looped, 5, "a.com", TYPE_A).is_err());
    }

    #[test]
    fn reads_hosts_and_resolv_conf() {
        let hosts = "127.0.0.1 localhost\n10.0.0.5  Box box.lan # my server\n::1 box\nbad line\n";
        assert_eq!(hosts_lookup(hosts, "box"), vec!["10.0.0.5".parse::<IpAddr>().unwrap(), "::1".parse().unwrap()]);
        assert!(hosts_lookup(hosts, "lan").is_empty());
        let conf = "# comment\nnameserver 127.0.0.53\nnameserver 2606:4700::1111\noptions edns0\n";
        assert_eq!(nameservers(conf), vec!["127.0.0.53".parse::<IpAddr>().unwrap(), "2606:4700::1111".parse().unwrap()]);
    }

    #[test]
    fn literals_and_localhost() {
        assert_eq!(resolve("10.1.2.3").unwrap(), vec!["10.1.2.3".parse::<IpAddr>().unwrap()]);
        assert_eq!(resolve("::1").unwrap(), vec!["::1".parse::<IpAddr>().unwrap()]);
        assert_eq!(resolve("app.LOCALHOST.").unwrap().len(), 2);
    }

    #[test]
    fn falls_back_to_tcp() {
        // A fake DNS server on this machine. Over UDP it says "truncated".
        // Over TCP, on the same port, it gives the real answer.
        let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = udp.local_addr().unwrap();
        let tcp = TcpListener::bind(addr).unwrap();
        let server = std::thread::spawn(move || {
            // `thread::spawn` runs this function at the same time as the rest of the test.
            // `move` gives the function its own `udp` and `tcp`.
            let mut buf = [0u8; 512];
            let (n, from) = udp.recv_from(&mut buf).unwrap();
            udp.send_to(&reply(&buf[..n], 0x02, &[], 0), from).unwrap(); // 0x02: the "truncated" flag

            let (mut s, _) = tcp.accept().unwrap();
            let mut len = [0u8; 2];
            s.read_exact(&mut len).unwrap();
            let mut q = vec![0; u16::from_be_bytes(len).into()];
            s.read_exact(&mut q).unwrap();
            let r = reply(&q, 0, &record(&[0xC0, 12], TYPE_A, 300, &[93, 184, 215, 14]), 1);
            s.write_all(&(r.len() as u16).to_be_bytes()).unwrap();
            s.write_all(&r).unwrap();
        });
        let answer = query(addr, "example.com", TYPE_A).unwrap().unwrap();
        assert_eq!(answer.addrs, vec!["93.184.215.14".parse::<IpAddr>().unwrap()]);
        assert_eq!(answer.ttl, 300);
        // Check first. If the client never used TCP, the server still waits for it,
        // and waiting for the server would hang the test instead of failing it.
        server.join().unwrap();
        // Wait for the fake server to finish.
    }

    #[test]
    fn random_packets_never_crash() {
        // Break a good packet in random ways. Any result is fine, but not a panic or a hang.
        let q = build_query(5, "a.com", TYPE_A).unwrap();
        let good = reply(&q, 0, &record(&[0xC0, 12], TYPE_A, 60, &[1, 2, 3, 4]), 1);
        let mut state = 0x2545F4914F6CDD1Du64;
        let mut next = move || {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            state.wrapping_mul(0x2545F4914F6CDD1D)
        };
        for _ in 0..50_000 {
            let mut p = good.clone();
            for _ in 0..1 + next() % 6 {
                let at = (next() % p.len() as u64) as usize;
                match next() % 4 {
                    0 => p[at] = next() as u8,
                    1 => p[at] = 0xC0,
                    2 => p.truncate(at),
                    _ => p.insert(at, next() as u8),
                }
                if p.is_empty() {
                    break;
                }
            }
            let _ = parse_response(&p, 5, "a.com", TYPE_A);
        }
    }

    #[test]
    fn a_cached_answer_needs_no_network() {
        let ip: IpAddr = "192.0.2.7".parse().unwrap();
        CACHE.lock().unwrap().insert("cached.example.invalid".into(), (vec![ip], Instant::now() + Duration::from_secs(60)));
        assert_eq!(resolve("cached.example.invalid"), Ok(vec![ip]));
        // The name does not exist. Only the cache can answer, so no question went out.
    }
}
