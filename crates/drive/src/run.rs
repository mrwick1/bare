//! Run the steps. Each step is tried, and tried again until it passes or its time limit ends: between tries the driver runs the tasks of
//! the page that are due (`Page::tick`) and sleeps until the next one, so a step waits for a condition and never for a fixed time. When
//! no task is queued, nothing can change the page (the network calls run inside the tasks), so the step fails at once.
//!
//! Failure kinds: `not_found`, `duplicate` (a locator must match one element), `hidden`, `disabled`, `covered`, `detached` (it matched,
//! then left the document), `not_editable`, `no_focus`, `expect` (a check was false), `page` (a load failed, the page stopped or has no
//! layout). `duplicate`, `not_editable`, `no_focus` and a failed load fail at once; the others wait.
//!
//! Secrets: the values of `--secrets` and each text typed into a password field are replaced by `[secret]` in all the output (also in
//! lower case and URL-encoded). A capture is withheld when a secret is in the text or in a field of the page (a password field shows
//! dots, so it does not count).

use crate::locate::{self, Locator};
use crate::steps::{Action, Check, State, Step};
use bare::control::{Handle, Log, Refusal, Remote};
use bare::page::Page;
use bare_paint::canvas::Canvas;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub struct Options {
    pub size: (u32, u32),
    pub timeout: Duration,
    /// Where `result.json` and the captures go.
    pub out: Option<PathBuf>,
    /// Show the page in a Wayland window (`Remote`), else run it headless on this thread.
    pub window: bool,
    /// Print one line for each step on stderr.
    pub progress: bool,
}

/// Replaces each secret in a text by `[secret]`.
#[derive(Clone, Default)]
pub struct Redactor {
    /// ASCII lower case, the longest first.
    words: Vec<String>,
}

impl Redactor {
    pub fn add(&mut self, secret: &str) {
        if secret.is_empty() {
            return;
        }
        let form = percent(secret, true);
        for w in [secret.to_ascii_lowercase(), percent(secret, false).to_ascii_lowercase(), form.to_ascii_lowercase()] {
            if !self.words.contains(&w) {
                self.words.push(w);
            }
        }
        self.words.sort_by_key(|w| std::cmp::Reverse(w.len()));
    }

    pub fn clean(&self, s: &str) -> String {
        let mut out = s.to_string();
        for w in &self.words {
            let mut at = 0;
            while let Some(i) = lower_find(&out[at..], w) {
                out.replace_range(at + i..at + i + w.len(), "[secret]");
                at += i + "[secret]".len();
            }
        }
        out
    }

    pub fn hits(&self, s: &str) -> bool {
        self.words.iter().any(|w| lower_find(s, w).is_some())
    }
}

/// The first place of `w` (lower case) in `s`, in any case. Only ASCII letters change case, so the places of the bytes stay.
fn lower_find(s: &str, w: &str) -> Option<usize> {
    let (s, w) = (s.as_bytes(), w.as_bytes());
    (0..(s.len() + 1).saturating_sub(w.len())).find(|&i| s[i..i + w.len()].eq_ignore_ascii_case(w))
}

/// `encodeURIComponent`, or the form encoding (a space is `+`).
fn percent(s: &str, form: bool) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')' => out.push(b as char),
            b' ' if form => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The page: none yet, one on this thread, or one in a window on its own thread.
enum Host {
    None,
    Local(Box<Page>),
    Window(Remote),
}

impl Host {
    fn open(&mut self, url: &str, size: (u32, u32), window: bool) -> Result<(), String> {
        let new = move |url: &str| {
            let mut p = Page::open_with(url, size, None)?;
            p.quiet = true;
            Ok::<_, String>(p)
        };
        match self {
            Host::Window(r) => {
                let url = url.to_string();
                r.call(move |p| new(&url).map(|n| *p = n))?
            }
            _ if window => {
                *self = Host::Window(Remote::open(url, size)?);
                Ok(())
            }
            _ => {
                *self = Host::Local(Box::new(new(url)?));
                Ok(())
            }
        }
    }

