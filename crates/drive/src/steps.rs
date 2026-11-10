//! The step file: one step on each line. `#` starts a comment line. A quoted text is `"..."` with `\"` and `\\`; `${NAME}` in a quoted
//! text or a URL is the value of a variable or a secret (`--var`, `--secrets`).
//!
//! ```text
//! open URL                    a new session (a new profile: empty storage), as a new window
//! open URL state="FILE"       the same, but the session starts from a file that `save-state` (or Playwright's `storageState`) made:
//!                             its cookies, `localStorage` and `sessionStorage`. A missing or bad file fails the step
//! save-state "FILE"           write the cookies, `localStorage` and `sessionStorage` of the session to FILE (mode 0600; it holds live
//!                             credentials). The step output gives counts only. IndexedDB is not saved
//! goto URL                    the address bar of the session
//! reload
//! click LOC
//! type LOC "text"             click the field, select its text, type one key in each tick (a line feed is Enter); "" clears it
//! press KEY                   at the focused element: Enter, Tab, Escape, a, Shift+Tab, Control+a
//! scroll LOC|page DY          the wheel over the element (or the middle of the window), DY CSS pixels down
//! expect LOC STATE            visible hidden enabled disabled checked unchecked focused | count N | text="x" | value~"x"
//! expect url="x" | url~"x"
//! expect title="x" | title~"x"
//! expect js "expression"      true in the page (an assertion: do not change the page in it)
//! expect cookie NAME [absent | value="x" | value~"x"]   the jar has (or lacks) the cookie that a request to the page would send
//! cookie set NAME VALUE [domain=D] [path=P]   put a cookie in the jar of the session (quote a VALUE that holds `=`); D is a host
//!                             (default: the host of the page), P defaults to /. A refused cookie (a public suffix) fails the step
//! wait ...                    the same as expect
//! shot NAME                   a PNG of the window: OUT/NAME.png
//! timeout MS                  the time limit of each later step (default 10000)
//! ```
//!
//! `open`, `goto` and `reload` return at once; the later steps wait for the document.
//!
//! LOC is `css="..."`, `role=NAME` with an optional `name="..."` or `name~"..."`, `label=`, `text=` or `placeholder=` (see `locate.rs`).

use crate::locate::{Locator, Text};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Visible,
    Hidden,
    Enabled,
    Disabled,
    Checked,
    Unchecked,
    Focused,
    Count(usize),
    Text(Text),
    Value(Text),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Check {
    El(Locator, State),
    Url(Text),
    Title(Text),
    Js(String),
    Cookie(String, CookieState),
}

#[derive(Clone, Debug, PartialEq)]
pub enum CookieState {
    Present,
    Absent,
    Value(Text),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// The URL, and the session file to start from.
    Open(String, Option<String>),
    SaveState(String),
    Goto(String),
    Reload,
    Click(Locator),
    Type(Locator, String),
    Press(String),
    Scroll(Option<Locator>, f64),
    Expect(Check),
    Shot(String),
    Timeout(u64),
    CookieSet { name: String, value: String, domain: Option<String>, path: Option<String> },
}

#[derive(Clone, Debug)]
pub struct Step {
    /// The line number in the file, from 1.
    pub line: usize,
    /// The line as written, with `${NAME}` and not the value.
    pub text: String,
    pub action: Action,
}

/// One word of a line: a bare word, `key=value` or `key~value` (the value may be quoted), or a quoted text.
#[derive(Debug, PartialEq)]
enum Word {
    Bare(String),
    Pair(String, bool, String),
    Quoted(String),
}

fn words(line: &str) -> Result<Vec<Word>, String> {
    let mut out = Vec::new();
    let mut it = line.chars().peekable();
    loop {
        while it.next_if(|c| c.is_whitespace()).is_some() {}
        let Some(&c) = it.peek() else { return Ok(out) };
        if c == '"' {
            out.push(Word::Quoted(quoted(&mut it)?));
            continue;
        }
        let mut head = String::new();
        while let Some(c) = it.next_if(|c| !c.is_whitespace() && *c != '=' && *c != '~' && *c != '"') {
            head.push(c);
        }
        match it.next_if(|c| *c == '=' || *c == '~') {
            Some(op) => {
                let value = if it.peek() == Some(&'"') {
                    quoted(&mut it)?
                } else {
                    let mut v = String::new();
                    while let Some(c) = it.next_if(|c| !c.is_whitespace() && *c != '"') {
                        v.push(c);
                    }
                    if it.peek() == Some(&'"') {
                        return Err(format!("a quote inside {head}={v}: quote the whole value"));
                    }
                    v
                };
                out.push(Word::Pair(head, op == '~', value));
            }
            None if it.peek() == Some(&'"') => return Err(format!("a quote after {head:?}: put a space or = before it")),
            None => out.push(Word::Bare(head)),
        }
    }
}

