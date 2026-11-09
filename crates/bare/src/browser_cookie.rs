//! `document.cookie`: the getter and the setter on `document`. The jar is `bare_net::cookie::Jar` (`crates/net/src/cookie.rs`: the rules of RFC
//! 6265 and 6265bis with the details of headless Chrome 155, checked in `tools/fixtures/ui/cookie-cases.txt` and `chrome-cookie.txt`). It is the same
//! jar that the network client fills from `Set-Cookie` and reads for `Cookie` (`Client::jar`), and it lives in a
//! [`crate::browser_navigation_storage::WebStorage`], so a reload keeps it and a new tab of the session shares it. The getter never shows an
//! `HttpOnly` cookie, and the setter cannot make one or replace one. A cookie value is never put in a log, a console message or an error.

use crate::browser::*;
use bare_js::eval::{Error, Interp};
use bare_js::value::Value;
use bare_net::cookie::{At, Via};

// ------------------------------------------------------------------------------------------ JS

/// Whether the context is secure (`https` or a loopback host), the host and the path of the document, or `Unsupported` while the document URL is not
/// configured.
fn doc(d: &Dom) -> R<(bool, String, String)> {
    let s = d.nav.s.borrow();
    let e = s.entries.get(s.cur).ok_or_else(|| Error::Unsupported(unsupported(crate::browser_navigation::NOURL)))?;
    let host = e.url.hostname();
    Ok((e.url.protocol() == "https:" || bare_net::cookie::loopback(&host), host, e.url.pathname().to_string()))
}

pub(crate) fn doc_cookie_get(d: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> R<Value> {
    let (secure, host, path) = doc(d)?;
    let jar = d.nav.s.borrow().store.jar();
    let s = jar.lock().unwrap_or_else(|e| e.into_inner()).script_get(&At { secure, host: &host, path: &path }, it.wall_now_ms());
    d.js_str(it, &s)
}

pub(crate) fn doc_cookie_set(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    let text = d.string_arg(it, a.first().copied().unwrap_or(Value::Undefined))?;
    let (secure, host, path) = doc(d)?;
    let jar = d.nav.s.borrow().store.jar();
    let mut jar = jar.lock().unwrap_or_else(|e| e.into_inner());
    it.charge(1 + (jar.len() / 64) as u64)?;
    jar.set(&At { secure, host: &host, path: &path }, &text, Via::Script, it.wall_now_ms());
    Ok(Value::Undefined)
}

