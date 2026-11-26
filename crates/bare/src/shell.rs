//! The shell of the window: a list of pages in one window, driven by keys, with no browser chrome (no address bar, no tab strip, no
//! toolbar). The model is vimb: modes, a `:` command line, link hints and find.
//!
//! # Keys (NORMAL mode; the page does not see them)
//!
//! | Key | Action |
//! |---|---|
//! | `j` `k` | scroll down and up by 60 px |
//! | `d` `u` | scroll half a window down and up |
//! | `gg` `G` | top, bottom |
//! | `H` `L` | back, forward in the history |
//! | `r` | reload |
//! | `o` `O` | command line with `open `, and with `open <current URL>` |
//! | `t` | command line with `tabopen ` |
//! | `gt` `gT` | next page, previous page |
//! | `x` | close the page (the last one closes the window) |
//! | `y` `p` | copy the URL; open the URL on the clipboard |
//! | `f` `F` | hints: click the element; open the link in a new page |
//! | `i` | focus the first text field (INSERT) |
//! | `/` `n` `N` | find; next match; previous match |
//! | `:` | command line |
//!
//! Any other key, and any key with Ctrl, Alt or Meta, goes to the page (Alt+Left still goes back).
//!
//! INSERT starts by itself when an editable element has the focus, and ends when the focus leaves. Every key goes to the page there.
//! Escape goes to the page, then takes the focus off the element and returns to NORMAL.
//!
//! COMMAND line: type; Backspace, Delete, Left, Right, Home, End; Ctrl+U clears; Ctrl+W deletes a word; Ctrl+V pastes; Up and Down walk
//! the commands of this run; Tab completes a command name; Enter runs; Escape cancels. Commands: `open|o URL`, `tabopen|t URL`,
//! `buffers|ls`, `buffer|b N`, `bdelete|bd [N]`, `reload|r`, `back`, `forward`, `quit|q`. Pages are numbered from 1, as in the title.
//! URL rule: a scheme stays; `localhost` and an IP address get `http://`; text with a dot and no space gets `https://`; the rest is a
//! search (`shell_keys::SEARCH`).
//!
//! HINT: type the letters of a label (home row); a complete label acts. Escape cancels. FIND: type the text, Enter; `[k/n]` shows
//! for 3 seconds, the match is selected and boxed until Escape.
//!
//! # Pages
//!
//! `Shell` is the `bare_window::App` of `bare --window URL`. It owns the pages and shows the current one. The window size goes to every
//! page, the input and the frame go to the current page, and the tick goes to every page: a page in the background keeps its timers,
//! its fetches and its WebSockets alive.
//!
//! A page opened by the shell (`open`) shares the `localStorage`, the cookies, the `indexedDB` stores and the permissions of the first
//! page, and has its own `sessionStorage` (`WebStorage::new_session`): a new tab of one browser. It shares the network client too.
//! One `Saver` writes the profile for all the pages. The shell owns it, so closing a page never stops the saves, and the drop of the
//! shell writes the last changes.
//!
//! `window.open(url)` and a link with `target=_blank` open a page and switch to it. The shell does it with the `agent` policy: the
//! policy records `Event::Tab`, and the shell reads the events after each input event and tick. The policy answers the dialogs as a
//! window with no policy does (`alert` does nothing, `confirm` gives false, `prompt` gives null); the shell drops the dialog events.
//!
//! The title of the window is `[i/n] <document.title or URL> — bare`. It changes when the page, its title or the count changes.
//!
//! The keys, the status line and the hints are in `shell_keys.rs` (the logic), `shell_page.rs` (what the keys do to a page) and
//! `shell_draw.rs` (the overlay).
//!
//! Gaps: `window.open` still gives `null` (the page gets no window object, so it cannot script the new page). A page has no
//! `window.opener`. There is no config file, no bookmark, no history file, no download list and no tab strip.

use crate::browser_navigation::WebStorage;
use crate::page::Page;
use bare_net::http::Client;
use crate::shell_draw::{self, Label, Overlay};
use crate::shell_keys::{self, Act, Cmd, Key, Keys, Mode};
use crate::shell_page::{self, Cand, Found, Move};
use bare_window::{App, Control, Event};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long a message stays in the status line.
const MESSAGE: Duration = Duration::from_secs(3);

/// What the next answer of the clipboard is for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Paste {
    Open,
    Line,
}

/// The text that `/` searched, and the match that is boxed now.
struct Search {
    needle: String,
    cur: usize,
    mark: Option<Found>,
    /// The document that the match belongs to (`Page::doc_key`).
    doc: usize,
}

