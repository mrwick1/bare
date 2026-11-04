#!/usr/bin/env python3
"""Build vectors.txt from public ECDSA test-vector files. Run:

  python3 -I data/crypto/ecdsa/make_vectors.py <dir with NIST SigVer.rsp and PKV.rsp> \
      <dir with the two Wycheproof p1363 JSON files> > data/crypto/ecdsa/vectors.txt

hashlib only hashes the test messages (digests are the input of `ecc::verify`).
The hand cases need the `cryptography` package (OpenSSL) as an independent signer.
"""
import hashlib, json, re, sys

nist, wyc = sys.argv[1], sys.argv[2]
SIZE = {"P-256": 32, "P-384": 48}
NAME = {"P-256": "p256", "P-384": "p384"}
HASH = {"SHA-256": hashlib.sha256, "SHA-384": hashlib.sha384}

def pad(h, n):
    h = h.rjust(2 * n, "0")
    return h if len(h) % 2 == 0 else "0" + h  # an over-long coordinate stays over-long

print("# ECDSA verification vectors. Source and licenses: data/crypto/ecdsa/README.md")
print("# key <name> <curve> <uncompressed SEC1 hex>")
print("# sig <key> <digest hex> <r||s hex> <ok|bad> <label>")
print("# pkv <curve> <key hex> <ok|bad> <label>")

def blocks(path):
    head, out = None, []
    for chunk in re.split(r"\n\s*\n", open(path).read()):
        d = {}
        for line in chunk.splitlines():
            if line.startswith("["):
                head = line.strip("[]")
            elif "=" in line:
                k, v = line.split("=", 1)
                d[k.strip()] = v.strip()
        if d:
            out.append((head, d))
    return out

# NIST CAVS 11.0 SigVer (FIPS 186-3 ECDSA), the selected curve/hash pairs.
n = 0
for head, d in blocks(nist + "/SigVer.rsp"):
    curve, sha = head.split(",")
    if curve not in SIZE or sha not in HASH or "R" not in d:
        continue
    sz = SIZE[curve]
    key = "04" + pad(d["Qx"], sz) + pad(d["Qy"], sz)
    dig = HASH[sha](bytes.fromhex(d["Msg"])).hexdigest()
    ok = "ok" if d["Result"].startswith("P") else "bad"
    label = "nist-sigver-%s-%s-%s" % (NAME[curve], sha.lower(), d["Result"].split(None, 1)[-1].strip("() ").replace(" ", "_") if ok == "bad" else "pass")
    print("key nist%d %s %s" % (n, NAME[curve], key))
    print("sig nist%d %s %s %s %s" % (n, dig, pad(d["R"], sz) + pad(d["S"], sz), ok, label))
    n += 1

# NIST CAVS 11.0 PKV (public key validation).
for head, d in blocks(nist + "/PKV.rsp"):
    if head not in SIZE or "Qx" not in d:
        continue
    sz = SIZE[head]
    key = "04" + pad(d["Qx"], sz) + pad(d["Qy"], sz)
    ok = "ok" if d["Result"].startswith("P") else "bad"
    reason = d["Result"].split("-", 1)[-1].strip("() ").replace(" ", "_") if ok == "bad" else "valid"
    print("pkv %s %s %s nist-pkv-%s" % (NAME[head], key, ok, reason))

# Project Wycheproof (Apache-2.0) ECDSA P1363 files.
for f, curve, sha in [("ecdsa_secp256r1_sha256_p1363_test.json", "P-256", "SHA-256"),
                      ("ecdsa_secp384r1_sha384_p1363_test.json", "P-384", "SHA-384")]:
    data = json.load(open(wyc + "/" + f))
    for gi, g in enumerate(data["testGroups"]):
        name = "w%s_%d" % (NAME[curve], gi)
        print("key %s %s %s" % (name, NAME[curve], g["publicKey"]["uncompressed"]))
        for t in g["tests"]:
            dig = HASH[sha](bytes.fromhex(t["msg"])).hexdigest()
            assert t["result"] in ("valid", "invalid")
            print("sig %s %s %s %s wycheproof-%d-%s" % (
                name, dig, t["sig"] or "-", "ok" if t["result"] == "valid" else "bad",
                t["tcId"], "+".join(t["flags"]) or "none"))

# Hand cases signed by OpenSSL (through `cryptography`): extreme keys d = 1, 2, n-1.
from cryptography.hazmat.primitives.asymmetric import ec, utils
from cryptography.hazmat.primitives import hashes
for curve, cls, h, hc in [("P-256", ec.SECP256R1, hashlib.sha256, hashes.SHA256()),
                          ("P-384", ec.SECP384R1, hashlib.sha384, hashes.SHA384())]:
    sz = SIZE[curve]
    order = int({"P-256": "ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551",
                 "P-384": "ffffffffffffffffffffffffffffffffffffffffffffffffc7634d81f4372ddf581a0db248b0a77aecec196accc52973"}[curve], 16)
    for dname, d in [("d1", 1), ("d2", 2), ("dn1", order - 1)]:
        priv = ec.derive_private_key(d, cls())
        nums = priv.public_key().public_numbers()
        key = "04%0*x%0*x" % (2 * sz, nums.x, 2 * sz, nums.y)
        name = "hand_%s_%s" % (NAME[curve], dname)
        print("key %s %s %s" % (name, NAME[curve], key))
        for i in range(3):
            dig = h(b"bare hand case %d" % i).digest()
            r, s = utils.decode_dss_signature(priv.sign(dig, ec.ECDSA(utils.Prehashed(hc))))
            sig = "%0*x%0*x" % (2 * sz, r, 2 * sz, s)
            hs = "%0*x" % (2 * sz, order - s)  # the high-s twin is valid ECDSA too
            print("sig %s %s %s ok hand-openssl-%s-%d" % (name, dig.hex(), sig, dname, i))
            print("sig %s %s %s ok hand-openssl-%s-%d-high-s" % (name, dig.hex(), sig[:2 * sz] + hs, dname, i))
            print("sig %s %s %s bad hand-openssl-%s-%d-other-digest" % (name, h(b"other").hexdigest(), sig, dname, i))
