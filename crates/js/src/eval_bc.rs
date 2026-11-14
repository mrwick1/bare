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
    /// For each constant: its global cell. A name has its key and the position of the property in the dictionary of the global
    /// object (`u32::MAX` until a slow path learns it). `GetGlobal` and `SetGlobal` read it. Other constants hold a key that is never
    /// read.
    cells: Box<[(PropertyKey, std::cell::Cell<u32>)]>,
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
    Tree,
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
