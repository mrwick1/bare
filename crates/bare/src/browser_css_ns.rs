//! The `CSS` namespace (CSSOM, CSS Conditional Rules): `CSS.supports` and `CSS.escape`.
//!
//! Observed need: old.reddit.com reads `CSS.supports(...)` at load and stopped on `ReferenceError: CSS is not defined`.
//!
//! - `CSS.supports(conditionText)` answers like an `@supports` prelude: it calls `bare_css::at_rules::supports_matches`, so the
//!   page and a style sheet get the same answer. A text that is not a condition (`display: grid`) is tried again inside
//!   parentheses, as the browsers do. `CSS.supports(property, value)` asks for `(property: value)`. A property that is not an
//!   identifier, or a value with unbalanced parentheses, is `false`: it cannot widen the condition.
//! - `CSS.escape(ident)` is "serialize an identifier" (CSSOM): U+0000 is U+FFFD; controls are `\hex `; a digit first, or after a leading
//!   `-`, is `\3N `; a lone `-` is `\-`; other characters outside `[A-Za-z0-9_-]` and above U+0080 get a backslash.
//! - Not here: `registerProperty` (the typed OM and Houdini; the property stays undefined), `CSS.highlights`, the unit
//!   functions (`CSS.px`) and `CSS.paintWorklet`.
//!
//! Lazy: the global `CSS` is an accessor until the first read or write (as `crypto` and `Intl`). A page that never reads it pays for
//! one function. The first read makes the namespace and replaces the accessor with a data property (writable, configurable, not
//! enumerable, as WebIDL says for a namespace).
//!
//! Bounds: a condition text is at most `max_selector_len` bytes (`Limit(Host("dom-selector"))`, as a selector is), and a
//! `selector()` term is not parsed when it would take the document past `max_names`. Each call is charged by its text size.
//! A lone surrogate in an argument is `Unsupported`, as for every DOM string (`string_arg`).

use crate::browser::*;
use bare_js::eval::Interp;
use bare_js::heap::PropertyDescriptor;
use bare_js::value::{PropertyKey, Value};

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    let g = it.global_object();
    let f = d.add_fn(it, lazy, "CSS", 0, false)?;
    let k = d.key(it, "CSS")?;
    let desc = PropertyDescriptor { get: Some(Some(f)), set: Some(Some(f)), enumerable: Some(false), configurable: Some(true), ..Default::default() };
    it.heap.define_own_property(g, k, &desc).map_err(|e| it.heap_error(e))?;
    Ok(())
}

/// The getter (no argument) and the setter (the new value) of the global.
fn lazy(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    let g = it.global_object();
    let k = d.key(it, "CSS")?;
    if let Some(&v) = a.first() {
        it.define_data(g, k, v, true, false, true)?;
        return Ok(Value::Undefined);
    }
    let ns = d.obj(it, it.object_prototype())?;
    for (name, h) in [("supports", supports as H), ("escape", escape)] {
        let key = d.key(it, name)?;
        let f = d.add_fn(it, h, name, 1, false)?;
        it.define_data(ns, key, Value::Object(f), true, true, true)?;
    }
    let (ks, kt) = (d.key(it, "Symbol")?, d.key(it, "toStringTag")?);
    let sym = it.get_value(Value::Object(g), ks)?;
    if let Value::Symbol(s) = it.get_value(sym, kt)? {
        let tag = d.js_str(it, "CSS")?;
        it.define_data(ns, PropertyKey::Symbol(s), tag, false, false, true)?;
    }
    it.define_data(g, k, Value::Object(ns), true, false, true)?;
    Ok(Value::Object(ns))
}

/// True when every `(` has its `)` and none comes first.
fn balanced(s: &str) -> bool {
    let mut depth = 0usize;
    for c in s.chars() {
        match c {
            '(' => depth += 1,
            ')' => match depth.checked_sub(1) {
                Some(n) => depth = n,
                None => return false,
            },
            _ => {}
        }
    }
    depth == 0
}

fn ident(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || !c.is_ascii())
}

fn supports(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    let Some(&first) = a.first() else {
        return Err(it.type_error("Failed to execute 'supports' on 'CSS': 1 argument required, but only 0 present."));
    };
    let first = d.string_arg(it, first)?;
    let text = match a.get(1) {
        Some(&v) => {
            let value = d.string_arg(it, v)?;
            if !ident(&first) || !balanced(&value) {
                return Ok(Value::Bool(false));
            }
            format!("({first}: {value})")
        }
        None => first,
    };
    if text.len() > d.lim.max_selector_len {
        return Err(d.lim("dom-selector"));
    }
    it.charge(1 + (text.len() / 16) as u64)?;
    let mut doc = d.doc.borrow_mut();
    if doc.name_count() + text.len() > d.lim.max_names {
        return Err(d.lim("dom-names"));
    }
    let mut yes = bare_css::at_rules::supports_matches(&mut doc, &text);
    // `display: grid` is not a condition: try `(display: grid)`
    if !yes && a.len() == 1 && !text.starts_with('(') && text.contains(':') && balanced(&text) {
        yes = bare_css::at_rules::supports_matches(&mut doc, &format!("({text})"));
    }
    Ok(Value::Bool(yes))
}

/// CSSOM "serialize an identifier".
fn escape(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    let Some(&v) = a.first() else {
        return Err(it.type_error("Failed to execute 'escape' on 'CSS': 1 argument required, but only 0 present."));
    };
    let s = d.string_arg(it, v)?;
    it.charge(1 + (s.len() / 16) as u64)?;
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().enumerate().peekable();
    let only = s.chars().count() == 1;
    let first = s.chars().next();
    while let Some((i, c)) = chars.next() {
        match c {
            '\0' => out.push('\u{FFFD}'),
            '\u{1}'..='\u{1F}' | '\u{7F}' => out.push_str(&format!("\\{:x} ", c as u32)),
            '0'..='9' if i == 0 || (i == 1 && first == Some('-')) => out.push_str(&format!("\\{:x} ", c as u32)),
            '-' if only => out.push_str("\\-"),
            c if c >= '\u{80}' || c == '-' || c == '_' || c.is_ascii_alphanumeric() => out.push(c),
            c => {
                out.push('\\');
                out.push(c);
            }
        }
    }
    Ok(d.js_str(it, &out)?)
}
