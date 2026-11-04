//! TLS and its crypto, written by hand.
//!
//! Crypto foundations: SHA-256, SHA-384, HMAC-SHA256, HKDF-SHA256 and AES-128-GCM.
//! X25519 provides key exchange with OS random input. X.509 parsing is available; chain validation and a handshake are not yet connected.
//!
//! WARNING: this code is hand-written. Nobody has audited it. It checks known-answer
//! vectors only. It does not promise constant-time behavior. Do not use it to protect
//! real secrets. `bare` cannot open an HTTPS page yet.

pub mod aes_gcm;
pub mod der;
pub mod x509;
pub mod hkdf;
pub mod hmac;
pub mod sha256;
pub mod sha384;
pub mod x25519;
pub mod random;
#[cfg(all(target_arch = "x86_64", not(feature = "force-portable")))]
mod sha256_x86;
