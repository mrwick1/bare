//! The session file of `save-state` and `open URL state="FILE"`: Playwright's `storageState` JSON, with one more field, `sessionStorage`,
//! in each origin (Playwright drops it). The file holds live credentials. It is made with mode 0600 (Unix), and no message of this
//! module holds a value: an error names the file and the field only.
//!
//! ```text
//! {"cookies":[{"name":"sid","value":"..","domain":".example.com","path":"/","expires":-1,"httpOnly":true,"secure":true,"sameSite":"Lax"}],
//!  "origins":[{"origin":"https://example.com","localStorage":[{"name":"k","value":"v"}],"sessionStorage":[{"name":"t","value":"x"}]}]}
//! ```
//!
//! A domain with a leading dot is a domain cookie, one without is host-only. `expires` is Unix seconds (`-1`: a session cookie).
//! `sameSite` is `Strict`, `Lax` or `None`; no value, or another one, is "not set". Unknown fields (`indexedDB`, `partitionKey`) are
//! ignored. IndexedDB is not saved.

use bare::page::StorageState;
use bare_net::cookie::{Cookie, SameSite};
use serde_json::{Map, Value, json};
use std::path::Path;

/// The text of a session file.
pub fn to_json(st: &StorageState) -> String {
    let cookies: Vec<Value> = st
        .cookies
        .iter()
        .map(|c| {
            let mut o = Map::new();
            o.insert("name".into(), json!(c.name));
            o.insert("value".into(), json!(c.value));
            o.insert("domain".into(), json!(if c.host_only { c.domain.clone() } else { format!(".{}", c.domain) }));
            o.insert("path".into(), json!(c.path));
            o.insert("expires".into(), json!(c.expires.map_or(-1.0, |ms| ms / 1000.0)));
            o.insert("httpOnly".into(), json!(c.http_only));
            o.insert("secure".into(), json!(c.secure));
            match c.same_site {
                SameSite::Unset => {}
                SameSite::None => drop(o.insert("sameSite".into(), json!("None"))),
                SameSite::Lax => drop(o.insert("sameSite".into(), json!("Lax"))),
                SameSite::Strict => drop(o.insert("sameSite".into(), json!("Strict"))),
            }
            Value::Object(o)
        })
        .collect();
    let items = |l: &[(String, String)]| l.iter().map(|(k, v)| json!({"name": k, "value": v})).collect::<Vec<_>>();
    let mut origins: Vec<&String> = st.local.iter().chain(&st.session).map(|(o, _)| o).collect();
    origins.sort();
    origins.dedup();
    let origins: Vec<Value> = origins
        .into_iter()
        .map(|o| {
            let find = |l: &'_ [(String, Vec<(String, String)>)]| l.iter().find(|(x, _)| x == o).map_or_else(Vec::new, |(_, i)| items(i));
            json!({"origin": o, "localStorage": find(&st.local), "sessionStorage": find(&st.session)})
        })
        .collect();
    json!({"cookies": cookies, "origins": origins}).to_string()
}

