/// The inline cache tables of the tree walker. Empty until step A2 of `docs/engine-plan.md`.
///
/// A2 adds `expr_ic: Vec<u32>` (for each syntax node, 0 or an index) and `ics: Vec<Ic>` (one entry for each member site that ran
/// twice). The bytecode interpreter keeps its own entries in `Code::ics`. Both call `Heap::ic_own` and `Heap::get_ic`. A lane adds
/// its state here, in this struct, and does not add fields to `Interp`.
#[derive(Default)]
struct IcState {}
