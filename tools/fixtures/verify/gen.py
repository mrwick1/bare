#!/usr/bin/env python3 -I
"""Generate public synthetic chains for crates/tls/src/verify.rs tests (needs the openssl CLI).

Run: cd tools/fixtures/verify && python3 -I gen.py
Writes pack.pem (certificates only), cv.txt (CertificateVerify signatures) and oracle.txt
(what `openssl verify` says for each chain). Private keys live in a temp dir and are deleted.
"""
import hashlib, os, subprocess, tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
NOW = 1_800_000_000  # the time the tests use for synthetic chains
D = tempfile.mkdtemp()
serial = [100]


def run(*a, inp=None):
    return subprocess.run(a, check=True, capture_output=True, input=inp)


def key(name, kind):
    p = f"{D}/{name}.key"
    if kind == "rsa":
        run("openssl", "genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048", "-out", p)
    else:
        run("openssl", "genpkey", "-algorithm", "EC", "-pkeyopt", f"ec_paramgen_curve:{kind}", "-out", p)
    return p


def cert(name, cn, k, exts, issuer=None, days=36500, md="sha256"):
    csr, ext, pem = f"{D}/{name}.csr", f"{D}/{name}.ext", f"{D}/{name}.pem"
    run("openssl", "req", "-new", "-key", k, "-subj", f"/CN={cn}", "-out", csr)
    open(ext, "w").write("[v]\n" + "\n".join(exts) + "\n")
    serial[0] += 1
    a = ["openssl", "x509", "-req", "-in", csr, "-days", str(days), "-" + md, "-extfile", ext,
         "-extensions", "v", "-set_serial", str(serial[0]), "-out", pem]
    a += ["-signkey", k] if issuer is None else ["-CA", issuer[0], "-CAkey", issuer[1]]
    run(*a)
    return pem


ROOT = ["basicConstraints=critical,CA:TRUE", "keyUsage=critical,keyCertSign,cRLSign", "subjectKeyIdentifier=hash"]
INTER = ["basicConstraints=critical,CA:TRUE,pathlen:0", "keyUsage=critical,keyCertSign,cRLSign",
         "subjectKeyIdentifier=hash", "authorityKeyIdentifier=keyid"]
LEAF = ["basicConstraints=critical,CA:FALSE", "keyUsage=critical,digitalSignature", "extendedKeyUsage=serverAuth",
        "subjectAltName=DNS:test.example", "subjectKeyIdentifier=hash", "authorityKeyIdentifier=keyid"]


def sub(base, **over):
    """Replace the extension line that starts with a key; None removes it."""
    out = []
    for e in base:
        k = e.split("=")[0]
        if k in over:
            if over[k] is not None:
                out.append(f"{k}={over[k]}")
        else:
            out.append(e)
    for k, v in over.items():
        if v is not None and not any(e.startswith(k + "=") for e in base):
            out.append(f"{k}={v}")
    return out


root_k = key("root", "P-256")
root = cert("root", "Bare Test Root", root_k, ROOT)
R = (root, root_k)
inter_k = {"P-256": key("i256", "P-256"), "P-384": key("i384", "P-384")}


def ski(pem):
    t = run("openssl", "x509", "-in", pem, "-noout", "-ext", "subjectKeyIdentifier").stdout.decode()
    return t.strip().splitlines()[-1].strip()


cases = {}  # name -> (list of pem paths, host, expected-by-us text, now, store)
cvkeys = {}


def chain(name, expect, *, ik="P-256", lk="P-256", inter=INTER, leaf=LEAF, imd="sha256", lmd="sha256",
          idays=36500, host="test.example", now=NOW, store="root", extra=None, cn="test.example", sign_cv=False):
    ikey = inter_k[ik]
    i = cert(f"{name}-i", "Bare Test Inter", ikey, inter, issuer=R, days=idays, md=imd)
    certs, iss = [i], (i, ikey)
    if extra:  # one more intermediate below the first
        k2 = key(f"{name}-k2", "P-256")
        i2 = cert(f"{name}-i2", "Bare Test Inter 2", k2, extra, issuer=iss)
        certs, iss = [i2, i], (i2, k2)
    lkey = key(f"{name}-l", lk)
    l = cert(f"{name}-l", cn, lkey, leaf, issuer=iss, md=lmd)
    cases[name] = ([l] + certs, host, expect, now, store)
    if sign_cv:
        cvkeys[name] = lkey