fn quoted(it: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Result<String, String> {
    it.next();
    let mut s = String::new();
    loop {
        match it.next() {
            Some('"') => return Ok(s),
            Some('\\') => match it.next() {
                Some('n') => s.push('\n'),
                Some(c) => s.push(c),
                None => return Err("a quoted text ends with \\".into()),
            },
            Some(c) => s.push(c),
            None => return Err("a quoted text has no closing quote".into()),
        }
    }
}

/// Put the values of `${NAME}` in.
fn expand(s: &str, vars: &HashMap<String, String>) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("${") {
        out.push_str(&rest[..i]);
        let end = rest[i..].find('}').ok_or("a ${ with no }")? + i;
        let name = &rest[i + 2..end];
        out.push_str(vars.get(name).ok_or_else(|| format!("no variable or secret named {name}"))?);
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Parse the step file. `vars` holds the variables and the secrets.
pub fn parse(src: &str, vars: &HashMap<String, String>) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    for (i, line) in src.lines().enumerate() {
        let text = line.trim();
        if text.is_empty() || text.starts_with('#') {
            continue;
        }
        let action = parse_line(text, vars).map_err(|e| format!("line {}: {e}", i + 1))?;
        steps.push(Step { line: i + 1, text: text.to_string(), action });
    }
    Ok(steps)
}

fn parse_line(line: &str, vars: &HashMap<String, String>) -> Result<Action, String> {
    let (verb, raw) = line.split_once(char::is_whitespace).map_or((line, ""), |(v, r)| (v, r.trim()));
    // A URL can hold `=` and `~`: the argument of these verbs is the rest of the line, quoted or not.
    let one = || {
        let arg = if raw.starts_with('"') {
            let mut it = raw.chars().peekable();
            let s = quoted(&mut it)?;
            if it.next().is_some() {
                return Err(format!("{verb} takes one argument"));
            }
            s
        } else if raw.is_empty() || raw.contains(char::is_whitespace) {
            return Err(format!("{verb} takes one argument"));
        } else {
            raw.to_string()
        };
        expand(&arg, vars)
    };
    if verb == "open" {
        // `open URL state="FILE"`: the URL is the first word; a URL can hold `=` and `~`, so only a last `state=` word is cut.
        if let Some(i) = raw.rfind(" state=") {
            let (url, f) = (raw[..i].trim(), raw[i + 7..].trim());
            let mut it = f.chars().peekable();
            let file = if f.starts_with('"') { quoted(&mut it).ok().filter(|_| it.next().is_none()) } else { Some(f.to_string()).filter(|f| !f.is_empty() && !f.contains(char::is_whitespace)) };
            let (Some(file), false) = (file, url.is_empty() || url.contains(char::is_whitespace)) else { return Err("open URL state=\"FILE\"".into()) };
            return Ok(Action::Open(expand(url, vars)?, Some(expand(&file, vars)?)));
        }
    }
    let w = if matches!(verb, "open" | "goto" | "press" | "shot" | "timeout" | "save-state") { Vec::new() } else { words(raw)? };
    let rest = &w[..];
    Ok(match verb {
        "open" => Action::Open(one()?, None),
        "save-state" => Action::SaveState(one()?),
        "goto" => Action::Goto(one()?),
        "reload" if raw.is_empty() => Action::Reload,
        "click" => match locator(rest, vars)? {
            (loc, n) if n == rest.len() => Action::Click(loc),
            _ => return Err("click LOC".into()),
        },
        "type" => {
            let (loc, used) = locator(rest, vars)?;
            match &rest[used..] {
                [Word::Quoted(t)] => Action::Type(loc, expand(t, vars)?),
                _ => return Err("type LOC \"text\"".into()),
            }
        }
        "press" => {
            let key = one()?;
            bare::control::key_events(&key).ok_or_else(|| format!("not a key: {key}"))?;
            Action::Press(key)
        }
        "scroll" => {
            let (loc, used) = match rest.first() {
                Some(Word::Bare(p)) if p == "page" => (None, 1),
                _ => locator(rest, vars).map(|(l, n)| (Some(l), n))?,
            };
            match &rest[used..] {
                [Word::Bare(d)] => Action::Scroll(loc, d.parse().map_err(|_| format!("not a number: {d}"))?),
                _ => return Err("scroll LOC|page DY".into()),
            }
        }
        "expect" | "wait" => Action::Expect(check(rest, vars)?),
        "cookie" => match rest {
            [Word::Bare(set), name, value, opts @ ..] if set == "set" => {
                let (mut domain, mut path) = (None, None);
                for o in opts {
                    match o {
                        Word::Pair(k, false, v) if k == "domain" => domain = Some(expand(v, vars)?),
                        Word::Pair(k, false, v) if k == "path" => path = Some(expand(v, vars)?),
                        _ => return Err("cookie set NAME VALUE [domain=D] [path=P]".into()),
                    }
                }
                Action::CookieSet { name: plain(name, vars)?, value: plain(value, vars)?, domain, path }
            }
            _ => return Err("cookie set NAME VALUE [domain=D] [path=P]".into()),
        },
        "shot" => {
            let name = one()?;
            if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                return Err("a shot name has only letters, digits, - and _".into());
            }
            Action::Shot(name)
        }
        "timeout" => Action::Timeout(one()?.parse().map_err(|_| "timeout MS")?),
        _ => return Err(format!("not a step: {verb}")),
    })
}