/// Read a session file. The error names the file and the field.
pub fn from_json(text: &str, file: &str) -> Result<StorageState, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("{file}: not JSON (line {}, column {})", e.line(), e.column()))?;
    let bad = |field: &str| format!("{file}: bad field {field}");
    let obj = v.as_object().ok_or_else(|| format!("{file}: the top level is not an object"))?;
    let list = |v: &Value, field: &str| -> Result<Vec<Value>, String> {
        match v {
            Value::Null => Ok(Vec::new()),
            Value::Array(a) => Ok(a.clone()),
            _ => Err(bad(field)),
        }
    };
    let mut st = StorageState::default();
    for (i, c) in list(obj.get("cookies").unwrap_or(&Value::Null), "cookies")?.iter().enumerate() {
        let f = |k: &str| format!("cookies[{i}].{k}");
        let s = |k: &str| c.get(k).and_then(Value::as_str).ok_or_else(|| bad(&f(k)));
        let dom = s("domain")?;
        let (host_only, domain) = match dom.strip_prefix('.') {
            Some(d) => (false, d),
            None => (true, dom),
        };
        let mut k = Cookie::new(s("name")?, s("value")?, &domain.to_ascii_lowercase(), host_only, c.get("path").and_then(Value::as_str).unwrap_or("/"));
        k.secure = c.get("secure").and_then(Value::as_bool).unwrap_or(false);
        k.http_only = c.get("httpOnly").and_then(Value::as_bool).unwrap_or(false);
        k.expires = c.get("expires").and_then(Value::as_f64).filter(|e| *e >= 0.0).map(|e| e * 1000.0);
        k.same_site = match c.get("sameSite").and_then(Value::as_str) {
            Some("Strict") => SameSite::Strict,
            Some("Lax") => SameSite::Lax,
            Some("None") => SameSite::None,
            _ => SameSite::Unset,
        };
        st.cookies.push(k);
    }
    for (i, o) in list(obj.get("origins").unwrap_or(&Value::Null), "origins")?.iter().enumerate() {
        let origin = o.get("origin").and_then(Value::as_str).ok_or_else(|| bad(&format!("origins[{i}].origin")))?;
        for (key, out) in [("localStorage", &mut st.local), ("sessionStorage", &mut st.session)] {
            let field = format!("origins[{i}].{key}");
            let mut items = Vec::new();
            for it in list(o.get(key).unwrap_or(&Value::Null), &field)? {
                match (it.get("name").and_then(Value::as_str), it.get("value").and_then(Value::as_str)) {
                    (Some(n), Some(v)) => items.push((n.to_string(), v.to_string())),
                    _ => return Err(bad(&field)),
                }
            }
            if !items.is_empty() {
                out.push((origin.to_string(), items));
            }
        }
    }
    Ok(st)
}

pub fn read(file: &str) -> Result<StorageState, String> {
    let text = std::fs::read_to_string(file).map_err(|e| format!("{file}: cannot read the state file ({})", e.kind()))?;
    from_json(&text, file)
}

/// Write the file: a temporary file in the same folder (mode 0600 on Unix), then a rename.
pub fn write(file: &str, st: &StorageState) -> Result<(), String> {
    use std::io::Write;
    let path = Path::new(file);
    let name = path.file_name().ok_or_else(|| format!("{file}: not a file name"))?.to_string_lossy();
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    crate::run::private(&mut o);
    let err = |e: std::io::Error| format!("{file}: cannot write the state file ({})", e.kind());
    let done = o.open(&tmp).and_then(|mut f| f.write_all(to_json(st).as_bytes()).and_then(|_| f.sync_all())).and_then(|_| std::fs::rename(&tmp, path));
    if let Err(e) = done {
        let _ = std::fs::remove_file(&tmp);
        return Err(err(e));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_round_trips_and_reads_a_playwright_file() {
        let mut c = Cookie::new("sid", "s3", "a.test", false, "/p");
        c.expires = Some(1_800_000_000_500.0);
        c.same_site = SameSite::Lax;
        c.secure = true;
        let st = StorageState { cookies: vec![c.clone()], local: vec![("https://a.test".into(), vec![("k".into(), "v".into())])], session: vec![("https://b.test".into(), vec![("t".into(), "x".into())])] };
        let j = to_json(&st);
        assert!(j.contains("\".a.test\"") && j.contains("\"expires\":1800000000.5"), "{j}");
        let back = from_json(&j, "f").unwrap();
        assert_eq!(back.cookies, vec![c]);
        assert_eq!((back.local, back.session), (st.local, st.session));
        let pw = r#"{"cookies":[{"name":"a","value":"1","domain":"h.test","path":"/","expires":-1,"sameSite":"Weird","partitionKey":"x"}],"origins":[{"origin":"https://h.test","localStorage":[{"name":"k","value":"v"}]}],"indexedDB":[]}"#;
        let st = from_json(pw, "f").unwrap();
        assert!(st.cookies[0].host_only && st.cookies[0].expires.is_none() && st.cookies[0].same_site == SameSite::Unset);
        assert!(st.session.is_empty() && st.local.len() == 1);
    }

    #[test]
    fn an_error_names_the_field_and_never_a_value() {
        let e = from_json(r#"{"cookies":[{"name":"a","value":5,"domain":"h"}]}"#, "f.json").unwrap_err();
        assert_eq!(e, "f.json: bad field cookies[0].value");
        assert!(from_json("secret-text", "f.json").unwrap_err().starts_with("f.json: not JSON"));
    }
}
