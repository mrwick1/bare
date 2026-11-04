#!/usr/bin/env python3
"""Make random ECDSA cases checked by OpenSSL (through `cryptography`) for tools/crypto/ecc_verify_load.rs.

  python3 -I data/crypto/ecdsa/oracle_gen.py <cases per curve> > cases.txt

Line: `<curve> <uncompressed key hex> <digest hex> <r||s hex> <ok|bad>`. The expected value
comes from OpenSSL, not from this repository. For each curve: valid signatures with 32-byte
and 48-byte digests (so the leftmost-bits rule is checked too), high-s twins, and copies with
one flipped bit in the key, digest or signature.
"""
import os, random, sys
from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.asymmetric import ec, utils

count = int(sys.argv[1])
rng = random.Random(1)  # fixed seed: same cases every run (keys and nonces come from OpenSSL's RNG)
CURVES = [("p256", ec.SECP256R1, 32, 0xffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551),
          ("p384", ec.SECP384R1, 48, 0xffffffffffffffffffffffffffffffffffffffffffffffffc7634d81f4372ddf581a0db248b0a77aecec196accc52973)]
H = {32: hashes.SHA256(), 48: hashes.SHA384()}

def oracle(cls, key, digest, rs, sz):
    try:
        pub = ec.EllipticCurvePublicKey.from_encoded_point(cls(), key)
        pub.verify(utils.encode_dss_signature(int.from_bytes(rs[:sz], "big"), int.from_bytes(rs[sz:], "big")),
                   digest, ec.ECDSA(utils.Prehashed(H[len(digest)])))
        return "ok"
    except (InvalidSignature, ValueError):
        return "bad"

def flip(b):
    b = bytearray(b)
    i = rng.randrange(len(b) * 8)
    b[i // 8] ^= 1 << (i % 8)
    return bytes(b)

for name, cls, sz, order in CURVES:
    priv = None
    for i in range(count):
        if i % 50 == 0:
            priv = ec.generate_private_key(cls())
            nums = priv.public_key().public_numbers()
            key = b"\x04" + nums.x.to_bytes(sz, "big") + nums.y.to_bytes(sz, "big")
        dlen = rng.choice([32, 48])
        digest = os.urandom(dlen)
        r, s = utils.decode_dss_signature(priv.sign(digest, ec.ECDSA(utils.Prehashed(H[dlen]))))
        rs = r.to_bytes(sz, "big") + s.to_bytes(sz, "big")
        twin = r.to_bytes(sz, "big") + (order - s).to_bytes(sz, "big")
        cases = [(key, digest, rs), (key, digest, twin),
                 (key, flip(digest), rs), (key, digest, flip(rs)), (flip(key), digest, rs)]
        for k, d, sig in cases:
            print(name, k.hex(), d.hex(), sig.hex(), oracle(cls, k, d, sig, sz))
