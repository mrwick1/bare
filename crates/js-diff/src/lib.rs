//! Run one script on the old engine (`bare-js`) or on the new one (`bare-vex`) and give the same kind of result. The test262 runner
//! (`examples/test262_run.rs`) and the differential runner (`examples/diff_run.rs`, `diff.rs`) are built on [`run`].
pub mod diff;

use bare_js::eval::{Error, EvalLimits, Exec, Interp};
use bare_js::heap::Heap;
use bare_js::value::Value as OldValue;
use bare_vex::interp::Exit;
use bare_vex::value::Value as NewValue;
use std::panic::{AssertUnwindSafe, catch_unwind};

/// The host script (`host.js`).
pub const HOST: &str = include_str!("host.js");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    Old,
    New,
}

impl Engine {
    pub fn from_name(s: &str) -> Option<Engine> {
        match s {
            "old" => Some(Engine::Old),
            "new" => Some(Engine::New),
            _ => None,
        }
    }
}

/// How a run ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Out {
    /// The script ran. The text is what `print` collected, and the shown value of the global that `__name` names.
    Done(String),
    /// The script threw (a syntax error is a throw of `SyntaxError`). `name` is the name of the constructor.
    Threw { name: String, message: String },
    /// The engine refuses a construct (a feature that is not built).
    Unsupported(String),
    /// The step budget ran out, or another limit.
    Limit(String),
    /// A panic or an error of the engine itself.
    Internal(String),
}

/// The mode of the old engine: `BARE_JS_EXEC` (`tree` when it is not set, as in the other examples).
fn old_exec() -> Exec {
    match std::env::var("BARE_JS_EXEC") {
        Err(_) => Exec::Tree,
        Ok(v) => Exec::from_name(&v).unwrap_or_else(|| {
            eprintln!("BARE_JS_EXEC: expected tree, auto or byte, got {v:?}");
            std::process::exit(2)
        }),
    }
}

fn threw(text: &str) -> Out {
    let (name, message) = text.split_once('\u{1}').unwrap_or((text, ""));
    Out::Threw { name: name.into(), message: message.into() }
}

fn run_old(code: &str, steps: u64) -> Out {
    let limits = EvalLimits { max_steps: steps, max_call_depth: 1000, max_traversal_depth: 512, max_statement_frames: 4096, exec: old_exec(), ..Default::default() };
    let mut it = match Interp::realm(Heap::default(), limits) {
        Ok(it) => it,
        Err(e) => return Out::Internal(format!("realm: {e:?}")),
    };
    let host = format!("{HOST}\nvar __engine = 'old';");
    if let Err(e) = it.run_script(&host) {
        return Out::Internal(format!("host script: {e:?}"));
    }
    let end = |it: &mut Interp<'_>, name: &str, args: &[OldValue]| -> Result<String, String> {
        // A thrown value is not printed: its Debug text has an address, and the results must be the same from run to run.
        let why = |e: Error| if matches!(e, Error::Throw(_)) { format!("{name} threw") } else { format!("{name}: {e:?}") };
        let f = it.run_script(name).map_err(why)?;
        let v = it.call(f, OldValue::Undefined, args).map_err(why)?;
        Ok(it.display(v))
    };
    // The host runs the jobs when the script ends: the stack is empty.
    match it.run_script(code).and_then(|v| it.drain_microtasks().map(|_| v)) {
        Ok(_) => end(&mut it, "__finish", &[]).map_or_else(Out::Internal, Out::Done),
        Err(Error::Throw(v)) => end(&mut it, "__thrown", &[v]).map_or_else(Out::Internal, |t| threw(&t)),
        Err(Error::Unsupported(u)) => Out::Unsupported(u.what.to_string()),
        Err(Error::Limit(l)) => Out::Limit(format!("{l:?}")),
        Err(e) => Out::Internal(format!("{e:?}")),
    }
}

fn run_new(code: &str, steps: u64) -> Out {
    let host = format!("{HOST}\nvar __engine = 'new';");
    let mut vm = match bare_vex::script::load_with(&host, code) {
        Ok(vm) => vm,
        Err(e) if e.starts_with("not supported yet") => return Out::Unsupported(e),
        Err(e) if e.starts_with("SyntaxError: ") => return Out::Threw { name: "SyntaxError".into(), message: e["SyntaxError: ".len()..].into() },
        Err(e) => return Out::Internal(e),
    };
    vm.fuel = steps;
    let end = |vm: &mut bare_vex::interp::Vm, name: &str, args: &[NewValue]| -> Result<String, String> {
        let f = vm.global(name);
        let v = vm.call(f, NewValue::UNDEFINED, args).map_err(|e| format!("{name} {}", if matches!(e, Exit::Fuel) { "ran out of budget" } else { "threw" }))?;
        Ok(vm.display(v, false))
    };
    // The host runs the jobs when the script ends: the stack is empty.
    match vm.run_main(0).and_then(|v| vm.drain_microtasks(&mut |_, _| {}).map(|_| v).map_err(|_| Exit::Fuel)) {
        Ok(_) => end(&mut vm, "__finish", &[]).map_or_else(Out::Internal, Out::Done),
        // A function that holds an unbuilt construct fails at its first call (lazy compile): an Error of that text is "unsupported" too.
        Err(Exit::Throw(e)) => end(&mut vm, "__thrown", &[e]).map_or_else(Out::Internal, |t| match threw(&t) {
            Out::Threw { name, message } if name == "Error" && message.starts_with("not supported yet") => Out::Unsupported(message),
            o => o,
        }),
        Err(Exit::Fuel) => Out::Limit("the budget ran out".into()),
    }
}

/// Run `code` in a new realm of `engine` with the host script before it. `steps` is the budget (back edges in the new engine, steps in
/// the old one: the two numbers are not the same unit). A panic is [`Out::Internal`].
pub fn run(engine: Engine, code: &str, steps: u64) -> Out {
    catch_unwind(AssertUnwindSafe(|| match engine {
        Engine::Old => run_old(code, steps),
        Engine::New => run_new(code, steps),
    }))
    .unwrap_or_else(|p| Out::Internal(format!("panic: {}", p.downcast_ref::<String>().map(String::as_str).or(p.downcast_ref::<&str>().copied()).unwrap_or(""))))
}
