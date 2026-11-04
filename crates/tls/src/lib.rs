//! TLS and its crypto, written by hand.
//!
//! Crypto foundations: SHA-256, SHA-384, HMAC-SHA256, HKDF-SHA256 and AES-128-GCM.
//! There is no handshake, X.509 or X25519 yet.
//!
//! WARNING: this code is hand-written. Nobody has audited it. It checks known-answer
//! vectors only. It does not promise constant-time behavior. Do not use it to protect
//! real secrets. `bare` cannot open an HTTPS page yet.

pub mod aes_gcm;
pub mod hkdf;
pub mod hmac;
pub mod sha256;
pub mod sha384;
#[cfg(all(target_arch = "x86_64", not(feature = "force-portable")))]
mod sha256_x86;
