//! The shared suite (`src/suite.rs`) on both engines: `cargo test -p bare-js-api --features suite`.
#![cfg(feature = "suite")]

use bare_js::eval::{Error, EvalLimits, Interp};
use bare_js::heap::Heap;
use bare_js_api::suite::Harness;
use bare_js2::api::RunError;
use bare_js2::heap::Trigger;
use bare_js2::interp::Vm;
use bare_js2::script;

struct Old(Interp<'static>, bool);

impl Harness for Old {
    type E = Interp<'static>;

    fn new(stress: bool) -> Self {
        Old(Interp::realm(Heap::default(), EvalLimits { max_steps: 1_000_000, ..Default::default() }).unwrap(), stress)
    }

    fn engine(&mut self) -> &mut Self::E {
        &mut self.0
    }

    fn eval(&mut self, src: &str) -> String {
        let r = match self.0.run_script(src) {
            Ok(v) => self.0.display(v),
            Err(Error::Throw(v)) => format!("throw:{}", self.0.display(v)),
            Err(Error::Limit(_)) => "limit".into(),
            Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
            Err(e) => format!("other:{e:?}"),
        };
        if self.1 {
            self.collect();
        }
        r
    }

    fn collect(&mut self) {
        self.0.collect(&[]).unwrap();
    }
}

struct New(Vm);

impl Harness for New {
    type E = bare_js2::interp_rt::Realm;

    fn new(stress: bool) -> Self {
        let mut vm = script::load("").unwrap();
        vm.realm.set_budget(1_000_000);
        if stress {
            vm.realm.heap.set_trigger(Trigger { min: 1 << 10, slack: 1 << 10 });
        }
        New(vm)
    }

    fn engine(&mut self) -> &mut Self::E {
        &mut self.0.realm
    }

    fn eval(&mut self, src: &str) -> String {
        match self.0.realm.run_text(&script::eval_program(src)) {
            Ok(()) => self.0.global_text("__result"),
            Err(RunError::Throw(v)) => format!("throw:{}", self.0.display(v, false)),
            Err(RunError::Abort(bare_js_api::Abort::Limit(_))) => "limit".into(),
            Err(RunError::Abort(bare_js_api::Abort::Unsupported(what))) => format!("unsupported:{what}"),
        }
    }

    fn collect(&mut self) {
        self.0.collect_idle();
    }
}

/// One `#[test]` for each test of the suite, for a harness and a stress setting. The tests run on a thread with a large stack: the old
/// engine recurses on the native stack.
macro_rules! suite {
    ($m:ident, $h:ty, $stress:expr, [$($t:ident),* $(,)?]) => {
        mod $m {
            use super::*;
            $(
                #[test]
                fn $t() {
                    std::thread::Builder::new().stack_size(64 << 20).spawn(|| bare_js_api::suite::$t::<$h>($stress)).unwrap().join().unwrap();
                }
            )*
        }
    };
}

macro_rules! both {
    ($($t:ident),* $(,)?) => {
        suite!(old, Old, false, [$($t),*]);
        suite!(old_stress, Old, true, [$($t),*]);
        suite!(new, New, false, [$($t),*]);
        suite!(new_stress, New, true, [$($t),*]);
    };
}

both!(
    host_functions_run_in_scripts,
    an_engine_error_crosses_the_host_unchanged,
    a_persistent_handle_keeps_its_value_through_a_collection,
    values_objects_and_strings_through_a_scope,
    a_window_is_read_after_calls_into_script,
    constructors_take_new_target_as_declared,
    errors_made_by_the_host_reach_the_script_with_their_constructor,
    conversions_run_the_script_of_the_value,
    arrays_prototypes_and_accessors,
    own_properties_are_read_without_running_code_except_getters,
    strings_from_code_units_keep_lone_surrogates,
    handles_survive_a_top_level_call_that_allocates,
    a_table_installs_methods_accessors_and_constants,
    two_realms_install_the_same_table,
    a_promise_of_the_host_settles_and_its_reactions_run_on_the_drain,
    a_second_settle_by_the_host_changes_nothing,
    promises_cross_between_script_and_host,
    a_microtask_runs_once_on_the_drain,
    a_checkpoint_inside_a_script_function_does_nothing,
    a_promise_survives_collections_as_a_local_and_as_a_persistent_handle,
    between_jobs_runs_before_each_job_and_an_error_ends_the_drain,
    the_host_hears_thrown_callbacks_and_rejections,
    the_rejections_over_the_bound_are_counted,
);
