//! TLS and its crypto, written by hand.
//!
//! Crypto foundations: SHA-256, SHA-384, HMAC-SHA256, HKDF-SHA256 and AES-128-GCM.
//! X25519 provides key exchange with OS random input. X.509 path validation uses independent roots. `client::connect` runs a TLS 1.3 client handshake with validated chains.
//!
//! WARNING: this code is hand-written. Nobody has audited it. It has known-answer and independent-oracle checks. It does not promise constant-time behavior. Do not use it to protect
//! real secrets.

pub mod aes_gcm;
pub mod bigint;
pub mod client;
pub mod ecc;
pub mod der;
pub mod x509;
pub mod hkdf;
pub mod hmac;
pub mod hello;
pub mod key_schedule;
pub mod record;
pub mod rsa;
pub mod hostname;
pub mod trust_store;
pub mod verify;
pub mod sha256;
pub mod sha384;
pub mod x25519;
pub mod random;
#[cfg(all(target_arch = "x86_64", not(feature = "force-portable")))]
mod sha256_x86;
