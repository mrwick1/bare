use crate::browser::*;
use bare_html::tree;
use bare_js::{eval::{Error, EvalLimits}, heap::HeapLimits, value::Value};
const ORIGIN: &str = "https://redis.io";

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn mk(page: &str, steps: u64) -> Browser {
    let mut b = Browser::new(tree::parse(page), BrowserLimits::default(), EvalLimits { max_steps: steps, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.set_document_url(&format!("{ORIGIN}/login")).unwrap();
    b
}

fn at(page: &str) -> Browser {
    mk(page, 5_000_000)
}

fn show(b: &mut Browser, r: Result<Value, Error>) -> String {
    match r {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(Error::Throw(v)) => format!("throw-value:{}", b.it.display(v)),
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(e) => format!("other:{e:?}"),
    }
}

fn js(b: &mut Browser, src: &str) -> String {
    let r = b.run_script(src);
    show(b, r)
}

#[test]
fn sandbox_list_identity_and_length() { big(|| {
    let mut b = at("<html><body><iframe id=f></iframe></body></html>");
    assert_eq!(js(&mut b, "var f=document.getElementById('f'); [typeof f.sandbox, f.sandbox===f.sandbox, f.sandbox.length].join()"), "object,true,0");
}); }

#[test]
fn sandbox_supports_adds_only_supported_tokens() { big(|| {
    let mut b = at("<html><body><iframe id=f></iframe></body></html>");
    // Google's sign-in expression: add a token only when `supports` accepts it.
    assert_eq!(js(&mut b, "var a=document.getElementById('f'); a.setAttribute('sandbox',''); for (var tok of ['allow-scripts','allow-storage-access-by-user-activation']) a.sandbox.supports&&!a.sandbox.supports(tok)||a.sandbox.add(tok); a.getAttribute('sandbox')"), "allow-scripts");
}); }

#[test]
fn sandbox_supports_is_case_insensitive_and_strict() { big(|| {
    let mut b = at("<html><body><iframe id=f></iframe></body></html>");
    assert_eq!(js(&mut b, "var s=document.getElementById('f').sandbox; [s.supports('ALLOW-FORMS'), s.supports('allow-bogus'), s.supports('allow-storage-access-by-user-activation')].join()"), "true,false,false");
}); }

#[test]
fn sandbox_setter_writes_attribute() { big(|| {
    let mut b = at("<html><body><iframe id=f></iframe></body></html>");
    assert_eq!(js(&mut b, "var f=document.getElementById('f'); f.sandbox = 'allow-forms allow-popups'; [f.getAttribute('sandbox'), f.sandbox.contains('allow-popups')].join()"), "allow-forms allow-popups,true");
}); }

#[test]
fn sandbox_getter_rejects_wrong_this() { big(|| {
    let mut b = at("<html><body><iframe id=f></iframe></body></html>");
    assert_eq!(js(&mut b, "try { Object.getOwnPropertyDescriptor(HTMLIFrameElement.prototype,'sandbox').get.call(document.body) } catch(e) { e.name }"), "TypeError");
}); }
