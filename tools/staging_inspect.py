#!/usr/bin/env python3
"""Inspect the public staging site: TLS, HTTP and resources. Python stdlib and openssl only.

  python3 tools/staging_inspect.py selftest
  python3 tools/staging_inspect.py tls   [--host H]
  python3 tools/staging_inspect.py fetch [--host H] [--out FILE] [--max-requests N] [--max-bytes N]

`selftest` runs offline against tools/fixtures/staging/. `tls` and `fetch` contact the host.
Limits: one connection at a time, no crawling beyond the resources named by the
pages, a request cap, a body cap and a time cap. This is not a load tool.
Never send credentials. Never inspect an authenticated page.
"""
import argparse, gzip, hashlib, http.client, io, json, re, ssl, subprocess, sys, time, zlib
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import urljoin, urlsplit

HOST = "staging.app.8vox.ai"
PAGES = ["/", "/login"]
FIXTURES = Path(__file__).parent / "fixtures" / "staging"
KEEP = ("content-type", "content-length", "transfer-encoding", "content-encoding", "cache-control",
        "etag", "last-modified", "vary", "connection", "location", "server", "access-control-allow-origin",
        "accept-ranges", "keep-alive")


class Manifest(HTMLParser):
    """Collects every resource that a page names, in document order."""

    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.items, self.inline_scripts = [], 0

    def handle_starttag(self, tag, attrs):
        a = dict(attrs)
        if tag == "script":
            if "src" in a:
                self.items.append({"kind": "script", "url": a["src"], "async": "async" in a,
                                   "defer": "defer" in a, "nomodule": "nomodule" in a,
                                   "module": a.get("type") == "module"})
            else:
                self.inline_scripts += 1
        elif tag == "link" and "href" in a:
            rel = (a.get("rel") or "").lower().split()
            kind = ("style" if "stylesheet" in rel else "preload-" + a.get("as", "") if "preload" in rel
                    else "icon" if "icon" in rel else None)
            if kind:
                self.items.append({"kind": kind, "url": a["href"]})


def parse_manifest(html):
    p = Manifest()
    p.feed(html)
    p.close()
    return p.items, p.inline_scripts


def lazy_chunks(js):
    """Read the webpack runtime. Returns (public_path, lazy script paths, lazy css paths)."""
    public = re.search(r'\.p="([^"]*)"', js)
    out, css = [], re.findall(r'"(static/css/[0-9a-f]+\.css)"', js)
    u = re.search(r'\.u=function\(\w\)\{return"static/chunks/"\+\((.*?)\)\+"\."\+\(\{(.*?)\}\)\[\w\]\+"\.js"\}', js)
    if u:
        names = dict(re.findall(r'(\d+)===\w\?"([^"]+)"', u.group(1)))
        for cid, h in re.findall(r'(\d+):"([0-9a-f]+)"', u.group(2)):
            out.append("static/chunks/%s.%s.js" % (names.get(cid, cid), h))
    return public.group(1) if public else None, out, css


def decode(data, enc, max_bytes=8 << 20):
    """Decode supported encodings with a cap on the decoded body."""
    if enc in (None, "", "identity"):
        result = data
    elif enc in ("gzip", "x-gzip"):
        with gzip.GzipFile(fileobj=io.BytesIO(data)) as stream:
            result = stream.read(max_bytes + 1)
    elif enc == "deflate":
        def inflate(wbits):
            decoder = zlib.decompressobj(wbits)
            result = decoder.decompress(data, max_bytes + 1)
            if len(result) > max_bytes:
                raise ValueError("decoded body over cap")
            if not decoder.eof:
                raise zlib.error("incomplete deflate body")
            return result
        try:
            result = inflate(15)
        except zlib.error:
            result = inflate(-15)
    else:
        return None
    if len(result) > max_bytes:
        raise ValueError("decoded body over cap")
    return result


def sha(b):
    return hashlib.sha256(b).hexdigest()


class Client:
    def __init__(self, host, max_requests, max_bytes, deadline):
        self.host, self.left, self.max_bytes, self.deadline = host, max_requests, max_bytes, deadline
        self.conn, self.connects = None, 0

    def get(self, path, enc=None, method="GET", extra=None):
        if self.left <= 0 or time.monotonic() > self.deadline:
            raise SystemExit("limit reached: request or time cap")
        hdr = {"Host": self.host, "User-Agent": "bare-staging-inspect/1", "Accept": "*/*"}
        if enc is not None:
            hdr["Accept-Encoding"] = enc
        hdr.update(extra or {})
        for attempt in (0, 1):
            remaining = self.deadline - time.monotonic()
            if remaining <= 0 or self.left <= 0:
                raise SystemExit("limit reached: request or time cap")
            self.left -= 1
            timeout = min(20, remaining)
            if self.conn is None:
                self.conn = http.client.HTTPSConnection(self.host, 443, timeout=timeout, context=ssl.create_default_context())
                self.connects += 1
            elif self.conn.sock is not None:
                self.conn.sock.settimeout(timeout)
            try:
                self.conn.putrequest(method, path, skip_host=True, skip_accept_encoding=True)
                for key, value in hdr.items():
                    self.conn.putheader(key, value)
                self.conn.endheaders()
                r = self.conn.getresponse()
                body = r.read(self.max_bytes + 1)
                if len(body) > self.max_bytes:
                    raise SystemExit("body over cap: " + path)
                if r.will_close:
                    self.conn.close(); self.conn = None
                return r.status, {k.lower(): v for k, v in r.getheaders()}, body
            except (http.client.RemoteDisconnected, ConnectionError):
                self.conn.close()
                self.conn = None
                if attempt:
                    raise


