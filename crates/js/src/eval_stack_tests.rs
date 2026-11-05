//! `Error.stack` tests. Node is not an oracle here: the format is bare's own (see `eval_stack.rs`). Each test runs
//! on a big stack. Script ids are the realm slots: `realm` makes slot 0 (empty), so the first script is `script#1`.

use super::*;
use crate::heap::Heap;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn realm(limits: EvalLimits) -> Interp<'static> {
    Interp::realm(Heap::default(), limits).expect("realm")
}

fn out(it: &mut Interp, src: &str) -> String {
    match it.run_script(src) {
        Ok(v) => it.display(v),
        Err(Error::Throw(v)) => format!("throw:{}", it.get_str(v, "message")),
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        Err(e) => format!("other:{e:?}"),
    }
}

fn lim() -> EvalLimits {
    EvalLimits { max_steps: 5_000_000, ..Default::default() }
}

#[test]
fn plain_error_has_a_string_stack_with_header_and_trim_works() {
    big(|| {
        let mut it = realm(lim());
        assert_eq!(out(&mut it, "typeof Error('x').stack"), "string");
        assert_eq!(out(&mut it, "Error('x').stack.trim().split('\\n')[0]"), "Error: x");
        assert_eq!(out(&mut it, "new TypeError('t').stack.split('\\n')[0]"), "TypeError: t");
        assert_eq!(out(&mut it, "new RangeError().stack.split('\\n')[0]"), "RangeError");
        // top level: one frame, the script itself
        assert_eq!(out(&mut it, "new Error('top').stack"), "Error: top\n    at script#5:1:1");
        assert_eq!(out(&mut it, "new AggregateError([], 'agg').stack.split('\\n')[0]"), "AggregateError: agg");
    });
}

#[test]
fn nested_functions_show_real_frames_in_order_with_positions() {
    big(|| {
        let mut it = realm(lim());
        let src = "function inner() { return new Error('boom'); }\nfunction outer() { return inner(); }\nvar e = outer();\ne.stack";
        assert_eq!(out(&mut it, src), "Error: boom\n    at inner (script#1:1:27)\n    at outer (script#1:2:27)\n    at script#1:3:9");
    });
}

#[test]
fn constructors_and_classes_are_new_frames() {
    big(|| {
        let mut it = realm(lim());
        let src = "function Foo() { this.e = new Error('f'); }\nclass A { constructor() { this.e = new Error('a'); } }\nvar f = new Foo();\nvar a = new A();\nf.e.stack + '|' + a.e.stack";
        assert_eq!(
            out(&mut it, src),
            "Error: f\n    at new Foo (script#1:1:27)\n    at script#1:3:9|Error: a\n    at new A (script#1:2:36)\n    at script#1:4:9"
        );
        let src = "class E extends Error { constructor(m) { super(m); } }\nnew E('sub').stack";
        assert_eq!(out(&mut it, src), "Error: sub\n    at new E (script#2:1:42)\n    at script#2:2:1");
        // the default derived constructor is a frame too
        assert_eq!(out(&mut it, "class D extends Error {}\nnew D('d').stack"), "Error: d\n    at new D (script#3)\n    at script#3:2:1");
    });
}

#[test]
fn frames_name_their_own_script_across_scripts() {
    big(|| {
        let mut it = realm(lim());
        assert_eq!(out(&mut it, "function mk() { return new Error('x'); }"), "undefined");
        assert_eq!(out(&mut it, "\n\n  function call() { return mk(); }\ncall().stack"), "Error: x\n    at mk (script#1:1:24)\n    at call (script#2:3:28)\n    at script#2:4:1");
    });
}

#[test]
fn frame_count_and_text_are_capped() {
    big(|| {
        let mut it = realm(lim());
        assert_eq!(out(&mut it, "function r(n) { return n ? r(n - 1) : new Error('d'); }\nr(40).stack.split('\\n').length"), "11");
        // a huge message does not make a huge stack
        assert_eq!(out(&mut it, "var m = 'x'; for (var i = 0; i < 17; i++) m += m; new Error(m).stack.length <= 2048"), "true");
        assert_eq!(out(&mut it, "new Error(m).message.length"), "131072");
    });
}

