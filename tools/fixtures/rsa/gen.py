#!/usr/bin/env python3 -I
"""Generate RSA test vectors for crates/tls/src/rsa.rs (needs the openssl CLI).

Run: cd tools/fixtures/rsa && python3 -I gen.py
Writes vectors.txt. Private keys live in a temp dir and in memory, and are deleted.

Keys: openssl generates the standard sizes (2048, 3072, 4096; e = 65537, 3, 17).
Python generates three odd sizes (2049, 2050, 2052 bits) and one with e = 2^32 - 1, because
openssl cannot make them. These test the `emLen = k - 1` edge of RSASSA-PSS.

Cases:
- `ok`: valid signatures. For the openssl keys they come from `openssl dgst -sign`.
  The script checks that openssl itself accepts them and that Python gives the same
  PKCS#1 v1.5 bytes.
- `bad`: signatures over an encoded message that breaks one rule. Python builds the
  encoded message and applies the private key, so the RSA step is valid and only the
  padding is wrong.

Format: `key NAME DER_HEX` (RSAPublicKey), then `case NAME KEY MODE DIGEST_HEX SIG_HEX EXPECT`
with MODE in pkcs1-sha256, pkcs1-sha384, pss-sha256, pss-sha384.
"""
import hashlib, math, os, random, subprocess, tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
D = tempfile.mkdtemp()
rnd = random.Random(20261109)  # fixed seed: the odd-size keys and bad cases are repeatable


def run(*a, inp=None):
    return subprocess.run(a, check=True, capture_output=True, input=inp)


