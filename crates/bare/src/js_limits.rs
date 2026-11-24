//! The limits, the clock and the stats that the browser passes to its JavaScript engine (Vex). The types live here, not in the engine.

/// What a script may do before the browser stops it. `max_steps` is the back-edge budget of one task.
#[derive(Clone, Copy, Debug)]
pub struct EvalLimits {
    pub max_steps: u64,
}

impl EvalLimits {
    /// The limits of a browser page.
    pub fn session() -> Self {
        Self::default()
    }
}

impl Default for EvalLimits {
    fn default() -> Self {
        EvalLimits { max_steps: 100_000_000 }
    }
}

/// Heap limits. Vex bounds its own heap, so there is no field.
#[derive(Clone, Copy, Debug, Default)]
pub struct HeapLimits;

/// Why a script ended. `Host` is a cap of the browser: the name says which.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitKind {
    Steps,
    CallDepth,
    TraversalDepth,
    StatementFrames,
    DrainJobs,
    Host(&'static str),
}

/// The clock that `Date.now()` and `new Date()` read.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WallClock {
    System,
    /// A fixed value: a whole number of milliseconds within +-8.64e15.
    Virtual(f64),
    /// The benchmark clock of the process.
    Host,
}

/// What a collection freed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Collected {
    pub objects: usize,
    pub strings: usize,
    pub symbols: usize,
}