#[test]
fn nothing_runs_after_a_limit_and_frames_unwind() {
    big(|| {
        let mut it = realm(EvalLimits { max_call_depth: 20, ..lim() });
        assert_eq!(out(&mut it, "function r() { return r(); }\nr()"), "limit:CallDepth");
        assert!(it.stk.frames.is_empty() && it.stk.pos == 0);
        // frames of the failed call do not leak into the next error
        assert_eq!(out(&mut it, "new Error('after').stack"), "Error: after\n    at script#2:1:1");
        // a throw caught inside the same frame leaves no stale position
        assert_eq!(out(&mut it, "function t() { try { g(); } catch (e) {} try { null.x } catch (e) { return e.stack } }\nfunction g() { throw 1 }\nt()"), "TypeError: Cannot read properties of null (reading 'x')\n    at t (script#3)\n    at script#3:3:1");
        assert!(it.stk.frames.is_empty());
        let mut it = realm(EvalLimits { max_steps: 300, ..lim() });
        assert_eq!(out(&mut it, "function r(n) { return n ? r(n - 1) : 1; }\nfor (;;) r(5)"), "limit:Steps");
        assert!(it.stk.frames.is_empty() && it.stk.pos == 0);
    });
}

#[test]
fn interpreter_errors_get_frames_without_inventing_positions() {
    big(|| {
        let mut it = realm(lim());
        assert_eq!(out(&mut it, "function f() { try { null.x } catch (e) { return e.stack } }\nf()"), "TypeError: Cannot read properties of null (reading 'x')\n    at f (script#1)\n    at script#1:2:1");
        assert_eq!(out(&mut it, "try { undefined() } catch (e) { e.stack }"), "TypeError: undefined is not a function\n    at script#2:1:7");
    });
}

#[test]
fn reused_slots_and_deleted_globals_do_not_change_frames() {
    big(|| {
        let mut it = realm(lim());
        assert_eq!(out(&mut it, "globalThis.g = function named() { return new Error('x') }; var keep = g; delete globalThis.g; keep().stack"), "Error: x\n    at named (script#1:1:42)\n    at script#1:1:95");
        // a rejected script keeps no slot; the next script has its own id and its own frames
        assert!(out(&mut it, "let 1 = 2").starts_with("throw:"));
        assert_eq!(out(&mut it, "new Error('n').stack"), "Error: n\n    at script#2:1:1");
        assert_eq!(out(&mut it, "keep().stack"), "Error: x\n    at named (script#1:1:42)\n    at script#3:1:1");
    });
}

#[test]
fn async_frames_start_at_the_async_function_and_callbacks_have_no_sync_caller() {
    big(|| {
        let mut it = realm(lim());
        let src = "var s = [];\nasync function a() { await 1; s.push(new Error('x').stack); }\nfunction sync() { a(); }\nsync();";
        assert_eq!(out(&mut it, src), "undefined");
        it.drain_microtasks().unwrap();
        assert_eq!(out(&mut it, "s[0]"), "Error: x\n    at a (script#1:2:38)");
        let src = "var c;\nfunction viaThen() { Promise.resolve(1).then(function cb() { c = new Error('c').stack; }); }\nviaThen();";
        out(&mut it, src);
        it.drain_microtasks().unwrap();
        assert_eq!(out(&mut it, "c"), "Error: c\n    at cb (script#3:2:66)");
        // before the first await the caller is still on the stack
        let src = "var e2;\nasync function b() { e2 = new Error('y').stack; await 0; }\nfunction s2() { b(); }\ns2();\ne2";
        assert_eq!(out(&mut it, src), "Error: y\n    at b (script#5:2:27)\n    at s2 (script#5:3:17)\n    at script#5:4:1");
    });
}

#[test]
fn a_queued_callback_has_only_its_own_frame() {
    big(|| {
        let mut it = realm(lim());
        out(&mut it, "var r; function cb() { r = new Error('q').stack; } function mk() { return cb; }");
        let f = it.run_script("mk()").unwrap();
        it.queue_microtask(f).unwrap();
        it.drain_microtasks().unwrap();
        assert_eq!(out(&mut it, "r"), "Error: q\n    at cb (script#1:1:28)");
    });
}

