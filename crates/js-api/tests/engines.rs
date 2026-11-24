//! The shared suite (`src/suite.rs`) on Vex: `cargo test -p bare-js-api --features suite`.
#![cfg(feature = "suite")]

use bare_js_api::suite::Harness;
use bare_js_api::Error as ApiError;
use bare_vex::heap::Trigger;
use bare_vex::interp::Vm;
use bare_vex::script;

struct New(Vm);

impl Harness for New {
    type E = bare_vex::interp_rt::Realm;

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
        use bare_js_api::{Abort, Engine};
        let vm = &mut self.0;
        let r = bare_js_api::task(&mut vm.realm, |s| match s.run_script(src) {
            Ok(v) => Ok(s.engine().raw(v)),
            Err(ApiError::Throw(v)) => Err(Ok(s.engine().raw(v))),
            Err(ApiError::Abort(a)) => Err(Err(a)),
        });
        match r.expect("a task") {
            Ok(v) => vm.display(v, false),
            Err(Ok(v)) => format!("throw:{}", vm.display(v, false)),
            Err(Err(Abort::Limit(_))) => "limit".into(),
            Err(Err(Abort::Unsupported(what))) => format!("unsupported:{what}"),
        }
    }

    fn collect(&mut self) {
        self.0.collect_idle();
    }
}

/// One `#[test]` for each test of the suite, for a harness and a stress setting. The tests run on a thread with a large stack.
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

macro_rules! vex {
    ($($t:ident),* $(,)?) => {
        suite!(new, New, false, [$($t),*]);
        suite!(new_stress, New, true, [$($t),*]);
    };
}

vex!(
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
    modules_link_and_run_in_order_with_live_bindings,
    modules_report_link_and_parse_errors,
    dynamic_import_reaches_the_host,
    a_table_installs_methods_accessors_and_constants,
    two_realms_install_the_same_table,
    run_script_gives_the_completion_value,
    eval_runs_in_the_global_environment,
    handler_functions_look_names_up_in_the_scope,
    the_host_walks_iterables,
    symbols_and_the_iterator_intrinsics,
    a_promise_of_the_host_settles_and_its_reactions_run_on_the_drain,
    a_second_settle_by_the_host_changes_nothing,
    promises_cross_between_script_and_host,
    a_microtask_runs_once_on_the_drain,
    a_checkpoint_inside_a_script_function_does_nothing,
    a_promise_survives_collections_as_a_local_and_as_a_persistent_handle,
    between_jobs_runs_before_each_job_and_an_error_ends_the_drain,
    the_host_hears_thrown_callbacks_and_rejections,
    the_rejections_over_the_bound_are_counted,
    owners_are_traced_while_alive_and_pruned_once,
    a_host_indexed_object_reads_the_host,
    a_host_named_object_reads_the_host,
    a_lazy_member_is_made_at_the_first_miss,
    a_live_list_asks_the_fill_step_and_reads_again,
    a_named_write_is_accepted_refused_or_throws,
    a_named_write_falls_through_to_an_ordinary_property,
);
