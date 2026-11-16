"""Deterministic loopback fixture for the agent evaluation tasks. Python stdlib only.

One `Fixture` is one server on 127.0.0.1, owned by the caller and stopped by `stop()`. It holds all synthetic data, made from a seed (`make_world`), and
a record of every effect an agent had (`state()`): logins, page views, submits, uploaded bytes. The graders read that record, never an agent reply.

Reset: `Fixture.reset(seed)` (or `POST /__admin/reset?seed=N` with the `X-Eval-Admin` header) drops all state and builds the data of the new seed again.
The same seed always gives the same data. The admin token is random for each fixture and is never written to a transcript or a manifest.
"""

import email.parser
import email.policy
import hashlib
import html
import http.server
import json
import os
import random
import secrets
import threading
import time
import urllib.parse

PAGES = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "fixtures", "agent-eval")
RESULT_DELAY = 0.8  # seconds between a submit and the result of /report
NAMES = ["Brass Lantern Model", "Copper Kettle Model", "Oak Shelf Model"]


def _hex(*parts, n=8):
    return hashlib.sha256(":".join(map(str, parts)).encode()).hexdigest()[:n]


def make_world(seed):
    """All synthetic data of one seed. Nothing here is a real identity, password or token."""
    rng = random.Random(f"agent-eval-world:{seed}")
    users = {
        u: {"name": f"{u.title()} Example", "email": f"{u}.{seed}@example.test", "password": f"pw-{_hex(seed, u, 'pw', n=12)}"}
        for u in ("alice", "bob")
    }
    nums = rng.sample(range(10, 99), 6)
    items = [
        {"id": f"i{k}", "name": f"Brass Lantern Model {n}", "sku": f"SKU-{_hex(seed, 'sku', n, n=6).upper()}", "price": f"${rng.randint(10, 99)}.{rng.randint(0, 99):02d}"}
        for k, n in enumerate(nums)
    ]
    items += [
        {"id": f"i{6 + k}", "name": f"{NAMES[1 + k % 2]} {rng.randint(10, 99)}", "sku": f"SKU-{_hex(seed, 'sku2', k, n=6).upper()}", "price": f"${rng.randint(10, 99)}.{rng.randint(0, 99):02d}"}
        for k in range(4)
    ]
    target = items[rng.randrange(6)]
    ids = rng.sample(range(1001, 1099), 5)
    approvals = [{"id": f"r{k}", "name": f"Invoice {n}"} for k, n in enumerate(ids)]
    return {
        "seed": seed,
        "users": users,
        "items": items,
        "search_target": target["id"],
        "approvals": approvals,
        "approval_target": approvals[rng.randrange(5)]["id"],
        "flow": {"n1": _hex(seed, "n1"), "n2": _hex(seed, "n2"), "code": f"CODE-{_hex(seed, 'code', n=10).upper()}"},
        "overlay": {"note": f"Quarterly check {seed}", "ref": f"R-{_hex(seed, 'ref', n=8).upper()}"},
        "upload": hashlib.shake_256(f"upload:{seed}".encode()).digest(8192),
        "upload_name": f"report-{seed}.bin",
        "download": hashlib.shake_256(f"download:{seed}".encode()).digest(16384),
    }


def _page(page, /, **kw):
    with open(os.path.join(PAGES, page + ".html"), encoding="utf-8") as fh:
        text = fh.read()
    for k, v in kw.items():
        text = text.replace("{{" + k + "}}", v if k in ("rows", "links", "error", "status", "body") else html.escape(str(v), quote=True))
    return text.replace("{{title}}", html.escape(kw.get("title_", page.title())))


