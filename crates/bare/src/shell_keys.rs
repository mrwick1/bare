//! The keys of the shell as a pure state machine: (mode, key) -> (new mode, actions). No window, no page, no clock of its own (the
//! caller gives the time). `shell.rs` runs the actions; `shell_keys_tests.rs` tests this file alone.
//!
//! Modes: NORMAL (keys are commands), INSERT (every key goes to the page), COMMAND (the `:` line), HINT (typing a label) and FIND (the
//! `/` line). The shell flips NORMAL and INSERT by itself from the focus of the page (`Keys::focus`).
//!
//! A key that the machine eats never reaches the page, and neither does its release. A key with Ctrl, Alt or Meta in NORMAL goes to the
//! page. The two-key commands `gg`, `gt` and `gT` wait one second for the second key.
//!
//! Also here, because they need no window: the command parser (`parse`), the URL rule (`url_of`) and the hint labels (`hint_labels`).
//!
//! Gaps: no count prefix (`3gt`), no `:set`, no marks, no key map. The command line has no history file: the history lasts for the run.

use std::time::{Duration, Instant};

/// How long `g` waits for its second key.
const PENDING: Duration = Duration::from_secs(1);
/// The letters of the hint labels: the home row.
const HOME_ROW: &str = "asdfghjkl";
/// The search page for text that is not a URL. The text is appended, percent-encoded.
pub const SEARCH: &str = "https://duckduckgo.com/html/?q=";
/// The command names, for Tab.
const NAMES: [&str; 11] = ["open", "tabopen", "buffers", "buffer", "bdelete", "reload", "back", "forward", "quit", "ls", "bd"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Insert,
    Command,
    Hint,
    Find,
}

/// One key press (or repeat). `key` is the DOM `key`: one character for a printable key, else a name such as `Escape`.
#[derive(Debug, Clone, Copy)]
pub struct Key<'a> {
    pub key: &'a str,
    pub ctrl: bool,
    pub alt: bool,
    pub meta: bool,
}

impl<'a> Key<'a> {
    pub fn plain(key: &'a str) -> Key<'a> {
        Key { key, ctrl: false, alt: false, meta: false }
    }

    pub fn ctrl(key: &'a str) -> Key<'a> {
        Key { ctrl: true, ..Key::plain(key) }
    }

    fn mods(&self) -> bool {
        self.ctrl || self.alt || self.meta
    }

    fn ch(&self) -> Option<char> {
        let mut c = self.key.chars();
        c.next().filter(|_| c.next().is_none())
    }
}

/// What the shell does after a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Act {
    /// Scroll the page by this many pixels (down is positive).
    Scroll(i32),
    /// Scroll by half a window (down is positive: `1`, or `-1`).
    Half(i32),
    Top,
    Bottom,
    Back,
    Forward,
    Reload,
    NextPage,
    PrevPage,
    ClosePage,
    /// Put the URL of the page on the clipboard.
    CopyUrl,
    /// Open the command line with `open ` and the URL of the page.
    OpenCurrent,
    /// Read the clipboard and open it as a URL.
    PasteOpen,
    /// Read the clipboard into the command line.
    PasteLine,
    /// Label the clickable elements. `true`: `F`, open a link in a new page.
    Hints(bool),
    /// The label at this index was typed to its end. `true`: open in a new page.
    Pick(usize, bool),
    /// Focus the first text field in the window.
    FocusField,
    /// Take the focus off the element (Escape in INSERT).
    Blur,
    /// The command line ended with Enter.
    Run(String),
    /// The find line ended with Enter.
    Find(String),
    /// The next (`1`) or previous (`-1`) match.
    FindStep(i32),
    /// The mode ended and the overlay goes away (Escape).
    Cancel,
    /// Something to redraw: the line or the typed label changed.
    Redraw,
}

/// The result of one key.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Out {
    /// The page must not see the key (nor its release).
    pub eat: bool,
    pub acts: Vec<Act>,
}

pub struct Keys {
    pub mode: Mode,
    /// The first key of a two-key command, and when it came.
    pending: Option<(char, Instant)>,
    /// The line of COMMAND and FIND: its text, the caret (a char index) and the prompt (`:` or `/`).
    pub line: String,
    pub caret: usize,
    pub prompt: char,
    history: Vec<String>,
    /// The place in `history` while Up and Down walk it, and the line that the walk left.
    walk: Option<(usize, String)>,
    /// HINT: the labels, what was typed, and whether the pick opens a new page.
    labels: Vec<String>,
    pub typed: String,
    new_page: bool,
    /// The keys whose press was eaten: their release is eaten too.
    eaten: Vec<String>,
}