chain("ok", "ok", sign_cv=True)
chain("ok-p384-inter", "ok", ik="P-384", lmd="sha384")
chain("ok-sha256-by-p384", "ok", ik="P-384", lmd="sha256")
chain("ok-two-inter", "ok", inter=sub(INTER, basicConstraints="critical,CA:TRUE,pathlen:1"), extra=INTER)
chain("ok-no-eku", "ok", leaf=sub(LEAF, extendedKeyUsage=None))
chain("ok-any-eku", "LeafEku", leaf=sub(LEAF, extendedKeyUsage="anyExtendedKeyUsage"))
chain("ok-ip", "ok", leaf=sub(LEAF, subjectAltName="IP:192.0.2.1"), host="192.0.2.1")
chain("ok-san-critical", "ok", leaf=sub(LEAF, subjectAltName="critical,DNS:test.example"))
chain("ok-leaf-p384", "ok", lk="P-384", sign_cv=True)
chain("inter-not-ca", "NotCa", inter=sub(INTER, basicConstraints="critical,CA:FALSE"))
chain("inter-no-bc", "NotCa", inter=sub(INTER, basicConstraints=None))
chain("inter-no-keycertsign", "CaKeyUsage", inter=sub(INTER, keyUsage="critical,digitalSignature"))
chain("inter-eku-client", "CaEku", inter=sub(INTER, extendedKeyUsage="clientAuth"))
chain("inter-pathlen0-two", "PathLen", extra=INTER)
chain("inter-nc", "UnsupportedConstraint", inter=sub(INTER, nameConstraints="permitted;DNS:test.example"))
chain("inter-nc-critical", "UnsupportedConstraint", inter=sub(INTER, nameConstraints="critical,permitted;DNS:test.example"))
chain("inter-policy-constraints", "UnsupportedConstraint", inter=sub(INTER, policyConstraints="requireExplicitPolicy:0"))
chain("inter-unknown-critical", "UnhandledCritical", inter=sub(INTER, **{"1.2.3.4": "critical,ASN1:NULL"}))
chain("inter-expired", "Expired", idays=1)
chain("leaf-eku-client", "LeafEku", leaf=sub(LEAF, extendedKeyUsage="clientAuth"))
chain("leaf-ku-keyenc", "LeafKeyUsage", leaf=sub(LEAF, keyUsage="critical,keyEncipherment"))
chain("leaf-ca-true", "LeafIsCa", leaf=sub(LEAF, basicConstraints="critical,CA:TRUE"))
chain("leaf-no-san", "HostNoMatch", leaf=sub(LEAF, subjectAltName=None))
chain("leaf-wildcard", "HostNoMatch", leaf=sub(LEAF, subjectAltName="DNS:*.example"), host="a.example")
chain("leaf-unknown-critical", "UnhandledCritical", leaf=sub(LEAF, **{"1.2.3.4": "critical,ASN1:NULL"}))
chain("leaf-san-critical-email", "BadSan", leaf=sub(LEAF, subjectAltName="critical,DNS:test.example,email:a@test.example"))
chain("leaf-wrong-host", "HostNoMatch", host="other.example")
chain("leaf-nc", "UnsupportedConstraint", leaf=sub(LEAF, nameConstraints="permitted;DNS:test.example"))

# Depth: N intermediates without pathLen. 4 is the limit of verify.rs.
for n in (4, 5):
    iss, certs = R, []
    for j in range(n):
        k = key(f"deep{n}-{j}", "P-256")
        c = cert(f"deep{n}-{j}", f"Bare Deep {j}", k, sub(INTER, basicConstraints="critical,CA:TRUE"), issuer=iss)
        certs.insert(0, c)
        iss = (c, k)
    lk = key(f"deep{n}-l", "P-256")
    cases[f"deep-{n}"] = ([cert(f"deep{n}-l", "test.example", lk, LEAF, issuer=iss)] + certs, "test.example",
                          "ok" if n == 4 else "NoIssuer", NOW, "root")

# RSA anchor: an EC intermediate signed by an RSA root. The link needs RSA verification.
rsa_k = key("rsaroot", "rsa")
rsa_root = cert("rsaroot", "Bare RSA Root", rsa_k, ROOT)
ik = inter_k["P-256"]
ri = cert("rsa-i", "Bare Test Inter", ik, INTER, issuer=(rsa_root, rsa_k))
lk = key("rsa-l", "P-256")
rl = cert("rsa-l", "test.example", lk, LEAF, issuer=(ri, ik))
cases["rsa-root"] = ([rl, ri], "test.example", "UnsupportedAlgorithm", NOW, "rsaroot")
# An RSA intermediate that signs an EC leaf with sha256WithRSA.
rik = key("rsai", "rsa")
rii = cert("rsai-i", "Bare Test Inter", rik, INTER, issuer=R)
rll = cert("rsai-l", "test.example", key("rsai-l", "P-256"), LEAF, issuer=(rii, rik), md="sha256")
cases["rsa-inter"] = ([rll, rii], "test.example", "UnsupportedAlgorithm", NOW, "root")