    fn run<R: Send + 'static>(&mut self, f: impl FnOnce(&mut Page) -> R + Send + 'static) -> Result<R, String> {
        match self {
            Host::None => Err("no page: the first step must be open".into()),
            Host::Local(p) => Ok(f(p)),
            Host::Window(r) => r.call(f),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Fail {
    pub kind: &'static str,
    pub message: String,
}

fn fail(kind: &'static str, message: impl Into<String>) -> Fail {
    Fail { kind, message: message.into() }
}

enum Try {
    Done(Option<String>),
    Again(Fail),
    Stop(Fail),
}

/// What the driver keeps between steps (it goes to the page thread and back).
#[derive(Default)]
struct Memo {
    /// The element that each locator found last: a later miss of a node that left the document is `detached`.
    seen: HashMap<String, Handle>,
    /// The texts typed into password fields since the last step.
    typed: Vec<String>,
}

pub struct StepOut {
    pub line: usize,
    pub text: String,
    pub ok: bool,
    pub ms: f64,
    pub busy_ms: f64,
    pub waited_ms: f64,
    pub tries: u32,
    pub note: Option<String>,
}

pub struct Report {
    pub ok: bool,
    pub steps: Vec<StepOut>,
    pub failure: Option<(usize, Fail, Option<String>)>,
    pub logs: Vec<(usize, Log)>,
    pub net: Vec<(usize, bare_net::trace::Rec)>,
    pub ms: f64,
    pub window: bool,
}

/// Run the steps in order until one fails. `red` gets the secrets that the steps type into password fields.
pub fn run(steps: &[Step], opt: &Options, red: &mut Redactor) -> Report {
    bare_net::trace::enable();
    let _ = bare_net::trace::take();
    let start = Instant::now();
    let mut host = Host::None;
    let mut memo = Memo::default();
    let mut timeout = opt.timeout;
    let mut rep = Report { ok: true, steps: Vec::new(), failure: None, logs: Vec::new(), net: Vec::new(), ms: 0.0, window: opt.window };
    for step in steps {
        let t0 = Instant::now();
        let deadline = t0 + timeout;
        let (mut tries, mut busy, mut waited) = (0u32, Duration::ZERO, Duration::ZERO);
        let result: Result<Option<String>, Fail> = loop {
            tries += 1;
            let t = Instant::now();
            let (tried, wait) = match &step.action {
                Action::Timeout(ms) => {
                    timeout = Duration::from_millis(*ms);
                    break Ok(None);
                }
                Action::Open(url) => {
                    memo.seen.clear();
                    let opened = host.open(url, opt.size, opt.window).and_then(|_| host.run(|p| p.take_logs()));
                    busy += t.elapsed();
                    break opened.map(|logs| rep.logs.extend(logs.into_iter().map(|l| (step.line, l)))).map(|_| None).map_err(|e| fail("page", e));
                }
                a => {
                    let (a, mut m) = (a.clone(), std::mem::take(&mut memo));
                    let (out, ws) = (opt.out.clone(), red.clone());
                    match host.run(move |p| {
                        let tried = attempt(p, &mut m, &a, out.as_ref(), &ws);
                        let wait = p.tick();
                        (m, tried, wait, p.take_logs())
                    }) {
                        Ok((m, tried, wait, logs)) => {
                            memo = m;
                            for s in memo.typed.drain(..) {
                                red.add(&s);
                            }
                            rep.logs.extend(logs.into_iter().map(|l| (step.line, l)));
                            (tried, wait)
                        }
                        Err(e) => break Err(fail("page", e)),
                    }
                }
            };
            busy += t.elapsed();
            match tried {
                Try::Done(note) => break Ok(note),
                Try::Stop(f) => break Err(f),
                Try::Again(f) => {
                    let now = Instant::now();
                    let next = match wait {
                        (true, _) => now,
                        (false, Some(at)) => at,
                        (false, None) => {
                            break Err(Fail { message: format!("{} (the page is idle: no task is queued, so nothing can change it)", f.message), ..f });
                        }
                    };
                    if now >= deadline {
                        break Err(Fail { message: format!("{} (time limit {} ms)", f.message, timeout.as_millis()), ..f });
                    }
                    let sleep = next.min(deadline).saturating_duration_since(now);
                    std::thread::sleep(sleep);
                    waited += sleep;
                }
            }
        };
        rep.net.extend(bare_net::trace::take().into_iter().map(|r| (step.line, r)));
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        let ok = result.is_ok();
        let note = result.as_ref().ok().cloned().flatten();
        rep.steps.push(StepOut { line: step.line, text: step.text.clone(), ok, ms: ms(t0.elapsed()), busy_ms: ms(busy), waited_ms: ms(waited), tries, note });
        if opt.progress {
            let s = rep.steps.last().expect("pushed");
            let what = match &result {
                Ok(_) => "ok".to_string(),
                Err(f) => format!("FAIL {}: {}", f.kind, f.message),
            };
            eprintln!("{}", red.clean(&format!("L{} {} {what} ({:.0} ms, {} tries)", s.line, s.text, s.ms, s.tries)));
        }
        if let Err(f) = result {
            rep.ok = false;
            let shot = opt.out.as_ref().and_then(|dir| {
                let ws = red.clone();
                let canvas = host.run(move |p| shown(p, &ws).then(|| p.capture())).ok()?;
                Some(match canvas {
                    Some(c) => save(&c, dir, &format!("fail-L{}", step.line)),
                    None => "withheld: a secret is on the page".to_string(),
                })
            });
            rep.failure = Some((step.line, f, shot));
            break;
        }
    }
    drop(host);
    rep.ms = start.elapsed().as_secs_f64() * 1000.0;
    rep
}

fn save(c: &Canvas, dir: &std::path::Path, name: &str) -> String {
    let path = dir.join(format!("{name}.png"));
    match bare_paint::png::write_file(c, &path) {
        Ok(()) => path.display().to_string(),
        Err(e) => format!("not written: {e}"),
    }
}

/// May the page be captured: no secret in its text or in a field that is not a password field?
fn shown(p: &mut Page, red: &Redactor) -> bool {
    let fields = p.query_all("input, textarea").unwrap_or_default();
    for h in fields {
        if !p.is_password(h) && p.value(h).is_some_and(|v| red.hits(&v)) {
            return false;
        }
    }
    p.inspect(|doc, _| {
        let mut stack = vec![doc.root()];
        while let Some(n) = stack.pop() {
            if let bare_html::dom::NodeData::Text(span) = doc.data(n)
                && red.hits(doc.text(*span))
            {
                return false;
            }
            stack.extend(doc.children(n));
            stack.extend(doc.shadow_root(n));
        }
        true
    })
    .unwrap_or(true)
}

/// The URL for a message: no fragment, and the values of the query hidden (a query can hold a token).
pub fn safe_url(u: &str) -> String {
    let u = u.split('#').next().unwrap_or("");
    match u.split_once('?') {
        Some((path, q)) => format!("{path}?{}", q.split('&').map(|kv| kv.split('=').next().unwrap_or("").to_string() + "=…").collect::<Vec<_>>().join("&")),
        None => u.to_string(),
    }
}

fn short(s: &str) -> String {
    let s = locate::norm(s);
    if s.chars().count() > 120 { s.chars().take(120).collect::<String>() + "…" } else { s }
}

/// The one element that `loc` names now.
fn one(p: &mut Page, m: &mut Memo, loc: &Locator) -> Result<Handle, Try> {
    let found = locate::find(p, loc).map_err(|e| if e.contains("not valid") { Try::Stop(fail("not_found", e)) } else { Try::Again(fail("page", e)) })?;
    let key = loc.to_string();
    match found.shown.as_slice() {
        [h] => {
            m.seen.insert(key, *h);
            Ok(*h)
        }
        [] => Err(Try::Again(match m.seen.get(&key) {
            Some(&old) if old.doc == p.generation() && !p.connected(old) => fail("detached", format!("{loc} matched {} before; it left the document", p.describe(old))),
            _ if found.hidden > 0 => fail("hidden", format!("{loc}: {} element(s) match, none is rendered (display: none, visibility: hidden or aria-hidden)", found.hidden)),
            _ => fail("not_found", format!("no element matches {loc}")),
        })),
        many => {
            let list: Vec<String> = many.iter().take(5).map(|&h| p.describe(h)).collect();
            Err(Try::Stop(fail("duplicate", format!("{loc} matches {} elements: {}{}", many.len(), list.join(", "), if many.len() > 5 { ", …" } else { "" }))))
        }
    }
}

fn refused(p: &mut Page, h: Handle, r: Refusal) -> Try {
    let what = p.describe(h);
    match r {
        Refusal::Detached => Try::Again(fail("detached", format!("{what} left the document"))),
        Refusal::Hidden(why) => Try::Again(fail("hidden", format!("{what} is hidden: {why}"))),
        Refusal::Disabled => Try::Again(fail("disabled", format!("{what} is disabled"))),
        Refusal::Covered { by, at } => Try::Again(fail("covered", format!("{what} is covered by {by} at ({}, {})", at.0, at.1))),
        Refusal::NotEditable => Try::Stop(fail("not_editable", format!("{what} is not a text field"))),
        Refusal::NoFocus => Try::Stop(fail("no_focus", format!("{what} did not take the focus after the click"))),
        Refusal::NoPage(e) => Try::Again(fail("page", e)),
    }
}

/// One try of a step, on the page thread.
fn attempt(p: &mut Page, m: &mut Memo, a: &Action, out: Option<&PathBuf>, red: &Redactor) -> Try {
    let r = (|| -> Result<Option<String>, Try> {
        match a {
            Action::Goto(url) => p.navigate(Some(url)).map_err(|e| Try::Stop(fail("page", e)))?,
            Action::Reload => p.navigate(None).map_err(|e| Try::Stop(fail("page", e)))?,
            Action::Click(loc) => {
                let h = one(p, m, loc)?;
                p.click(h).map_err(|r| refused(p, h, r))?;
            }
            Action::Type(loc, text) => {
                let h = one(p, m, loc)?;
                if p.is_password(h) {
                    m.typed.push(text.clone());
                }
                p.type_text(h, text).map_err(|r| refused(p, h, r))?;
            }
            Action::Press(key) => p.press(key).map_err(|e| Try::Stop(fail("not_found", e)))?,
            Action::Scroll(loc, dy) => {
                let h = loc.as_ref().map(|l| one(p, m, l)).transpose()?;
                if !p.wheel(h, 0.0, *dy).map_err(|r| refused(p, h.expect("a refusal is about an element"), r))? {
                    return Ok(Some("nothing scrolled".into()));
                }
            }
            Action::Expect(c) => check(p, m, c)?,
            Action::Shot(name) => {
                let Some(dir) = out else { return Ok(Some("no --out: not saved".into())) };
                if !shown(p, red) {
                    return Ok(Some("withheld: a secret is on the page".into()));
                }
                return Ok(Some(save(&p.capture(), dir, name)));
            }
            Action::Open(_) | Action::Timeout(_) => unreachable!("the driver runs these"),
        }
        Ok(None)
    })();
    match r {
        Ok(note) => Try::Done(note),
        Err(t) => t,
    }
}

fn check(p: &mut Page, m: &mut Memo, c: &Check) -> Result<(), Try> {
    let no = |msg: String| Err(Try::Again(fail("expect", msg)));
    match c {
        Check::Url(t) => {
            let url = p.url().unwrap_or_default();
            if !t.matches(&url) {
                return no(format!("expected url{t}, the URL is {}", safe_url(&url)));
            }
        }
        Check::Title(t) => {
            let title = locate::title(p).unwrap_or_default();
            if !t.matches(&title) {
                return no(format!("expected title{t}, the title is {:?}", short(&title)));
            }
        }
        Check::Js(code) => {
            let b = p.browser().ok_or_else(|| Try::Again(fail("page", "no document")))?;
            let r = b.run_script(&format!("!!({code}\n)"));
            p.touched();
            match r {
                Ok(bare_js::value::Value::Bool(true)) => {}
                Ok(_) => return no(format!("the script gave false: {code}")),
                Err(e) => return no(format!("the script threw: {}", short(&format!("{e:?}")))),
            }
        }
        Check::El(loc, State::Hidden) => {
            let found = locate::find(p, loc).map_err(|e| Try::Again(fail("page", e)))?;
            let shown: Vec<Handle> = found.shown.into_iter().filter(|&h| p.visible_box(h).is_ok()).collect();
            if let Some(&h) = shown.first() {
                return no(format!("expected {loc} hidden, {} is rendered", p.describe(h)));
            }
        }
        Check::El(loc, State::Count(n)) => {
            let found = locate::find(p, loc).map_err(|e| Try::Again(fail("page", e)))?;
            if found.shown.len() != *n {
                return no(format!("expected {n} of {loc}, found {}", found.shown.len()));
            }
        }
        Check::El(loc, state) => {
            let h = one(p, m, loc)?;
            let what = p.describe(h);
            let (good, got) = match state {
                State::Visible => match p.visible_box(h) {
                    Ok(_) => (true, String::new()),
                    Err(Refusal::Hidden(why)) => (false, format!("hidden: {why}")),
                    Err(r) => return Err(refused(p, h, r)),
                },
                State::Enabled => (!p.disabled(h), "disabled".into()),
                State::Disabled => (p.disabled(h), "enabled".into()),
                State::Checked => (p.checked(h), "unchecked".into()),
                State::Unchecked => (!p.checked(h), "checked".into()),
                State::Focused => (p.focused(h), "not focused".into()),
                State::Text(t) => {
                    let s = locate::text(p, h).unwrap_or_default();
                    (t.matches(&s), format!("its text is {:?}", short(&s)))
                }
                State::Value(t) => {
                    let s = p.value(h).unwrap_or_default();
                    (t.matches(&s), format!("its value is {:?}", short(&s)))
                }
                State::Hidden | State::Count(_) => unreachable!("above"),
            };
            if !good {
                return no(format!("expected {loc} {}: {what} is {got}", state_name(state)));
            }
        }
    }
    Ok(())
}

fn state_name(s: &State) -> String {
    match s {
        State::Visible => "visible".into(),
        State::Hidden => "hidden".into(),
        State::Enabled => "enabled".into(),
        State::Disabled => "disabled".into(),
        State::Checked => "checked".into(),
        State::Unchecked => "unchecked".into(),
        State::Focused => "focused".into(),
        State::Count(n) => format!("count {n}"),
        State::Text(t) => format!("text{t}"),
        State::Value(t) => format!("value{t}"),
    }
}

/// A JSON string.
fn q(s: &str) -> String {
    let mut out = String::from('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

impl Report {
    /// The result as JSON. Each text goes through `red` here, so a secret learned late is also hidden in the earlier records.
    pub fn json(&self, red: &Redactor) -> String {
        let s = |t: &str| q(&red.clean(t));
        let steps: Vec<String> = self
            .steps
            .iter()
            .map(|x| {
                format!(
                    "{{\"line\":{},\"step\":{},\"ok\":{},\"ms\":{:.1},\"busy_ms\":{:.1},\"waited_ms\":{:.1},\"tries\":{}{}}}",
                    x.line,
                    s(&x.text),
                    x.ok,
                    x.ms,
                    x.busy_ms,
                    x.waited_ms,
                    x.tries,
                    x.note.as_ref().map_or(String::new(), |n| format!(",\"note\":{}", s(n)))
                )
            })
            .collect();
        let failure = self.failure.as_ref().map_or("null".to_string(), |(line, f, shot)| {
            format!("{{\"line\":{line},\"kind\":{},\"message\":{},\"capture\":{}}}", q(f.kind), s(&f.message), shot.as_ref().map_or("null".into(), |c| s(c)))
        });
        let logs: Vec<String> = self.logs.iter().map(|(line, l)| format!("{{\"line\":{line},\"level\":{},\"text\":{}}}", q(l.level), s(&l.text))).collect();
        let net: Vec<String> = self
            .net
            .iter()
            .map(|(line, r)| {
                format!(
                    "{{\"line\":{line},\"url\":{},\"status\":{},\"bytes\":{},\"ms\":{:.1}}}",
                    s(&r.url),
                    r.status,
                    r.bytes,
                    (r.wait + r.dns + r.tcp + r.tls + r.ttfb + r.body) as f64 / 1000.0
                )
            })
            .collect();
        format!(
            "{{\"ok\":{},\"window\":{},\"ms\":{:.1},\"steps\":[{}],\"failure\":{},\"console\":[{}],\"network\":[{}]}}",
            self.ok,
            self.window,
            self.ms,
            steps.join(","),
            failure,
            logs.join(","),
            net.join(",")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction() {
        let mut r = Redactor::default();
        r.add("Pa ss@1");
        r.add("Admin@Example.test");
        assert_eq!(r.clean("x Pa ss@1 y pa ss@1"), "x [secret] y [secret]");
        assert_eq!(r.clean("q=Pa%20ss%401&f=Pa+ss%401"), "q=[secret]&f=[secret]");
        assert_eq!(r.clean("hello admin@example.test"), "hello [secret]");
        assert!(r.hits("ADMIN@EXAMPLE.TEST") && !r.hits("admin"));
        r.add("Ünï");
        assert_eq!(r.clean("x Ünï y ünï"), "x [secret] y ünï");
        assert_eq!(Redactor::default().clean("a"), "a");
        assert_eq!(safe_url("http://a.test/x?q=s3cr3t&b=2#t"), "http://a.test/x?q=…&b=…");
    }
}
