//! The data methods of `CharacterData` (`substringData`, `appendData`, `insertData`, `deleteData`, `replaceData`), `Text.splitText` and
//! `Node.normalize`. Source: DOM Standard, "Interface CharacterData", "Interface Text" and "normalize", then compared with headless Chrome
//! (`tools/fixtures/ui/liverange-cases.txt`).
//!
//! Offsets and counts are UTF-16 units over text that the host stores as UTF-8, so a cut in the middle of a surrogate pair gives U+FFFD (the
//! same known deviation as `Range.toString`). Each method that changes text tells the live ranges first (`Dom::range_data`, `range_split`,
//! `range_merge`, see `browser_range.rs`); when no range exists it does no more work for that.

use crate::eng::Eng;
use crate::browser::*;
use crate::browser_scoped::*;
use bare_js_api::{Args, Local};
use crate::browser_range::utf16_len;
use bare_html::dom::NodeData;
use crate::js_num::to_uint32;

type V<E: Eng> = R<E, E::Raw>;

/// The node and its text, for a Text or a Comment.
fn data_of<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, (bare_html::dom::NodeId, String)> {
    let n = d.nd_s(s, this)?;
    let doc = d.doc.borrow();
    match *doc.data(n) {
        NodeData::Text(st) | NodeData::Comment(st) => Ok((n, doc.text(st).to_string())),
        _ => Err(s.type_error("Illegal invocation")),
    }
}

fn args<'s, E: Eng>(s: &mut Sc<'s, E>, who: &str, iface: &str, a: Args<'s>, need: usize) -> Res<'s, ()> {
    if a.len() < need {
        return Err(s.type_error(&format!("Failed to execute '{who}' on '{iface}': {need} argument{} required, but only {} present.", if need == 1 { "" } else { "s" }, a.len())));
    }
    Ok(())
}

fn offset<'s, E: Eng>(s: &mut Sc<'s, E>, a: Args<'s>, i: usize) -> Res<'s, u32> {
    let argi = arg_s::<E>(s, a, i);
    Ok(to_uint32(s.to_number(argi)?))
}

fn out_of_range<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, who: &str, iface: &str, off: u32, len: u32) -> bare_js_api::Error<'s> {
    d.dom_err_s(s, "IndexSizeError", &out_of_range_msg(who, iface, off, len))
}

fn out_of_range_msg(who: &str, iface: &str, off: u32, len: u32) -> String {
    if iface == "Text" {
        format!("Failed to execute '{who}' on 'Text': The offset {off} is larger than the Text node's length.")
    } else {
        format!("Failed to execute '{who}' on '{iface}': The offset {off} is greater than the node's length ({len}).")
    }
}

/// `replaceData(offset, count, data)` on the text of `n`: the live ranges first, then the text.
pub(crate) fn replace<E: Eng>(d: &Dom<E>, it: &mut E::Vm, who: &str, (n, text): (bare_html::dom::NodeId, &str), off: u32, count: u32, data: &str) -> V<E> {
    let u: Vec<u16> = text.encode_utf16().collect();
    let len = u.len() as u32;
    if off > len {
        return Err(d.dom_err(it, "IndexSizeError", &out_of_range_msg(who, "CharacterData", off, len)));
    }
    let count = count.min(len - off);
    let data16: Vec<u16> = data.encode_utf16().collect();
    let new = String::from_utf16_lossy(&[&u[..off as usize], &data16, &u[(off + count) as usize..]].concat());
    {
        let mut doc = d.doc.borrow_mut();
        d.room(&doc, new.len())?;
        d.range_data(n, off, count, data16.len() as u32);
        doc.set_text(n, &new);
    }
    Ok(E::undef())
}

pub(crate) fn substring_data<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let (_, text) = data_of(d, s, this)?;
    args::<E>(s, "substringData", "CharacterData", a, 2)?;
    let (off, count) = (offset::<E>(s, a, 0)?, offset::<E>(s, a, 1)?);
    let u: Vec<u16> = text.encode_utf16().collect();
    if off as usize > u.len() {
        return Err(out_of_range(d, s, "substringData", "CharacterData", off, u.len() as u32));
    }
    let end = (off as usize).saturating_add(count as usize).min(u.len());
    d.js_str_s(s, &String::from_utf16_lossy(&u[off as usize..end]))
}

pub(crate) fn append_data<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let (n, text) = data_of(d, s, this)?;
    args::<E>(s, "appendData", "CharacterData", a, 1)?;
    let data = d.arg_str_s(s, a, 0)?;
    old_v::<E>(s, |it| replace(d, it, "appendData", (n, &text), utf16_len(&text), 0, &data))
}

