/// Which engine runs script code (`Interp::set_exec`). `docs/decisions.md`, "Bytecode": the switch and the tier rule.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Exec {
    /// Never compile. The tree walker runs all code.
    #[default]
    Tree,
    /// Tiers: a function runs on the tree walker at its first call and on bytecode from its second call, or from its first call
    /// when it has a loop or a call.
    Auto,
    /// Compile each function at its first call, and a script body that has a loop or a call.
    Byte,
}

impl Exec {
    /// The mode of a name: `tree`, `auto` or `byte` (the values of `BARE_JS_EXEC` in the examples).
    pub fn from_name(s: &str) -> Option<Exec> {
        match s {
            "tree" => Some(Exec::Tree),
            "auto" => Some(Exec::Auto),
            "byte" => Some(Exec::Byte),
            _ => None,
        }
    }
}

/// The code of a function for the loop (`bc_vm.rs`): the bytecode and the values of its constants.
struct Compiled {
    code: crate::bc::Code,
    /// For each constant: a number, an interned string (rooted by `bc_roots`), or `undefined` for a name.
    kv: Box<[Value]>,
    /// For each constant: its global cell. `GetGlobal` and `SetGlobal` read it. Constants that are not names hold a key that is
    /// never read.
    cells: Box<[GCell]>,
}

/// A global name of compiled code and what the loop learned about it (`GetGlobal`, `SetGlobal`). Both hits are what the full path
/// would find: a binding of environment 0 (a `let`, `const` or `class` of a script) shadows the global object.
struct GCell {
    key: PropertyKey,
    /// `bindings << 32 | pos`: the position of the property in the dictionary of the global object, learned when environment 0 had
    /// that many bindings. The name was not one of them, and environment 0 only gains bindings, so `pos` is valid while the count
    /// is the same. `LEX << 32 | i`: the name is binding `i` of environment 0, and a binding keeps its index. `NONE` (all ones):
    /// not learned.
    st: std::cell::Cell<u64>,
}

/// The count field of a cell that holds a binding of environment 0.
const GC_LEX: u32 = u32::MAX - 1;
const GC_NONE: u64 = u64::MAX;

impl GCell {
    fn new(key: PropertyKey) -> GCell {
        GCell { key, st: std::cell::Cell::new(GC_NONE) }
    }

    fn forget(&self) {
        self.st.set(GC_NONE);
    }
}

/// The state of one function for the switch.
#[derive(Clone, Default)]
enum BcSlot {
    /// Not called yet.
    #[default]
    New,
    /// Called once on the tree walker (`Exec::Auto`).
    Seen,
    /// The code generator refused it: it stays on the tree walker.
    Tree(&'static str),
    Byte(Rc<Compiled>),
}

/// The counters of the switch (`Interp::bc_stats`).
#[derive(Clone, Copy, Debug, Default)]
pub struct BcStats {
    /// Functions and script bodies that compiled, and the ones that stay on the tree walker.
    pub compiled: u64,
    pub fallbacks: u64,
    /// Runs of the loop.
    pub runs: u64,
    /// Calls of script functions that the lane ran (also the inline calls of register functions), and the calls that the tree
    /// walker ran because the lane was on and refused the function (or it is a generator or an async function).
    pub calls_lane: u64,
    pub calls_tree: u64,
}

/// The state of the bytecode interpreter (B3 of `docs/engine-plan.md`). The format is in `bc.rs`, the loop in `bc_vm.rs`.
///
/// A lane adds its state here, in this struct, and does not add fields to `Interp`.
#[derive(Default)]
struct BcState {
    exec: Exec,
    /// For each script: the scope in `sc_all` of each function, made when the code generator first needs it (`bc_scope.rs`).
    fn_scopes: Vec<Box<[u32]>>,
    /// For each script, for each function: its state.
    codes: Vec<Box<[BcSlot]>>,
    /// Register files of finished frames, for reuse. A running frame owns its registers. They are not GC roots: `collect`
    /// refuses to run during a call, and a script body that runs on bytecode reaches host code only through a call.
    pool: Vec<bc_vm::RegStack>,
    /// Stage 2 off: no function compiles as a register function (the stage 1 listings and tests).
    pub(super) rf_off: bool,
    /// The bindings of the environment of a scope (`PushEnv`, `DeclEnv`), by scope.
    tmpl: FxMap<u32, Rc<[(u32, u8)]>>,
    stats: BcStats,
    /// For each reason that the code generator refused a function: the calls that the tree walker then ran.
    refused: FxMap<&'static str, u64>,
    /// Why the last `bc_finish` refused.
    why: &'static str,
}

impl BcState {
    fn new(exec: Exec) -> Self {
        let mut b = BcState::default();
        b.exec = exec;
        b
    }
}

/// `BARE_JS_STATS=1` prints the counters of a lane that was used, when its interpreter goes (for the measurement of the lane).
impl Drop for BcState {
    fn drop(&mut self) {
        let s = self.stats;
        if s.calls_lane + s.calls_tree > 0 && std::env::var_os("BARE_JS_STATS").is_some() {
            let mut r: Vec<_> = self.refused.iter().collect();
            r.sort();
            eprintln!("bcstats compiled={} fallbacks={} runs={} calls_lane={} calls_tree={} refused={r:?}", s.compiled, s.fallbacks, s.runs, s.calls_lane, s.calls_tree);
        }
    }
}

/// The scope data of the code generator (step B1). A child module of `eval`, so it reads the tables of the tree walker.
#[path = "bc_scope.rs"]
mod bc_scope;
pub use bc_scope::ScopeCheck;

/// The code generator (step B2) and the verifier of its code.
#[path = "bc_gen.rs"]
mod bc_gen;
#[path = "bc_verify.rs"]
mod bc_verify;
pub use bc_gen::GenCheck;

/// The loop (step B3).
#[path = "bc_vm.rs"]
mod bc_vm;
