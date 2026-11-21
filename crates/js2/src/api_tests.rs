//! Tests of the interface to the host (`api.rs`). The shared suite (`crates/js-api/src/suite.rs`) runs on this engine from
//! `crates/js-api/tests/engines.rs`; these are the checks that need the internals of the realm.
use super::*;
use crate::script;
use bare_js_api::{Scope, enter};
use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};

fn realm() -> Realm {
    script::load("").unwrap().realm
}

/// A host with one function, `id` 0, that runs `f`.
struct One<F>(F);

impl<F: for<'s> Fn(&mut Scope<'s, Realm>, Local<'s>, Args<'s>) -> Result<Local<'s>, Error<'s>>> Host<Realm> for One<F> {
    fn call<'s>(&self, s: &mut Scope<'s, Realm>, _id: u32, this: Local<'s>, args: Args<'s>, _is_new: bool) -> Result<Local<'s>, Error<'s>> {
        (self.0)(s, this, args)
    }
}

fn install<F>(r: &mut Realm, f: F) -> Value
where
    F: for<'s> Fn(&mut Scope<'s, Realm>, Local<'s>, Args<'s>) -> Result<Local<'s>, Error<'s>> + 'static,
{
    r.set_host(Rc::new(One(f)));
    enter(r, |s| {
        let h = s.host_function("probe", 0, 2, HostFn::Method).unwrap();
        s.engine().raw(h)
    })
}

/// A call from the host with no script on the Rust stack, so a collection may run inside the host function. The window of the call
/// (`this` and the arguments) is the only root that holds the values the call reads afterwards.
#[test]
fn the_window_of_a_host_call_is_a_root_of_a_collection() {
    let mut r = realm();
    let f = install(&mut r, |s, this, args| {
        s.engine().collect(&mut [], |_| {});
        // The cells were not moved or reused: the same reads as before the collection.
        let tag = s.get_str(this, "tag")?;
        assert_eq!(s.as_str(tag).unwrap().to_string_lossy(), "this-tag");
        let (a, b) = (args.get(0).unwrap(), args.get(1).unwrap());
        assert_eq!(s.as_str(a).unwrap().to_string_lossy(), "argument one");
        let n = s.get_str(b, "n")?;
        assert_eq!(s.as_number(n), Some(7.0));
        s.string("done")
    });
    let before = r.collections;
    let (this, a, b) = {
        let o = Obj::new(&mut r.heap, &r.shapes, r.s_object).unwrap();
        let t = r.string("this-tag");
        r.set_own(o, "tag", t);
        let b = Obj::new(&mut r.heap, &r.shapes, r.s_object).unwrap();
        r.set_own(b, "n", Value::int(7));
        (o.value(), r.string(&format!("argument {}", "one")), b.value())
    };
    let out = r.call_held(f, this, &[a, b]).unwrap();
    assert!(r.collections > before, "the host function collected");
    assert_eq!(r.text(Realm::str_of(out).unwrap()), "done");
    assert!(r.api.win.is_null());
}

#[test]
fn a_panic_in_a_host_function_unlinks_its_window() {
    let mut r = realm();
    let f = install(&mut r, |_, _, _| panic!("the host broke"));
    let g = r.global.value();
    let caught = catch_unwind(AssertUnwindSafe(|| r.call_held(f, g, &[Value::int(1)])));
    assert!(caught.is_err());
    assert!(r.api.win.is_null(), "the window of the dead call is gone");
    r.collect(&mut [], |_| {});
    assert_eq!(r.stack_len(), 0, "the handle stack was cut back by the scope");
}

#[test]
#[should_panic(expected = "no slot")]
fn a_handle_that_names_no_slot_is_a_panic() {
    let r = realm();
    r.a_at(Local::from_slot(5));
}

#[test]
#[should_panic(expected = "past the arguments")]
fn an_argument_past_the_arguments_is_a_panic() {
    let mut r = realm();
    let f = install(&mut r, |s, _, _| {
        let bad = Local::from_slot(WIN + 3);
        s.type_of(bad);
        s.undefined().pipe_ok()
    });
    let g = r.global.value();
    let _ = r.call_held(f, g, &[Value::int(1)]);
}

trait PipeOk<'s> {
    fn pipe_ok(self) -> Result<Local<'s>, Error<'s>>;
}

impl<'s> PipeOk<'s> for Local<'s> {
    fn pipe_ok(self) -> Result<Local<'s>, Error<'s>> {
        Ok(self)
    }
}

#[test]
fn what_the_realm_does_not_have_is_unsupported_and_never_a_panic() {
    let mut r = realm();
    enter(&mut r, |s| {
        let o = s.new_object().unwrap();
        let u = s.undefined();
        assert_eq!(s.new_promise().err(), Some(Error::Abort(Abort::Unsupported("new_promise"))));
        assert_eq!(s.queue_microtask(u).err(), Some(Error::Abort(Abort::Unsupported("queue_microtask"))));
        assert_eq!(s.iter_open(o).err(), Some(Error::Abort(Abort::Unsupported("iter_open"))));
        assert_eq!(s.set_host_owner(o, 1).err(), Some(Error::Abort(Abort::Unsupported("set_host_owner"))));
        assert_eq!(s.promise_status(o), None);
        assert_eq!(s.engine().pending_jobs(), 0);
    });
}

#[test]
fn a_rope_is_flat_when_the_host_reads_it() {
    let mut r = realm();
    r.run_text("var r = ''; for (var i = 0; i < 300; i++) r += 'ab' + i;").unwrap();
    let want: String = (0..300).map(|i| format!("ab{i}")).collect();
    enter(&mut r, |s| {
        let o = s.global();
        let v = s.get_str(o, "r").unwrap();
        assert_eq!(s.as_str(v).unwrap().to_string_lossy(), want);
    });
}

#[test]
fn a_step_budget_stops_a_script_at_the_top_and_in_a_nested_call() {
    let mut r = realm();
    r.set_budget(1000);
    assert!(matches!(r.run_text("for (;;) {}"), Err(RunError::Abort(Abort::Limit(_)))));
    // A limit in a call from a host function reaches the top as the same abort, and a script cannot catch it.
    let f = install(&mut r, |s, _, args| {
        let (f, this) = (args.get(0).unwrap(), s.undefined());
        s.call(f, this, &[])
    });
    r.set_own(r.global, "probe", f);
    let e = r.run_text("try { probe(function () { for (;;) {} }) } catch (e) { var caught = 1 }");
    assert!(matches!(e, Err(RunError::Abort(Abort::Limit(_)))), "{e:?}");
    r.set_budget(u64::MAX);
    assert!(r.run_text("var ok = 1").is_ok());
}

#[test]
fn a_syntax_error_is_a_thrown_syntax_error() {
    let mut r = realm();
    match r.run_text("var = ;") {
        Err(RunError::Throw(e)) => assert!(r.error_text(e).starts_with("SyntaxError")),
        other => panic!("{other:?}"),
    }
}

#[test]
fn tasks_nest_and_the_budget_starts_at_the_outer_one() {
    let mut r = realm();
    r.set_budget(500);
    let seen = Cell::new(0);
    let out = r.task(|r| {
        r.task(|r| {
            seen.set(r.fuel);
        })
        .unwrap();
        r.fuel
    });
    assert_eq!(out, Ok(500));
    assert_eq!(seen.get(), 500);
}
