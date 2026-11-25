//! A DNS server that answers late. systemd-resolved answers a cold name in up to about 2.1 s when the upstream server is slow
//! (measured on archMachine, 2026-11-25: 3 of 12 cold lookups took 1.4 s to 2.08 s). glibc waits 5 s for each try (`RES_TIMEOUT`).

use super::tests::{record, reply};
use super::*;

#[test]
fn a_reply_after_two_and_a_half_seconds_is_used() {
    let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
    let addr = udp.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let mut buf = [0u8; 512];
        let (n, from) = udp.recv_from(&mut buf).unwrap();
        std::thread::sleep(Duration::from_millis(2500));
        udp.send_to(&reply(&buf[..n], 0, &record(&[0xC0, 12], TYPE_A, 300, &[93, 184, 215, 14]), 1), from).unwrap();
    });
    let answer = query(addr, "example.com", TYPE_A).expect("the late reply").unwrap();
    assert_eq!(answer.addrs, vec!["93.184.215.14".parse::<IpAddr>().unwrap()]);
    server.join().unwrap();
}
