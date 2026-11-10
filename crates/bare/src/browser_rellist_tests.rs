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
fn frozen_docking_link_callback() { big(|| {
let mut b = at("<html><head></head><body></body></html>");
assert_eq!(js(&mut b, r#"var observed=0, continued=0; var z={linkRelObserver:{observe(e,o){observed++; e.setAttribute('data-observed',o.attributeFilter[0])}}}; var N = e=>{if(e instanceof HTMLStyleElement&&e.sheet)W(e.sheet);else if(e instanceof HTMLLinkElement&&e.relList.contains("stylesheet"))(e=>{let t=z.linkOriginalMedia.get(e);if(!t){if(!e.media||!tc(e.media))return;t=e.media,z.linkOriginalMedia.set(e,t)}tn(z,z.linksByMql,t,e,(e,n)=>{var r,o,l;if(!e.isConnected)return!1;return o=null!=(r=z.linkOriginalMedia.get(e))?r:t,l=n,e.media=ti(o).map(e=>{var t;return null===tc(e)?e:l?null!=(t=tu(e))?t:"all":"not all"}).join(", "),!0})})(e),e.sheet?W(e.sheet):e.addEventListener("load",()=>{e.sheet&&W(e.sheet)});else{var t,n;if(e instanceof HTMLLinkElement&&(t=e).relList.contains("preload")&&"style"===t.getAttribute("as")&&!t.relList.contains("stylesheet")){null==(n=z.linkRelObserver)||n.observe(e,{attributes:!0,attributeFilter:["rel"]})}}}; var link=document.createElement('link'); link.rel='preload'; link.setAttribute('as','style'); N(link); continued++; [observed,continued,link.getAttribute('data-observed')].join()"#), "1,1,rel");
assert_eq!(js(&mut b, "var delivered=0; var mo=new MutationObserver(function(rs){for(var r of rs)for(var n of r.addedNodes){N(n); delivered++}}); mo.observe(document.head,{childList:true}); var added=document.createElement('link'); added.rel='preload'; added.setAttribute('as','style'); document.head.appendChild(added); 1"), "1");
b.drain().unwrap();
assert_eq!(js(&mut b, "[delivered,observed,added.getAttribute('data-observed')].join()"), "1,2,rel");
assert_eq!(js(&mut b, "var changed=0; var attrs=new MutationObserver(function(rs){for(var r of rs){N(r.target); changed++}}); attrs.observe(added,{attributes:true,attributeFilter:['rel']}); added.rel='preload icon'; 1"), "1");
b.drain().unwrap();
assert_eq!(js(&mut b, "[changed,observed,added.relList.contains('icon')].join()"), "1,3,true");


}); }

#[test]
fn live_rel_tokens() { big(|| {
    let mut b = at("<html><head><link id=k rel='preload preload'></head><body></body></html>");
    assert_eq!(js(&mut b, "var k=document.getElementById('k'), r=k.relList; [r===k.relList,r!==k.classList,r.length,r.item(0),r.contains('PRELOAD')].join()"), "true,true,1,preload,false");
    assert_eq!(js(&mut b, "k.rel='stylesheet x'; r.add('preload'); r.remove('x'); r.toggle('a'); r.replace('a','b'); [k.rel,r.value,Array.from(r).join('|')].join()"), "stylesheet preload b,stylesheet preload b,stylesheet|preload|b");
    assert_eq!(js(&mut b, "k.setAttribute('rel','icon'); r.value='next search'; [k.rel,r.length,r.item(9),r.supports('STYLESHEET'),r.supports('unknown')].join()"), "next search,2,,true,false");
    assert_eq!(js(&mut b, "k.relList='preload'; [r.value,k.classList.value].join()"), "preload,");
    assert_eq!(js(&mut b, "var errors=[]; for(var t of ['', 'a b']) {try{r.add(t)}catch(e){errors.push(e.name)}} errors.join()"), "SyntaxError,InvalidCharacterError");
    assert_eq!(js(&mut b, "try {r.add('x','bad token')} catch(e) {} k.rel"), "preload");
    assert_eq!(js(&mut b, "try {r.contains.call(k,'x')} catch(e) {e.name}"), "TypeError");
    assert_eq!(js(&mut b, "['a','area'].map(function(t){var e=document.createElement(t); e.relList.add('noopener'); return e.getAttribute('rel')+':'+e.relList.supports('NOOPENER')+':'+e.relList.supports('stylesheet')}).join()"), "noopener:false:false,noopener:false:false");
}); }

#[test]
fn rel_tokens_gc_and_limits() { big(|| {
    let mut b = at("<html><head></head><body></body></html>");
    assert_eq!(js(&mut b, "var saved=document.createElement('link'); var cls=saved.classList; var rel=saved.relList; rel.add('stylesheet'); var iter=rel.values(); saved=null; 1"), "1");
    b.collect(&[]).unwrap();
    assert_eq!(js(&mut b, "[rel.value,iter.next().value,cls.value].join()"), "stylesheet,stylesheet,");
    js(&mut b, "rel=null; cls=null; iter=null");
    b.collect(&[]).unwrap();
    b.set_platform_limits(crate::browser_platform::PlatformLimits {max_token_lists:1, ..Default::default()});
    assert_eq!(js(&mut b, "var e=document.createElement('link'); e.relList.add('preload'); e.rel"), "preload");
    assert!(js(&mut b, "e.classList").starts_with("limit:"));
}); }
