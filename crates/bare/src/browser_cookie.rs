//! `document.cookie`: the getter and the setter on `document`. The jar is `bare_net::cookie::Jar` (`crates/net/src/cookie.rs`: the rules of RFC
//! 6265 and 6265bis with the details of headless Chrome 155, checked in `tools/fixtures/ui/cookie-cases.txt` and `chrome-cookie.txt`). It is the same
//! jar that the network client fills from `Set-Cookie` and reads for `Cookie` (`Client::jar`), and it lives in a
//! [`crate::browser_navigation_storage::WebStorage`], so a reload keeps it and a new tab of the session shares it. The getter never shows an
//! `HttpOnly` cookie, and the setter cannot make one or replace one. A cookie value is never put in a log, a console message or an error.

use crate::browser::*;
use crate::browser_scoped::*;
use bare_js_api::{Args, Local};
use bare_js::eval::Error;
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

pub(crate) fn doc_cookie_get<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let (secure, host, path) = lift(s, doc(d))?;
    let jar = d.nav.s.borrow().store.jar();
    let text = jar.lock().unwrap_or_else(|e| e.into_inner()).script_get(&At { secure, host: &host, path: &path }, s.wall_now_ms());
    d.js_str_s(s, &text)
}

pub(crate) fn doc_cookie_set<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let text = d.arg_str_s(s, a, 0)?;
    let (secure, host, path) = lift(s, doc(d))?;
    let jar = d.nav.s.borrow().store.jar();
    let mut jar = jar.lock().unwrap_or_else(|e| e.into_inner());
    s.charge(1 + (jar.len() / 64) as u64)?;
    jar.set(&At { secure, host: &host, path: &path }, &text, Via::Script, s.wall_now_ms());
    Ok(s.undefined())
}

