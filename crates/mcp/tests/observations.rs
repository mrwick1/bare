//! Observation identity: a ref belongs to one document of one session, a snapshot names its document and revision, and a diff says
//! what left the outline. Offline and headless, against pages that a test serves on loopback.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    id: u64,
}

impl Server {
    fn start() -> Server {
        let mut child = Command::new(env!("CARGO_BIN_EXE_bare-mcp")).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let (stdin, stdout) = (child.stdin.take().unwrap(), BufReader::new(child.stdout.take().unwrap()));
        let mut s = Server { child, stdin, stdout, id: 0 };
        s.ask("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}));
        writeln!(s.stdin, "{}", json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).unwrap();
        s
    }

    fn ask(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        writeln!(self.stdin, "{}", json!({"jsonrpc": "2.0", "id": self.id, "method": method, "params": params})).unwrap();
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    /// Call a tool: the text of the reply and whether it was an error.
    fn tool(&mut self, name: &str, args: Value) -> (String, bool) {
        let r = self.ask("tools/call", json!({"name": name, "arguments": args}));
        (r["result"]["content"][0]["text"].as_str().unwrap().to_string(), r["result"]["isError"] == true)
    }

    fn open(&mut self, session: &str, url: &str) -> String {
        let (r, error) = self.tool("open", json!({"session": session, "url": url, "snapshot": true, "timeout_ms": 30000}));
        assert!(!error, "{r}");
        r
    }

    fn snap(&mut self, session: &str, extra: Value) -> String {
        let mut a = json!({"session": session});
        a.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        let (r, error) = self.tool("snapshot", a);
        assert!(!error, "{r}");
        r
    }

    /// An act with a diff: a scroll changes nothing, so the reply is the diff of the page.
    fn diff(&mut self, session: &str, extra: Value) -> String {
        let mut a = json!({"session": session, "op": "scroll"});
        a.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        let (r, error) = self.tool("act", a);
        assert!(!error, "{r}");
        r
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn pages(list: &'static [(&str, &str)]) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut c in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let (mut head, mut b) = (Vec::new(), [0u8; 1]);
                while !head.ends_with(b"\r\n\r\n") && c.read(&mut b).unwrap_or(0) == 1 {
                    head.push(b[0]);
                }
                let head = String::from_utf8_lossy(&head).to_string();
                let path = head.split(' ').nth(1).unwrap_or("/").split('#').next().unwrap().to_string();
                let (code, body) = match list.iter().find(|(p, _)| *p == path) {
                    Some((_, h)) => (200, *h),
                    None => (404, "not found"),
                };
                let _ = write!(c, "HTTP/1.1 {code} X\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            });
        }
    });
    format!("http://127.0.0.1:{port}")
}

/// The ref (`e7`) of the first line that holds `what`.
fn ref_of(snap: &str, what: &str) -> String {
    let line = snap.lines().find(|l| l.contains(what)).unwrap_or_else(|| panic!("no {what} in {snap}"));
    line[line.find("[e").unwrap() + 1..line.find(']').unwrap()].to_string()
}

/// The `dN` of the `doc: dN rev M` line, and `M`.
fn doc_rev(r: &str) -> (String, u32) {
    let l = r.lines().find(|l| l.starts_with("doc: ")).unwrap_or_else(|| panic!("no doc line in {r}"));
    let mut w = l.split_whitespace().skip(1);
    let d = w.next().unwrap().to_string();
    assert_eq!(w.next(), Some("rev"), "{l}");
    (d, w.next().unwrap().parse().unwrap())
}

const SHOP: &str = "<!doctype html><body><h1>Shop</h1><button id=a onclick=\"document.getElementById('victim').remove()\">Remove</button>
<button id=victim>Victim</button><button id=c onclick=\"document.body.append(Object.assign(document.createElement('button'), {textContent: 'Extra'}))\">Add</button>
<a href=\"/two\">Next</a></body>";
const TWO: &str = "<!doctype html><body><h1>Two</h1><button>First</button></body>";

#[test]
fn a_ref_of_another_session_is_refused_even_when_both_pages_have_the_same_first_button() {
    let base = pages(&[("/", SHOP)]);
    let mut s = Server::start();
    let a = s.open("a", &format!("{base}/"));
    let b = s.open("b", &format!("{base}/"));
    let (ra, rb) = (ref_of(&a, "button \"Remove\""), ref_of(&b, "button \"Remove\""));
    assert_ne!(ra, rb, "two sessions gave the same ref to different elements");
    let (r, error) = s.tool("act", json!({"session": "b", "op": "click", "ref": ra, "snapshot": false}));
    assert!(error && r.contains("not a ref of this page"), "{r}");
    // The victim of b is still there: the click did not land.
    assert!(s.snap("b", json!({})).contains("button \"Victim\""));
    // Its own ref works.
    let (r, error) = s.tool("act", json!({"session": "b", "op": "click", "ref": rb, "snapshot": false}));
    assert!(!error, "{r}");
}

#[test]
fn a_ref_of_an_older_document_is_never_given_again() {
    let base = pages(&[("/", SHOP), ("/two", TWO)]);
    let mut s = Server::start();
    let one = s.open("main", &format!("{base}/"));
    let old = ref_of(&one, "button \"Remove\"");
    let (r, _) = s.tool("act", json!({"op": "goto", "url": format!("{base}/two"), "snapshot": false}));
    assert!(r.starts_with("ok"), "{r}");
    let two = s.snap("main", json!({}));
    assert_ne!(ref_of(&two, "button \"First\""), old);
    assert_ne!(doc_rev(&one).0, doc_rev(&two).0, "a navigation is a new document");
    let (r, error) = s.tool("act", json!({"op": "click", "ref": old, "snapshot": false}));
    assert!(error && r.contains("older page"), "{r}");
    // Close and reopen: a new document, and the old ref stays refused.
    s.tool("close", json!({}));
    let again = s.open("main", &format!("{base}/two"));
    assert_ne!(doc_rev(&again).0, doc_rev(&two).0);
    assert_ne!(ref_of(&again, "button \"First\""), ref_of(&two, "button \"First\""));
}

#[test]
fn a_diff_that_only_removes_says_what_was_removed() {
    let base = pages(&[("/", SHOP)]);
    let mut s = Server::start();
    let snap = s.open("main", &format!("{base}/"));
    let victim = ref_of(&snap, "button \"Victim\"");
    let (r, error) = s.tool("act", json!({"op": "click", "ref": ref_of(&snap, "button \"Remove\"")}));
    assert!(!error, "{r}");
    assert!(!r.contains("no change"), "{r}");
    assert!(r.contains(&format!("removed: {victim}")), "{r}");
    // Said once: the next diff has nothing.
    assert!(s.diff("main", json!({})).contains("no change"));
}

#[test]
fn removal_and_addition_in_one_step_show_both() {
    let base = pages(&[("/", SHOP)]);
    let mut s = Server::start();
    let snap = s.open("main", &format!("{base}/"));
    let victim = ref_of(&snap, "button \"Victim\"");
    s.tool("act", json!({"op": "click", "ref": ref_of(&snap, "button \"Add\""), "snapshot": false}));
    s.tool("act", json!({"op": "click", "ref": ref_of(&snap, "button \"Remove\""), "snapshot": false}));
    let r = s.diff("main", json!({}));
    assert!(r.contains("button \"Extra\"") && r.contains(&format!("removed: {victim}")), "{r}");
}

#[test]
fn repeated_observations_advance_the_revision_and_keep_the_document() {
    let base = pages(&[("/", SHOP)]);
    let mut s = Server::start();
    let first = s.open("main", &format!("{base}/"));
    let (d, r1) = doc_rev(&first);
    let a = s.diff("main", json!({}));
    assert!(a.contains("no change"), "{a}");
    let b = s.diff("main", json!({}));
    assert_eq!((doc_rev(&a), doc_rev(&b)), ((d.clone(), r1 + 1), (d, r1 + 2)));
    assert!(b.len() < 120, "{b}");
}

#[test]
fn a_hash_navigation_keeps_the_document_and_the_refs() {
    let base = pages(&[("/", SHOP)]);
    let mut s = Server::start();
    let first = s.open("main", &format!("{base}/"));
    let (r, _) = s.tool("act", json!({"op": "goto", "url": format!("{base}/#x"), "snapshot": false}));
    assert!(r.starts_with("ok"), "{r}");
    let again = s.snap("main", json!({}));
    assert_eq!(doc_rev(&first).0, doc_rev(&again).0);
    assert_eq!(ref_of(&first, "button \"Victim\""), ref_of(&again, "button \"Victim\""));
}

#[test]
fn a_mode_switch_gives_a_labelled_full_outline_not_no_change() {
    let base = pages(&[("/", SHOP)]);
    let mut s = Server::start();
    s.open("main", &format!("{base}/"));
    let r = s.diff("main", json!({"mode": "all"}));
    assert!(!r.contains("no change") && r.contains("reset") && r.contains("mode") && r.contains("button \"Victim\""), "{r}");
    // The baseline is now the `all` outline: the next diff compares with it.
    assert!(s.diff("main", json!({"mode": "all"})).contains("no change"));
}

#[test]
fn a_scoped_snapshot_does_not_touch_the_diff_baseline() {
    let base = pages(&[("/", SHOP)]);
    let mut s = Server::start();
    let snap = s.open("main", &format!("{base}/"));
    let (_, rev) = doc_rev(&snap);
    s.tool("act", json!({"op": "click", "ref": ref_of(&snap, "button \"Remove\""), "snapshot": false}));
    let scoped = s.snap("main", json!({"root": ref_of(&snap, "heading \"Shop\"")}));
    assert!(scoped.contains("heading \"Shop\""), "{scoped}");
    let r = s.diff("main", json!({}));
    assert!(r.contains("removed: ") && doc_rev(&r).1 == rev + 1, "{r}");
}

const MANY: &str = "<!doctype html><body><button id=go onclick=\"for (const b of document.querySelectorAll('.n')) b.textContent = 'renamed ' + b.id + ' ' + 'x'.repeat(30)\">Go</button>
<button id=drop onclick=\"for (const b of document.querySelectorAll('.n')) b.remove()\">Drop</button>
<script>for (let i = 0; i < 40; i++) { const b = document.createElement('button'); b.className = 'n'; b.id = 'b' + i; b.textContent = 'item ' + i; document.body.append(b) }</script></body>";

#[test]
fn a_truncated_diff_does_not_mark_the_cut_rows_as_delivered() {
    let base = pages(&[("/", MANY)]);
    let mut s = Server::start();
    let snap = s.open("main", &format!("{base}/"));
    s.tool("act", json!({"op": "click", "ref": ref_of(&snap, "button \"Go\""), "snapshot": false}));
    let cut = s.diff("main", json!({"max_chars": 600}));
    assert!(cut.contains("lines cut"), "{cut}");
    // The rows that the cut hid are still changed for the next diff.
    let full = s.diff("main", json!({"max_chars": 20000}));
    for i in [10, 20, 30] {
        assert!(full.contains(&format!("renamed b{i} ")) || cut.contains(&format!("renamed b{i} ")), "b{i} lost: {cut}\n---\n{full}");
    }
    let shown_first = cut.matches("renamed b").count();
    assert!(full.matches("renamed b").count() >= 40 - shown_first, "{cut}\n---\n{full}");
}

#[test]
fn many_removals_are_bounded_and_none_is_lost() {
    let base = pages(&[("/", MANY)]);
    let mut s = Server::start();
    let snap = s.open("main", &format!("{base}/"));
    s.tool("act", json!({"op": "click", "ref": ref_of(&snap, "button \"Drop\""), "snapshot": false}));
    let first = s.diff("main", json!({}));
    let line = first.lines().find(|l| l.starts_with("removed: ")).unwrap_or_else(|| panic!("{first}"));
    assert!(first.contains("more removed"), "the list is bounded: {first}");
    let refs_in = |l: &str| l.trim_start_matches("removed: ").split(" …").next().unwrap().split(", ").count();
    let listed = refs_in(line);
    let second = s.diff("main", json!({}));
    let more: usize = second.lines().find(|l| l.starts_with("removed: ")).map_or(0, refs_in);
    assert_eq!(listed + more, 40, "{first}\n---\n{second}");
    assert!(s.diff("main", json!({})).contains("no change"));
}

#[test]
fn a_paged_snapshot_does_not_hide_a_change_in_the_rows_it_skipped() {
    let base = pages(&[("/", MANY)]);
    let mut s = Server::start();
    let snap = s.open("main", &format!("{base}/"));
    s.tool("act", json!({"op": "click", "ref": ref_of(&snap, "button \"Go\""), "snapshot": false}));
    // A page from the end: the changed rows are before the offset.
    let paged = s.snap("main", json!({"offset": 35}));
    assert!(!paged.contains("renamed b2 "), "{paged}");
    let r = s.diff("main", json!({"max_chars": 20000}));
    assert!(r.contains("renamed b2 ") && r.contains("renamed b30 "), "{r}");
}

#[test]
fn changed_text_beyond_the_excerpt_is_not_lost() {
    let base = pages(&[("/", "<!doctype html><body><p>Before</p></body>")]);
    let mut s = Server::start();
    s.open("main", &format!("{base}/"));
    s.tool("eval", json!({"js": "document.body.innerHTML = '<p>update0</p><p>update1</p><p>update2</p><p>update3</p><p>update4</p><p>update5</p><p>update6</p>'"}));
    let first = s.diff("main", json!({}));
    assert!(first.contains("more changed text lines"), "{first}");
    let next = s.diff("main", json!({}));
    assert!(next.contains("update5") && next.contains("update6"), "{first}\n---\n{next}");
    assert!(s.diff("main", json!({})).contains("no change"));
}

#[test]
fn a_partial_snapshot_does_not_discard_unreported_removals() {
    let base = pages(&[("/", MANY)]);
    let mut s = Server::start();
    let snap = s.open("main", &format!("{base}/"));
    s.tool("act", json!({"op": "click", "ref": ref_of(&snap, "button \"Drop\""), "snapshot": false}));
    s.snap("main", json!({"offset": 1}));
    let next = s.diff("main", json!({}));
    assert!(next.contains("removed:"), "{next}");
}

const LIST: &str = "<!doctype html><body><button id=b onclick=\"var u=document.createElement('ul');u.setAttribute('role','listbox');\
for(var i=0;i<700;i++){var l=document.createElement('li');l.setAttribute('role','option');l.textContent='Country number '+i;u.append(l)}\
document.body.append(u)\">Open</button><button id=z onclick=\"document.querySelector('ul').remove()\">Close</button></body>";

/// A dropdown that adds 700 options gives a short diff: the first options, then one line that counts the rest.
#[test]
fn a_long_run_of_new_siblings_is_summarized() {
    let base = pages(&[("/", LIST)]);
    let mut s = Server::start();
    s.open("m", &format!("{base}/"));
    let (r, error) = s.tool("act", json!({"session": "m", "op": "click", "loc": "css=\"#b\""}));
    assert!(!error, "{r}");
    eprintln!("open diff: {} chars\n{}", r.len(), &r[..r.len().min(3000)]);
    assert!(r.contains("Country number 0"), "{r}");
    assert!(!r.contains("Country number 300"), "{r}");
    assert!(r.contains("more option"), "{r}");
    assert!(r.len() < 3000, "{} chars", r.len());
    // The summarized options are known now: the next diff does not list them again.
    assert!(!r.contains("text: \"Country number"), "{r}");
    let (r, error) = s.tool("act", json!({"session": "m", "op": "scroll", "dy": 0}));
    assert!(!error, "{r}");
    assert!(!r.contains("Country number 400"), "{r}");
}

/// A page whose timer always runs is ready soon after its load: a navigation does not wait for two looks with no task.
#[test]
fn a_navigation_to_a_busy_page_ends_soon_after_load() {
    let base = pages(&[("/", "<!doctype html><body><p>Start</p></body>"), ("/busy", "<!doctype html><body><h1>Busy</h1><script>setInterval(() => document.title = Date.now(), 5)</script></body>")]);
    let mut s = Server::start();
    let (r, error) = s.tool("open", json!({"session": "m", "url": format!("{base}/"), "snapshot": false, "timeout_ms": 4000}));
    assert!(!error, "{r}");
    let t = std::time::Instant::now();
    let (r, error) = s.tool("step", json!({"session": "m", "steps": format!("goto {base}/busy")}));
    let ms = t.elapsed().as_millis();
    assert!(!error && !r.contains("not ready"), "{r}");
    assert!(ms < 1500, "goto took {ms} ms: {r}");
}