# Other stores. `other` has another name. `impostor-*` have the real name and another key.
stores = {"root": [root], "rsaroot": [rsa_root]}
x2 = f"{D}/x2.pem"  # the independently sourced public ISRG Root X2 (see ../x509/PROVENANCE.md)
run("openssl", "x509", "-inform", "DER", "-in", f"{HERE}/../x509/isrg-root-x2.der", "-out", x2)
stores["x2"] = [x2]
stores["other"] = [cert("other", "Bare Other Root", key("other", "P-256"), ROOT)]
imp = key("imp", "P-256")
stores["impostor-aki"] = [cert("imp-aki", "Bare Test Root", imp, ROOT)]
stores["impostor-ski"] = [cert("imp-ski", "Bare Test Root", imp, sub(ROOT, subjectKeyIdentifier=ski(root).replace(" ", "")))]
# Same chain, plus a self-signed copy of the root at the end (peer-supplied, never a trust anchor).
cases["ok-with-root"] = (cases["ok"][0] + [root], "test.example", "ok", NOW, "root")
# A self-signed, untrusted CA that signs the leaf. Repeated in the chain it makes a cycle shape.
sk = key("selfca", "P-256")
sca = cert("selfca", "Bare Self CA", sk, ROOT)
sl = cert("selfca-l", "test.example", key("selfca-l", "P-256"), LEAF, issuer=(sca, sk))
cases["self-ca"] = ([sl, sca, sca, sca], "test.example", "NoIssuer", NOW, "root")

# CertificateVerify signatures by the leaf key (RFC 8446 4.4.3, server context).
th = hashlib.sha256(b"bare transcript").digest()
th2 = hashlib.sha256(b"other transcript").digest()
SRV, CLI = b"TLS 1.3, server CertificateVerify", b"TLS 1.3, client CertificateVerify"
variants = {
    "ok": (SRV, th, b" " * 64 + SRV + b"\0" + th),
    "client-ctx": (SRV, th, b" " * 64 + CLI + b"\0" + th),
    "no-pad": (SRV, th, SRV + b"\0" + th),
    "no-zero": (SRV, th, b" " * 64 + SRV + th),
    "other-hash": (SRV, th, b" " * 64 + SRV + b"\0" + th2),
}


def sign(keyfile, md, msg):
    p = f"{D}/msg"
    open(p, "wb").write(msg)
    return run("openssl", "dgst", f"-{md}", "-sign", keyfile, p).stdout.hex()


with open(f"{HERE}/cv.txt", "w") as f:
    f.write("# case|variant|DER signature hex. Signed with openssl dgst over the content built in gen.py.\n")
    for name, kf in cvkeys.items():
        for v, (_, _, msg) in variants.items():
            f.write(f"{name}|{v}|{sign(kf, 'sha256', msg)}\n")
        f.write(f"{name}|sha384|{sign(kf, 'sha384', variants['ok'][2])}\n")


def pem_text(p):
    return open(p).read()


def openssl_verify(name, certs, host, now, store):
    sp = f"{D}/store-{name}.pem"
    open(sp, "w").write("".join(pem_text(c) for c in stores[store]))
    up = f"{D}/untrusted-{name}.pem"
    open(up, "w").write("".join(pem_text(c) for c in certs[1:]))
    flag = "-verify_ip" if host[0].isdigit() else "-verify_hostname"
    r = subprocess.run(["openssl", "verify", "-attime", str(now), "-no-CApath", "-no-CAfile", "-CAfile", sp,
                        "-untrusted", up, "-purpose", "sslserver", flag, host, certs[0]], capture_output=True)
    return "accept" if r.returncode == 0 else "reject"


cases["rsa-root"] = cases["rsa-root"][:4] + ("rsaroot",)
with open(f"{HERE}/pack.pem", "w") as pack, open(f"{HERE}/oracle.txt", "w") as orc:
    orc.write(f"# name|openssl verify result at now={NOW}. Written by gen.py.\n")
    for s, certs in stores.items():
        pack.write(f"# store: {s}\n" + "".join(pem_text(c) for c in certs))
    for name, (certs, host, expect, now, store) in cases.items():
        pack.write(f"# case: {name}|{host}|{expect}|{now}|{store}\n" + "".join(pem_text(c) for c in certs))
        orc.write(f"{name}|{openssl_verify(name, certs, host, now, store)}\n")
import shutil
shutil.rmtree(D)
print("ok", len(cases), "cases")