/// One page of the list, with the URL that it was asked to load (the page has no document yet while it loads).
struct Slot {
    page: Page,
    asked: String,
}

pub struct Shell {
    pages: Vec<Slot>,
    cur: usize,
    net: Client,
    root: WebStorage,
    saver: Option<crate::profile::Saver>,
    size: (u32, u32),
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
    /// The title that the window shows, and what it was made from (the page, the count, the document key).
    shown: String,
    made: (usize, usize, (usize, u64, u64)),
    keys: Keys,
    /// The message of the status line, and when it goes.
    message: Option<(String, Instant)>,
    /// The elements that have a label in HINT mode.
    cands: Vec<Cand>,
    search: Option<Search>,
    /// Is the list of pages (`:buffers`) shown?
    listing: bool,
    want: Option<Paste>,
    painter: bare_paint::Painter,
    /// What the shell asked of the window (a `Control` cannot be read outside its crate): for the tests.
    #[cfg(test)]
    pub(crate) sent: Vec<String>,
}

impl Shell {
    /// A shell over its first page. The shell takes the saver of the page, and opens the next pages in `net`.
    pub fn new(mut first: Page, url: &str, net: Client, size: (u32, u32)) -> Shell {
        let saver = first.saver.take();
        let root = first.storage().unwrap_or_default();
        // The pages ask for tabs, and answer the dialogs as with no policy.
        crate::agent::set_policy(false, None);
        Shell { pages: vec![Slot { page: first, asked: url.to_string() }], cur: 0, net, root, saver, size, wake: None, shown: String::new(), made: (usize::MAX, 0, (0, 0, 0)), keys: Keys::new(), message: None, cands: Vec::new(), search: None, listing: false, want: None, painter: bare_paint::Painter::new(), #[cfg(test)] sent: Vec::new() }
    }

    /// Every page calls `wake` when a job ends, so the window loop sleeps until then (`Page::wake_on_done`).
    pub fn wake_on_done(&mut self, wake: impl Fn() + Send + Sync + 'static) {
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        for s in &mut self.pages {
            Shell::hook(&mut s.page, &wake);
        }
        self.wake = Some(wake);
    }

    fn hook(page: &mut Page, wake: &Arc<dyn Fn() + Send + Sync>) {
        let wake = wake.clone();
        page.wake_on_done(move || wake());
    }

    /// The index of the current page.
    pub fn current(&self) -> usize {
        self.cur
    }

    /// The current page.
    pub fn page(&mut self) -> &mut Page {
        &mut self.pages[self.cur].page
    }

    /// Page `i`.
    #[cfg(test)]
    pub(crate) fn page_at(&mut self, i: usize) -> &mut Page {
        &mut self.pages[i].page
    }

    /// How many pages there are.
    pub fn len(&self) -> usize {
        self.pages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    /// Open `url` in a new page and switch to it (`new_page`), or in the current page. A new page has its own `sessionStorage`; the
    /// current page that loads another URL ends its document (its history and its `sessionStorage` go with it).
    pub fn open(&mut self, url: &str, new_page: bool) -> Result<(), String> {
        let mut page = Page::open_shared(url, self.size, &self.net, &self.root)?;
        if let Some(w) = &self.wake {
            Shell::hook(&mut page, w);
        }
        let slot = Slot { page, asked: url.to_string() };
        if new_page {
            self.pages.push(slot);
            self.cur = self.pages.len() - 1;
        } else {
            self.pages[self.cur] = slot;
        }
        Ok(())
    }

    /// Show page `i` (an index that is out of range does nothing). Gives whether the current page changed.
    pub fn switch(&mut self, i: usize) -> bool {
        let moved = i < self.pages.len() && i != self.cur;
        if moved {
            self.cur = i;
        }
        moved
    }

    /// Show the next page, and the first after the last.
    pub fn next(&mut self) -> bool {
        self.switch((self.cur + 1) % self.pages.len())
    }

    /// Show the previous page, and the last before the first.
    pub fn prev(&mut self) -> bool {
        self.switch((self.cur + self.pages.len() - 1) % self.pages.len())
    }

    /// Close page `i`. The current page goes to the neighbor that takes its place (the one before it, when it was the last). Gives
    /// `true` when it was the last page: the window must close.
    pub fn close(&mut self, i: usize) -> bool {
        if i >= self.pages.len() {
            return false;
        }
        if self.pages.len() == 1 {
            return true;
        }
        self.pages.remove(i);
        if i < self.cur || self.cur == self.pages.len() {
            self.cur -= 1;
        }
        false
    }

    /// The index, the title and the URL of each page.
    pub fn list(&mut self) -> Vec<(usize, String, String)> {
        self.pages.iter_mut().enumerate().map(|(i, s)| (i, s.page.title(), s.page.url().unwrap_or_else(|| s.asked.clone()))).collect()
    }

    /// The title for the window: `[i/n] <document.title or URL> — bare`.
    pub fn title(&mut self) -> String {
        let (i, n) = (self.cur + 1, self.pages.len());
        let s = &mut self.pages[self.cur];
        let name = Some(s.page.title()).filter(|t| !t.is_empty()).or_else(|| s.page.url()).unwrap_or_else(|| s.asked.clone());
        format!("[{i}/{n}] {name} — bare")
    }

    /// Open a page for each tab that a page asked for, and switch to the last one. The policy ends with the read: set it again.
    fn tabs(&mut self) -> bool {
        let mut opened = false;
        for e in crate::agent::take() {
            if let crate::agent::Event::Tab(url) = e {
                match self.open(&url, true) {
                    Ok(()) => opened = true,
                    Err(e) => self.pages[self.cur].page.errors.push(format!("tab {url}: {e}")),
                }
            }
        }
        crate::agent::set_policy(false, None);
        opened
    }

    /// Tell the window what changed: the title, and a new frame when the current page changed.
    fn sync(&mut self, ctl: &mut Control, changed: bool) {
        if changed {
            ctl.redraw();
        }
        let key = (self.cur, self.pages.len(), self.pages[self.cur].page.doc_key());
        if key == self.made {
            return;
        }
        self.made = key;
        let t = self.title();
        if t != self.shown {
            ctl.set_title(t.clone());
            self.shown = t;
        }
    }
}

impl Shell {
    /// The mode of the keys.
    pub fn mode(&self) -> Mode {
        self.keys.mode
    }

    /// The message of the status line, while it shows.
    pub fn message(&self) -> Option<&str> {
        self.message.as_ref().map(|m| m.0.as_str())
    }

    fn sent(&mut self, _what: String) {
        #[cfg(test)]
        self.sent.push(_what);
    }

    fn quit(&mut self, ctl: &mut Control) {
        self.sent("close".into());
        ctl.close();
    }

    fn say(&mut self, text: impl Into<String>) {
        self.message = Some((text.into(), Instant::now() + MESSAGE));
    }

    /// The page may have given or taken the focus of an editable element: NORMAL and INSERT follow it.
    fn follow_focus(&mut self) {
        let editable = shell_page::editable_focus(self.page());
        self.keys.focus(editable);
    }

    /// The text of the clipboard answer: the first text type.
    fn text_of(items: &[(String, Vec<u8>)]) -> Option<String> {
        items.iter().find(|(m, _)| m.starts_with("text/")).map(|(_, b)| String::from_utf8_lossy(b).into_owned())
    }

    fn navigate(&mut self, url: Option<&str>) {
        if let Err(e) = self.page().navigate(url) {
            self.say(e);
        }
    }

    fn run(&mut self, line: &str, ctl: &mut Control) {
        match shell_keys::parse(line) {
            Err(e) => self.say(e),
            Ok(Cmd::Open(url)) => self.navigate(Some(&url)),
            Ok(Cmd::TabOpen(url)) => {
                if let Err(e) = self.open(&url, true) {
                    self.say(e);
                }
            }
            Ok(Cmd::Buffers) => self.listing = true,
            Ok(Cmd::Buffer(n)) => {
                if n > self.pages.len() {
                    self.say(format!("no page {n}"));
                } else {
                    self.switch(n - 1);
                }
            }
            Ok(Cmd::Bdelete(n)) => {
                let i = n.map_or(self.cur, |n| n - 1);
                if i >= self.pages.len() {
                    self.say(format!("no page {}", i + 1));
                } else if self.close(i) {
                    self.quit(ctl);
                }
            }
            Ok(Cmd::Reload) => self.navigate(None),
            Ok(Cmd::Back) => {
                self.page().history_go(-1);
            }
            Ok(Cmd::Forward) => {
                self.page().history_go(1);
            }
            Ok(Cmd::Quit) => self.quit(ctl),
        }
    }

    /// `/text`: select the first match after the top of the window.
    fn find(&mut self, needle: &str) {
        let found = shell_page::find_all(self.page(), needle);
        if found.is_empty() {
            self.search = Some(Search { needle: needle.to_string(), cur: 0, mark: None, doc: 0 });
            return self.say(format!("not found: {needle}"));
        }
        let cur = shell_page::first_after_top(self.page(), &found);
        self.show_match(needle, found, cur);
    }

    /// `n` and `N`: the search again (the page may have changed), and the next or previous match of it.
    fn step(&mut self, delta: i32) {
        let Some(needle) = self.search.as_ref().map(|s| s.needle.clone()).filter(|n| !n.is_empty()) else { return self.say("no search") };
        let found = shell_page::find_all(self.page(), &needle);
        if found.is_empty() {
            return self.say(format!("not found: {needle}"));
        }
        let last = self.search.as_ref().map_or(0, |s| s.cur) as i32;
        let cur = (last + delta).rem_euclid(found.len() as i32) as usize;
        self.show_match(&needle, found, cur);
    }

    fn show_match(&mut self, needle: &str, found: Vec<Found>, cur: usize) {
        shell_page::select(self.page(), &found[cur]);
        let n = found.len();
        let doc = self.page().doc_key().0;
        self.search = Some(Search { needle: needle.to_string(), cur, mark: Some(found[cur].clone()), doc });
        self.say(format!("[{}/{n}]", cur + 1));
    }

    /// Do what a key asked for.
    fn act(&mut self, act: Act, ctl: &mut Control) {
        match act {
            Act::Scroll(dy) => drop(shell_page::scroll(self.page(), Move::By(dy as f32))),
            Act::Half(d) => drop(shell_page::scroll(self.page(), Move::Half(d))),
            Act::Top => drop(shell_page::scroll(self.page(), Move::Top)),
            Act::Bottom => drop(shell_page::scroll(self.page(), Move::Bottom)),
            Act::Back => drop(self.page().history_go(-1)),
            Act::Forward => drop(self.page().history_go(1)),
            Act::Reload => self.navigate(None),
            Act::NextPage => drop(self.next()),
            Act::PrevPage => drop(self.prev()),
            Act::ClosePage => {
                if self.close(self.cur) {
                    self.quit(ctl);
                }
            }
            Act::CopyUrl => match self.page().url() {
                Some(url) => {
                    self.sent(format!("copy {url}"));
                    ctl.set_clipboard(vec![("text/plain".to_string(), Arc::from(url.as_bytes()))]);
                    self.say(format!("copied {url}"));
                }
                None => self.say("no URL yet"),
            },
            Act::PasteOpen | Act::PasteLine => {
                self.want = Some(if act == Act::PasteOpen { Paste::Open } else { Paste::Line });
                self.sent("paste".into());
                ctl.read_clipboard(&[bare_window::clipboard::TEXT]);
            }
            Act::OpenCurrent => {
                let url = self.page().url().unwrap_or_default();
                self.keys.command_line(&format!("open {url}"));
            }
            Act::Hints(_) => {
                self.cands = shell_page::candidates(self.page(), false);
                self.keys.set_labels(shell_keys::hint_labels(self.cands.len()));
                if self.cands.is_empty() {
                    self.say("nothing to click");
                }
            }
            Act::Pick(i, new_page) => {
                let Some(c) = self.cands.get(i).cloned() else { return };
                self.cands.clear();
                let link = c.href.as_deref().filter(|_| new_page && !c.field).and_then(|h| shell_page::resolve(self.page(), h));
                match link {
                    Some(url) => {
                        if let Err(e) = self.open(&url, true) {
                            self.say(e);
                        }
                    }
                    None => drop(shell_page::click(self.page(), c.rect)),
                }
            }
            Act::FocusField => match shell_page::candidates(self.page(), true).first() {
                Some(c) => drop(shell_page::click(self.page(), c.rect)),
                None => self.say("no text field"),
            },
            Act::Blur => {
                if let Some(b) = self.page().browser() {
                    let _ = b.blur();
                }
                self.page().touched();
            }
            Act::Run(line) => self.run(&line, ctl),
            Act::Find(text) => self.find(&text),
            Act::FindStep(d) => self.step(d),
            Act::Cancel => self.cands.clear(),
            Act::Redraw => {}
        }
    }

    /// The overlay for the frame now.
    fn overlay(&mut self) -> Overlay {
        let mut o = Overlay { insert: self.keys.mode == Mode::Insert, ..Overlay::default() };
        o.line = match self.keys.mode {
            Mode::Command | Mode::Find => Some((format!("{}{}", self.keys.prompt, self.keys.line), Some(1 + self.keys.caret))),
            Mode::Hint => Some((format!("-- HINT -- {}", self.keys.typed), None)),
            _ => self.message.as_ref().map(|m| (m.0.clone(), None)),
        };
        if self.keys.mode == Mode::Hint {
            let typed = self.keys.typed.len();
            o.labels = self.keys.live().filter_map(|(i, l)| self.cands.get(i).map(|c| Label { text: l[typed..].to_string(), x: c.rect[0], y: c.rect[1] })).collect();
        }
        if let Some(m) = self.search.as_ref().and_then(|s| s.mark.clone()) {
            o.find = shell_page::found_rect(self.page(), &m);
        }
        if self.listing {
            let cur = self.cur;
            o.pages = self.list().into_iter().map(|(i, t, u)| format!("{} {}  {}  {}", if i == cur { '*' } else { ' ' }, i + 1, if t.is_empty() { "(no title)" } else { &t }, u)).collect();
        }
        o
    }
}

impl App for Shell {
    fn resize(&mut self, width: u32, height: u32) {
        self.size = (width, height);
        for s in &mut self.pages {
            s.page.resize(width, height);
        }
    }

    fn frame(&mut self, ctl: &mut Control, out: &mut [u32]) -> bool {
        let drew = self.pages[self.cur].page.frame(ctl, out);
        if drew && (self.keys.mode != Mode::Normal || self.message.is_some() || self.search.as_ref().is_some_and(|s| s.mark.is_some()) || self.listing) {
            let o = self.overlay();
            let (w, h) = self.size;
            if !o.is_empty() {
                shell_draw::draw(&mut bare_paint::canvas::Canvas::over(w, h, out), &mut self.painter.glyphs, &o);
            }
        }
        drew
    }

    fn event(&mut self, event: Event, ctl: &mut Control) {
        let before = self.keys.mode;
        let mut ui = false;
        let mut focus = false;
        match event {
            Event::Key { ref key, pressed, ref mods, .. } => {
                if pressed && std::mem::take(&mut self.listing) {
                    ui = true;
                }
                if pressed && key == "Escape" && self.keys.mode == Mode::Normal && self.search.as_mut().is_some_and(|s| s.mark.take().is_some()) {
                    ui = true;
                }
                let out = self.keys.key(Key { key, ctrl: mods.ctrl, alt: mods.alt, meta: mods.meta }, pressed, Instant::now());
                ui |= !out.acts.is_empty() || (out.eat && self.keys.mode != Mode::Normal);
                if !out.eat {
                    self.pages[self.cur].page.event(event, ctl);
                }
                for act in out.acts {
                    self.act(act, ctl);
                }
                focus = pressed;
            }
            Event::Clipboard { ref items } if self.want.is_some() => {
                let text = Shell::text_of(items).unwrap_or_default();
                match self.want.take() {
                    Some(Paste::Line) => self.keys.insert_text(&text),
                    _ if text.trim().is_empty() => self.say("the clipboard has no text"),
                    _ => self.navigate(Some(&shell_keys::url_of(&text))),
                }
                ui = true;
            }
            // A click or the wheel moves the page under the labels.
            Event::PointerButton { .. } | Event::Wheel { .. } => {
                if self.keys.mode == Mode::Hint {
                    self.keys.reset();
                    self.cands.clear();
                    ui = true;
                }
                focus = matches!(event, Event::PointerButton { .. });
                self.pages[self.cur].page.event(event, ctl);
            }
            _ => self.pages[self.cur].page.event(event, ctl),
        }
        if focus {
            self.follow_focus();
        }
        ui |= before != self.keys.mode;
        let changed = self.tabs();
        self.sync(ctl, changed || ui);
    }

    fn tick(&mut self, ctl: &mut Control) -> Option<Instant> {
        let mut next: Option<Instant> = None;
        let mut redraw = false;
        for (i, s) in self.pages.iter_mut().enumerate() {
            let (ran, due) = s.page.tick();
            redraw |= ran && i == self.cur;
            next = [next, due].into_iter().flatten().min();
        }
        if redraw {
            self.follow_focus();
        }
        if let Some(due) = self.saver.as_mut().and_then(|s| s.tick(bare_net::vclock::instant())) {
            next = Some(next.map_or(due, |n| n.min(due)));
        }
        // The message goes, and the box of a match goes with the document it belonged to.
        match self.message {
            Some((_, until)) if until <= Instant::now() => {
                self.message = None;
                redraw = true;
            }
            Some((_, until)) => next = Some(next.map_or(until, |n| n.min(until))),
            None => {}
        }
        if self.search.as_ref().is_some_and(|s| s.mark.is_some() && s.doc != self.pages[self.cur].page.doc_key().0) {
            self.search = self.search.take().map(|s| Search { mark: None, ..s });
            redraw = true;
        }
        let changed = self.tabs();
        // A new page is due now: it starts to load at its first tick.
        if changed {
            next = Some(bare_net::vclock::instant());
        }
        self.sync(ctl, redraw || changed);
        next
    }
}
