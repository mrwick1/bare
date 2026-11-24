//! The cost of one call from a script to a host function, on both engines.
//!   host_call_load ENGINE MODE N      ENGINE: old | new      MODE: host | native | none      N: turns of the loop
//! `host`: `nop(i)`, a host function that gives its argument back. `native`: `Object.is(i, 1)`, a built-in of the engine, for the cost of
//! a plain native call. `none`: the loop with no call. The cost of one call is the count of instructions at N minus the count at N = 0
//! (the setup), divided by N, less the count of `none`: `tools/js/instr.sh target/release/examples/host_call_load ENGINE MODE N`.
use bare_js_api::{Args, Attrs, Engine, Error, Host, HostFn, Local, Scope, enter};
use std::rc::Rc;

struct Nop;

impl<E: Engine> Host<E> for Nop {
    fn call<'s>(&self, s: &mut Scope<'s, E>, _id: u32, _this: Local<'s>, args: Args<'s>, _is_new: bool) -> Result<Local<'s>, Error<'s>> {
        Ok(args.get(0).unwrap_or_else(|| s.undefined()))
    }
}

fn install<E: Engine>(e: &mut E) {
    e.set_host(Rc::new(Nop));
    enter(e, |s| {
        let f = s.host_function("nop", 0, 1, HostFn::Method).unwrap();
        let g = s.global();
        s.define_str(g, "nop", f, Attrs::METHOD).unwrap();
    });
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (engine, mode, n) = (a[1].clone(), a[2].clone(), a[3].parse::<usize>().unwrap());
    let call = match mode.as_str() {
        "host" => "x = nop(i)",
        "native" => "x = Object.is(i, 1)",
        "none" => "x = i",
        _ => panic!("mode"),
    };
    let src = format!("var x; for (var i = 0; i < {n}; i++) {{ {call} }}");
    // The old engine recurses on the native stack.
    std::thread::Builder::new().stack_size(64 << 20).spawn(move || run(&engine, &src)).unwrap().join().unwrap();
}

fn run(engine: &str, src: &str) {
    match engine {
        "old" => {
            use bare_js::eval::{EvalLimits, Interp};
            let mut it = Interp::realm(bare_js::heap::Heap::default(), EvalLimits { max_steps: u64::MAX, ..Default::default() }).unwrap();
            install(&mut it);
            it.run_script(src).unwrap();
        }
        "new" => {
            let mut vm = bare_vex::script::load("").unwrap();
            install(&mut vm.realm);
            vm.realm.run_text(src).unwrap();
        }
        _ => panic!("engine"),
    }
}