pub(crate) fn insert_data<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let (n, text) = data_of(d, s, this)?;
    args::<E>(s, "insertData", "CharacterData", a, 2)?;
    let off = offset::<E>(s, a, 0)?;
    let data = d.arg_str_s(s, a, 1)?;
    old_v::<E>(s, |it| replace(d, it, "insertData", (n, &text), off, 0, &data))
}

pub(crate) fn delete_data<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let (n, text) = data_of(d, s, this)?;
    args::<E>(s, "deleteData", "CharacterData", a, 2)?;
    let (off, count) = (offset::<E>(s, a, 0)?, offset::<E>(s, a, 1)?);
    old_v::<E>(s, |it| replace(d, it, "deleteData", (n, &text), off, count, ""))
}

pub(crate) fn replace_data<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let (n, text) = data_of(d, s, this)?;
    args::<E>(s, "replaceData", "CharacterData", a, 3)?;
    let (off, count) = (offset::<E>(s, a, 0)?, offset::<E>(s, a, 1)?);
    let data = d.arg_str_s(s, a, 2)?;
    old_v::<E>(s, |it| replace(d, it, "replaceData", (n, &text), off, count, &data))
}

pub(crate) fn split_text<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let (n, text) = data_of(d, s, this)?;
    args::<E>(s, "splitText", "Text", a, 1)?;
    let off = offset::<E>(s, a, 0)?;
    if off > utf16_len(&text) {
        return Err(out_of_range(d, s, "splitText", "Text", off, utf16_len(&text)));
    }
    let new = lift::<E, _>(s, split_at(d, n, &text, off))?;
    d.wrap_s(s, new)
}

/// Split the text node `n` (its text is `text`) at `off` UTF-16 units, which are in range. The order is Chrome's: the data of `n` first (one record),
/// then the new node in (another one). The live ranges follow as the standard says.
pub(crate) fn split_at<E: Eng>(d: &Dom<E>, n: bare_html::dom::NodeId, text: &str, off: u32) -> R<E, bare_html::dom::NodeId> {
    let u: Vec<u16> = text.encode_utf16().collect();
    let (head, tail) = (String::from_utf16_lossy(&u[..off as usize]), String::from_utf16_lossy(&u[off as usize..]));
    let mut doc = d.doc.borrow_mut();
    d.room(&doc, text.len() + 3)?;
    let new = doc.create_text(&tail);
    doc.set_text(n, &head);
    if let Some(p) = doc.parent(n) {
        let next = doc.next_sibling(n);
        doc.move_to(new, p, next);
        d.range_split(&doc, n, new, off);
    } else {
        // No parent: the points that stay after `offset` go to `offset`.
        d.range_data(n, off, u.len() as u32 - off, 0);
    }
    Ok(new)
}

/// `Node.normalize()`: the runs of Text nodes below `this` become one node each, and the empty ones go.
pub(crate) fn normalize<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let root = d.nd_s(s, this)?;
    let mut doc = d.doc.borrow_mut();
    let mut texts = Vec::new();
    let mut stack: Vec<_> = doc.children(root).collect();
    stack.reverse();
    while let Some(n) = stack.pop() {
        s.charge(1)?;
        if matches!(doc.data(n), NodeData::Text(_)) {
            texts.push(n);
        }
        let first = stack.len();
        stack.extend(doc.children(n));
        stack[first..].reverse();
    }
    for t in texts {
        // A text that an earlier run took in has no parent now.
        let NodeData::Text(sp) = *doc.data(t) else { continue };
        if doc.parent(t).is_none() {
            continue;
        }
        let mut len = utf16_len(doc.text(sp));
        if len == 0 {
            doc.detach(t);
            continue;
        }
        let mut run = Vec::new();
        let mut cur = doc.next_sibling(t);
        while let Some(c) = cur
            && let NodeData::Text(st) = *doc.data(c)
        {
            run.push((c, st));
            cur = doc.next_sibling(c);
        }
        if run.is_empty() {
            continue;
        }
        let mut joined = doc.text(sp).to_string();
        d.room_s(s, &doc, joined.len() + run.iter().map(|&(_, st)| doc.text(st).len()).sum::<usize>())?;
        for &(c, st) in &run {
            // As in Chrome, an empty text only leaves (the standard also moves the points that sit on it).
            if doc.text(st).is_empty() {
                continue;
            }
            d.range_merge(&doc, t, c, len);
            joined += doc.text(st);
            len += utf16_len(doc.text(st));
        }
        doc.set_text(t, &joined);
        for (c, _) in run {
            doc.detach(c);
        }
    }
    Ok(s.undefined())
}