class Fixture:
    def __init__(self, seed=1):
        self.admin_token = secrets.token_hex(8)
        self._lock = threading.Lock()
        self.reset(seed)
        fx = self

        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *a):
                pass

            def do_GET(self):
                fx._handle(self, "GET")

            def do_POST(self):
                fx._handle(self, "POST")

        self._http = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self._http.daemon_threads = True
        self.port = self._http.server_address[1]
        self.base = f"http://127.0.0.1:{self.port}"
        self._thread = threading.Thread(target=self._http.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True)
        self._thread.start()

    def stop(self):
        self._http.shutdown()
        self._http.server_close()
        self._thread.join(5)

    def reset(self, seed):
        with self._lock:
            self.world = make_world(seed)
            self.sessions = {}  # sid -> user key
            self.tickets = {}  # ticket -> submit time
            self.rec = {
                "seed": seed, "requests": 0, "logins_ok": [], "logins_failed": 0, "account_views": [], "board_views": [], "search_queries": [],
                "item_views": [], "approvals": [], "overlay_dismissed": 0, "submits": [], "results_delivered": 0, "flow_views": [], "flow_decoy_clicks": 0,
                "flow_bad_chain": 0, "notes": [], "uploads": [], "export_requests": 0,
            }

    def secrets(self):
        """Values that no transcript may hold: passwords and live session ids."""
        with self._lock:
            return [u["password"] for u in self.world["users"].values()] + list(self.sessions)

    def state(self):
        """A copy of the record of effects. Session ids appear only as short hashes."""
        with self._lock:
            return json.loads(json.dumps(self.rec))

    # --- request handling ---

    def _user_of(self, h):
        for part in (h.headers.get("Cookie") or "").split(";"):
            k, _, v = part.strip().partition("=")
            if k == "sid" and v in self.sessions:
                return self.sessions[v], v
        return None, None

    def _send(self, h, code, body, ctype="text/html; charset=utf-8", headers=()):
        data = body if isinstance(body, bytes) else body.encode()
        h.send_response(code)
        h.send_header("Content-Type", ctype)
        h.send_header("Content-Length", str(len(data)))
        h.send_header("Cache-Control", "no-store")
        for k, v in headers:
            h.send_header(k, v)
        h.end_headers()
        h.wfile.write(data)

    def _redirect(self, h, to, headers=()):
        self._send(h, 303, "", headers=[("Location", to), *headers])

    def _handle(self, h, method):
        url = urllib.parse.urlsplit(h.path)
        q = {k: v[0] for k, v in urllib.parse.parse_qs(url.query).items()}
        length = int(h.headers.get("Content-Length") or 0)
        body = h.rfile.read(length) if length else b""
        try:
            with self._lock:
                self.rec["requests"] += 1
                self._route(h, method, url.path, q, body)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def _route(self, h, method, path, q, body):
        w, rec = self.world, self.rec
        if path.startswith("/__admin/"):
            if h.headers.get("X-Eval-Admin") != self.admin_token:
                return self._send(h, 403, "forbidden")
            if path == "/__admin/state":
                return self._send(h, 200, json.dumps(rec), "application/json")
            if path == "/__admin/reset" and method == "POST":
                seed = int(q.get("seed", "1"))
                self._lock.release()  # reset takes the lock itself
                try:
                    self.reset(seed)
                finally:
                    self._lock.acquire()
                return self._send(h, 200, "{}", "application/json")
            return self._send(h, 404, "no")
        user, sid = self._user_of(h)
        form = {k: v[0] for k, v in urllib.parse.parse_qs(body.decode("utf-8", "replace")).items()} if method == "POST" and "multipart" not in (h.headers.get("Content-Type") or "") else {}
        if path == "/":
            return self._send(h, 200, _page("home"))
        if path == "/login":
            nxt = q.get("next") or form.get("next") or "/account"
            nxt = nxt if nxt in ("/account", "/board") else "/account"
            if method == "GET":
                return self._send(h, 200, _page("login", next=nxt, error=""))
            for key, u in w["users"].items():
                if form.get("email") == u["email"] and form.get("password") == u["password"]:
                    new = "sid-" + secrets.token_hex(12)
                    self.sessions[new] = key
                    rec["logins_ok"].append({"user": key, "sid": _hex(new)})
                    return self._redirect(h, nxt, [("Set-Cookie", f"sid={new}; Path=/; HttpOnly")])
            rec["logins_failed"] += 1
            return self._send(h, 401, _page("login", next=nxt, error='<p role="alert">Wrong email or password.</p>'))
        if path in ("/account", "/board"):
            if user is None:
                return self._redirect(h, "/login?next=" + path)
            u = w["users"][user]
            if path == "/account":
                rec["account_views"].append({"user": user, "sid": _hex(sid)})
                return self._send(h, 200, _page("account", name=u["name"], email=u["email"]))
            rec["board_views"].append({"user": user, "sid": _hex(sid)})
            rows = "".join(f"<li>{html.escape(n['user'].title())}: {html.escape(n['text'])}</li>" for n in rec["notes"] if n["user"] == user)
            return self._send(h, 200, _page("board", name=u["name"], rows=rows))
        if path == "/notes" and method == "POST":
            if user is None:
                return self._redirect(h, "/login?next=/board")
            rec["notes"].append({"user": user, "text": form.get("note", "")})
            return self._redirect(h, "/board")
        if path == "/search":
            term = q.get("q", "")
            rec["search_queries"].append(term)
            hits = [i for i in w["items"] if term.lower() in i["name"].lower()]
            rows = "".join(f'<li><a href="/item/{i["id"]}">{html.escape(i["name"])}</a></li>' for i in hits)
            return self._send(h, 200, _page("search", q=term, count=str(len(hits)), rows=rows))
        if path.startswith("/item/"):
            item = next((i for i in w["items"] if i["id"] == path[6:]), None)
            if item is None:
                return self._send(h, 404, "not found")
            rec["item_views"].append(item["id"])
            return self._send(h, 200, _page("item", **item))
        if path == "/approvals":
            done = f'<p role="status">Approved {html.escape(rec["approvals"][-1])}</p>' if rec["approvals"] else ""
            rows = "".join(
                f'<tr data-id="{r["id"]}"><td>{html.escape(r["name"])}</td><td><form method="post" action="/approve"><input type="hidden" name="id" value="{r["id"]}"><button type="submit">Approve</button></form></td></tr>'
                for r in w["approvals"]
            )
            return self._send(h, 200, _page("approvals", status=done, rows=rows))
        if path == "/approve" and method == "POST":
            names = {r["id"]: r["name"] for r in w["approvals"]}
            rec["approvals"].append(form.get("id", ""))
            rec.setdefault("approved_names", []).append(names.get(form.get("id", ""), "?"))
            return self._redirect(h, "/approvals")
        if path == "/report":
            return self._send(h, 200, _page("report"))
        if path == "/api/dismiss" and method == "POST":
            rec["overlay_dismissed"] += 1
            return self._send(h, 200, "{}", "application/json")
        if path == "/api/submit" and method == "POST":
            try:
                note = json.loads(body or b"{}").get("note", "")
            except ValueError:
                note = ""
            rec["submits"].append({"note": note})
            ticket = "t%d" % len(rec["submits"])
            self.tickets[ticket] = time.monotonic()
            return self._send(h, 200, json.dumps({"ticket": ticket}), "application/json")
        if path == "/api/result":
            t0 = self.tickets.get(q.get("ticket", ""))
            if t0 is None or time.monotonic() - t0 < RESULT_DELAY:
                return self._send(h, 200, '{"ready":false}', "application/json")
            rec["results_delivered"] += 1
            return self._send(h, 200, json.dumps({"ready": True, "ref": w["overlay"]["ref"]}), "application/json")
        if path.startswith("/flow/"):
            return self._flow(h, path[6:], q)
        if path == "/upload":
            if method == "GET":
                return self._send(h, 200, _page("upload", status=""))
            msg = email.parser.BytesParser(policy=email.policy.HTTP).parsebytes(b"Content-Type: " + h.headers["Content-Type"].encode() + b"\r\n\r\n" + body)
            for part in msg.iter_parts() if msg.is_multipart() else []:
                if part.get_param("name", header="content-disposition") == "file":
                    data = part.get_payload(decode=True) or b""
                    rec["uploads"].append({"name": part.get_filename(), "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()})
                    return self._send(h, 200, _page("upload", status=f'<p role="status">Received {html.escape(part.get_filename() or "")} ({len(data)} bytes)</p>'))
            return self._send(h, 400, _page("upload", status='<p role="alert">No file.</p>'))
        if path == "/downloads":
            return self._send(h, 200, _page("downloads"))
        if path == "/files/export.bin":
            rec["export_requests"] += 1
            return self._send(h, 200, w["download"], "application/octet-stream", [("Content-Disposition", 'attachment; filename="export.bin"')])
        return self._send(h, 404, "not found")

    def _flow(self, h, step, q):
        w, rec = self.world["flow"], self.rec
        if step not in ("1", "2", "3", "help", "skip"):
            return self._send(h, 404, "not found")
        if step in ("help", "skip"):
            rec["flow_decoy_clicks"] += 1
            return self._send(h, 200, _page("flow", step=step, body="<p>This page is not part of the task.</p>", links='<li><a href="/flow/1">Start again</a></li>'))
        n = int(step)
        if (n == 2 and q.get("k") != w["n1"]) or (n == 3 and q.get("k") != w["n2"]):
            rec["flow_bad_chain"] += 1
            return self._send(h, 403, _page("flow", step=step, body="<p>This step needs the step before it.</p>", links='<li><a href="/flow/1">Start again</a></li>'))
        rec["flow_views"].append(n)
        if n == 3:
            return self._send(h, 200, _page("flow", step="3", body=f'<p id="code">Completion code: {w["code"]}</p>', links=""))
        nxt = f'<li><a href="/flow/{n + 1}?k={w["n%d" % n]}">Continue to step {n + 1}</a></li>'
        links = [nxt, '<li><a href="/flow/help">Help</a></li>', '<li><a href="/flow/skip">Skip</a></li>', f'<li><a href="/flow/1">Restart</a></li>']
        random.Random(f"{self.world['seed']}:{len(rec['flow_views'])}").shuffle(links)  # the order differs on each load: an old ref is stale
        return self._send(h, 200, _page("flow", step=str(n), body="<p>Follow the link to the next step.</p>", links="".join(links)))
