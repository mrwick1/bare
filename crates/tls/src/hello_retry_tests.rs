//! HelloRetryRequest parsing and the second ClientHello (RFC 8446 §4.1.2, §4.1.4).

use super::*;

const SID: [u8; 32] = [0x22; 32];

fn ext(ty: u16, body: &[u8]) -> Vec<u8> {
    [&ty.to_be_bytes()[..], &(body.len() as u16).to_be_bytes(), body].concat()
}

fn hello(random: &[u8; 32], exts: &[u8]) -> Vec<u8> {
    let mut b = vec![3, 3];
    b.extend_from_slice(random);
    b.push(SID.len() as u8);
    b.extend_from_slice(&SID);
    b.extend_from_slice(&[0x13, 0x01, 0]);
    b.extend_from_slice(&(exts.len() as u16).to_be_bytes());
    b.extend_from_slice(exts);
    [&[SERVER_HELLO, 0, (b.len() >> 8) as u8, b.len() as u8][..], &b].concat()
}

fn hrr(exts: &[Vec<u8>]) -> Vec<u8> {
    hello(&HELLO_RETRY_REQUEST_RANDOM, &exts.concat())
}

fn sv13() -> Vec<u8> {
    ext(43, &[3, 4])
}

#[test]
fn a_retry_for_p256_is_read_with_and_without_a_cookie() {
    let m = hrr(&[sv13(), ext(51, &[0, 0x17])]);
    assert!(is_hello_retry_request(&m));
    assert_eq!(parse_hello_retry_request(&m, &SID), Ok(&[][..]));
    let m = hrr(&[sv13(), ext(51, &[0, 0x17]), ext(44, &[0, 3, 7, 8, 9])]);
    assert_eq!(parse_hello_retry_request(&m, &SID), Ok(&[7, 8, 9][..]));
}

#[test]
fn a_bad_retry_is_refused() {
    let p = |exts: &[Vec<u8>]| parse_hello_retry_request(&hrr(exts), &SID).map(|c| c.to_vec());
    // X25519 is the share we already sent: asking for it again is an error, so is any group we did not offer.
    assert_eq!(p(&[sv13(), ext(51, &[0, 0x1d])]), Err(HelloError::BadKeyShare));
    assert_eq!(p(&[sv13(), ext(51, &[0, 0x18])]), Err(HelloError::BadKeyShare));
    assert_eq!(p(&[sv13()]), Err(HelloError::MissingExtension(51)));
    assert_eq!(p(&[ext(51, &[0, 0x17])]), Err(HelloError::MissingExtension(43)));
    assert_eq!(p(&[sv13(), ext(51, &[0, 0x17]), ext(51, &[0, 0x17])]), Err(HelloError::DuplicateExtension(51)));
    assert_eq!(p(&[sv13(), ext(51, &[0, 0x17]), ext(16, &[])]), Err(HelloError::UnsolicitedExtension(16)));
    assert_eq!(p(&[sv13(), ext(51, &[0, 0x17]), ext(44, &[0, 0])]), Err(HelloError::BadLength));
    let big = [&((MAX_COOKIE_LEN + 1) as u16).to_be_bytes()[..], &[1; MAX_COOKIE_LEN + 1]].concat();
    assert_eq!(p(&[sv13(), ext(51, &[0, 0x17]), ext(44, &big)]), Err(HelloError::TooLarge));
    assert_eq!(parse_hello_retry_request(&hrr(&[sv13(), ext(51, &[0, 0x17])]), &[1; 32]), Err(HelloError::SessionIdMismatch));
    // A normal ServerHello is not a retry, and a retry is not a ServerHello.
    let sh = hello(&[5; 32], &[sv13(), ext(51, &[0, 0x17])].concat());
    assert!(!is_hello_retry_request(&sh));
    assert_eq!(parse_hello_retry_request(&sh, &SID), Err(HelloError::BadMessageType));
    assert_eq!(parse_server_hello_p256(&hrr(&[sv13(), ext(51, &[0, 0x17])]), &SID), Err(HelloError::HelloRetryRequest));
}

#[test]
fn the_server_hello_after_a_retry_carries_a_p256_share() {
    let point = [4; 65];
    let share = [&[0, 0x17, 0, 65][..], &point].concat();
    assert_eq!(parse_server_hello_p256(&hello(&[5; 32], &[sv13(), ext(51, &share)].concat()), &SID), Ok(point));
    let x = [&[0, 0x1d, 0, 32][..], &[7; 32]].concat();
    assert_eq!(parse_server_hello_p256(&hello(&[5; 32], &[sv13(), ext(51, &x)].concat()), &SID), Err(HelloError::BadKeyShare));
    assert_eq!(parse_server_hello_p256(&hello(&[5; 32], &[sv13(), ext(51, &share[..60])].concat()), &SID), Err(HelloError::BadKeyShare));
}

#[test]
fn the_second_hello_swaps_the_share_and_echoes_the_cookie() {
    let p = ClientHelloParams { random: &[1; 32], session_id: &SID, x25519_public: &[2; 32], server_name: b"x.test", alpn_h2: true };
    let mut buf = [0u8; MAX_CLIENT_HELLO_LEN];
    let first_len = encode_client_hello(&p, &mut buf).unwrap();
    let first = &buf[..first_len];
    let mut out = [0u8; MAX_RETRY_HELLO_LEN];
    let n = encode_retry_hello(&p, &[4; 65], &[9, 9, 9], &mut out).unwrap();
    let second = &out[..n];
    // The P-256 share replaces the X25519 share and the cookie extension follows key_share. Nothing else changes.
    assert_eq!(n, first.len() + 33 + 9);
    assert!(second.windows(71).any(|w| w == [&[0, 0x33, 0, 0x47, 0, 0x45, 0, 0x17, 0, 65][..], &[4; 61]].concat()));
    assert!(second.windows(9).any(|w| w == [0, 44, 0, 5, 0, 3, 9, 9, 9]));
    assert!(!second.windows(32).any(|w| w == [2; 32]));
    // The largest second hello fits its bound exactly.
    let name = ["a".repeat(63), "a".repeat(63), "a".repeat(63), "a".repeat(61)].join(".").into_bytes();
    let p = ClientHelloParams { server_name: &name, ..p };
    assert_eq!(encode_retry_hello(&p, &[4; 65], &[1; MAX_COOKIE_LEN], &mut out), Ok(MAX_RETRY_HELLO_LEN));
    assert_eq!(encode_retry_hello(&p, &[4; 65], &[1; MAX_COOKIE_LEN + 1], &mut out), Err(HelloError::BadInput));
}
