//! The clipboard keys: Ctrl+C, Ctrl+X, Ctrl+V, Ctrl+Insert, Shift+Delete and Shift+Insert, as the default action of a `keydown` that no
//! listener canceled. A child module of `browser_ui.rs`: it uses the edit helpers of that file.
//!
//! - Copy and cut fire `copy` or `cut` (a `ClipboardEvent`, trusted, bubbling, cancelable, composed) at the focused element or the body.
//!   Its `clipboardData` starts empty. Not canceled: the data is the selected text, from a text field or from `getSelection()` (a password
//!   field gives none, as in Chrome). Canceled: the data is what the page put with `setData`. The data waits for the window in
//!   `Browser::take_clipboard`. A cut that was not canceled then deletes the selection of an editable field or host (`deleteByCut`).
//! - Paste only says that it wants the clipboard (`Browser::take_paste_request`). The window reads it and gives the items to
//!   `Browser::ui_paste`. That fires `paste` with a read-only `DataTransfer` that holds the text as `text/plain` and the first image as a
//!   `File` (`image.png`, and so on). Not canceled: the text goes into the focused field (`insertFromPaste`: `beforeinput` with `data`,
//!   then `input`) or editing host (`beforeinput` with `dataTransfer` and no `data`, then `input`). A single-line field takes the text
//!   with each line break as a space.
//! - The events come from `shims/clipboard.js` (`browser_shims.rs`): the host asks its helper for the event and reads the data back.
//!   A page that defines its own `DataTransfer` before the shim ran gets no clipboard events.
//!
//! Gaps: no `navigator.clipboard`; no `text/html` on a copy (a page can still set it); a pasted image is never inserted into a host (the
//! page reads `files`); no menu or mouse paste.

use super::*;
use crate::eng::Val;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Cmd {
    Copy,
    Cut,
    Paste,
}

/// The clipboard command of a key, if it is one.
pub(crate) fn command(k: &KeyInput) -> Option<Cmd> {
    let m = k.mods;
    if m.alt || m.meta {
        return None;
    }
    match (k.key.as_str(), m.ctrl, m.shift) {
        ("c" | "C", true, false) | ("Insert", true, false) => Some(Cmd::Copy),
        ("x" | "X", true, false) | ("Delete", false, true) => Some(Cmd::Cut),
        ("v" | "V", true, _) | ("Insert", false, true) => Some(Cmd::Paste),
        _ => None,
    }
}

/// Text from the window clipboard, with the line ends of the web (`\n`).
fn text_of(items: &[(String, Vec<u8>)]) -> Option<String> {
    let (_, b) = items.iter().find(|(m, _)| bare_window::clipboard::TEXT.contains(&m.as_str()))?;
    Some(String::from_utf8_lossy(b).replace("\r\n", "\n").replace('\r', "\n"))
}

impl<E: Eng> Dom<E> {
    /// The helper of `shims/clipboard.js`. `None` when the page has its own `DataTransfer`.
    fn clip_helper(&self, it: &mut E::Vm) -> R<E, Option<E::Raw>> {
        if self.st.borrow().ui.clip.is_none() {
            crate::browser_shims::define(self, it, "DataTransfer")?;
        }
        let st = self.st.borrow();
        Ok(st.ui.clip.as_ref().map(|p| it.persistent_value(p)))
    }

    fn clip_call(&self, it: &mut E::Vm, recv: E::Raw, name: &str, args: &[E::Raw]) -> R<E, E::Raw> {
        let k = self.key(it, name)?;
        let f = it.get_value(recv, k)?;
        it.call_o(f, recv, args)
    }

    /// Dispatch the event `ev` (made by the helper) at `target` as a trusted event. False when a listener canceled it.
    fn clip_fire(&self, it: &mut E::Vm, target: NodeId, ev: E::Raw) -> R<E, bool> {
        let Val::Object(o) = E::kind(ev) else { return Ok(true) };
        if let Some(e) = self.st.borrow_mut().events.get_mut(&o) {
            e.trusted = true;
        }
        self.dispatch(it, Target::Node(target), o)
    }

    /// The default action of a clipboard key, after `keydown` was not canceled.
    pub(super) fn clip_key(&self, it: &mut E::Vm, cmd: Cmd) -> R<E, ()> {
        match cmd {
            Cmd::Paste => {
                self.st.borrow_mut().ui.paste_wanted = true;
                Ok(())
            }
            Cmd::Copy => self.clip_copy(it, false),
            Cmd::Cut => self.clip_copy(it, true),
        }
    }

    fn clip_copy(&self, it: &mut E::Vm, cut: bool) -> R<E, ()> {
        let Some(h) = self.clip_helper(it)? else { return Ok(()) };
        let target = self.focus_target(it)?;
        let ty = self.js_str(it, if cut { "cut" } else { "copy" })?;
        let ev = self.clip_call(it, h, "copy", &[ty])?;
        let text = self.selected_text(it)?;
        let go = self.clip_fire(it, target, ev)?;
        let items = if go {
            if text.is_empty() { Vec::new() } else { vec![("text/plain".to_string(), text.clone().into_bytes())] }
        } else {
            let flat = self.clip_call(it, h, "read", &[ev])?;
            self.string_pairs(it, flat)?
        };
        if !items.is_empty() {
            self.st.borrow_mut().ui.clip_out = Some(items);
        }
        if cut && go && !text.is_empty() {
            self.cut_default(it)?;
        }
        Ok(())
    }

