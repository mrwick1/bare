"""The task corpus: prompt, independent grader and scripted sequence of each task. Python stdlib only.

A grader reads only the fixture record (`Fixture.state()`), the files the browser saved, and the agent's final answer text for an extraction. It never reads a
tool receipt such as "ok", and never the agent's claim that an action worked. A grader gives checks: dicts {name, ok, detail}. An action that must happen once
is a check too, so a repeated submit fails a task whose last page says success.

A scripted sequence is a fixed list of tool calls. It runs a task through real stdio MCP to test the protocol and the grader. It is not an agent.
"""

import hashlib
import re
from dataclasses import dataclass
from typing import Callable, Optional

VERSION = 1  # the version of the task corpus: a change of a prompt, a fixture or a grader raises it


@dataclass
class Ctx:
    world: dict
    base: str
    state: dict
    answer: Optional[str] = None  # the final answer text of the agent, when the task needs one
    downloads_dir: Optional[str] = None
    inputs_dir: Optional[str] = None
    upload_path: Optional[str] = None


@dataclass
class Task:
    id: str
    title: str
    prompt: Callable
    grade: Callable
    script: Callable
    needs_answer: bool = False
    sessions: tuple = ("main",)
    duplicates: Callable = lambda ctx: {}


def check(name, ok, detail=""):
    return {"name": name, "ok": bool(ok), "detail": detail}


def sha(b):
    return hashlib.sha256(b).hexdigest()


def _excess(items):
    return max(len(items) - 1, 0)


# --- 1. login and session restore ---


def _login_prompt(c):
    u = c.world["users"]["alice"]
    return (
        f"Open {c.base}/account. Sign in with email {u['email']} and password {u['password']}. Then end the page and come back to {c.base}/account "
        "from the saved session, without signing in a second time, and confirm that the page shows your name."
    )


def _login_grade(c):
    st = c.state
    ok_logins = st["logins_ok"]
    sids = {}
    for v in st["account_views"]:
        sids[v["sid"]] = sids.get(v["sid"], 0) + 1
    return [
        check("signed_in_once", len(ok_logins) == 1 and ok_logins[0]["user"] == "alice", f"successful logins: {[x['user'] for x in ok_logins]}"),
        check("account_loaded_twice_with_one_session", any(n >= 2 for n in sids.values()), f"authenticated /account loads per session: {sorted(sids.values())}"),
    ]


# --- 2. search and extraction ---


def _search_target(c):
    return next(i for i in c.world["items"] if i["id"] == c.world["search_target"])


def _search_prompt(c):
    t = _search_target(c)
    return f"Open {c.base}/search. Search for 'Lantern'. Open the item named exactly '{t['name']}' and report its SKU and its price."


def _search_grade(c):
    t, ans = _search_target(c), c.answer or ""
    decoys = [i for i in c.world["items"] if i["id"] != t["id"]]
    return [
        check("answer_has_sku", t["sku"] in ans, f"expected {t['sku']}"),
        check("answer_has_price", t["price"] in ans, f"expected {t['price']}"),
        check("answer_has_no_other_sku", not any(d["sku"] in ans for d in decoys), "a sku of another item is in the answer"),
        check("item_page_was_opened", t["id"] in c.state["item_views"], f"item pages opened: {c.state['item_views']}"),
    ]


# --- 3. duplicate button disambiguation ---


def _dup_name(c):
    return next(r["name"] for r in c.world["approvals"] if r["id"] == c.world["approval_target"])


def _dup_prompt(c):
    return f"Open {c.base}/approvals. Every row has an Approve button. Approve only the request '{_dup_name(c)}'."


def _dup_grade(c):
    got, want = c.state["approvals"], c.world["approval_target"]
    return [
        check("target_approved", want in got, f"approved ids: {got}"),
        check("nothing_else_approved", all(x == want for x in got), f"approved ids: {got}"),
        check("approved_once", got.count(want) == 1, f"approvals of the target: {got.count(want)}"),
    ]


# --- 4. overlay, then a delayed result ---


def _overlay_prompt(c):
    return (
        f"Open {c.base}/report. Close the cookie notice, then send a report with the note '{c.world['overlay']['note']}' exactly once. "
        "The page shows a reference after a short delay. Wait for it."
    )


def _overlay_grade(c):
    st, want = c.state, c.world["overlay"]["note"]
    return [
        check("overlay_dismissed_by_the_page", st["overlay_dismissed"] >= 1, f"dismissals: {st['overlay_dismissed']}"),
        check("report_sent_once", len(st["submits"]) == 1, f"submits: {len(st['submits'])}"),
        check("note_is_exact", [s["note"] for s in st["submits"]] == [want], f"notes: {[s['note'] for s in st['submits']]}"),
        check("result_was_delivered_to_the_page", st["results_delivered"] >= 1, f"result polls answered: {st['results_delivered']}"),
    ]


