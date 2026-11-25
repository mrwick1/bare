//! `HTMLStyleElement` reflects `media` and `type` (Fela in Teams reads `style.media.length` on each of its style elements).

use crate::eng_new::Peek;
use crate::browser::*;
use crate::eng_new::NewFault as Error;
use crate::js_limits::EvalLimits;
use crate::js_limits::HeapLimits;
use bare_html::tree;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if crate::eng_new::is_object(v) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(e) => format!("other:{e:?}"),
    }
}

#[test]
fn style_reflects_media_and_type() {
    big(|| {
        let html = "<head><style id=a media=print type=text/css></style></head><body></body>";
        let mut b = Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
        assert_eq!(js(&mut b, "var a = document.getElementById('a'); [a.media, a.type].join()"), "print,text/css");
        assert_eq!(js(&mut b, "var s = document.createElement('style'); [JSON.stringify(s.media), s.media.length, JSON.stringify(s.type)].join()"), "\"\",0,\"\"");
        assert_eq!(js(&mut b, "s.media = 'screen'; s.type = 'text/css'; [s.getAttribute('media'), s.getAttribute('type')].join()"), "screen,text/css");
        assert_eq!(js(&mut b, "'media' in HTMLStyleElement.prototype && !s.hasOwnProperty('media')"), "true");
    });
}
