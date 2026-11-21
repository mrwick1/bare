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

use crate::eng::{Eng, Ops};
use crate::browser::*;
use crate::browser_scoped::*;
use bare_js_api::{Args, Attrs, HostFn, Intrinsic, Local, Type};

pub(crate) fn install<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>) -> Res<'s, ()> {
    table!(lazy = [
        Row::both(1, "CSS", lazy),
    ]);
    let g = s.global();
    d.install_rows(s, &[g], &lazy::T::<E>::V)
}

/// The getter (no argument) and the setter (the new value) of the global.
fn lazy<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let g = s.global();
    let attrs = Attrs { writable: true, enumerable: false, configurable: true };
    if let Some(v) = a.get(0) {
        s.define_str(g, "CSS", v, attrs)?;
        return Ok(s.undefined());
    }
    let proto = s.engine().object_prototype();
    let ns = d.obj_with_s(s, proto)?;
    for (name, h) in [("supports", supports as HS<E>), ("escape", escape), ("registerProperty", register_property)] {
        let f = d.add_fn_s(s, h, name, 1, HostFn::Method)?;
        s.define_str(ns, name, f, Attrs::ALL)?;
    }
    let key = s.intrinsic(Intrinsic::SymbolToStringTag);
    let tag = d.js_str_s(s, "CSS")?;
    s.define(ns, key, tag, Attrs { writable: false, enumerable: false, configurable: true })?;
    s.define_str(g, "CSS", ns, attrs)?;
    Ok(ns)
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

fn supports<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let Some(first) = a.get(0) else {
        return Err(s.type_error("Failed to execute 'supports' on 'CSS': 1 argument required, but only 0 present."));
    };
    let first = d.string_arg_s(s, first)?;
    let text = match a.get(1) {
        Some(v) => {
            let value = d.string_arg_s(s, v)?;
            if !ident(&first) || !balanced(&value) {
                return Ok(s.boolean(false));
            }
            format!("({first}: {value})")
        }
        None => first,
    };
    if text.len() > d.lim.max_selector_len {
        return Err(d.lim_s(s, "dom-selector"));
    }
    s.charge(1 + (text.len() / 16) as u64)?;
    let mut doc = d.doc.borrow_mut();
    if doc.name_count() + text.len() > d.lim.max_names {
        return Err(d.lim_s(s, "dom-names"));
    }
    let mut yes = bare_css::at_rules::supports_matches(&mut doc, &text);
    // `display: grid` is not a condition: try `(display: grid)`
    if !yes && a.len() == 1 && !text.starts_with('(') && text.contains(':') && balanced(&text) {
        yes = bare_css::at_rules::supports_matches(&mut doc, &format!("({text})"));
    }
    Ok(s.boolean(yes))
}

/// CSSOM "serialize an identifier".
fn escape<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let Some(v) = a.get(0) else {
        return Err(s.type_error("Failed to execute 'escape' on 'CSS': 1 argument required, but only 0 present."));
    };
    let text = d.string_arg_s(s, v)?;
    s.charge(1 + (text.len() / 16) as u64)?;
    let mut out = String::with_capacity(text.len());
    let only = text.chars().count() == 1;
    let first = text.chars().next();
    for (i, c) in text.chars().enumerate() {
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
    d.js_str_s(s, &out)
}

fn register_property<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let Some(dict) = a.get(0).filter(|&v| is_object::<E>(s, v)) else {
        return Err(s.type_error("Failed to execute 'registerProperty' on 'CSS': 1 argument required, but only 0 present."));
    };
    let member = |s: &mut Sc<'s, E>, name: &str| -> Res<'s, Option<Local<'s>>> {
        let x = s.get_str(dict, name)?;
        Ok(Some(x).filter(|&x| s.type_of(x) != Type::Undefined))
    };
    let name = member(s, "name")?;
    let syntax = member(s, "syntax")?;
    let inherits = member(s, "inherits")?;
    let initial = member(s, "initialValue")?;
    let (Some(name), Some(syntax), Some(inherits)) = (name, syntax, inherits) else {
        return Err(s.type_error("Failed to execute 'registerProperty' on 'CSS': name, syntax and inherits are required."));
    };
    let (name, syntax) = (d.string_arg_s(s, name)?, d.string_arg_s(s, syntax)?);
    let inherits = s.to_boolean(inherits);
    let initial = match initial {
        Some(v) => Some(d.string_arg_s(s, v)?),
        None => None,
    };
    let bad = |s: &str| s.contains(['{', '}', ';']);
    if !name.starts_with("--") || name.len() == 2 || name.contains(|c: char| c.is_whitespace() || c == '{' || c == '}' || c == ';') || bad(&syntax) || syntax.contains('"') {
        return Err(d.dom_err_s(s, "SyntaxError", &format!("Failed to execute 'registerProperty' on 'CSS': Invalid property name or syntax '{name}'.")));
    }
    if initial.as_deref().is_some_and(|v| bad(v) || v.trim().is_empty()) || (initial.is_none() && syntax.trim() != "*") {
        return Err(d.dom_err_s(s, "SyntaxError", "Failed to execute 'registerProperty' on 'CSS': The initial value is missing or not valid."));
    }
    if name.len() + syntax.len() + initial.as_ref().map_or(0, |v| v.len()) > d.lim.max_selector_len {
        return Err(d.lim_s(s, "dom-selector"));
    }
    s.charge(1 + (name.len() / 16) as u64)?;
    let mut all = d.computed.registered.borrow_mut();
    if all.iter().any(|r| r.0 == name) {
        return Err(d.dom_err_s(s, "InvalidModificationError", &format!("Failed to execute 'registerProperty' on 'CSS': The name '{name}' has already been registered.")));
    }
    let init = initial.map_or(String::new(), |v| format!("initial-value: {v};"));
    all.push((name.clone(), format!("@property {name} {{ syntax: \"{}\"; inherits: {inherits}; {init} }}\n", syntax.trim())));
    drop(all);
    d.computed.sheet_gen.set(d.computed.sheet_gen.get() + 1);
    Ok(s.undefined())
}