# --- 5. navigation and stale refs ---


def _flow_prompt(c):
    return (
        f"Open {c.base}/flow/1. Follow the guided flow through all 3 steps with the 'Continue' links only. The order of the links changes on every page load. "
        "Report the completion code on step 3."
    )


def _flow_grade(c):
    st = c.state
    return [
        check("steps_visited_in_order", _subseq([1, 2, 3], st["flow_views"]), f"steps loaded: {st['flow_views']}"),
        check("answer_has_the_code", c.world["flow"]["code"] in (c.answer or ""), f"expected {c.world['flow']['code']}"),
        check("no_step_skipped", st["flow_bad_chain"] == 0, f"steps asked without the step before: {st['flow_bad_chain']}"),
    ]


def _subseq(want, got):
    it = iter(got)
    return all(x in it for x in want)


# --- 6. two independent sessions ---


def _two_prompt(c):
    u = c.world["users"]
    s = c.world["seed"]
    return (
        f"Use two sessions. In session 'alice' open {c.base}/board, sign in with email {u['alice']['email']} and password {u['alice']['password']}, and post the note "
        f"'Alice note {s}'. In session 'bob' open {c.base}/board, sign in with email {u['bob']['email']} and password {u['bob']['password']}, and post the note "
        f"'Bob note {s}'. Post each note once. Both sessions stay open."
    )


def _two_grade(c):
    st, s = c.state, c.world["seed"]
    got = sorted((n["user"], n["text"]) for n in st["notes"])
    want = sorted([("alice", f"Alice note {s}"), ("bob", f"Bob note {s}")])
    users = sorted(x["user"] for x in st["logins_ok"])
    return [
        check("each_user_signed_in_once", users == ["alice", "bob"], f"successful logins: {users}"),
        check("notes_posted_by_the_right_users_once", got == want, f"notes: {got}"),
    ]


# --- 7. upload ---


def _upload_prompt(c):
    return f"Open {c.base}/upload and upload the file {c.upload_path} once."


def _upload_grade(c):
    ups, w = c.state["uploads"], c.world
    return [
        check("uploaded_once", len(ups) == 1, f"uploads: {len(ups)}"),
        check("bytes_are_exact", len(ups) >= 1 and ups[-1]["sha256"] == sha(w["upload"]) and ups[-1]["bytes"] == len(w["upload"]), f"received {[(u['bytes'], u['sha256'][:12]) for u in ups]}, expected {(len(w['upload']), sha(w['upload'])[:12])}"),
        check("file_name_is_kept", len(ups) >= 1 and ups[-1]["name"] == w["upload_name"], f"names: {[u['name'] for u in ups]}"),
    ]


# --- 8. download ---


def _download_prompt(c):
    return f"Open {c.base}/downloads and download the export file. The browser must save it in {c.downloads_dir}."


def _download_grade(c):
    import os

    files = sorted(os.listdir(c.downloads_dir)) if c.downloads_dir and os.path.isdir(c.downloads_dir) else []
    want = sha(c.world["download"])
    got = []
    for name in files:
        path = os.path.join(c.downloads_dir, name)
        if os.path.isfile(path):
            with open(path, "rb") as fh:
                got.append(sha(fh.read()))
    return [
        check("one_file_saved", len(got) == 1, f"files in the downloads folder: {files}"),
        check("saved_bytes_are_exact", got == [want], f"sha256 of saved files {[g[:12] for g in got]}, expected {want[:12]}"),
        check("export_was_requested", c.state["export_requests"] >= 1, f"requests of the file: {c.state['export_requests']}"),
    ]


# --- scripted sequences (the protocol smoke runs) ---


def _script_login(c, call):
    u = c.world["users"]["alice"]
    login = f'type label="Email" "{u["email"]}"\ntype label="Password" "{u["password"]}"\nclick role=button name="Sign in"'
    args = dict(name="eval", url=f"{c.base}/account", login=login, check=f'expect text~"Signed in as {u["name"]}" visible', timeout_ms=10000)
    call("ensure_session", **args)
    call("ensure_session", **args)
    call("close")


