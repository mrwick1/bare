# TLS client fixtures

These files are public test data. They hold no secret.

- `leaf.der`: a synthetic self-signed P-256 certificate. Subject and SAN: `client-test.bare.invalid`. Valid from 2026-11-04 to 2126-10-11. It stands for no real host.
- `cv_alpn.der`, `cv_plain.der`: ECDSA-SHA256 CertificateVerify signatures. Each covers the transcript of the fixed ClientHello in `tools/crypto/client_fixture.rs` plus one EncryptedExtensions variant and the Certificate message.
- 2026-11-09: new key, certificate and signatures, because the ClientHello now offers RSA signature algorithms (the transcript changed). Same subject, serial and validity.
- The private key lived in RAM (`/dev/shm`) only. It was deleted after signing. Nothing can sign new data with it.

To regenerate (after a ClientHello change): run `cargo test client::tests::print_cv_input -- --ignored --nocapture`, make a new key and self-signed certificate in `/dev/shm`, sign each printed input with `openssl dgst -sha256 -sign`, replace the three files, and delete the key.
