//! `cs(op, ...)`: the native op of `shims/caches.js` on the Cache API store of the session (`cache_store.rs`). The shim checks the
//! arguments and makes the `Request` and `Response` objects. Headers cross as text: one `name:value` line per header, `\n` between
//! lines (a header name has no `:`, a value has no line break). The origin is the origin of the document.

use crate::browser::*;
use crate::browser_scoped::*;
use crate::cache_store::{CacheEntry, Query};
use crate::eng::Eng;
use bare_js_api::{Args, Local, Type};

fn text<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, a: Args<'s>, i: usize) -> Res<'s, String> {
    let v = arg_s::<E>(s, a, i);
    if matches!(s.type_of(v), Type::Undefined | Type::Null) {
        return Ok(String::new());
    }
    d.string_arg_s(s, v)
}

fn flag<'s, E: Eng>(s: &mut Sc<'s, E>, a: Args<'s>, i: usize) -> bool {
    let v = arg_s::<E>(s, a, i);
    s.to_boolean(v)
}

fn lines(t: &str) -> Vec<(String, String)> {
    t.split('\n').filter_map(|l| l.split_once(':')).map(|(k, v)| (k.to_ascii_lowercase(), v.to_string())).collect()
}

fn join(list: &[(String, String)]) -> String {
    list.iter().map(|(k, v)| format!("{k}:{v}")).collect::<Vec<_>>().join("\n")
}

/// The ops, numbered as `OP` in the shim. A request is `(url, method, header lines)` in three arguments.
pub(crate) fn cs_host<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let op = {
        let v = arg_s::<E>(s, a, 0);
        s.as_number(v).unwrap_or(-1.0) as i32
    };
    let origin = crate::browser_basics::doc_origin(d);
    let store = d.nav.s.borrow().store.clone();
    match op {
        // open(name)
        0 => {
            let name = text(d, s, a, 1)?;
            store.caches().open(&origin, &name);
            Ok(s.undefined())
        }
        // has(name)
        1 => {
            let name = text(d, s, a, 1)?;
            let ok = store.caches().has(&origin, &name);
            Ok(s.boolean(ok))
        }
        // delete(name)
        2 => {
            let name = text(d, s, a, 1)?;
            let ok = store.caches().delete(&origin, &name);
            Ok(s.boolean(ok))
        }
        // names()
        3 => {
            let names = store.caches().names(&origin);
            let mut out = Vec::with_capacity(names.len());
            for n in &names {
                out.push(s.string(n)?);
            }
            s.new_array(&out)
        }
        // put(name, url, method, headers, status, statusText, headers, body, response URL) -> false when the cache is gone
        4 => {
            let (name, url, req_headers) = (text(d, s, a, 1)?, text(d, s, a, 2)?, lines(&text(d, s, a, 4)?));
            let status = {
                let v = arg_s::<E>(s, a, 5);
                s.as_number(v).unwrap_or(200.0) as u16
            };
            let (status_text, headers) = (text(d, s, a, 6)?, lines(&text(d, s, a, 7)?));
            let body: std::sync::Arc<[u8]> = {
                let v = arg_s::<E>(s, a, 8);
                s.bytes(v).unwrap_or(&[]).into()
            };
            s.charge((body.len() / 64) as u64)?;
            let resp_url = text(d, s, a, 9)?;
            let names = crate::cache_store::vary_names(&headers);
            let vary = req_headers.iter().filter(|(k, _)| names.contains(k)).cloned().collect();
            let url = url.split('#').next().unwrap_or("").to_string();
            let ok = store.caches().put(&origin, &name, &req_headers, CacheEntry { url, vary, status, status_text, headers, body, resp_url });
            Ok(s.boolean(ok))
        }
        // match(name or null, url or null, method, headers, ignoreSearch, ignoreMethod, ignoreVary, first)
        //   -> [[status, statusText, headers, Uint8Array, URL, request URL, request headers]]
        // deleteEntries(name, url, method, headers, ...options) -> bool (op 6)
        5 | 6 => {
            let name = arg_s::<E>(s, a, 1);
            let name = if matches!(s.type_of(name), Type::Null | Type::Undefined) { None } else { Some(d.string_arg_s(s, name)?) };
            let url = arg_s::<E>(s, a, 2);
            let url = if matches!(s.type_of(url), Type::Null | Type::Undefined) { None } else { Some(d.string_arg_s(s, url)?) };
            let (method, headers) = (text(d, s, a, 3)?, lines(&text(d, s, a, 4)?));
            let (ignore_search, ignore_method, ignore_vary, first) = (flag::<E>(s, a, 5), flag::<E>(s, a, 6), flag::<E>(s, a, 7), flag::<E>(s, a, 8));
            let q = url.as_deref().map(|url| Query { url, method: &method, headers: &headers, ignore_search, ignore_method, ignore_vary });
            if op == 6 {
                let ok = match (&q, &name) {
                    (Some(q), Some(n)) => store.caches().delete_entries(&origin, n, q),
                    _ => false,
                };
                return Ok(s.boolean(ok));
            }
            let found = store.caches().find(&origin, name.as_deref(), q.as_ref(), first);
            let mut out = Vec::with_capacity(found.len());
            for e in found {
                let items = [s.number(f64::from(e.status)), s.string(&e.status_text)?, s.string(&join(&e.headers))?, s.new_uint8_array(e.body.to_vec())?, s.string(&e.resp_url)?, s.string(&e.url)?, s.string(&join(&e.vary))?];
                out.push(s.new_array(&items)?);
            }
            s.new_array(&out)
        }
        _ => Ok(s.undefined()),
    }
}
