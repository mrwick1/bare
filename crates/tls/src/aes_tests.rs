//! Known answers for `aes.rs`. FIPS 197 appendix C for the blocks. The mode vectors come from Python `cryptography` 50
//! (OpenSSL), made once by hand: key = bytes 0..n, CBC IV = bytes 100..116, GCM IV = bytes 200..212, AAD = "header".

use super::*;

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

fn key(n: u8) -> Vec<u8> {
    (0..n).collect()
}

const MSG: &[u8] = b"The quick brown fox jumps over the lazy dog";

#[test]
fn fips197_blocks_both_ways_for_each_key_size() {
    for (n, want) in [(16, "69c4e0d86a7b0430d8cdb78070b4c55a"), (24, "dda97ca4864cdfe06eaf70a0ec0d7191"), (32, "8ea2b7ca516745bfeafc49904b496089")] {
        let aes = Aes::new(&key(n)).unwrap();
        let mut b: [u8; 16] = unhex("00112233445566778899aabbccddeeff").try_into().unwrap();
        aes.encrypt_block(&mut b);
        assert_eq!(b.to_vec(), unhex(want), "{n}");
        aes.decrypt_block(&mut b);
        assert_eq!(b.to_vec(), unhex("00112233445566778899aabbccddeeff"), "{n}");
    }
    assert!(Aes::new(&key(20)).is_none());
}

#[test]
fn cbc_matches_openssl_and_round_trips() {
    let iv: Vec<u8> = (100..116).collect();
    for (n, data, want) in [
        (16, MSG, "46051c2688e3b68888c8ee9274d57ebdd361e3860a97891b70319ac9540a8b57f3cb9fad5d51e455e53fd5deef4b3b04"),
        (16, &b""[..], "a82eb5a05312bb3bd0654452081202cd"),
        (32, MSG, "f865a2689e1ff9cfbe3ab97f7f5d3e8513b1d396e276da84a7cbfb9436dc8ccc3971ed4818719ef8a6ffd1ad3ac479a7"),
        (32, &b""[..], "f795f6e8802fd151a021a41150d4ab69"),
    ] {
        let ct = cbc_encrypt(&key(n), &iv, data).unwrap();
        assert_eq!(ct, unhex(want), "{n}");
        assert_eq!(cbc_decrypt(&key(n), &iv, &ct).unwrap(), data, "{n}");
    }
    let mut ct = cbc_encrypt(&key(32), &iv, MSG).unwrap();
    assert_eq!(cbc_decrypt(&key(32), &iv[..15], &ct), Err(Error::Bad));
    assert_eq!(cbc_decrypt(&key(32), &iv, &ct[..40]), Err(Error::Bad));
    *ct.last_mut().unwrap() ^= 1;
    assert_eq!(cbc_decrypt(&key(32), &iv, &ct), Err(Error::Bad));
}

#[test]
fn gcm_matches_openssl_and_rejects_any_change() {
    let iv: Vec<u8> = (200..212).collect();
    for (n, want) in [
        (16, "6240f19e7f46424ab5b7ca30ae160d86167f8529bb5a7063a79728be738dd1b1965fb671f5f0f2158cd94fc7d478a1fef57c00ed919bbd0c4882ba"),
        (24, "edee7908e76339d7770c74852aa4ec9651971ad829347ac9e00f4460a328759256f887b325f7d86577851588b345f2886bc8bf449ec4e4ec1bf49d"),
        (32, "451004fbf7b992431f1c7dcc7171853486e894886e1ec5549e895497bfe38b9da4e90acaf365538a56902361e30ef518e5d6510149d632253c42ca"),
    ] {
        let ct = gcm_seal(&key(n), &iv, b"header", MSG, 16).unwrap();
        assert_eq!(ct, unhex(want), "{n}");
        assert_eq!(gcm_open(&key(n), &iv, b"header", &ct, 16).unwrap(), MSG, "{n}");
        assert_eq!(gcm_open(&key(n), &iv, b"headeR", &ct, 16), Err(Error::AuthFailed), "{n}");
        let short = gcm_seal(&key(n), &iv, b"header", MSG, 12).unwrap();
        assert_eq!(short[..], unhex(want)[..MSG.len() + 12]);
        assert_eq!(gcm_open(&key(n), &iv, b"header", &short, 12).unwrap(), MSG);
    }
    assert_eq!(gcm_seal(&key(32), &iv, b"", b"", 16).unwrap(), unhex("31678598ce3f216c1798f4f79aa87f67"));
    assert_eq!(gcm_seal(&key(32), &iv[..8], b"", b"", 16), Err(Error::Bad));
    assert_eq!(gcm_open(&key(32), &iv, b"", &[0; 3], 16), Err(Error::AuthFailed));
}
