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
//! - `CSS.registerProperty({name, syntax, inherits, initialValue})` adds an `@property` rule as the last sheet part of the document, so the
//!   cascade reads it like the rule of a style sheet (no rule is made: `RuleSet::registered`). `TypeError` for a missing `name`, `syntax`
//!   or `inherits`. `SyntaxError` for a name that is not `--x`, a syntax other than `*` with no `initialValue`, or an `initialValue` with
//!   `{`, `}` or `;`. `InvalidModificationError` for a name that an earlier call registered. Types are not checked, and a name that a
//!   style sheet `@property` has is not seen as a duplicate.
//! - Not here: the typed OM, `CSS.highlights`, the unit
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
    for (name, h) in [("supports", supports as H), ("escape", escape), ("registerProperty", register_property)] {
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
    let only = s.chars().count() == 1;
    let first = s.chars().next();
    for (i, c) in s.chars().enumerate() {
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
    d.js_str(it, &out)
}

fn register_property(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    let Some(&dict) = a.first().filter(|v| matches!(v, Value::Object(_))) else {
        return Err(it.type_error("Failed to execute 'registerProperty' on 'CSS': 1 argument required, but only 0 present."));
    };
    let member = |it: &mut Interp<'_>, name: &str| -> R<Option<Value>> {
        let k = d.key(it, name)?;
        Ok(Some(it.get_value(dict, k)?).filter(|v| !matches!(v, Value::Undefined)))
    };
    let name = member(it, "name")?;
    let syntax = member(it, "syntax")?;
    let inherits = member(it, "inherits")?;
    let initial = member(it, "initialValue")?;
    let (Some(name), Some(syntax), Some(inherits)) = (name, syntax, inherits) else {
        return Err(it.type_error("Failed to execute 'registerProperty' on 'CSS': name, syntax and inherits are required."));
    };
    let (name, syntax) = (d.string_arg(it, name)?, d.string_arg(it, syntax)?);
    let inherits = it.heap.to_boolean(inherits).map_err(|e| it.heap_error(e))?;
    let initial = match initial {
        Some(v) => Some(d.string_arg(it, v)?),
        None => None,
    };
    let bad = |s: &str| s.contains(['{', '}', ';']);
    if !name.starts_with("--") || name.len() == 2 || name.contains(|c: char| c.is_whitespace() || c == '{' || c == '}' || c == ';') || bad(&syntax) || syntax.contains('"') {
        return Err(d.dom_err(it, "SyntaxError", &format!("Failed to execute 'registerProperty' on 'CSS': Invalid property name or syntax '{name}'.")));
    }
    if initial.as_deref().is_some_and(|v| bad(v) || v.trim().is_empty()) || (initial.is_none() && syntax.trim() != "*") {
        return Err(d.dom_err(it, "SyntaxError", "Failed to execute 'registerProperty' on 'CSS': The initial value is missing or not valid."));
    }
    if name.len() + syntax.len() + initial.as_ref().map_or(0, |v| v.len()) > d.lim.max_selector_len {
        return Err(d.lim("dom-selector"));
    }
    it.charge(1 + (name.len() / 16) as u64)?;
    let mut all = d.computed.registered.borrow_mut();
    if all.iter().any(|r| r.0 == name) {
        return Err(d.dom_err(it, "InvalidModificationError", &format!("Failed to execute 'registerProperty' on 'CSS': The name '{name}' has already been registered.")));
    }
    let init = initial.map_or(String::new(), |v| format!("initial-value: {v};"));
    all.push((name.clone(), format!("@property {name} {{ syntax: \"{}\"; inherits: {inherits}; {init} }}\n", syntax.trim())));
    drop(all);
    d.computed.sheet_gen.set(d.computed.sheet_gen.get() + 1);
    Ok(Value::Undefined)
}
