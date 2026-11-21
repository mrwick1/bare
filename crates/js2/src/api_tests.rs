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
        assert_eq!(s.queue_microtask(u).err().map(|e| matches!(e, Error::Throw(_))), Some(true), "a microtask that is no function is a TypeError");
        assert_eq!(s.promise_status(o), None);
        assert_eq!(s.engine().pending_jobs(), 0);
        assert_eq!(s.resolve_promise(o, o).err(), Some(Error::Abort(Abort::Unsupported("resolve_promise"))));
    });
}

/// The text of the global `name` (a string, a number or an array of them).
fn text(r: &mut Realm, name: &str) -> String {
    let k = r.intern(name);
    let g = r.global.value();
    let v = r.get_prop(g, Key::Name(k)).unwrap();
    let s = r.to_str(v).unwrap();
    r.text(s)
}

fn global<'s>(s: &mut Scope<'s, Realm>, name: &str) -> Local<'s> {
    let g = s.global();
    s.get_str(g, name).unwrap()
}

/// A host whose `between_jobs` runs a script function `hook` and whose `report_exception` and `promise_rejections` count.
#[derive(Default)]
struct Hooks {
    between: Cell<u32>,
    reports: Cell<u32>,
    rejections: Cell<u32>,
    collect: bool,
}

impl Host<Realm> for Hooks {
    fn call<'s>(&self, s: &mut Scope<'s, Realm>, _id: u32, _this: Local<'s>, _args: Args<'s>, _is_new: bool) -> Result<Local<'s>, Error<'s>> {
        // `probe()`: a checkpoint from a host call is nothing, and a drain is refused.
        assert_eq!(s.checkpoint().unwrap(), 0);
        assert_eq!(s.drain_microtasks().err(), Some(Error::Abort(Abort::Unsupported("drain_microtasks during a call"))));
        Ok(s.undefined())
    }

    fn between_jobs<'s>(&self, s: &mut Scope<'s, Realm>) -> Result<(), Error<'s>> {
        self.between.set(self.between.get() + 1);
        if self.collect {
            s.engine().collect(&mut [], |_| {});
            return Ok(());
        }
        let hook = global(s, "hook");
        let this = s.undefined();
        s.call(hook, this, &[]).map(|_| ())
    }

    fn report_exception<'s>(&self, _s: &mut Scope<'s, Realm>, _thrown: Local<'s>) {
        self.reports.set(self.reports.get() + 1);
    }

    fn promise_rejections<'s>(&self, _s: &mut Scope<'s, Realm>, unhandled: &[Local<'s>], _handled: &[Local<'s>], _dropped: u32) -> Result<(), Error<'s>> {
        self.rejections.set(self.rejections.get() + unhandled.len() as u32);
        Ok(())
    }
}

/// A script that a hook runs at the top gets a budget of its own, and the budget of the checkpoint comes back after it.
#[test]
fn between_jobs_runs_script_on_its_own_budget() {
    let mut r = realm();
    r.run_text("var spin = n => { for (var i = 0; i < n; i++); }; var hook = () => spin(6000); var job = () => spin(2000);").unwrap();
    r.set_budget(10_000);
    let h = Rc::new(Hooks::default());
    r.set_host(h.clone());
    enter(&mut r, |s| {
        let job = global(s, "job");
        for _ in 0..3 {
            s.queue_microtask(job).unwrap();
        }
        // 3 jobs of 2,000 steps fit in the budget of the drain (10,000); the 4 hooks of 6,000 steps do not, unless they run on their own.
        assert_eq!(s.drain_microtasks().unwrap(), 3);
        assert_eq!(s.engine().api.abort, None);
    });
    assert_eq!(h.between.get(), 4);
}

/// A job that runs out of budget ends the drain as a limit. The failure stays until `clear_failed_job`; the job is lost, the next one is not.
#[test]
fn a_job_out_of_budget_fails_the_drain_until_it_is_cleared() {
    let mut r = realm();
    r.run_text("var n = 0; var forever = () => { for (;;); }; var next = () => { n++ };").unwrap();
    r.set_budget(1_000);
    enter(&mut r, |s| {
        let (forever, next) = (global(s, "forever"), global(s, "next"));
        s.queue_microtask(forever).unwrap();
        s.queue_microtask(next).unwrap();
        assert_eq!(s.drain_microtasks().err(), Some(Error::Abort(Abort::Limit("the step budget"))));
        assert_eq!(s.drain_microtasks().err(), Some(Error::Abort(Abort::Limit("the step budget"))), "sticky");
        assert_eq!(s.pending_jobs(), 1);
        s.clear_failed_job();
        assert_eq!(s.drain_microtasks().unwrap(), 1);
    });
    assert_eq!(text(&mut r, "n"), "1");
}

/// The drain collects inside the jobs (`between_jobs` collects too, with the promise held by a handle and its value held by the queue).
#[test]
fn the_drain_collects_and_the_values_of_the_host_survive() {
    let mut r = realm();
    r.run_text("var log = []; var ok = v => { var a = []; for (var i = 0; i < 500; i++) a.push({ i: i, s: 'x' + i }); log.push(v.tag) };").unwrap();
    r.heap.set_trigger(crate::heap::Trigger { min: 1 << 10, slack: 1 << 10 });
    let h = Rc::new(Hooks { collect: true, ..Default::default() });
    r.set_host(h.clone());
    let before = r.collections;
    enter(&mut r, |s| {
        let ok = global(s, "ok");
        let none = s.undefined();
        for tag in ["a", "b", "c"] {
            let p = s.new_promise().unwrap();
            s.promise_then(p, ok, none).unwrap();
            let v = s.new_object().unwrap();
            let t = s.string(tag).unwrap();
            s.define_str(v, "tag", t, bare_js_api::Attrs::ALL).unwrap();
            s.resolve_promise(p, v).unwrap();
        }
        assert_eq!(s.drain_microtasks().unwrap(), 3);
    });
    assert!(r.collections > before + 4, "collected between the jobs and inside them");
    assert_eq!(text(&mut r, "log"), "a,b,c");
    assert_eq!(r.stack_len(), 0);
}

/// A handler that the host gives to `promise_then` that throws makes its derived promise an unhandled rejection that the host hears.
#[test]
fn a_throw_of_a_host_reaction_is_an_unhandled_rejection() {
    let mut r = realm();
    r.run_text("var boom = () => { throw 1 }; var probeJob = () => probe(); var hook = () => {};").unwrap();
    let h = Rc::new(Hooks::default());
    r.set_host(h.clone());
    enter(&mut r, |s| {
        let f = s.host_function("probe", 0, 0, HostFn::Method).unwrap();
        let g = s.global();
        s.define_str(g, "probe", f, bare_js_api::Attrs::ALL).unwrap();
        let (boom, none) = (global(s, "boom"), s.undefined());
        let p = s.new_promise().unwrap();
        s.promise_then(p, boom, none).unwrap();
        s.resolve_promise(p, none).unwrap();
        // A job that makes a host call: the checkpoint inside it is nothing, and the drain is refused.
        let probe = global(s, "probeJob");
        s.queue_microtask(probe).unwrap();
        assert_eq!(s.drain_microtasks().unwrap(), 2);
    });
    assert_eq!(h.rejections.get(), 1);
    assert_eq!(h.reports.get(), 0);
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