fn text_of(op: bool, v: String) -> Text {
    if op { Text::Has(v) } else { Text::Is(v) }
}

/// A locator at the start of `w`, and how many words it used.
fn locator(w: &[Word], vars: &HashMap<String, String>) -> Result<(Locator, usize), String> {
    let Some(Word::Pair(k, op, v)) = w.first() else { return Err("a locator is missing (css=, role=, label=, text=, placeholder=)".into()) };
    let v = expand(v, vars)?;
    let t = text_of(*op, v.clone());
    Ok(match k.as_str() {
        "css" if !op => (Locator::Css(v), 1),
        "role" if !op => match w.get(1) {
            Some(Word::Pair(n, op, name)) if n == "name" => (Locator::Role(v.to_ascii_lowercase(), Some(text_of(*op, expand(name, vars)?))), 2),
            _ => (Locator::Role(v.to_ascii_lowercase(), None), 1),
        },
        "label" => (Locator::Label(t), 1),
        "text" => (Locator::Text(t), 1),
        "placeholder" => (Locator::Placeholder(t), 1),
        _ => return Err(format!("not a locator: {k}")),
    })
}

/// A bare or quoted word, with its variables put in.
fn plain(w: &Word, vars: &HashMap<String, String>) -> Result<String, String> {
    match w {
        Word::Bare(s) | Word::Quoted(s) => expand(s, vars),
        Word::Pair(k, ..) => Err(format!("quote a text that holds = or ~: {k}")),
    }
}

