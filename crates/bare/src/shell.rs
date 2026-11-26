//! The shell of the window: a list of pages in one window, and no browser chrome (no address bar, no tab strip, no toolbar).
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
//! Gaps: `window.open` still gives `null` (the page gets no window object, so it cannot script the new page). A page has no
//! `window.opener`. The pages have no keys yet: the shell has `open`, `switch`, `next`, `prev` and `close` for the key layer of the
//! next step.

use crate::browser_navigation::WebStorage;
use crate::page::Page;
use bare_net::http::Client;
use bare_window::{App, Control, Event};
use std::sync::Arc;
use std::time::Instant;

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
}

impl Shell {
    /// A shell over its first page. The shell takes the saver of the page, and opens the next pages in `net`.
    pub fn new(mut first: Page, url: &str, net: Client, size: (u32, u32)) -> Shell {
        let saver = first.saver.take();
        let root = first.storage().unwrap_or_default();
        // The pages ask for tabs, and answer the dialogs as with no policy.
        crate::agent::set_policy(false, None);
        Shell { pages: vec![Slot { page: first, asked: url.to_string() }], cur: 0, net, root, saver, size, wake: None, shown: String::new(), made: (usize::MAX, 0, (0, 0, 0)) }
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

impl App for Shell {
    fn resize(&mut self, width: u32, height: u32) {
        self.size = (width, height);
        for s in &mut self.pages {
            s.page.resize(width, height);
        }
    }

    fn frame(&mut self, ctl: &mut Control, out: &mut [u32]) -> bool {
        self.pages[self.cur].page.frame(ctl, out)
    }

    fn event(&mut self, event: Event, ctl: &mut Control) {
        let before = self.cur;
        self.pages[before].page.event(event, ctl);
        let changed = self.tabs();
        self.sync(ctl, changed);
    }

    fn tick(&mut self, ctl: &mut Control) -> Option<Instant> {
        let mut next: Option<Instant> = None;
        let mut redraw = false;
        for (i, s) in self.pages.iter_mut().enumerate() {
            let (ran, due) = s.page.tick();
            redraw |= ran && i == self.cur;
            next = [next, due].into_iter().flatten().min();
        }
        if let Some(due) = self.saver.as_mut().and_then(|s| s.tick(bare_net::vclock::instant())) {
            next = Some(next.map_or(due, |n| n.min(due)));
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