impl Default for Keys {
    fn default() -> Keys {
        Keys { mode: Mode::Normal, pending: None, line: String::new(), caret: 0, prompt: ':', history: Vec::new(), walk: None, labels: Vec::new(), typed: String::new(), new_page: false, eaten: Vec::new() }
    }
}

impl Keys {
    pub fn new() -> Keys {
        Keys::default()
    }

    /// The labels of the hint mode, once the shell has them (after `Act::Hints`). No label: the mode ends at once.
    pub fn set_labels(&mut self, labels: Vec<String>) {
        if labels.is_empty() {
            self.mode = Mode::Normal;
        }
        self.labels = labels;
        self.typed.clear();
    }

    /// The labels that the typed letters still match, with their index.
    pub fn live(&self) -> impl Iterator<Item = (usize, &str)> {
        self.labels.iter().enumerate().filter(|(_, l)| l.starts_with(&self.typed)).map(|(i, l)| (i, l.as_str()))
    }

    /// The page says whether an editable element has the focus. NORMAL and INSERT follow it; the other modes do not move.
    pub fn focus(&mut self, editable: bool) {
        match (self.mode, editable) {
            (Mode::Normal, true) => self.mode = Mode::Insert,
            (Mode::Insert, false) => self.mode = Mode::Normal,
            _ => {}
        }
    }

    /// Back to NORMAL, with no hint and no pending key (a click or the wheel ended the mode).
    pub fn reset(&mut self) {
        if self.mode != Mode::Insert {
            self.mode = Mode::Normal;
        }
        (self.pending, self.labels, self.typed) = (None, Vec::new(), String::new());
    }

    /// Open the command line with `text` already in it.
    pub fn command_line(&mut self, text: &str) {
        self.begin(Mode::Command, ':', text);
    }

    fn begin(&mut self, mode: Mode, prompt: char, text: &str) {
        (self.mode, self.prompt, self.line, self.caret, self.walk) = (mode, prompt, text.to_string(), text.chars().count(), None);
    }

    /// Text that arrived from the clipboard for the line (`Act::PasteLine`). Line breaks go: the line has one line.
    pub fn insert_text(&mut self, text: &str) {
        for c in text.chars().filter(|c| !c.is_control()) {
            self.put(c);
        }
    }

    /// A key press or repeat (`pressed`), or a release. The release of an eaten key is eaten.
    pub fn key(&mut self, k: Key, pressed: bool, now: Instant) -> Out {
        if !pressed {
            let at = self.eaten.iter().position(|e| e == k.key);
            return Out { eat: at.map(|i| self.eaten.remove(i)).is_some(), acts: Vec::new() };
        }
        let out = match self.mode {
            Mode::Insert => self.insert(k),
            Mode::Normal => self.normal(k, now),
            Mode::Command | Mode::Find => self.edit(k),
            Mode::Hint => self.hint(k),
        };
        if out.eat && !self.eaten.iter().any(|e| e == k.key) {
            self.eaten.push(k.key.to_string());
        }
        out
    }

    fn insert(&mut self, k: Key) -> Out {
        if k.key == "Escape" && !k.mods() {
            self.mode = Mode::Normal;
            // The page gets the Escape first (a menu or a dialog closes), then loses the focus.
            return Out { eat: false, acts: vec![Act::Blur] };
        }
        Out::default()
    }

    fn normal(&mut self, k: Key, now: Instant) -> Out {
        // A modifier key alone (the Shift of `gT`) does not end a two-key command.
        if matches!(k.key, "Shift" | "Control" | "Alt" | "Meta" | "AltGraph" | "CapsLock") {
            return Out::default();
        }
        if k.mods() {
            self.pending = None;
            return Out::default();
        }
        let Some(c) = k.ch() else {
            self.pending = None;
            return Out::default();
        };
        let acts = |a: Act| Out { eat: true, acts: vec![a] };
        if let Some((first, _)) = self.pending.take().filter(|(_, at)| now.duration_since(*at) < PENDING) {
            match (first, c) {
                ('g', 'g') => return acts(Act::Top),
                ('g', 't') => return acts(Act::NextPage),
                ('g', 'T') => return acts(Act::PrevPage),
                _ => {}
            }
        }
        match c {
            'g' => {
                self.pending = Some(('g', now));
                Out { eat: true, acts: Vec::new() }
            }
            'j' => acts(Act::Scroll(60)),
            'k' => acts(Act::Scroll(-60)),
            'd' => acts(Act::Half(1)),
            'u' => acts(Act::Half(-1)),
            'G' => acts(Act::Bottom),
            'H' => acts(Act::Back),
            'L' => acts(Act::Forward),
            'r' => acts(Act::Reload),
            'x' => acts(Act::ClosePage),
            'y' => acts(Act::CopyUrl),
            'p' => acts(Act::PasteOpen),
            'i' => acts(Act::FocusField),
            'n' => acts(Act::FindStep(1)),
            'N' => acts(Act::FindStep(-1)),
            'o' => self.line_out(Mode::Command, ':', "open "),
            't' => self.line_out(Mode::Command, ':', "tabopen "),
            // The shell fills in the current URL (`Keys::command_line`): it knows the URL.
            'O' => acts(Act::OpenCurrent),
            ':' => self.line_out(Mode::Command, ':', ""),
            '/' => self.line_out(Mode::Find, '/', ""),
            'f' | 'F' => {
                self.mode = Mode::Hint;
                self.new_page = c == 'F';
                self.typed.clear();
                self.labels.clear();
                acts(Act::Hints(self.new_page))
            }
            _ => Out::default(),
        }
    }