def _script_search(c, call):
    t = _search_target(c)
    call("open", url=f"{c.base}/search?q=Lantern", timeout_ms=10000)
    call("act", op="click", loc=f'role=link name="{t["name"]}"', snapshot=False)
    text, _ = call("read")
    sku, price = re.search(r"SKU-[0-9A-F]{6}", text), re.search(r"\$\d+\.\d\d", text)
    c.answer = f"SKU {sku.group(0) if sku else '?'}, price {price.group(0) if price else '?'}"
    call("close")


def _script_dup(c, call):
    call("open", url=f"{c.base}/approvals", timeout_ms=10000)
    call("act", op="click", loc=f'css="tr[data-id={c.world["approval_target"]}] button"', snapshot=False)
    call("close")


def _script_overlay(c, call):
    call("open", url=f"{c.base}/report", timeout_ms=10000)
    call("act", op="click", loc='role=button name="Accept cookies"', snapshot=False)
    call("act", op="type", loc='label="Note"', text=c.world["overlay"]["note"], snapshot=False)
    call("act", op="click", loc='role=button name="Send report"', snapshot=False)
    call("act", op="wait", loc='text~"Report accepted"', text="visible", snapshot=False)
    call("close")


def _script_flow(c, call):
    call("open", url=f"{c.base}/flow/1", timeout_ms=10000)
    snap, _ = call("snapshot")
    old_ref = re.search(r'\[(e\d+)\] link "Continue to step 2"', snap)
    call("act", op="click", ref=old_ref.group(1), snapshot=False) if old_ref else None
    # Recovery: a ref of the old page must be refused, so the next calls use a locator and a new snapshot.
    call("act", op="hover", ref=old_ref.group(1), snapshot=False) if old_ref else None
    call("snapshot")
    call("act", op="click", loc='role=link name="Continue to step 3"', snapshot=False)
    text, _ = call("read")
    m = re.search(r"CODE-[0-9A-F]+", text)
    c.answer = f"The completion code is {m.group(0) if m else '?'}"
    call("close")


def _script_two(c, call):
    u, s = c.world["users"], c.world["seed"]
    for who in ("alice", "bob"):
        call("open", session=who, url=f"{c.base}/board", timeout_ms=10000)
        call("act", session=who, op="type", loc='label="Email"', text=u[who]["email"], snapshot=False)
        call("act", session=who, op="type", loc='label="Password"', text=u[who]["password"], snapshot=False)
        call("act", session=who, op="click", loc='role=button name="Sign in"', snapshot=False)
        call("act", session=who, op="type", loc='label="Note"', text=f"{who.title()} note {s}", snapshot=False)
        call("act", session=who, op="click", loc='role=button name="Post note"', snapshot=False)
    call("close", session="alice")
    call("close", session="bob")


def _script_upload(c, call):
    call("open", url=f"{c.base}/upload", timeout_ms=10000)
    call("act", op="upload", loc='css="input[type=file]"', files=[c.upload_path], snapshot=False)
    call("act", op="click", loc='role=button name="Upload"', snapshot=False)
    call("close")


def _script_download(c, call):
    call("open", url=f"{c.base}/downloads", timeout_ms=10000, downloads_dir=c.downloads_dir)
    call("act", op="click", loc='role=link name="Download export"', snapshot=False)
    call("downloads")
    call("close")


TASKS = {
    t.id: t
    for t in [
        Task("login_restore", "Sign in, then restore the session without a second login", _login_prompt, _login_grade, _script_login, duplicates=lambda c: {"login": _excess(c.state["logins_ok"])}),
        Task("search_extract", "Search, open one item, report its SKU and price", _search_prompt, _search_grade, _script_search, needs_answer=True),
        Task("duplicate_button", "Click the right one of five identical Approve buttons", _dup_prompt, _dup_grade, _script_dup, duplicates=lambda c: {"approve": max(len(c.state["approvals"]) - 1, 0)}),
        Task("overlay_delayed", "Close an overlay, submit once, wait for the delayed result", _overlay_prompt, _overlay_grade, _script_overlay, duplicates=lambda c: {"submit": _excess(c.state["submits"])}),
        Task("nav_stale_ref", "Follow a 3 step flow whose links change order on each load", _flow_prompt, _flow_grade, _script_flow, needs_answer=True),
        Task("two_sessions", "Use two sessions as two users, one note each", _two_prompt, _two_grade, _script_two, sessions=("alice", "bob"), duplicates=lambda c: {"note": max(len(c.state["notes"]) - 2, 0)}),
        Task("upload", "Upload a file of known bytes once", _upload_prompt, _upload_grade, _script_upload, duplicates=lambda c: {"upload": _excess(c.state["uploads"])}),
        Task("download", "Download a file and keep its exact bytes", _download_prompt, _download_grade, _script_download),
    ]
}
