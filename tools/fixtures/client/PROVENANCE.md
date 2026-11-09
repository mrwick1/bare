# TLS client fixtures

These files are public test data. They hold no secret.

- `leaf.der`: a synthetic self-signed P-256 certificate. Subject and SAN: `client-test.bare.invalid`. Valid from 2026-11-04 to 2126-10-11. It stands for no real host.
- `cv_alpn.der`, `cv_plain.der`: ECDSA-SHA256 CertificateVerify signatures. Each covers the transcript of the fixed ClientHello in `tools/crypto/client_fixture.rs` plus one EncryptedExtensions variant and the Certificate message.
- 2026-11-09: new key, certificate and signatures, because the ClientHello now offers RSA signature algorithms (the transcript changed). Same subject, serial and validity.
- 2026-11-09 (second change): new key, certificate and signatures again, because the ClientHello now offers 0x0503 in signature_algorithms. Same subject, serial and validity.
- 2026-11-09 (third change, TLS 1.2): new key, certificate and signatures again, because the ClientHello now offers TLS 1.2 (the transcript changed). The new files `ske_ecdsa.der` and `ske_ecdsa_p256.der` are ECDSA-SHA256 signatures of a TLS 1.2 ServerKeyExchange (client_random, server_random, X25519 or P-256 params) for the tests in `crates/tls/src/tls12.rs`. Same subject, serial and validity.
- The private key lived in RAM (`/dev/shm`) only. It was deleted after signing. Nothing can sign new data with it.

To regenerate (after a ClientHello change): run `cargo test client::tests::print_cv_input -- --ignored --nocapture`, make a new key and self-signed certificate in `/dev/shm`, sign each printed input with `openssl dgst -sha256 -sign`, replace the cv files and the two ske files (run the ignored `client::tls12::tests::print_ske_input` test too), and delete the key.