    fn line_out(&mut self, mode: Mode, prompt: char, text: &str) -> Out {
        self.begin(mode, prompt, text);
        Out { eat: true, acts: vec![Act::Redraw] }
    }

    fn byte(&self, chars: usize) -> usize {
        self.line.char_indices().nth(chars).map_or(self.line.len(), |(i, _)| i)
    }

    fn put(&mut self, c: char) {
        let at = self.byte(self.caret);
        self.line.insert(at, c);
        self.caret += 1;
    }

    /// COMMAND and FIND: every key is eaten.
    fn edit(&mut self, k: Key) -> Out {
        let mut acts = vec![Act::Redraw];
        let len = self.line.chars().count();
        if k.ctrl && !k.alt && !k.meta {
            match k.key {
                "u" => (self.line, self.caret) = (String::new(), 0),
                "w" => {
                    let head: Vec<char> = self.line.chars().take(self.caret).collect();
                    let mut cut = head.len();
                    while cut > 0 && head[cut - 1] == ' ' {
                        cut -= 1;
                    }
                    while cut > 0 && head[cut - 1] != ' ' {
                        cut -= 1;
                    }
                    let (from, to) = (self.byte(cut), self.byte(self.caret));
                    self.line.replace_range(from..to, "");
                    self.caret = cut;
                }
                "v" => acts.push(Act::PasteLine),
                _ => {}
            }
            return Out { eat: true, acts };
        }
        if k.alt || k.meta {
            return Out { eat: true, acts: Vec::new() };
        }
        match k.key {
            "Escape" => {
                self.mode = Mode::Normal;
                acts = vec![Act::Cancel];
            }
            "Enter" => {
                let text = std::mem::take(&mut self.line);
                let (find, mode) = (self.mode == Mode::Find, self.mode);
                self.mode = Mode::Normal;
                self.caret = 0;
                if !text.trim().is_empty() && mode == Mode::Command && self.history.last() != Some(&text) {
                    self.history.push(text.clone());
                }
                acts = vec![if find { Act::Find(text) } else { Act::Run(text) }];
            }
            "Backspace" => {
                if self.caret > 0 {
                    let (from, to) = (self.byte(self.caret - 1), self.byte(self.caret));
                    self.line.replace_range(from..to, "");
                    self.caret -= 1;
                }
            }
            "Delete" => {
                if self.caret < len {
                    let (from, to) = (self.byte(self.caret), self.byte(self.caret + 1));
                    self.line.replace_range(from..to, "");
                }
            }
            "ArrowLeft" => self.caret = self.caret.saturating_sub(1),
            "ArrowRight" => self.caret = (self.caret + 1).min(len),
            "Home" => self.caret = 0,
            "End" => self.caret = len,
            "ArrowUp" | "ArrowDown" if self.mode == Mode::Command => self.walk_history(k.key == "ArrowUp"),
            "Tab" if self.mode == Mode::Command => self.complete(),
            _ => {
                if let Some(c) = k.ch() {
                    self.put(c);
                }
            }
        }
        Out { eat: true, acts }
    }

    fn walk_history(&mut self, up: bool) {
        let n = self.history.len();
        let (at, saved) = match self.walk.take() {
            Some(w) => w,
            None => (n, self.line.clone()),
        };
        let at = if up { at.saturating_sub(1) } else { (at + 1).min(n) };
        // Up at the oldest command stays there; Down past the newest gives back the line that was typed.
        self.line = if at == n { saved.clone() } else { self.history[at].clone() };
        self.caret = self.line.chars().count();
        self.walk = Some((at, saved));
    }

    /// Tab: complete the command name, when the line is one word with no space. The names that share the word: the longest common start.
    fn complete(&mut self) {
        if self.line.contains(' ') || self.caret != self.line.chars().count() {
            return;
        }
        let names: Vec<&str> = NAMES.iter().copied().filter(|n| n.starts_with(&self.line)).collect();
        let Some(first) = names.first() else { return };
        let mut common = first.len();
        for n in &names {
            common = common.min(first.bytes().zip(n.bytes()).take_while(|(a, b)| a == b).count());
        }
        self.line = first[..common].to_string();
        if names.len() == 1 {
            self.line.push(' ');
        }
        self.caret = self.line.chars().count();
    }