def der_len(n):
    if n < 128:
        return bytes([n])
    b = n.to_bytes((n.bit_length() + 7) // 8, "big")
    return bytes([0x80 | len(b)]) + b


def tlv(tag, c):
    return bytes([tag]) + der_len(len(c)) + c


def der_int(v):
    b = v.to_bytes(max(1, (v.bit_length() + 7) // 8), "big")
    if b[0] & 0x80:
        b = b"\0" + b
    return tlv(2, b)


def pub_der(n, e):
    return tlv(0x30, der_int(n) + der_int(e))


# ---------------------------------------------------------------------------- keys

def is_prime(n):
    if n < 4:
        return n in (2, 3)
    if n % 2 == 0:
        return False
    for p in (3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37):
        if n % p == 0:
            return n == p
    d, s = n - 1, 0
    while d % 2 == 0:
        d //= 2
        s += 1
    for _ in range(24):
        a = rnd.randrange(2, n - 1)
        x = pow(a, d, n)
        if x in (1, n - 1):
            continue
        for _ in range(s - 1):
            x = x * x % n
            if x == n - 1:
                break
        else:
            return False
    return True


def prime(bits, e):
    while True:
        p = rnd.getrandbits(bits) | (3 << (bits - 2)) | 1  # top two bits set: the product is exactly the size
        if is_prime(p) and math.gcd(e, p - 1) == 1:
            return p


def py_key(bits, e):
    while True:
        p, q = prime(bits // 2, e), prime(bits - bits // 2, e)
        n = p * q
        if p != q and n.bit_length() == bits:
            try:
                return n, e, pow(e, -1, (p - 1) * (q - 1))
            except ValueError:
                pass


def ossl_key(name, bits, e):
    path = f"{D}/{name}.pem"
    run("openssl", "genpkey", "-algorithm", "RSA", "-pkeyopt", f"rsa_keygen_bits:{bits}",
        "-pkeyopt", f"rsa_keygen_pubexp:{e}", "-out", path)
    txt = run("openssl", "rsa", "-in", path, "-noout", "-text").stdout.decode()

    def field(label):
        out, on = "", False
        for line in txt.splitlines():
            if line.startswith(label + ":"):
                on = True
                continue
            if on:
                if not line.startswith("    "):
                    break
                out += line.strip().replace(":", "")
        return int(out, 16)

    n, d = field("modulus"), field("privateExponent")
    assert n.bit_length() == bits, (name, n.bit_length())
    return path, n, e, d


# ---------------------------------------------------------------------------- encodings

HASH = {"sha256": hashlib.sha256, "sha384": hashlib.sha384}
DIGEST_INFO = {
    "sha256": bytes.fromhex("3031300d060960864801650304020105000420"),
    "sha384": bytes.fromhex("3041300d060960864801650304020205000430"),
}


def mgf1(seed, n, h):
    out = b""
    c = 0
    while len(out) < n:
        out += h(seed + c.to_bytes(4, "big")).digest()
        c += 1
    return out[:n]


def pkcs1_em(k, di, digest, ps_len=None, bt=1, first=0, trailer=b""):
    t = di + digest + trailer
    ps_len = k - 3 - len(t) if ps_len is None else ps_len
    em = bytes([first, bt]) + b"\xff" * ps_len + b"\0" + t
    return em + b"\0" * (k - len(em)) if len(em) < k else em


def pss_em(mod_bits, digest, hn, salt=None, trailer=0xBC, mgf=None, top_bit=False, ps=b"", sep=b"\x01",
           m_prefix=b"\0" * 8, flip_h=False):
    """EMSA-PSS-ENCODE (RFC 8017 9.1.1) with one rule broken on request. Returns the k-byte block."""
    h = HASH[hn]
    hl = h().digest_size
    mgf_h = HASH[mgf or hn]
    salt = rnd.randbytes(hl) if salt is None else salt
    em_bits = mod_bits - 1
    em_len = (em_bits + 7) // 8
    hh = h(m_prefix + digest + salt).digest()
    if flip_h:
        hh = bytes([hh[0] ^ 1]) + hh[1:]
    db = ps.ljust(em_len - hl - len(salt) - 1 - len(sep) - 1 + 1, b"\0")[: em_len - hl - len(salt) - 1 - len(sep)]
    db = db + sep + salt
    assert len(db) == em_len - hl - 1, (len(db), em_len, hl)
    mask = mgf1(hh, len(db), mgf_h)
    masked = bytearray(a ^ b for a, b in zip(db, mask))
    masked[0] &= 0xFF >> (8 * em_len - em_bits)
    if top_bit:
        masked[0] |= 0x80
    em = bytes(masked) + hh + bytes([trailer])
    k = (mod_bits + 7) // 8
    return b"\0" * (k - em_len) + em


def sign_raw(key, em):
    n, e, d = key
    k = (n.bit_length() + 7) // 8
    s = pow(int.from_bytes(em, "big"), d, n)
    assert pow(s, e, n) == int.from_bytes(em, "big") % n
    return s.to_bytes(k, "big")


# ---------------------------------------------------------------------------- main

keys = {}   # name -> (n, e, d, pem path or None)
for name, bits, e in [("rsa2048", 2048, 65537), ("rsa3072", 3072, 65537), ("rsa4096", 4096, 65537),
                      ("rsa2048-e3", 2048, 3), ("rsa2048-e17", 2048, 17)]:
    path, n, e, d = ossl_key(name, bits, e)
    keys[name] = (n, e, d, path)
for name, bits, e in [("rsa2049", 2049, 65537), ("rsa2050", 2050, 65537), ("rsa2052", 2052, 65537),
                      ("rsa2048-emax", 2048, 4294967295)]:
    n, e, d = py_key(bits, e)
    keys[name] = (n, e, d, None)

lines = []
lines.append("# RSA vectors for crates/tls/src/rsa.rs. Generated by gen.py. Do not edit.")
for name, (n, e, d, _) in keys.items():
    lines.append(f"key {name} {pub_der(n, e).hex()}")

count = 0


def case(name, key, mode, digest, sig, expect):
    global count
    count += 1
    lines.append(f"case {name} {key} {mode} {digest.hex()} {sig.hex()} {expect}")


for kname, (n, e, d, path) in keys.items():
    key = (n, e, d)
    bits = n.bit_length()
    k = (bits + 7) // 8
    for hn in ("sha256", "sha384"):
        msg = f"rsa vector {kname} {hn}".encode()
        digest = HASH[hn](msg).digest()
        # ---- valid signatures
        if path:
            mf = f"{D}/msg"
            open(mf, "wb").write(msg)
            sig = run("openssl", "dgst", f"-{hn}", "-sign", path, mf).stdout
            assert sig == sign_raw(key, pkcs1_em(k, DIGEST_INFO[hn], digest)), "pkcs1 bytes differ"
            case("ok", kname, f"pkcs1-{hn}", digest, sig, "ok")
            sig = run("openssl", "dgst", f"-{hn}", "-sign", path, "-sigopt", "rsa_padding_mode:pss",
                      "-sigopt", "rsa_pss_saltlen:digest", "-sigopt", f"rsa_mgf1_md:{hn}", mf).stdout
            case("ok", kname, f"pss-{hn}", digest, sig, "ok")
            pub = f"{D}/{kname}.pub"
            run("openssl", "pkey", "-in", path, "-pubout", "-out", pub)
            open(f"{D}/sig", "wb").write(sig)
            run("openssl", "dgst", f"-{hn}", "-verify", pub, "-sigopt", "rsa_padding_mode:pss",
                "-sigopt", "rsa_pss_saltlen:digest", "-sigopt", f"rsa_mgf1_md:{hn}", "-signature", f"{D}/sig", mf)
        else:
            case("ok", kname, f"pkcs1-{hn}", digest, sign_raw(key, pkcs1_em(k, DIGEST_INFO[hn], digest)), "ok")
            case("ok", kname, f"pss-{hn}", digest, sign_raw(key, pss_em(bits, digest, hn)), "ok")
        # ---- crafted bad padding: only on three keys, to keep the file small
        if kname not in ("rsa2048", "rsa2049", "rsa2052"):
            continue
        di = DIGEST_INFO[hn]
        pk = f"pkcs1-{hn}"
        h = len(digest)
        bad = lambda nm, mode, em: case(nm, kname, mode, digest, sign_raw(key, em), "bad")
        bad("pkcs1-ps7-garbage-after", pk, pkcs1_em(k, di, digest, ps_len=7, trailer=b"\x41" * (k - 3 - 7 - len(di) - h)))
        bad("pkcs1-garbage-after", pk, pkcs1_em(k, di, digest, ps_len=k - 3 - len(di) - h - 4, trailer=b"\x41" * 4))
        no_null = bytes([0x30, di[1] - 2, 0x30, 0x0b]) + di[4:15] + di[17:]
        bad("pkcs1-no-null", pk, pkcs1_em(k, no_null, digest))
        bad("pkcs1-bt2", pk, pkcs1_em(k, di, digest, bt=2))
        bad("pkcs1-first-byte-1", pk, pkcs1_em(k, di, digest, first=1))
        ps = k - 3 - len(di) - h
        em = b"\0\1" + b"\xff" * 10 + b"\0" + b"\xff" * (ps - 11) + b"\0" + di + digest
        bad("pkcs1-zero-in-ps", pk, em)
        bad("pkcs1-sha1-info", pk, pkcs1_em(k, bytes.fromhex("3021300906052b0e03021a05000414"), digest[:20]))
        bad("pkcs1-digest-short", pk, pkcs1_em(k, di, digest[:-1]))
        # PSS
        pm = f"pss-{hn}"
        bad("pss-salt-0", pm, pss_em(bits, digest, hn, salt=b""))
        bad("pss-salt-20", pm, pss_em(bits, digest, hn, salt=rnd.randbytes(20)))
        bad("pss-salt-hlen-plus-1", pm, pss_em(bits, digest, hn, salt=rnd.randbytes(h + 1)))
        bad("pss-salt-hlen-minus-1", pm, pss_em(bits, digest, hn, salt=rnd.randbytes(h - 1)))
        bad("pss-trailer-cc", pm, pss_em(bits, digest, hn, trailer=0xCC))
        bad("pss-trailer-bd", pm, pss_em(bits, digest, hn, trailer=0xBD))
        if bits % 8 != 1:  # with 8k+1 bits there is no spare top bit in maskedDB
            bad("pss-top-bit", pm, pss_em(bits, digest, hn, top_bit=True))
        bad("pss-ps-nonzero", pm, pss_em(bits, digest, hn, ps=b"\x02"))
        bad("pss-no-separator", pm, pss_em(bits, digest, hn, sep=b"\x00"))
        bad("pss-wrong-h", pm, pss_em(bits, digest, hn, flip_h=True))
        bad("pss-mgf-other-hash", pm, pss_em(bits, digest, hn, mgf="sha384" if hn == "sha256" else "sha256"))
        bad("pss-no-zero-prefix", pm, pss_em(bits, digest, hn, m_prefix=b""))
        bad("pss-prefix-7", pm, pss_em(bits, digest, hn, m_prefix=b"\0" * 7))
        # the same block as PKCS#1 v1.5 and the reverse
        bad("pss-block-as-pkcs1", pk, pss_em(bits, digest, hn))
        bad("pkcs1-block-as-pss", pm, pkcs1_em(k, di, digest))

open(os.path.join(HERE, "vectors.txt"), "w").write("\n".join(lines) + "\n")
print(len(keys), "keys,", count, "cases")
