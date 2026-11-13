//! Attribute reflection (HTML Standard "Reflecting content attributes in IDL attributes"): an IDL property that reads and writes one content
//! attribute. The `reflect!` macro defines a list of them on a prototype, one line each, so a module states its reflected members as a table.
//!
//! Cause: pages read standard members such as `table.rows`, `input.placeholder` or `meta.content`; a missing one is `undefined`, and the page
//! breaks quietly (a `TypeError` later, or a wrong value sent to the server).
//!
//! Kinds (`Kind`):
//! - `Str`: the attribute, else `""`. The setter writes the string.
//! - `Bool`: the attribute is there. The setter writes `""` or removes it.
//! - `Url`: the attribute resolved against the base URL (`browser_elements::base`), else the attribute text when it does not parse, else `""`.
//! - `UrlOrDoc`: like `Url`, but a missing or empty attribute gives the document URL (`form.action`).
//! - `Long(def, lo)`: the attribute by the "rules for parsing integers"; no number or one below `lo` gives `def`. The setter writes `ToInt32`;
//!   when `lo >= 0`, a value below `lo` is an `IndexSizeError` ("limited to only non-negative numbers", or positive ones for `lo == 1`).
//! - `Clamp(def, lo, hi)`: a non-negative integer clamped to `[lo, hi]`; no number gives `def` (`colSpan`, `rowSpan`).
//! - `Enum(words, def)`: the attribute when it is one of `words` (ASCII case-insensitive), in the case of `words`; else `def`.
//! - `Cors`: `crossOrigin`. No attribute is `null`, `use-credentials` is itself, any other value is `anonymous`. The setter takes `null`
//!   to remove the attribute.
//!
//! Each getter checks that `this` is an HTML element of one of the listed tags, else `TypeError: Illegal invocation`, as a browser does.

use crate::browser::*;
use crate::browser_platform::arg;
use bare_html::dom::NodeId;
use bare_js::eval::Interp;
use bare_js::value::Value;

/// How a property reads its attribute. See the header. `Copy`: a small value that is copied like a JS number, not moved.
#[derive(Clone, Copy)]
pub(crate) enum Kind {
    Str,
    Bool,
    Url,
    UrlOrDoc,
    Long(i32, i32),
    Clamp(u32, u32, u32),
    Enum(&'static [&'static str], &'static str),
    Cors,
}

/// Define reflected properties on the prototype `$o`: `"prop" => "attr": Kind`. `$tags` is a constant list of the tags that may be `this`.
/// A closure that captures nothing is a plain function pointer in Rust (like a JS function that uses no outer variable), so it fits the `H`
/// type of the handlers; the constants in it are part of its code.
macro_rules! reflect {
    ($d:expr, $it:expr, $o:expr, $tags:expr, { $($prop:literal => $attr:literal : $($kind:tt)::+ $(($($k:expr),*))?),* $(,)? }) => {{
        $(
            $d.def_get($it, $o, $prop,
                |d, it, t, _| $crate::browser_reflect::get(d, it, t, $tags, $attr, $crate::browser_reflect::Kind::$($kind)::+$(($($k),*))?),
                Some(|d, it, t, a| $crate::browser_reflect::set(d, it, t, a, $tags, $prop, $attr, $crate::browser_reflect::Kind::$($kind)::+$(($($k),*))?)))?;
        )*
    }};
}
pub(crate) use reflect;

/// The HTML element `this`, when it is one of `tags`.
pub(crate) fn this_of(d: &Dom, it: &mut Interp<'_>, this: Value, tags: &[&str]) -> R<NodeId> {
    let n = d.el(it, this)?;
    let ok = {
        let doc = d.doc.borrow();
        tags.iter().any(|t| doc.is_html(n, t))
    };
    if ok { Ok(n) } else { Err(it.type_error("Illegal invocation")) }
}

/// The attribute `name` of `n` as an owned string.
pub(crate) fn text(d: &Dom, n: NodeId, name: &str) -> Option<String> {
    let doc = d.doc.borrow();
    doc.find_atom(name).and_then(|k| doc.attr(n, k)).map(str::to_string)
}

/// "Rules for parsing floating-point number values": the longest number at the start, after white space. `None`: no number, or not finite.
pub(crate) fn parse_float(s: &str) -> Option<f64> {
    let t = s.trim_start_matches([' ', '\t', '\n', '\x0c', '\r']);
    let t = t.strip_prefix('+').unwrap_or(t);
    let mut end = t.bytes().take_while(|b| b.is_ascii_digit() || matches!(b, b'-' | b'.' | b'e' | b'E' | b'+')).count();
    // Shorten until the prefix parses: "1.5e" reads as 1.5, "2-3" as 2.
    while end > 0 {
        if let Ok(v) = t[..end].parse::<f64>() {
            return v.is_finite().then_some(v);
        }
        end -= 1;
    }
    None
}

