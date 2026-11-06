//! The data methods of `CharacterData` (`substringData`, `appendData`, `insertData`, `deleteData`, `replaceData`), `Text.splitText` and
//! `Node.normalize`. Source: DOM Standard, "Interface CharacterData", "Interface Text" and "normalize", then compared with headless Chrome
//! (`tools/fixtures/ui/liverange-cases.txt`).
//!
//! Offsets and counts are UTF-16 units over text that the host stores as UTF-8, so a cut in the middle of a surrogate pair gives U+FFFD (the
//! same known deviation as `Range.toString`). Each method that changes text tells the live ranges first (`Dom::range_data`, `range_split`,
//! `range_merge`, see `browser_range.rs`); when no range exists it does no more work for that.

use crate::browser::*;
use crate::browser_dom::arg;
use crate::browser_range::utf16_len;
use bare_html::dom::NodeData;
use bare_js::eval::{Error, Interp};
use bare_js::value::{to_uint32, Value};

type V = R<Value>;

/// The node and its text, for a Text or a Comment.
fn data_of(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<(bare_html::dom::NodeId, String)> {
    let n = d.nd(it, this)?;
    let doc = d.doc.borrow();
    match *doc.data(n) {
        NodeData::Text(s) | NodeData::Comment(s) => Ok((n, doc.text(s).to_string())),
        _ => Err(it.type_error("Illegal invocation")),
    }
}

fn args(it: &mut Interp<'_>, who: &str, iface: &str, a: &[Value], need: usize) -> R<()> {
    if a.len() < need {
        return Err(it.type_error(&format!("Failed to execute '{who}' on '{iface}': {need} argument{} required, but only {} present.", if need == 1 { "" } else { "s" }, a.len())));
    }
    Ok(())
}

fn offset(it: &mut Interp<'_>, a: &[Value], i: usize) -> R<u32> {
    Ok(to_uint32(it.to_js_number(arg(a, i))?))
}

fn out_of_range(d: &Dom, it: &mut Interp<'_>, who: &str, iface: &str, off: u32, len: u32) -> Error {
    let m = if iface == "Text" {
        format!("Failed to execute '{who}' on 'Text': The offset {off} is larger than the Text node's length.")
    } else {
        format!("Failed to execute '{who}' on '{iface}': The offset {off} is greater than the node's length ({len}).")
    };
    d.dom_err(it, "IndexSizeError", &m)
}

/// `replaceData(offset, count, data)` on the text of `n`: the live ranges first, then the text.
fn replace(d: &Dom, it: &mut Interp<'_>, who: &str, (n, text): (bare_html::dom::NodeId, &str), off: u32, count: u32, data: &str) -> V {
    let u: Vec<u16> = text.encode_utf16().collect();
    let len = u.len() as u32;
    if off > len {
        return Err(out_of_range(d, it, who, "CharacterData", off, len));
    }
    let count = count.min(len - off);
    let data16: Vec<u16> = data.encode_utf16().collect();
    let new = String::from_utf16_lossy(&[&u[..off as usize], &data16, &u[(off + count) as usize..]].concat());
    {
        let mut doc = d.doc.borrow_mut();
        d.room(&doc, 0, new.len())?;
        d.range_data(n, off, count, data16.len() as u32);
        doc.set_text(n, &new);
    }
    Ok(Value::Undefined)
}

pub(crate) fn substring_data(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let (_, text) = data_of(d, it, this)?;
    args(it, "substringData", "CharacterData", a, 2)?;
    let (off, count) = (offset(it, a, 0)?, offset(it, a, 1)?);
    let u: Vec<u16> = text.encode_utf16().collect();
    if off as usize > u.len() {
        return Err(out_of_range(d, it, "substringData", "CharacterData", off, u.len() as u32));
    }
    let end = (off as usize).saturating_add(count as usize).min(u.len());
    d.js_str(it, &String::from_utf16_lossy(&u[off as usize..end]))
}

pub(crate) fn append_data(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let (n, text) = data_of(d, it, this)?;
    args(it, "appendData", "CharacterData", a, 1)?;
    let data = d.string_arg(it, arg(a, 0))?;
    replace(d, it, "appendData", (n, &text), utf16_len(&text), 0, &data)
}

pub(crate) fn insert_data(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let (n, text) = data_of(d, it, this)?;
    args(it, "insertData", "CharacterData", a, 2)?;
    let off = offset(it, a, 0)?;
    let data = d.string_arg(it, arg(a, 1))?;
    replace(d, it, "insertData", (n, &text), off, 0, &data)
}

pub(crate) fn delete_data(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let (n, text) = data_of(d, it, this)?;
    args(it, "deleteData", "CharacterData", a, 2)?;
    let (off, count) = (offset(it, a, 0)?, offset(it, a, 1)?);
    replace(d, it, "deleteData", (n, &text), off, count, "")
}

pub(crate) fn replace_data(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let (n, text) = data_of(d, it, this)?;
    args(it, "replaceData", "CharacterData", a, 3)?;
    let (off, count) = (offset(it, a, 0)?, offset(it, a, 1)?);
    let data = d.string_arg(it, arg(a, 2))?;
    replace(d, it, "replaceData", (n, &text), off, count, &data)
}

pub(crate) fn split_text(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let (n, text) = data_of(d, it, this)?;
    args(it, "splitText", "Text", a, 1)?;
    let off = offset(it, a, 0)?;
    let u: Vec<u16> = text.encode_utf16().collect();
    if off as usize > u.len() {
        return Err(out_of_range(d, it, "splitText", "Text", off, u.len() as u32));
    }
    let (head, tail) = (String::from_utf16_lossy(&u[..off as usize]), String::from_utf16_lossy(&u[off as usize..]));
    let new = {
        let mut doc = d.doc.borrow_mut();
        d.room(&doc, 1, text.len() + 3)?;
        let new = doc.create_text(&tail);
        if let Some(p) = doc.parent(n) {
            let next = doc.next_sibling(n);
            doc.move_to(new, p, next);
            d.range_split(&doc, n, new, off);
        }
        // The points that stay after `offset` (no parent: all of them) go to `offset`.
        d.range_data(n, off, u.len() as u32 - off, 0);
        doc.set_text(n, &head);
        new
    };
    d.wrap(it, new)
}

/// `Node.normalize()`: the runs of Text nodes below `this` become one node each, and the empty ones go.
pub(crate) fn normalize(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let root = d.nd(it, this)?;
    let mut doc = d.doc.borrow_mut();
    let mut texts = Vec::new();
    let mut stack: Vec<_> = doc.children(root).collect();
    stack.reverse();
    while let Some(n) = stack.pop() {
        it.charge(1)?;
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
            && let NodeData::Text(s) = *doc.data(c)
        {
            run.push((c, s));
            cur = doc.next_sibling(c);
        }
        if run.is_empty() {
            continue;
        }
        let mut joined = doc.text(sp).to_string();
        d.room(&doc, 0, joined.len() + run.iter().map(|&(_, s)| doc.text(s).len()).sum::<usize>())?;
        for &(c, s) in &run {
            // As in Chrome, an empty text only leaves (the standard also moves the points that sit on it).
            if doc.text(s).is_empty() {
                continue;
            }
            d.range_merge(&doc, t, c, len);
            joined += doc.text(s);
            len += utf16_len(doc.text(s));
        }
        doc.set_text(t, &joined);
        for (c, _) in run {
            doc.detach(c);
        }
    }
    Ok(Value::Undefined)
}
