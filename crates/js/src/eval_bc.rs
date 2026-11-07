/// The state of the bytecode interpreter. B3 of `docs/engine-plan.md` adds the code of each function, the register stack and the
/// frames. The format is in `bc.rs`.
///
/// A lane adds its state here, in this struct, and does not add fields to `Interp`.
#[derive(Default)]
struct BcState {
    /// For each script: the scope in `sc_all` of each function, made when the code generator first needs it (`bc_scope.rs`).
    #[allow(dead_code)] // read from step B3
    fn_scopes: Vec<Box<[u32]>>,
}

/// The scope data of the code generator (step B1). A child module of `eval`, so it reads the tables of the tree walker.
#[path = "bc_scope.rs"]
mod bc_scope;
pub use bc_scope::ScopeCheck;
