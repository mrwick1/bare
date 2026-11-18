//! The old engine behind `bare-js-api` (plan step S0.3): host functions, handles, rooting and errors through the interface.

use super::*;
use crate::heap::Heap;
use bare_js_api::{Args, Attrs, Engine, Local, Persistent, Scope, Type, enter};
use std::cell::RefCell;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

/// A host written only against the interface, so it would run on any engine.
#[derive(Default)]
struct H {
    kept: RefCell<Vec<Persistent>>,
}

impl<E: Engine> bare_js_api::Host<E> for H {
    fn call<'s>(&self, s: &mut Scope<'s, E>, id: u32, _this: Local<'s>, args: Args<'s>, _is_new: bool) -> Result<Local<'s>, bare_js_api::Error<'s>> {
        let arg = |s: &mut Scope<'s, E>, i| match args.get(i) {
            Some(v) => v,
            None => s.undefined(),
        };
        match id {
            // add(a, b)
            0 => {
                let (a, b) = (arg(s, 0), arg(s, 1));
                let n = s.as_number(a).unwrap_or(f64::NAN) + s.as_number(b).unwrap_or(f64::NAN);
                Ok(s.number(n))
            }
            // callback(f, x): f(x), from the host
            1 => {
                let (f, x, this) = (arg(s, 0), arg(s, 1), s.undefined());
                s.call(f, this, &[x])
            }
            // fail(v): throw v
            2 => Err(bare_js_api::Error::Throw(arg(s, 0))),
            // keep(v): a persistent handle
            3 => {
                let v = arg(s, 0);
                let p = s.persist(v);
                self.kept.borrow_mut().push(p);
                Ok(s.undefined())
            }
            // kept(i)
            4 => {
                let i = arg(s, 0);
                let i = s.as_number(i).unwrap_or(0.0) as usize;
                let kept = self.kept.borrow();
                Ok(s.local(&kept[i]))
            }
            // describe(o): "<o.name>:<typeof o.f>", built in an inner scope
            _ => {
                let o = arg(s, 0);
                s.escape_scope(|s| {
                    let name = s.get_str(o, "name")?;
                    let name = s.as_str(name).map(|t| t.to_string_lossy()).unwrap_or_default();
                    let f = s.get_str(o, "f")?;
                    let t = format!("{name}:{:?}", s.type_of(f));
                    s.string(&t)
                })
            }
        }
    }
}

fn setup() -> (Interp<'static>, Rc<H>) {
    let mut it = Interp::realm(Heap::default(), EvalLimits { max_steps: 100_000, ..Default::default() }).unwrap();
    let h = Rc::new(H::default());
    enter(&mut it, |s| {
        s.set_host(h.clone());
        let g = s.global();
        for (i, name) in ["add", "callback", "fail", "keep", "kept", "describe"].into_iter().enumerate() {
            let f = s.host_function(name, i as u32, 1).unwrap();
            let k = s.string(name).unwrap();
            s.define(g, k, f, Attrs::METHOD).unwrap();
        }
    });
    (it, h)
}

fn out(it: &mut Interp, src: &str) -> String {
    match it.run_script(src) {
        Ok(v) => it.display(v),
        Err(Error::Throw(v)) => format!("throw:{}", it.display(v)),
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        Err(e) => format!("other:{e:?}"),
    }
}

#[test]
fn host_functions_of_the_interface_run_in_scripts() {
    big(|| {
        let (mut it, _) = setup();
        assert_eq!(out(&mut it, "add(2, 3)"), "5");
        assert_eq!(out(&mut it, "callback(x => x * 10, 4)"), "40");
        assert_eq!(out(&mut it, "try { fail('no') } catch (e) { 'caught ' + e }"), "caught no");
        assert_eq!(out(&mut it, "describe({ name: 'box', f() {} })"), "box:Function");
        assert_eq!(it.stack_len(), 0, "every call gives its slots back");
    });
}

#[test]
fn an_engine_error_crosses_the_host_unchanged() {
    big(|| {
        let (mut it, _) = setup();
        assert_eq!(out(&mut it, "callback(() => { for (;;) {} }, 0)"), "limit:Steps");
        assert_eq!(it.stack_len(), 0);
    });
}

#[test]
fn a_persistent_handle_keeps_its_value_through_a_collection() {
    big(|| {
        let (mut it, h) = setup();
        out(&mut it, "keep({ tag: 'kept' }); var junk = []; for (var i = 0; i < 100; i++) junk.push({}); junk = null; 0");
        it.collect(&[]).unwrap();
        assert_eq!(out(&mut it, "kept(0).tag"), "kept");
        let p = h.kept.borrow_mut().pop().unwrap();
        enter(&mut it, |s| s.release(p));
        assert!(matches!(it.api.persistent[..], [Value::Undefined]));
        assert_eq!(it.api.free, vec![0]);
    });
}

#[test]
fn values_objects_and_strings_through_a_scope() {
    big(|| {
        let (mut it, _) = setup();
        enter(&mut it, |s| {
            let o = s.new_object().unwrap();
            let (k, v) = (s.string("x").unwrap(), s.number(1.5));
            s.set(o, k, v).unwrap();
            let got = s.get(o, k).unwrap();
            assert_eq!(s.as_number(got), Some(1.5));
            let t = s.string("héllo").unwrap();
            assert_eq!(s.as_str(t).unwrap().to_string_lossy(), "héllo");
            let n = s.null();
            assert_eq!(s.type_of(n), Type::Null);
            assert!(matches!(s.get(n, k), Err(bare_js_api::Error::Throw(_))), "null.x throws");
            let e = s.boolean(true);
            assert!(matches!(s.define(e, k, v, Attrs::ALL), Err(bare_js_api::Error::Throw(_))));
        });
        assert_eq!(it.stack_len(), 0);
    });
}
