//! The facade of `Browser` and the handlers on the interface (`docs/design-host-handlers.md`, step H0).

use crate::browser::*;
use crate::browser_scoped::Failed;
use bare_html::tree;
use bare_js_api::Engine;
use crate::js_limits::EvalLimits;
use crate::js_limits::HeapLimits;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn browser() -> Browser {
    let page = "<!doctype html><body><div id=a class=\"x y\" data-k=v><p class=z>t</p></div>";
    Browser::new(tree::parse(page), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser")
}

/// The text of a script result, or the failure as text. Written only against the facade.
fn js(b: &mut Browser, src: &str) -> String {
    match b.eval(src) {
        Ok(v) => b.display(&v),
        Err(Failed::Throw { name, message }) => format!("throw:{name}:{message}"),
        Err(Failed::Other(e)) => format!("other:{e}"),
    }
}

#[test]
fn the_facade_runs_scripts_reads_properties_and_calls_functions() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "1 + 2"), "3");
        assert_eq!(js(&mut b, "throw new TypeError('bad')"), "throw:TypeError:bad");
        assert_eq!(js(&mut b, "throw 5"), "other:throw:5");
        let o = b.eval("({ name: 'box', n: 7 })").unwrap();
        assert_eq!(b.get_str(&o, "name"), "box");
        assert_eq!(b.get_str(&o, "n"), "7");
        let f = b.eval("(function (x, y) { return this.n + x + y })").unwrap();
        let (x, y) = (b.eval("1").unwrap(), b.eval("2").unwrap());
        let r = b.call(&f, Some(&o), &[&x, &y]).unwrap();
        assert_eq!(b.display(&r), "10");
        let t = b.eval("(function () { throw new RangeError('far') })").unwrap();
        assert_eq!(b.call(&t, None, &[]).err(), Some(Failed::Throw { name: "RangeError".into(), message: "far".into() }));
        assert_eq!(b.with_scope(|s| s.run_script("2 * 3").map(|v| s.display(v)).ok()), Some("6".to_string()));
    });
}

#[test]
fn three_handlers_on_the_interface_behave_as_the_old_ones() {
    big(|| {
        let mut b = browser();
        let a = "var e = document.getElementById('a'); ";
        for (src, want) in [
            ("e.getAttribute('class')", "x y"),
            ("e.getAttribute('CLASS')", "x y"),
            ("e.getAttribute('missing')", "null"),
            ("e.getAttribute(1)", "null"),
            ("e.getAttribute({ toString() { return 'data-k' } })", "v"),
            ("e.hasAttribute('data-k') + ',' + e.hasAttribute('nope')", "true,false"),
            ("e.matches('div.x') + ',' + e.matches('p')", "true,false"),
            ("try { e.matches('::') } catch (x) { x.name }", "SyntaxError"),
            ("try { e.getAttribute() } catch (x) { x.name }", "null"),
            ("try { Element.prototype.getAttribute.call({}, 'a') } catch (x) { x.name + ':' + x.message }", "TypeError:Illegal invocation"),
            ("try { new e.getAttribute('a') } catch (x) { x.name }", "TypeError"),
            ("e.getAttribute.name + e.getAttribute.length + e.hasAttribute.length", "getAttribute11"),
        ] {
            assert_eq!(js(&mut b, &format!("{a}{src}")), want, "{src}");
        }
    });
}

#[test]
fn a_handler_on_the_interface_keeps_its_state_across_collections() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "var e = document.getElementById('a'), n = 0; for (var i = 0; i < 50; i++) { if (e.getAttribute('data-k') === 'v' && e.hasAttribute('id') && e.matches('.y')) n++ } n"), "50");
        assert_eq!(b.with_scope(|s| s.engine().stack_len()), 0, "every call gave its slots back");
    });
}

#[test]
fn the_hooks_around_a_host_call_run_for_a_handler_on_the_interface() {
    big(|| {
        let mut b = browser();
        // A mutation observer is armed by a change; the converted `getAttribute` call after it must still deliver the record.
        assert_eq!(js(&mut b, "var e = document.getElementById('a'), log = []; new MutationObserver(r => log.push(r.length)).observe(e, { attributes: true }); e.setAttribute('x', '1'); e.getAttribute('x'); Promise.resolve().then(() => log.push('t')); 0"), "0");
        b.drain().unwrap();
        assert_eq!(js(&mut b, "log.join()"), "1,t");
    });
}
