# ECDSA test data

These files test `crates/tls/src/ecc.rs`. They are data, not code that `bare` runs.

- `vectors.txt`: the vectors that the unit tests read. It has 656 signature cases and 24 public-key cases. `make_vectors.py` builds it.
- `make_vectors.py`: converts the sources below. It hashes test messages with Python `hashlib`, because `ecc::verify` takes digests. It signs the hand cases with OpenSSL through the `cryptography` package.
- `oracle_gen.py`: makes random cases and asks OpenSSL for the correct answer. Output goes to a scratch file for `tools/crypto/ecc_verify_load.rs`. It is not committed.
- `LICENSE-wycheproof-APACHE-2.0`: license of the Wycheproof data.

## Sources

| Source | Content | Used for | License |
| ------ | ------- | -------- | ------- |
| NIST CAVP, "ECDSA test vectors" `186-3ecdsatestvectors.zip` (CAVS 11.0, 2011), files `SigVer.rsp` and `PKV.rsp`. Same bytes as the 186-4 zip. <https://csrc.nist.gov/projects/cryptographic-algorithm-validation-program/digital-signatures> | known-answer signature checks, valid and invalid public keys (off the curve, coordinate out of range) | P-256 with SHA-256 and SHA-384, P-384 with SHA-256 and SHA-384 | US government work, no license restriction |
| Project Wycheproof (C2SP), `testvectors_v1/ecdsa_secp256r1_sha256_p1363_test.json` and `ecdsa_secp384r1_sha384_p1363_test.json`, version 0.9rc5. <https://github.com/C2SP/wycheproof> | edge cases: r or s at 0, n and 2^256, wrong lengths, high-s, special public keys, arithmetic carry cases, Shamir-trick infinity | P-256 and P-384 | Apache-2.0 |
| OpenSSL 3.6.4 through Python `cryptography` 50.0.1 | keys d = 1, 2 and n-1 (G, 2G and -G as public keys), their signatures, high-s twins, and wrong-digest cases | boundary public keys | the signatures are generated here |

Curve constants in `ecc.rs` were also compared with `openssl ecparam -name prime256v1|secp384r1 -param_enc explicit`.

## Rebuild

```sh
mkdir -p /tmp/ecdsa-src && cd /tmp/ecdsa-src
curl -LO https://csrc.nist.gov/CSRC/media/Projects/Cryptographic-Algorithm-Validation-Program/documents/dss/186-3ecdsatestvectors.zip
unzip 186-3ecdsatestvectors.zip -d nist
for f in ecdsa_secp256r1_sha256_p1363_test.json ecdsa_secp384r1_sha384_p1363_test.json; do
  curl -LO https://raw.githubusercontent.com/C2SP/wycheproof/main/testvectors_v1/$f
done
cd <repo> && python3 -I data/crypto/ecdsa/make_vectors.py /tmp/ecdsa-src/nist /tmp/ecdsa-src > data/crypto/ecdsa/vectors.txt
```

The hand cases use a new random nonce on each run. A rebuild changes those lines but not their meaning.