    fn hint(&mut self, k: Key) -> Out {
        let eat = |acts| Out { eat: true, acts };
        if k.mods() {
            return eat(Vec::new());
        }
        match k.key {
            "Escape" => {
                self.mode = Mode::Normal;
                eat(vec![Act::Cancel])
            }
            "Backspace" => {
                self.typed.pop();
                eat(vec![Act::Redraw])
            }
            _ => {
                let Some(c) = k.ch().map(|c| c.to_ascii_lowercase()).filter(|c| HOME_ROW.contains(*c)) else { return eat(Vec::new()) };
                let mut next = self.typed.clone();
                next.push(c);
                // A letter that no label continues with is ignored.
                if !self.labels.iter().any(|l| l.starts_with(&next)) {
                    return eat(Vec::new());
                }
                self.typed = next;
                match self.labels.iter().position(|l| *l == self.typed) {
                    Some(i) => {
                        self.mode = Mode::Normal;
                        eat(vec![Act::Pick(i, self.new_page)])
                    }
                    None => eat(vec![Act::Redraw]),
                }
            }
        }
    }
}

/// `n` labels from the home row: all different, and none the start of another (so a label is complete when it is typed). Short ones come
/// first.
pub fn hint_labels(n: usize) -> Vec<String> {
    let mut out: std::collections::VecDeque<String> = HOME_ROW.chars().map(String::from).collect();
    while out.len() < n {
        let first = out.pop_front().expect("not empty");
        out.extend(HOME_ROW.chars().map(|c| format!("{first}{c}")));
    }
    out.truncate(n);
    out.into()
}

/// A command of the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cmd {
    Open(String),
    TabOpen(String),
    Buffers,
    /// The number as shown, from 1.
    Buffer(usize),
    Bdelete(Option<usize>),
    Reload,
    Back,
    Forward,
    Quit,
}

/// Parse a command line (without the `:`).
pub fn parse(line: &str) -> Result<Cmd, String> {
    let line = line.trim();
    let (name, arg) = line.split_once(char::is_whitespace).map_or((line, ""), |(n, a)| (n, a.trim()));
    let number = |what: &str, need: bool| -> Result<Option<usize>, String> {
        match arg {
            "" if !need => Ok(None),
            "" => Err(format!("{what}: a page number is needed")),
            a => a.parse::<usize>().ok().filter(|&n| n >= 1).map(Some).ok_or_else(|| format!("{what}: {a}: not a page number")),
        }
    };
    let url = |what: &str| if arg.is_empty() { Err(format!("{what}: a URL is needed")) } else { Ok(url_of(arg)) };
    match name {
        "open" | "o" => url("open").map(Cmd::Open),
        "tabopen" | "t" => url("tabopen").map(Cmd::TabOpen),
        "buffers" | "ls" => Ok(Cmd::Buffers),
        "buffer" | "b" => number("buffer", true).map(|n| Cmd::Buffer(n.unwrap_or(1))),
        "bdelete" | "bd" => number("bdelete", false).map(Cmd::Bdelete),
        "reload" | "r" => Ok(Cmd::Reload),
        "back" => Ok(Cmd::Back),
        "forward" => Ok(Cmd::Forward),
        "quit" | "q" => Ok(Cmd::Quit),
        "" => Err("no command".into()),
        other => Err(format!("not a command: {other}")),
    }
}

/// The URL for what the user typed: a scheme stays as it is; `localhost` and an IP address get `http://`; text with a dot and no space
/// gets `https://`; anything else is a search.
pub fn url_of(text: &str) -> String {
    let t = text.trim();
    let scheme = t.split_once(':').is_some_and(|(s, rest)| {
        s.starts_with(|c: char| c.is_ascii_alphabetic()) && s.chars().all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c)) && (rest.starts_with("//") || ["about", "data", "file", "javascript", "mailto"].contains(&s.to_ascii_lowercase().as_str()))
    });
    if scheme {
        return t.to_string();
    }
    let host = if t.starts_with('[') { t.split_once(']').map_or(t, |(h, _)| &h[1..]) } else { t.split(['/', ':', '?', '#']).next().unwrap_or("") };
    if host.eq_ignore_ascii_case("localhost") || host.parse::<std::net::IpAddr>().is_ok() {
        return format!("http://{t}");
    }
    if t.contains('.') && !t.contains(char::is_whitespace) {
        return format!("https://{t}");
    }
    let mut out = String::from(SEARCH);
    for b in t.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