    /// The array `[a, b, c, d]` of the helper as the pairs `(a, b)`, `(c, d)`, as bytes.
    fn string_pairs(&self, it: &mut E::Vm, flat: E::Raw) -> R<E, Vec<(String, Vec<u8>)>> {
        let k = self.key(it, "length")?;
        let n = it.get_value(flat, k)?;
        let n = it.to_js_number(n)? as usize;
        let mut strs = Vec::with_capacity(n);
        for i in 0..n {
            let k = self.key(it, &i.to_string())?;
            let v = it.get_value(flat, k)?;
            let s = it.to_js_string(v)?;
            strs.push(it.h_to_string_lossy(s)?);
        }
        Ok(strs.chunks_exact(2).map(|p| (p[0].clone(), p[1].clone().into_bytes())).collect())
    }

    /// The text that a copy takes: the selected part of the focused text field, else the selection of the page. A password gives none.
    fn selected_text(&self, it: &mut E::Vm) -> R<E, String> {
        if let Some(f) = self.current_focus(it)?.filter(|&f| self.selection_applies(f)) {
            if self.input_kind_of(f) == Some("password") {
                return Ok(String::new());
            }
            let Some((sel, len)) = self.effective(None, f)? else { return Ok(String::new()) };
            let units: Vec<u16> = self.input_value_of(f).unwrap_or_default().encode_utf16().collect();
            let (a, b) = (sel.start.min(len) as usize, sel.end.min(len) as usize);
            return Ok(String::from_utf16_lossy(&units[a.min(b)..a.max(b)]));
        }
        let docv = E::obj_raw(*self.doc_wrapper.get().expect("installed"));
        let sel = self.clip_call(it, docv, "getSelection", &[])?;
        let s = self.clip_call(it, sel, "toString", &[])?;
        let s = it.to_js_string(s)?;
        it.h_to_string_lossy(s)
    }

    /// The default action of a cut: delete the selected text of the focused field or editing host.
    fn cut_default(&self, it: &mut E::Vm) -> R<E, ()> {
        let Some(f) = self.current_focus(it)? else { return Ok(()) };
        if let Some(host) = self.editing_host(f) {
            if self.input_ev(it, host, "beforeinput", "deleteByCut", None)? {
                self.delete_selected(it, "deleteByCut")?;
            }
        } else if self.editable(f) {
            self.edit(it, f, "deleteByCut", None)?;
        }
        Ok(())
    }

    /// The window gave the clipboard after a paste key: fire `paste`, and insert the text when no listener canceled it.
    pub(crate) fn clip_paste(&self, it: &mut E::Vm, items: &[(String, Vec<u8>)]) -> R<E, ()> {
        let Some(h) = self.clip_helper(it)? else { return Ok(()) };
        let text = text_of(items);
        let image = items.iter().find(|(m, _)| m.starts_with("image/"));
        let t = match &text {
            Some(t) => self.js_str(it, t)?,
            None => E::null(),
        };
        let (bin, mime) = match image {
            // The bytes travel as a string of code units below 256; the helper turns them into a `File`.
            Some((m, b)) => (self.js_str(it, &b.iter().map(|&c| c as char).collect::<String>())?, self.js_str(it, m)?),
            None => (E::null(), E::null()),
        };
        let ev = self.clip_call(it, h, "paste", &[t, bin, mime])?;
        let k = self.key(it, "clipboardData")?;
        let dt = it.get_value(ev, k)?;
        let keep = it.persist_value(dt);
        self.st.borrow_mut().ui.paste_dt = Some(keep);
        let target = self.focus_target(it)?;
        let r = self.clip_fire(it, target, ev).and_then(|go| match &text {
            Some(text) if go => self.paste_text(it, text),
            _ => Ok(()),
        });
        self.st.borrow_mut().ui.paste_dt = None;
        r
    }

    fn paste_text(&self, it: &mut E::Vm, text: &str) -> R<E, ()> {
        let Some(f) = self.current_focus(it)? else { return Ok(()) };
        if let Some(host) = self.editing_host(f) {
            self.caret_into_host(it, f)?;
            if self.input_ev(it, host, "beforeinput", "insertFromPaste", None)? {
                self.insert_text_as(it, text, "insertFromPaste")?;
            }
        } else if self.editable(f) {
            let line;
            let text = if is_tag(&self.doc.borrow(), f, "textarea") {
                text
            } else {
                line = text.replace('\n', " ");
                &line
            };
            self.edit(it, f, "insertFromPaste", Some(text))?;
        }
        Ok(())
    }
}

impl<E: Eng> Browser<E> {
    /// The data that the last copy or cut put on the clipboard (MIME type and bytes), once.
    pub fn take_clipboard(&mut self) -> Option<Vec<(String, Vec<u8>)>> {
        self.dom.st.borrow_mut().ui.clip_out.take()
    }

    /// Whether a paste key waits for the clipboard, once. The window reads the clipboard, then calls `ui_paste`.
    pub fn take_paste_request(&mut self) -> bool {
        std::mem::take(&mut self.dom.st.borrow_mut().ui.paste_wanted)
    }

    /// The clipboard that a paste key asked for: fire `paste` and insert the text. `items` may be empty (nothing to paste).
    pub fn ui_paste(&mut self, items: Vec<(String, Vec<u8>)>) -> R<E, UiOutcome> {
        self.ui_call(true, |d, it| d.clip_paste(it, &items))
    }
}
