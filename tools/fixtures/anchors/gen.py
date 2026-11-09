#!/usr/bin/env python3 -I
"""Generate trust-anchor test chains for crates/tls/src/verify.rs (needs the openssl CLI).

Run: cd tools/fixtures/anchors && python3 -I gen.py
Writes pack.pem. Private keys live in a temp dir and are deleted.

Real roots break two DER rules: serial number 0 (Go Daddy, Starfield, SECOM) and a
GeneralizedTime before 2050 (Certum). openssl writes the first with -set_serial 0. It never
writes the second, so this script rewrites the notAfter time of a root and signs it again.

Stores: `zero` (serial-0 root), `gentime` (root with a GeneralizedTime notAfter).
Cases: `# case: NAME|HOST|EXPECT|NOW|STORE`, chain in server order.
- ok-zero, ok-gentime: leaf + intermediate under those roots.
- ok-cross: the server also sends a copy of the root cross-signed by a root the store lacks
  (the archive.org shape), and the order is leaf, cross copy, intermediate.
- bad-inter-zero: an intermediate with serial 0 in the chain. Only store anchors may do that.
- bad-untrusted: the ok-cross chain against a store without the root.
"""
import os, subprocess, tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
D = tempfile.mkdtemp()
NOW = 1_800_000_000
ROOT = ["basicConstraints=critical,CA:TRUE", "keyUsage=critical,keyCertSign,cRLSign", "subjectKeyIdentifier=hash"]
INTER = ["basicConstraints=critical,CA:TRUE,pathlen:0", "keyUsage=critical,keyCertSign,cRLSign",
         "subjectKeyIdentifier=hash", "authorityKeyIdentifier=keyid"]
LEAF = ["basicConstraints=critical,CA:FALSE", "keyUsage=critical,digitalSignature", "extendedKeyUsage=serverAuth",
        "subjectAltName=DNS:test.example", "subjectKeyIdentifier=hash", "authorityKeyIdentifier=keyid"]


def run(*a, inp=None):
    return subprocess.run(a, check=True, capture_output=True, input=inp)


def key(name):
    p = f"{D}/{name}.key"
    run("openssl", "genpkey", "-algorithm", "EC", "-pkeyopt", "ec_paramgen_curve:P-256", "-out", p)
    return p


def cert(name, cn, k, exts, serial, issuer=None, days=36500):
    csr, ext, pem = f"{D}/{name}.csr", f"{D}/{name}.ext", f"{D}/{name}.pem"
    run("openssl", "req", "-new", "-key", k, "-subj", f"/CN={cn}", "-out", csr)
    open(ext, "w").write("[v]\n" + "\n".join(exts) + "\n")
    a = ["openssl", "x509", "-req", "-in", csr, "-days", str(days), "-sha256", "-extfile", ext,
         "-extensions", "v", "-set_serial", str(serial), "-out", pem]
    a += ["-signkey", k] if issuer is None else ["-CA", issuer[0], "-CAkey", issuer[1]]
    run(*a)
    return pem


def der_of(pem):
    return run("openssl", "x509", "-in", pem, "-outform", "DER").stdout


def pem_of(der):
    return run("openssl", "x509", "-inform", "DER", "-in", "/dev/stdin", inp=der).stdout.decode()


def tlv(tag, c):
    n = len(c)
    ln = bytes([n]) if n < 128 else bytes([0x80 | (1 if n < 256 else 2)]) + n.to_bytes(1 if n < 256 else 2, "big")
    return bytes([tag]) + ln + c


def kids(b):
    out, i = [], 0
    while i < len(b):
        j = i + 2 if b[i + 1] < 128 else i + 2 + (b[i + 1] & 0x7F)
        n = b[i + 1] if b[i + 1] < 128 else int.from_bytes(b[i + 2:j], "big")
        out.append(b[i:j + n])
        i = j + n
    return out


def body(t):
    return t[2:] if t[1] < 128 else t[2 + (t[1] & 0x7F):]


def gentime_root(pem, k, year):
    """Rewrite notAfter as GeneralizedTime `year`0101000000Z and sign again."""
    outer = kids(body(der_of(pem)))  # tbs, sigalg, sig
    tbs = kids(body(outer[0]))
    vi = 3 if tbs[0][0] == 0xA0 else 2  # with explicit version: serial, sigalg, issuer, validity
    vi += 1
    nb = kids(body(tbs[vi]))[0]
    tbs[vi] = tlv(0x30, nb + tlv(0x18, f"{year}0101000000Z".encode()))
    new_tbs = tlv(0x30, b"".join(tbs))
    open(f"{D}/g.tbs", "wb").write(new_tbs)
    sig = run("openssl", "dgst", "-sha256", "-sign", k, f"{D}/g.tbs").stdout
    return pem_of(tlv(0x30, new_tbs + outer[1] + tlv(0x03, b"\0" + sig)))


def text(p):
    return open(p).read()


rk = key("root")
root0 = cert("root0", "Bare Anchor Root", rk, ROOT, 0)
rootg = f"{D}/rootg.pem"
open(rootg, "w").write(gentime_root(cert("rootg0", "Bare Gen Root", rk, ROOT, 7), rk, 2046))
ik, lk = key("inter"), key("leaf")
inter = cert("inter", "Bare Anchor Inter", ik, INTER, 21, issuer=(root0, rk))
leaf = cert("leaf", "test.example", lk, LEAF, 22, issuer=(inter, ik))
# The same root name and key, cross-signed by a root that is in no store.
uk = key("unknown")
unknown = cert("unknown", "Bare Unknown Root", uk, ROOT, 5)
cross = cert("cross", "Bare Anchor Root", rk, ["basicConstraints=critical,CA:TRUE", "keyUsage=critical,keyCertSign,cRLSign",
                                              "subjectKeyIdentifier=hash", "authorityKeyIdentifier=keyid"], 23, issuer=(unknown, uk))
interg = cert("interg", "Bare Gen Inter", ik, INTER, 24, issuer=(rootg, rk))
leafg = cert("leafg", "test.example", lk, LEAF, 25, issuer=(interg, ik))
# An intermediate with serial 0 in the chain: the strict parser rejects it.
interz = cert("interz", "Bare Zero Inter", ik, INTER, 0, issuer=(root0, rk))
leafz = cert("leafz", "test.example", lk, LEAF, 26, issuer=(interz, ik))
other = cert("other", "Bare Other Root", key("other"), ROOT, 9)

stores = {"zero": [root0], "gentime": [rootg], "other": [other]}
cases = {
    "ok-zero": ([leaf, inter], "ok", "zero"),
    "ok-gentime": ([leafg, interg], "ok", "gentime"),
    "ok-cross": ([leaf, cross, inter, unknown], "ok", "zero"),
    "bad-inter-zero": ([leafz, interz], "NoIssuer", "zero"),
    "bad-untrusted": ([leaf, cross, inter, unknown], "NoIssuer", "other"),
    "bad-untrusted-root": ([leaf, inter, root0], "NoIssuer", "other"),
}
with open(f"{HERE}/pack.pem", "w") as pack:
    for s, certs in stores.items():
        pack.write(f"# store: {s}\n" + "".join(text(c) for c in certs))
    for name, (certs, expect, store) in cases.items():
        pack.write(f"# case: {name}|test.example|{expect}|{NOW}|{store}\n" + "".join(text(c) for c in certs))
import shutil
shutil.rmtree(D)
print("ok", len(cases), "cases")
