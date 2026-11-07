/// The state of the bytecode interpreter. Empty until step B3 of `docs/engine-plan.md`.
///
/// B3 adds the compiled code of each function (`Vec<Option<Rc<Code>>>`), the register stack and the frames. The format is in `bc.rs`.
/// A lane adds its state here, in this struct, and does not add fields to `Interp`.
#[derive(Default)]
struct BcState {}