/// The URL in the attribute `name` of `n`, resolved (see `Kind::Url`). `None`: no attribute.
pub(crate) fn url(d: &Dom, it: &mut Interp<'_>, n: NodeId, name: &str) -> R<Option<String>> {
    let Some(raw) = text(d, n, name) else { return Ok(None) };
    it.charge(raw.len() as u64 / 32)?;
    Ok(Some(match crate::browser_navigation_url::parse(&raw, crate::browser_elements::base(d).as_ref()) {
        Ok(r) => r.href(),
        Err(_) => raw,
    }))
}

pub(crate) fn get(d: &Dom, it: &mut Interp<'_>, this: Value, tags: &[&str], attr: &str, kind: Kind) -> R<Value> {
    let n = this_of(d, it, this, tags)?;
    match kind {
        Kind::Bool => Ok(Value::Bool(text(d, n, attr).is_some())),
        Kind::Long(def, lo) => Ok(Value::Number(text(d, n, attr).and_then(|s| crate::browser_element::parse_int(&s)).filter(|&v| v >= lo).unwrap_or(def) as f64)),
        Kind::Clamp(def, lo, hi) => {
            let v = text(d, n, attr).and_then(|s| crate::browser_element::parse_int(&s)).filter(|&v| v >= 0);
            Ok(Value::Number(v.map_or(def, |v| (v as u32).clamp(lo, hi)) as f64))
        }
        Kind::Cors => match text(d, n, attr) {
            None => Ok(Value::Null),
            Some(s) => d.js_str(it, if s.eq_ignore_ascii_case("use-credentials") { "use-credentials" } else { "anonymous" }),
        },
        Kind::Enum(words, def) => {
            let s = text(d, n, attr).unwrap_or_default();
            let w = words.iter().find(|w| w.eq_ignore_ascii_case(&s)).copied().unwrap_or(def);
            d.js_str(it, w)
        }
        Kind::Url => {
            let s = url(d, it, n, attr)?.unwrap_or_default();
            d.js_str(it, &s)
        }
        Kind::UrlOrDoc => {
            let s = match url(d, it, n, attr)? {
                Some(s) if !s.is_empty() => s,
                _ => d.cur_rec()?.href(),
            };
            d.js_str(it, &s)
        }
        Kind::Str => {
            let s = text(d, n, attr).unwrap_or_default();
            d.js_str(it, &s)
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn set(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value], tags: &[&str], prop: &str, attr: &str, kind: Kind) -> R<Value> {
    let n = this_of(d, it, this, tags)?;
    let v = arg(a, 0);
    let s = match kind {
        Kind::Bool => {
            if !it.to_js_bool(v)? {
                d.doc.borrow_mut().remove_attr(n, attr);
                return Ok(Value::Undefined);
            }
            String::new()
        }
        Kind::Cors if matches!(v, Value::Null) => {
            d.doc.borrow_mut().remove_attr(n, attr);
            return Ok(Value::Undefined);
        }
        Kind::Long(_, lo) => {
            let x = bare_js::value::to_int32(it.to_js_number(v)?);
            if lo >= 0 && x < lo {
                let tag = d.doc.borrow().element_name(n).to_string();
                let why = if lo == 0 { format!("The value provided ({x}) is negative.") } else { format!("The value provided ({x}) is not positive.") };
                return Err(d.dom_err(it, "IndexSizeError", &format!("Failed to set the '{prop}' property on '{}': {why}", interface(&tag))));
            }
            x.to_string()
        }
        Kind::Clamp(..) => bare_js::value::to_uint32(it.to_js_number(v)?).to_string(),
        _ => d.string_arg(it, v)?,
    };
    crate::browser_dom::attr_set(d, it, n, attr, &s)?;
    if attr == "src" {
        d.frame_src(n);
    }
    Ok(Value::Undefined)
}

/// The interface name of the tags that use an error message here.
fn interface(tag: &str) -> &'static str {
    match tag {
        "input" => "HTMLInputElement",
        "textarea" => "HTMLTextAreaElement",
        "td" | "th" => "HTMLTableCellElement",
        _ => "HTMLElement",
    }
}