def fetch(args):
    c = Client(args.host, args.max_requests, args.max_bytes, time.monotonic() + args.max_seconds)
    rep = {"host": args.host, "date": time.strftime("%Y-%m-%d", time.gmtime()), "pages": {}, "resources": {}}
    seen = {}
    for page in PAGES:
        st, h, body = c.get(page)
        items, inline = parse_manifest(body.decode("utf-8"))
        rep["pages"][page] = {"status": st, "bytes": len(body), "sha256": sha(body), "inline_scripts": inline,
                              "headers": {k: h[k] for k in KEEP if k in h}, "refs": items}
        for it in items:
            seen.setdefault(urljoin("https://%s%s" % (args.host, page), it["url"]), it["kind"])
    # Lazy chunks named by the webpack runtime, and the favicon.
    rt = next((u for u in seen if "/chunks/webpack-" in u), None)
    if rt:
        _, _, b = c.get(urlsplit(rt).path)
        pub, lz, lcss = lazy_chunks(b.decode("utf-8"))
        rep["webpack"] = {"public_path": pub, "lazy_scripts": lz, "lazy_css": lcss}
        for p in lz: seen.setdefault("https://%s%s%s" % (args.host, pub, p), "lazy-script")
        for p in lcss: seen.setdefault("https://%s%s%s" % (args.host, pub, p), "lazy-style")
    for url, kind in seen.items():
        u = urlsplit(url)
        if u.netloc != args.host:
            rep["resources"][url] = {"kind": kind, "skipped": "other origin"}
            continue
        path = u.path + ("?" + u.query if u.query else "")
        st, h, raw = c.get(path)
        e = {"kind": kind, "status": st, "bytes": len(raw), "sha256": sha(raw),
             "headers": {k: h[k] for k in KEEP if k in h}, "encodings": {}}
        for enc in ("gzip", "deflate", "br", "zstd", "gzip, deflate, br, zstd"):
            st2, h2, wire = c.get(path, enc)
            ce = h2.get("content-encoding")
            try:
                dec = decode(wire, ce, c.max_bytes)
                ok = None if dec is None else dec == raw
            except Exception as ex:
                ok = "decode error: %s" % ex
            e["encodings"][enc] = {"status": st2, "content_encoding": ce, "wire_bytes": len(wire),
                                   "chunked": h2.get("transfer-encoding"), "decodes_to_identity": ok}
        rep["resources"][url] = e
    rep["connections_opened"], rep["requests_left"] = c.connects, c.left
    json.dump(rep, open(args.out, "w") if args.out else sys.stdout, indent=1, sort_keys=True)


def tls(args):
    """Probe with openssl s_client. One short handshake per probe. Prints a JSON summary."""
    def probe(*opt):
        p = subprocess.run(["openssl", "s_client", "-connect", args.host + ":443", "-servername", args.host,
                            "-showcerts", *opt], input=b"", capture_output=True, timeout=20)
        return (p.stdout + p.stderr).decode("utf-8", "replace")
    def pick(pat, t):
        m = re.search(pat, t, re.M); return m.group(1).strip() if m else None
    res = {"host": args.host}
    t = probe("-tls1_3")
    res["default_tls13"] = {k: pick(p, t) for k, p in [("cipher", r"Cipher is (\S+)"), ("group", r"Negotiated TLS1.3 group: (\S+)"),
                            ("cert_verify_sig", r"Peer signature type: (\S+)"), ("verify", r"Verification: (.*)")]}
    res["chain"] = re.findall(r"^ *\d s:(.*)\n *i:(.*)\n *a:(.*)\n *v:(.*)$", t, re.M)
    res["tls12_cipher"] = pick(r"Cipher is (\S+)", probe("-tls1_2"))
    res["tls_min_versions"] = {v: bool(pick(r"Cipher is (\S+)", probe("-" + v)) not in (None, "(NONE)")) for v in ("tls1_1", "tls1")}
    res["suites"] = {s: pick(r"Cipher is (\S+)", probe("-tls1_3", "-ciphersuites", s)) not in (None, "(NONE)")
                     for s in ("TLS_AES_128_GCM_SHA256", "TLS_AES_256_GCM_SHA384", "TLS_CHACHA20_POLY1305_SHA256", "TLS_AES_128_CCM_SHA256")}
    res["groups"] = {}
    for g in ("X25519", "secp256r1", "secp384r1", "secp521r1", "X448", "ffdhe2048", "X25519MLKEM768"):
        t = probe("-tls1_3", "-groups", g)
        res["groups"][g] = pick(r"Negotiated TLS1.3 group: (\S+)", t) or pick(r"Peer Temp Key: (.*)", t)
    res["sigalgs"] = {s: pick(r"Peer signature type: (\S+)", probe("-tls1_3", "-sigalgs", s))
                      for s in ("ecdsa_secp256r1_sha256", "ecdsa_secp384r1_sha384", "rsa_pss_rsae_sha256", "ed25519")}
    res["alpn"] = {a: pick(r"ALPN protocol: (\S+)", probe("-tls1_3", "-alpn", a)) for a in ("h2", "http/1.1")}
    res["no_sni_leaf"] = pick(r"^ 0 s:(.*)$", subprocess.run(
        ["openssl", "s_client", "-connect", args.host + ":443", "-noservername", "-tls1_3", "-showcerts"],
        input=b"", capture_output=True, timeout=20).stdout.decode("utf-8", "replace"))
    json.dump(res, sys.stdout, indent=1)


