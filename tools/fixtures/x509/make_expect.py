"""Write expect.txt: facts that `openssl x509 -text` prints for each fixture.
openssl is an oracle only. Run: python3 -I make_expect.py (inside this directory)."""
import calendar, glob, re, subprocess, time

KU = ["Digital Signature", "Non Repudiation", "Key Encipherment", "Data Encipherment",
      "Key Agreement", "Certificate Sign", "CRL Sign", "Encipher Only", "Decipher Only"]

def hx(s):
    return re.sub(r"[^0-9a-fA-F]", "", s).lower()

def unix(d):
    return calendar.timegm(time.strptime(d.strip(), "%b %d %H:%M:%S %Y GMT"))

def block(t, name):
    m = re.search(r"X509v3 %s:[ ]*(critical)?\n((?:\s{16}.*\n?)*)" % name, t)
    return (bool(m.group(1)), m.group(2).strip()) if m else None

out = []
for f in sorted(glob.glob("*.der")):
    t = subprocess.run(["openssl", "x509", "-inform", "DER", "-in", f, "-noout", "-text"],
                       capture_output=True, text=True, check=True).stdout
    k = f[:-4]
    def put(key, val): out.append("%s.%s=%s" % (k, key, val))
    put("serial", hx(re.search(r"Serial Number:\s*\n?\s*([0-9a-f:]+)", t).group(1)))
    put("not_before", unix(re.search(r"Not Before: (.*)", t).group(1)))
    put("not_after", unix(re.search(r"Not After : (.*)", t).group(1)))
    put("sig_alg", re.search(r"Signature Algorithm: (\S+)", t).group(1))
    put("sig", hx(t.rsplit("Signature Value:", 1)[1]))
    m = re.search(r"pub:\s*\n((?:\s+[0-9a-f:]+\n)+)", t)
    put("key", hx(m.group(1)) if m else "none")
    m = re.search(r"NIST CURVE: (\S+)", t)
    put("curve", m.group(1) if m else "none")
    b = block(t, "Basic Constraints")
    if b:
        put("bc_critical", int(b[0]))
        put("bc_ca", int("CA:TRUE" in b[1]))
        m = re.search(r"pathlen:(\d+)", b[1])
        put("bc_pathlen", m.group(1) if m else "none")
    else:
        put("bc_critical", "absent")
    b = block(t, "Key Usage")
    put("ku", sum(1 << i for i, n in enumerate(KU) if n in b[1]) if b else "absent")
    put("ku_critical", int(b[0]) if b else "absent")
    b = block(t, "Extended Key Usage")
    put("eku", ",".join(sorted(x.strip() for x in b[1].split(","))) if b else "absent")
    b = block(t, "Subject Alternative Name")
    put("san", ",".join(x.strip() for x in b[1].split(",")) if b else "absent")
    b = block(t, "Subject Key Identifier")
    put("ski", hx(b[1]) if b else "absent")
    b = block(t, "Authority Key Identifier")
    put("aki", hx(b[1].replace("keyid:", "")) if b else "absent")
    n = re.search(r"X509v3 extensions:\n((?:\s{12}.*\n)+)", t)
    put("ext_count", len(re.findall(r"^\s{12}(?:X509v3|Authority|CT |Netscape)", n.group(1), re.M)) if n else 0)
open("expect.txt", "w").write("\n".join(out) + "\n")
