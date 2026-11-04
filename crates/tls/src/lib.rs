//! TLS and its crypto, written by hand.
//!
//! Step 1 has three parts: SHA-256 (FIPS 180-4), HMAC-SHA256 (RFC 2104, RFC 4231)
//! and HKDF-SHA256 (RFC 5869). AES-128-GCM provides record protection. There is no handshake, X.509 or X25519 yet.
//!
//! WARNING: this code is hand-written. Nobody has audited it. It checks known-answer
//! vectors only. It does not promise constant-time behavior. Do not use it to protect
//! real secrets. `bare` cannot open an HTTPS page yet.

pub mod aes_gcm;
pub mod hkdf;
pub mod hmac;
pub mod sha256;
#[cfg(all(target_arch = "x86_64", not(feature = "force-portable")))]
mod sha256_x86;