#[test]
fn stack_is_an_ordinary_writable_own_property() {
    big(|| {
        let mut it = realm(lim());
        assert_eq!(
            out(&mut it, "var e = new Error('p'); var d = Object.getOwnPropertyDescriptor(e, 'stack'); [typeof d.value, d.writable, d.enumerable, d.configurable, Object.keys(e).length, JSON.stringify(e)].join()"),
            "string,true,false,true,0,{}"
        );
        assert_eq!(out(&mut it, "e.stack = 'mine'; e.stack"), "mine");
        assert_eq!(out(&mut it, "delete e.stack; typeof e.stack"), "undefined");
        // the header is the name and message at construction, later changes are not reflected
        assert_eq!(out(&mut it, "var f = new Error('one'); f.message = 'two'; f.stack.split('\\n')[0]"), "Error: one");
        // a throwing getter on the prototype chain is never run
        assert_eq!(out(&mut it, "class G extends Error {} Object.defineProperty(G.prototype, 'name', { get() { throw 9 } }); new G('m').stack.split('\\n')[0]"), "Error: m");
    });
}

#[test]
fn stack_text_has_no_arguments_source_or_urls() {
    big(|| {
        let mut it = realm(lim());
        let src = "function leak(secret) { return new Error('e'); }\nleak('hunter2-credential?token=abc').stack";
        let s = out(&mut it, src);
        assert!(!s.contains("hunter2") && !s.contains("token") && !s.contains("return new"), "{s}");
    });
}

#[test]
fn natives_have_no_frame_and_accessors_are_frames() {
    big(|| {
        let mut it = realm(lim());
        let src = "var o = { get p() { return new Error('g').stack; } };\nfunction run() { var r; [1].forEach(function each() { r = o.p; }); return r; }\nrun()";
        // a getter is a frame, but its key is not a declared function name, so it is anonymous; the property read
        // is not a call expression, so `each` has no position; forEach has no frame and `run` shows its call
        assert_eq!(out(&mut it, src), "Error: g\n    at script#1:1:28\n    at each (script#1)\n    at run (script#1:2:25)\n    at script#1:3:1");
        assert_eq!(out(&mut it, "var q = { m() { return new Error('m').stack; } }; q.m()"), "Error: m\n    at script#2:1:24\n    at script#2:1:51");
    });
}

#[test]
fn many_errors_keep_the_cost_and_the_frames_flat() {
    big(|| {
        for n in [1u32, 10, 100] {
            let mut it = realm(lim());
            let src = format!("function r(d) {{ return d ? r(d - 1) : new Error('e'); }}\nvar len = 0;\nfor (var i = 0; i < {n}; i++) len = r(30).stack.length;\nlen");
            let first = out(&mut it, &src);
            let n1: usize = first.parse().unwrap();
            assert!(n1 > 0 && n1 <= 2048, "{n1}");
            assert!(it.stk.frames.is_empty() && it.stk.pos == 0 && it.stk.base == 0);
            // the same result for every repeat: the size does not grow with the count
            assert_eq!(out(&mut it, "r(30).stack.length"), first);
            assert!(it.stats().steps < 200_000 * n as u64, "{}", it.stats().steps);
        }
    });
}

#[test]
fn failed_constructor_environment_does_not_mark_the_next_call_as_new() {
    big(|| {
        let mut it = realm(lim());
        it.run_script("function C() {} function ordinary() { return Error('later').stack; }").unwrap();
        let cap = it.limits.max_envs;
        it.limits.max_envs = 0;
        assert_eq!(out(&mut it, "new C()"), "limit:Envs");
        it.limits.max_envs = cap;
        assert_eq!(out(&mut it, "ordinary().includes('at new ordinary')"), "false");
    });
}

#[test]
fn errors_deep_in_a_one_line_script_scan_it_once() {
    big(|| {
        // a 1.5 MB one-line script, then 300 errors with two positioned frames each, near its end
        let filler = "var pad = 0;".repeat(1 << 17);
        let src = format!("{filler} function g() {{ return new Error('x'); }} function f() {{ return g(); }} var s = ''; for (var i = 0; i < 300; i++) s = f().stack; s.split('\\n').length + ':' + s.split(':').pop()");
        let mut it = realm(EvalLimits { max_steps: 3_000_000, ..Default::default() });
        // 600 full scans would take about 20 million steps; one scan and 600 short ones take about 150,000
        let r = out(&mut it, &src);
        assert!(r.starts_with("4:"), "{r}");
    });
}