fn check(w: &[Word], vars: &HashMap<String, String>) -> Result<Check, String> {
    match w {
        [Word::Bare(k), name, rest @ ..] if k == "cookie" => {
            let state = match rest {
                [] => CookieState::Present,
                [Word::Bare(a)] if a == "absent" => CookieState::Absent,
                [Word::Pair(k, op, v)] if k == "value" => CookieState::Value(text_of(*op, expand(v, vars)?)),
                _ => return Err("expect cookie NAME [absent | value=\"x\" | value~\"x\"]".into()),
            };
            return Ok(Check::Cookie(plain(name, vars)?, state));
        }
        [Word::Pair(k, op, v)] if k == "url" => return Ok(Check::Url(text_of(*op, expand(v, vars)?))),
        [Word::Pair(k, op, v)] if k == "title" => return Ok(Check::Title(text_of(*op, expand(v, vars)?))),
        [Word::Bare(k), Word::Quoted(js)] if k == "js" => return Ok(Check::Js(expand(js, vars)?)),
        _ => {}
    }
    let (loc, used) = locator(w, vars)?;
    let state = match &w[used..] {
        [Word::Bare(s)] => match s.as_str() {
            "visible" => State::Visible,
            "hidden" => State::Hidden,
            "enabled" => State::Enabled,
            "disabled" => State::Disabled,
            "checked" => State::Checked,
            "unchecked" => State::Unchecked,
            "focused" => State::Focused,
            _ => return Err(format!("not a state: {s}")),
        },
        [Word::Bare(c), Word::Bare(n)] if c == "count" => State::Count(n.parse().map_err(|_| format!("not a count: {n}"))?),
        [Word::Pair(k, op, v)] if k == "text" => State::Text(text_of(*op, expand(v, vars)?)),
        [Word::Pair(k, op, v)] if k == "value" => State::Value(text_of(*op, expand(v, vars)?)),
        _ => return Err("expect LOC visible|hidden|enabled|disabled|checked|unchecked|focused|count N|text=\"x\"|value=\"x\"".into()),
    };
    Ok(Check::El(loc, state))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(line: &str) -> Result<Action, String> {
        let vars = HashMap::from([("PASS".to_string(), "p\"w".to_string())]);
        parse_line(line, &vars)
    }

    #[test]
    fn steps_parse() {
        assert_eq!(p("click role=button name=\"Sign in\""), Ok(Action::Click(Locator::Role("button".into(), Some(Text::Is("Sign in".into()))))));
        assert_eq!(p("click role=menuitem name~\"sign out\""), Ok(Action::Click(Locator::Role("menuitem".into(), Some(Text::Has("sign out".into()))))));
        assert_eq!(p("type label=\"Password\" \"${PASS}\""), Ok(Action::Type(Locator::Label(Text::Is("Password".into())), "p\"w".into())));
        assert_eq!(p("type css=\"input[name=q]\" \"a\\\"b\\n\""), Ok(Action::Type(Locator::Css("input[name=q]".into()), "a\"b\n".into())));
        assert_eq!(p("expect css=\"li\" count 3"), Ok(Action::Expect(Check::El(Locator::Css("li".into()), State::Count(3)))));
        assert_eq!(p("expect url~\"/agents?q=x\""), Ok(Action::Expect(Check::Url(Text::Has("/agents?q=x".into())))));
        assert_eq!(p("wait js \"1 + 1 == 2\""), Ok(Action::Expect(Check::Js("1 + 1 == 2".into()))));
        assert_eq!(p("scroll page 600"), Ok(Action::Scroll(None, 600.0)));
        assert_eq!(p("cookie set consent 1"), Ok(Action::CookieSet { name: "consent".into(), value: "1".into(), domain: None, path: None }));
        assert_eq!(p("cookie set a \"b=c\" domain=x.test path=/p"), Ok(Action::CookieSet { name: "a".into(), value: "b=c".into(), domain: Some("x.test".into()), path: Some("/p".into()) }));
        assert_eq!(p("expect cookie sid"), Ok(Action::Expect(Check::Cookie("sid".into(), CookieState::Present))));
        assert_eq!(p("expect cookie sid absent"), Ok(Action::Expect(Check::Cookie("sid".into(), CookieState::Absent))));
        assert_eq!(p("expect cookie sid value~\"ab\""), Ok(Action::Expect(Check::Cookie("sid".into(), CookieState::Value(Text::Has("ab".into()))))));
        assert_eq!(p("open http://a.test/x?q=1"), Ok(Action::Open("http://a.test/x?q=1".into(), None)));
        assert_eq!(p("open http://a.test/x?q=1 state=\"s/${PASS}.json\""), Ok(Action::Open("http://a.test/x?q=1".into(), Some("s/p\"w.json".into()))));
        assert_eq!(p("open http://a.test state=f.json"), Ok(Action::Open("http://a.test".into(), Some("f.json".into()))));
        assert_eq!(p("save-state \"out/s.json\""), Ok(Action::SaveState("out/s.json".into())));
        assert_eq!(p("save-state s.json"), Ok(Action::SaveState("s.json".into())));
        for bad in ["open http://a.test state=", "open http://a.test state=\"f\" x", "save-state", "save-state a b"] {
            assert!(p(bad).is_err(), "{bad}");
        }
        assert_eq!(p("press Shift+Tab"), Ok(Action::Press("Shift+Tab".into())));
        assert_eq!(p("goto http://a.test/x?q=1&r=~2"), Ok(Action::Goto("http://a.test/x?q=1&r=~2".into())));
        assert_eq!(p("expect placeholder~\"search\" value=\"\""), Ok(Action::Expect(Check::El(Locator::Placeholder(Text::Has("search".into())), State::Value(Text::Is("".into()))))));
        for bad in ["cookie", "cookie set a", "cookie set a b secure=1", "expect cookie", "expect cookie a shiny", "click", "type css=\"a\"", "expect css=\"a\" shiny", "nope", "type css=\"a\" \"${NOPE}\"", "shot ../x", "press Hyper+x", "click role=button\"x\"", "type css=\"a\" \"open"] {
            assert!(p(bad).is_err(), "{bad}");
        }
    }
}
