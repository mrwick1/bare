# X.509 fixtures: provenance

All files are public certificates. There are no private keys and no credentials.

## Staging chain (`staging-*.der`)

- Source: ONE `openssl s_client -connect staging.app.8vox.ai:443 -servername staging.app.8vox.ai -showcerts` handshake on 2026-11-04 (OpenSSL 3.6.4). No load, no credentials.
- Each PEM block was converted to DER with `openssl x509 -outform DER`.
- The SHA-256 values for 0..3 match the table in `docs/staging-network.md`.
- The leaf expires 2027-01-13. Tests use fixed times, so expiry does not break parsing tests.

## `isrg-root-x2.der`

- Source: https://letsencrypt.org/certs/isrg-root-x2.der, fetched 2026-11-04. This is independent of the server chain.
- Its SHA-256 equals the system-store copy `/etc/ssl/certs/ISRG_Root_X2.pem` (DER): `69729b8e…1470`.

## `tbs/*.tbs.der`

- The TBSCertificate bytes of each fixture: `openssl asn1parse -inform DER -in <cert> -strparse 4 -noout -out <tbs>`. Oracle for the parser's `tbs` slice.

## `expect.txt`

- Written by `make_expect.py` from `openssl x509 -noout -text`. Oracle for serial, dates (Unix seconds), signature bytes, public key point, curve, basic constraints, key usage bits, EKU, SAN, SKI, AKI and extension count.
- Regenerate: `cd tools/fixtures/x509 && python3 -I make_expect.py`.

## SHA-256

```
69729b8e15a86efc177a57afb7171dfc64add28c2fca8cf1507e34453ccb1470  isrg-root-x2.der
cdf85d230b362015c23bcf9c1505f0ed8783d0bd5b9986bf8726b75c1570c35b  staging-0-leaf.der
97658de8c68dfa98ace1e5028a63d54a1aae911b3e21471076c6850cd08cbab4  staging-1-le-ye2.der
0fc0901cca2bae9e9fdbb02d50d02f1094f7b36672086991b9e897626dc485f0  staging-2-isrg-root-ye.der
ee5f7abd6981bb0255632cd8f49283451b4b18844d12040b44ee00f07b8fe2c6  staging-3-isrg-x2-crosssigned.der
4f6979e0ef5c5ded093e855812f9a101520aaf7a64ef480e8d300a4c76ff48eb  tbs/isrg-root-x2.tbs.der
39972eb734b3e5772f6c5d855b96178b04f655495a6241f7fd1f4fbc64432ad1  tbs/staging-0-leaf.tbs.der
df469a3e99fac188ea4807182b89fb621b43b315d63b2b1159e09329889b09bb  tbs/staging-1-le-ye2.tbs.der
369d1cf858e309c02aa2824c3763e12c05975e03291bff2bb462567bcbe93f21  tbs/staging-2-isrg-root-ye.tbs.der
8c5033973ffe4ff1ef0aa92984fc93281ba7a9278ef8bf641226b898d10d4e74  tbs/staging-3-isrg-x2-crosssigned.tbs.der
3468004340c109f4b803558436775261b1dea623e26c1af17c7cf8737c20b813  expect.txt
5fac657a5ff11e1803f01cde8f670acd33a256757f0920adc197047d26954e35  make_expect.py
```