def selftest():
    html = (FIXTURES / "login.html").read_text(encoding="utf-8")
    items, inline = parse_manifest(html)
    kinds = {}
    for i in items: kinds[i["kind"]] = kinds.get(i["kind"], 0) + 1
    assert kinds == {"preload-font": 4, "style": 2, "preload-script": 1, "script": 16, "icon": 1}, kinds
    assert inline == 6, inline
    assert items[4]["url"] == "/_next/static/css/f550c1958bfdbf7f.css"
    assert all(i["async"] for i in items if i["kind"] == "script" and not i["nomodule"])
    assert [i["url"] for i in items if i.get("nomodule")] == ["/_next/static/chunks/polyfills-42372ed130431b0a.js"]
    pub, lz, css = lazy_chunks((FIXTURES / "webpack-runtime.js").read_text(encoding="utf-8"))
    assert pub == "/_next/" and css == ["static/css/32df9a4d493f8b0f.css"], (pub, css)
    assert lz == ["static/chunks/1a258343.9598bf0e00059502.js", "static/chunks/3083.2e933fe3e6b915d3.js",
                  "static/chunks/3299.722ab16035874b78.js", "static/chunks/6231.594438b7aa1a0af8.js",
                  "static/chunks/9041.fc6c8a1f0df66e20.js"], lz
    man = json.loads((FIXTURES / "manifest-2026-11-04.json").read_text(encoding="utf-8"))
    assert sha(html.encode("utf-8")) == man["pages"]["/login"]["sha256"], "fixture differs from manifest"
    listed = {urljoin("https://x", i["url"]).split("x", 1)[1] for i in items}
    assert listed <= set(man["resources"]), sorted(listed - set(man["resources"]))
    body = b"hello " * 1000
    assert decode(gzip.compress(body), "gzip") == body
    assert decode(zlib.compress(body), "deflate") == body
    co = zlib.compressobj(wbits=-15); raw = co.compress(body) + co.flush()
    assert decode(raw, "deflate") == body and decode(b"x", "br") is None
    for enc, compressed in (("gzip", gzip.compress(body)), ("deflate", zlib.compress(body))):
        try:
            decode(compressed, enc, len(body) - 1)
        except ValueError:
            pass
        else:
            raise AssertionError("decoded size cap was ignored")
    from unittest.mock import Mock, patch
    conn = Mock()
    conn.getresponse.return_value = Mock(status=200, will_close=False)
    conn.getresponse.return_value.read.return_value = b"ok"
    conn.getresponse.return_value.getheaders.return_value = []
    with patch.object(http.client, "HTTPSConnection", return_value=conn):
        client = Client("example.invalid", 2, 100, time.monotonic() + 10)
        assert client.get("/")[2] == b"ok"
        assert client.connects == 1
        assert not any(call.args[0] == "Accept-Encoding" for call in conn.putheader.call_args_list)
        assert client.get("/")[2] == b"ok" and client.connects == 1
    conn.reset_mock()
    conn.getresponse.side_effect = http.client.RemoteDisconnected()
    with patch.object(http.client, "HTTPSConnection", return_value=conn):
        client = Client("example.invalid", 1, 100, time.monotonic() + 10)
        try:
            client.get("/")
        except SystemExit:
            pass
        else:
            raise AssertionError("retry exceeded the request cap")
        assert conn.putrequest.call_count == 1
    # Scale: the parser must stay linear. 100x the page, time bounded.
    t0 = time.perf_counter(); n = len(parse_manifest(html * 100)[0]); dt = time.perf_counter() - t0
    assert n == 100 * len(items) and dt < 5, (n, dt)
    print("selftest ok: %d refs, %d inline scripts, 100x parse %.3fs" % (len(items), inline, dt))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("cmd", choices=["selftest", "tls", "fetch"])
    ap.add_argument("--host", default=HOST)
    ap.add_argument("--out")
    ap.add_argument("--max-requests", type=int, default=400)
    ap.add_argument("--max-bytes", type=int, default=8 << 20)
    ap.add_argument("--max-seconds", type=int, default=300)
    a = ap.parse_args()
    {"selftest": lambda: selftest(), "tls": lambda: tls(a), "fetch": lambda: fetch(a)}[a.cmd]()


if __name__ == "__main__":
    main()
