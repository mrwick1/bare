//! The first bounded JavaScript evaluator: a tree walker over the [`crate::parse`] AST and the managed
//! [`Heap`]. It runs tiny real scripts. It is not a staging runtime and it makes no compatibility claim.
//!
//! How to use it
//! - `let prog = parse_script(src, Limits::default())?;`
//! - `let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default())?;`  (checks the whole tree first)
//! - `let v = it.run()?;` runs the script once and gives the completion value. `it.call(f, this, &args)`
//!   calls a function value (a host callback, for example a webpack queue `push`).
//! - Errors: [`Error::Throw`] is a JavaScript exception, [`Error::Unsupported`] is a construct or builtin that
//!   is not implemented, [`Error::Limit`] is an execution limit, [`Error::Heap`] is a heap fault (a bug).
//!
//! More scripts: `Interp::realm` makes an empty realm and [`Interp::run_script`] adds source text to the same heap,
//! global object and global lexical environment. Closures keep the code of their own script. See `eval_scripts.rs`.
//!
//! Unsupported is found BEFORE anything runs. `Interp::new` walks every reachable node. Syntax that is not
//! implemented gives [`Error::Unsupported`] with its span: async functions and `await`, tagged
//! templates, `super` outside a class element, `new.target` in a field initializer, `with`, `for await`, and `await` in `switch`, `for-in`, optional chains, `delete`, `++`/`--`, computed keys and binding patterns.
//! Async functions and arrows run in a bounded continuation machine: see `eval_async.rs`. A regexp literal is compiled by `Interp::new`;
//! an invalid pattern, lookbehind, named groups or the flags `u`, `v`, `d` are `Unsupported` there (see `eval_builtins.rs`).
//! A missing builtin (`WeakRef`, ...) is an ordinary `ReferenceError` at run time, as in a bare engine.
//! `JSON.parse`, `RegExp` and a small `String.prototype` are bound in `eval_builtins.rs`, which lists what is and is not there.
//! Classes are supported: see `eval_classes.rs` for what runs and what does not.
//!
//! Heap contract: the heap never collects by itself. [`Interp::roots`] lists every value the evaluator
//! owns (global object, builtin prototypes, every live environment binding and `this`, every function
//! object, bound arguments, cached string keys, the pending return and completion values). The heap cannot see
//! inside an evaluator environment, so a function object and its environment are linked here, not in the
//! heap: `funcs` maps a function `ObjId` to its code and environment. [`Interp::collect`] first frees the
//! environments and function entries that nothing reaches (a worklist over the object graph, no Rust
//! recursion), then calls `Heap::collect(roots)`. It refuses to run while a call is active. A value you keep
//! outside the evaluator (a `Throw` value, a result) must be passed as `extra` or it is stale after `collect`.
//! Host code must never inject handles from another heap. Heap ownership does not detect foreign-handle collisions.
//!
//! Limits ([`EvalLimits`]): steps (statements, expressions and traversed native elements), call depth (every call, native and bound
//! too), live environments, function slots. A limit ends the run with [`Error::Limit`], never a JS value, and
//! cannot be caught by `try`. A heap limit is [`LimitKind::Heap`].
//! Defaults: `max_call_depth` 100, `max_traversal_depth` 512 and `max_statement_frames` 512 (`EvalLimits::default`). Hard ceilings:
//! `max_call_depth` is clamped to 10,000 (`MAX_CALL_DEPTH`, about the depth of Chrome), `max_traversal_depth` to 768 and
//! `max_statement_frames` to 65,536. `EvalLimits::session()` is the browser page: all three at the ceiling. Three different things
//! are counted:
//! - `max_call_depth`: nested calls, native and bound too. It bounds the memory of the native stack (below).
//! - `max_traversal_depth`: NATIVE recursion of ONE call (expressions, patterns, constructors, the setup walk, `exec`). The count
//!   starts again at zero when a call, a script top level or a setup walk begins (`in_call`), and comes back when it ends. It is
//!   not a count of all calls: it was, up to the 2026-11-06 change, and then 768 allowed fewer than 200 calls of the React shape.
//! - `max_statement_frames`: pending control frames of blocks, loops, `switch`, `try` and labels, which live on the heap in the
//!   statement machine (`eval_statements.rs`), all synchronous calls together. A statement frame is not a native frame.
//!
//! Native stack. No caller has to choose a thread size. A call of a script function, a bound function, a proxy or the host
//! (`call_fn_inner`, `construct_inner`), a script top level and a setup walk compare the stack pointer with `stack_floor`
//! (`low_stack`, 4 instructions). A native callee does not: it reaches script code only through a call that does. When less than `STACK_RED` is left (4 MiB, 16 MiB in a debug build),
//! the call continues on a new segment (`stacker::grow`: `STACK_SEG` of mapped memory, 64 MiB, 256 MiB in a debug build, committed
//! only where the stack touches it, unmapped when the call returns). `STACK_RED` covers one call at `max_traversal_depth`
//! frames (measured 0.51 MB release and 2.9 MB debug, the worst of 20 shapes) and the native code that a call reaches. Measured
//! on the React commit shape: 2.8 KB of native stack for each JS call in release and 15 KB in debug (4.9 KB and 26 KB before).
//! At the ceiling of 10,000 calls a run used 31 to 58 MB of peak memory in release (recursion through `map`, `sort`, getters,
//! `toJSON`, `Reflect.apply`, `new`). Known gaps: `super()` and the host do not start a call through `in_call` in every path (the 768
//! frames still end such a recursion); `stacker::grow` aborts if the system refuses the mapping (a small address-space limit);
//! an `Interp` must run on one thread stack at a time (it is not `Send`).
//!
//! Known deviations, on purpose (each is a gate, not a silent skip):
//! - One environment per call holds parameters, `arguments`, `var`s and the body's `let`/`const`.
//!   Default-parameter expressions can see the body `var`s. Parameters have no TDZ.
//! - `arguments` is an unmapped ordinary object (not iterable); sloppy mode never maps it to the parameters.
//! - A sloppy function boxes a primitive `this` (a wrapper object, `eval_wrapper.rs`). A `BigInt` wrapper does not exist: no BigInt.
//! - Builtins are a small set (see `install_builtins` and `install_ext`). `String.prototype` has only the methods the
//!   staging bundles call. Symbols, the iterator protocol (with `IteratorClose`), Map and Set are in `eval_symbols.rs`
//!   and `eval_collections.rs`; their exact support and gates are listed at the top of those files. `ToPrimitive`,
//!   `Object.prototype.toString` and `instanceof` read `@@toPrimitive`, `@@toStringTag` and `@@hasInstance`.
//! - The script completion value is the value of the last expression statement that ran (reset to
//!   `undefined` by `if`, loops, `switch`, `try`). It is not full UpdateEmpty.
//! - A function's `toString` is absent.

use crate::ast::*;
use crate::date;
use crate::{ieee754, number};
use crate::heap::*;
use crate::value::*;
use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};
use std::marker::PhantomData;
use std::rc::Rc;

/// Hard ceilings of the three depth limits (see the header). `EvalLimits::session` uses all three.
pub const MAX_CALL_DEPTH: u32 = 10_000;
pub const MAX_TRAVERSAL_DEPTH: u32 = 768;
pub const MAX_STATEMENT_FRAMES: u32 = 1 << 16;
/// Stack that must remain below the start of a call (see `Interp::in_call`). One activation at `MAX_TRAVERSAL_DEPTH` frames
/// measured 0.51 MB in release and 2.9 MB in debug (a chain of 760 member reads, the worst of 20 shapes); the rest is for the
/// native code that a call reaches. A segment is `STACK_SEG` of mapped memory, committed only where the stack touches it.
const STACK_RED: usize = if cfg!(debug_assertions) { 16 << 20 } else { 4 << 20 };
const STACK_SEG: usize = if cfg!(debug_assertions) { 256 << 20 } else { 64 << 20 };

// ------------------------------------------------------------------ public types

/// Execution limits. Every one is checked before the thing grows.
#[derive(Clone, Copy, Debug)]
pub struct EvalLimits {
    /// Statements, expressions and traversed native elements, per `run` or top-level `call`.
    pub max_steps: u64,
    /// Nested calls. Clamped to 10,000. The stack grows by itself (see the header), so this bounds the memory of the native stack. The default is 100.
    pub max_call_depth: u32,
    /// Native recursion of ONE call: expressions, patterns, constructors, setup walks and `exec`. Counted from the start of each call. Clamped to 768. The default is 512.
    pub max_traversal_depth: u32,
    /// Pending statement control frames (blocks, loops, `switch`, `try`, labels) of all synchronous calls together. Heap, not
    /// native stack. Clamped to 65,536. The default is 512. Zero refuses the first body.
    pub max_statement_frames: u32,
    /// Live environments (scopes). Environments that no closure captured are freed on scope exit.
    pub max_envs: usize,
    /// Function objects the evaluator tracks (closures, natives, bound functions).
    pub max_functions: usize,
    /// Scripts a realm keeps (see [`Interp::run_script`]). The first slot, empty or not, counts.
    pub max_scripts: usize,
    /// Source bytes of all kept scripts together. A script that does not fit is refused before it is parsed.
    pub max_script_source: usize,
    /// Syntax tree nodes (plus list entries) of all kept scripts together. The parser stops at the budget.
    pub max_script_nodes: usize,
}

impl EvalLimits {
    /// The limits of a browser page: calls up to the ceiling (10,000, as Chrome), native recursion of one call up to its ceiling.
    pub fn session() -> Self {
        EvalLimits { max_call_depth: MAX_CALL_DEPTH, max_traversal_depth: MAX_TRAVERSAL_DEPTH, max_statement_frames: MAX_STATEMENT_FRAMES, ..Default::default() }
    }
}

impl Default for EvalLimits {
    fn default() -> Self {
        EvalLimits { max_steps: 100_000_000, max_call_depth: 100, max_traversal_depth: 512, max_statement_frames: 512, max_envs: 1 << 20, max_functions: 1 << 20, max_scripts: 4096, max_script_source: 64 << 20, max_script_nodes: 8 << 20 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitKind {
    Steps,
    CallDepth,
    TraversalDepth,
    /// Pending statement control frames (see [`EvalLimits::max_statement_frames`]).
    StatementFrames,
    Envs,
    Functions,
    /// Microtask queue length.
    PromiseQueue,
    /// Tracked promise objects.
    Promises,
    /// Reactions stored on pending promises.
    PromiseReactions,
    /// Jobs per microtask drain.
    DrainJobs,
    /// Suspended async function activations.
    AsyncActivations,
    /// Saved continuation frames of async activations (all together).
    AsyncFrames,
    /// Saved values, keys and counters of async activations (all together).
    AsyncSlots,
    /// Await-containing nodes marked by the preflight of async functions (all scripts together).
    AsyncCode,
    /// The realm keeps too many scripts.
    Scripts,
    /// The kept scripts have too many source bytes together.
    ScriptSource,
    /// The kept scripts have too many syntax nodes together, or one list or nesting is too large.
    ScriptNodes,
    /// A cap of the embedder's [`Host`] (nodes, listeners, traversal results, ...). The name says which.
    Host(&'static str),
    /// Private slots of all objects together (see `Interp::set_private_slot_limit`).
    PrivateSlots,
    Heap(Limit),
    /// A bound of a RegExp binding (see `eval_builtins.rs`).
    Regexp(RegexpLimit),
    /// A bound of the `JSON.parse` text parser.
    Json(crate::json::LimitKind),
    /// A bound of `JSON.stringify` (see `eval_json_stringify.rs`).
    Stringify(StringifyLimit),
    /// A cap of `Promise.all` and the other combinators (see [`AggLimit`]).
    Agg(AggLimit),
    /// Symbol registry, Map/Set, iterator and iteration-buffer caps (see [`ScLimit`]).
    Sc(ScLimit),
}

/// Valid syntax or a builtin that is not implemented. `span` is the byte range of the construct
/// (empty for a builtin).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unsupported {
    pub what: &'static str,
    pub span: Span,
}

#[derive(Clone, Copy, Debug)]
pub enum Error {
    /// A thrown JavaScript value. Root it (`extra` of [`Interp::collect`]) if you keep it.
    Throw(Value),
    Unsupported(Unsupported),
    Limit(LimitKind),
    Heap(HeapError),
}

/// Counters for tools and tests.
#[derive(Clone, Copy, Debug, Default)]
pub struct EvalStats {
    pub steps: u64,
    pub live_envs: usize,
    pub env_slots: usize,
    pub functions: usize,
    /// Scripts kept by the realm (the first slot counts), their source bytes and their syntax nodes.
    pub scripts: usize,
    pub script_source: usize,
    pub script_nodes: usize,
    /// Capacity bytes (see `Interp::stats`): syntax trees, the caches that go with them, environments, the function table.
    pub ast_bytes: usize,
    pub script_cache_bytes: usize,
    pub env_bytes: usize,
    pub func_bytes: usize,
}

// ------------------------------------------------------------------ small internals

/// `Interp::gc_due`: collect when the accounted heap is half as large again as what the previous collect left, and more than this many
/// bytes. QuickJS (1.5 x the allocated size) and SpiderMonkey (low-frequency growth of 150%) use the same factor.
const GC_MIN_BYTES: usize = 8 << 20;

/// Capacity to keep in a table of `len` entries after a collect: twice the live entries, or the live entries plus
/// `since` (what the program made since the previous collect), so steady churn does not shrink and regrow every
/// round. `Some` only when that frees at least 1 MiB and at least half of the capacity, as `Slab::trim` does.
fn trim_to(len: usize, cap: usize, since: usize, entry: usize) -> Option<usize> {
    let keep = (len * 2).max(len.saturating_add(since));
    (cap >= keep.saturating_mul(2) && (cap - keep).saturating_mul(entry) >= 1 << 20).then_some(keep)
}

#[derive(Default)]
struct Fx(u64);
impl Hasher for Fx {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, b: &[u8]) {
        for &x in b {
            self.write_u64(x as u64);
        }
    }
    fn write_u32(&mut self, n: u32) {
        self.write_u64(n as u64);
    }
    fn write_u64(&mut self, n: u64) {
        self.0 = (self.0.rotate_left(5) ^ n).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
    fn write_usize(&mut self, n: usize) {
        self.write_u64(n as u64);
    }
}
type FxMap<K, V> = HashMap<K, V, BuildHasherDefault<Fx>>;
type FxSet<K> = HashSet<K, BuildHasherDefault<Fx>>;

/// Statement control flow. Errors travel in `Err` too.
enum Ctl {
    Break(u32),
    Continue(u32),
    Return,
    Err(Error),
}
impl From<Error> for Ctl {
    fn from(e: Error) -> Ctl {
        Ctl::Err(e)
    }
}
type Res<T> = Result<T, Error>;
type Ex = Result<(), Ctl>;

macro_rules! h {
    ($s:expr, $e:expr) => {
        match $e {
            Ok(v) => v,
            Err(e) => return Err($s.herr(e).into()),
        }
    };
}

// `h!` for a heap property operation that can reach a host-indexed object. `NeedsHost` (a property of such an object has no
// JS object yet) is not an error: the host makes the object (`Host::fill_indexed`) and the operation runs again. The heap
// changed nothing when it asked, so a repeat is safe. At most 2 repeats: the host must have made the object by then, or the
// result is an error, never a loop. Only for expressions that move nothing, because they run again.
// `hx!(s, e, p)` and `hxw!(s, e, p)`: `p` is what to do when the operation met a `Proxy` (`HeapError::Proxy`). It is an expression
// of the same type as the answer. It runs the internal method of the proxy (`eval_proxy.rs`), which can call script. Without `p`, a
// proxy is `Unsupported` (`herr`): the path has no trap dispatch yet.
macro_rules! hx {
    ($s:expr, $e:expr, $px:expr) => {{
        let mut fills = 0u8;
        loop {
            match $e {
                Ok(v) => break v,
                Err(HeapError::NeedsHost { owner, index }) if fills < 2 => {
                    fills += 1;
                    if let Err(e) = $s.host_fill(owner, index) {
                        return Err(e.into());
                    }
                }
                Err(HeapError::Proxy(_)) => {
                    #[allow(unreachable_code)]
                    {
                        break $px;
                    }
                }
                Err(e) => return Err($s.herr(e).into()),
            }
        }
    }};
    ($s:expr, $e:expr) => {{
        let mut fills = 0u8;
        loop {
            match $e {
                Ok(v) => break v,
                Err(HeapError::NeedsHost { owner, index }) if fills < 2 => {
                    fills += 1;
                    if let Err(e) = $s.host_fill(owner, index) {
                        return Err(e.into());
                    }
                }
                Err(e) => return Err($s.herr(e).into()),
            }
        }
    }};
}

// `hxw!` is `hx!` for a write (`set`, `define_own_property`, `delete`). It also answers `NeedsNamed`: the host does the write of
// a host named object (`Host::named_write`), the heap keeps its answer, and the repeat returns it (`heap_named.rs`). Reads have
// no such arm: a read never asks for it, and the reads are the hot paths.
macro_rules! hxw {
    ($s:expr, $e:expr, $px:expr) => {{
        let mut fills = 0u8;
        loop {
            match $e {
                Ok(v) => break v,
                Err(ask @ (HeapError::NeedsHost { .. } | HeapError::NeedsNamed)) if fills < 2 => {
                    fills += 1;
                    if let Err(e) = $s.host_ask(ask) {
                        return Err(e.into());
                    }
                }
                Err(HeapError::Proxy(_)) => {
                    #[allow(unreachable_code)]
                    {
                        break $px;
                    }
                }
                Err(e) => return Err($s.herr(e).into()),
            }
        }
    }};
    ($s:expr, $e:expr) => {{
        let mut fills = 0u8;
        loop {
            match $e {
                Ok(v) => break v,
                Err(ask @ (HeapError::NeedsHost { .. } | HeapError::NeedsNamed)) if fills < 2 => {
                    fills += 1;
                    if let Err(e) = $s.host_ask(ask) {
                        return Err(e.into());
                    }
                }
                Err(e) => return Err($s.herr(e).into()),
            }
        }
    }};
}

const NONE: u32 = u32::MAX;
const INIT: u8 = 1;
const MUT: u8 = 2;
const SELF: u8 = 4;

#[derive(Clone, Copy)]
struct Binding {
    name: u32,
    flags: u8,
    value: Value,
}

struct Env {
    parent: u32,
    bindings: Vec<Binding>,
    index: Option<Box<FxMap<u32, u32>>>,
    this: Option<Value>,
    captured: bool,
    live: bool,
    var_scope: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Nat {
    ObjectCtor,
    ObjectKeys,
    ObjectDefineProperty,
    ObjectGetOwnPropertyDescriptor,
    ObjectCreate,
    ObjectGetPrototypeOf,
    ObjHasOwn,
    ObjToString,
    ObjValueOf,
    FnCall,
    FnApply,
    FnBind,
    FnCtor,
    ArrayCtor,
    ArrayIsArray,
    ArrPush,
    ArrPop,
    ArrForEach,
    ArrJoin,
    ErrorCtor(u8),
    ErrToString,
    StringFn,
    NumberFn,
    BooleanFn,
    IsNaN,
    ParseInt,
    EncodeUriComponent,
    ParseFloat,
    IsFinite,
    EncodeUri,
    DecodeUri,
    DecodeUriComponent,
    Escape,
    Unescape,
    /// JSON, RegExp and String.prototype bindings (`eval_builtins.rs`).
    Ext(Bi),
    Bin(BinFn),
    Ao(AoFn),
    Sc(ScFn),
    Refl(ReflFn),
    Px(PxFn),
    Date(DateFn),
    Intl(IntlFn),
    Big(BigFn),
}

enum Callee {
    /// `script` is the slot of the script that owns `fid`.
    Script { script: u32, fid: FuncId, env: u32, self_binding: bool },
    Native(Nat),
    Bound { target: ObjId, this: Value, args: Vec<Value> },
    Promise(PFn),
    /// A method of the embedder's [`Host`]. `ctor` allows `new`. `nt` (a constructor only) also takes a different `new.target`.
    Host { id: u32, ctor: bool, nt: bool },
    /// A class constructor. Its code and fields are in `ClassState::classes` (see `eval_classes.rs`).
    Class,
    /// A proxy whose target is callable (see `eval_proxy.rs`). The target and handler are in the heap, so a revoked proxy stays
    /// callable as a function and then throws on use. `ctor`: the target is a constructor.
    Proxy { ctor: bool },
    /// The `revoke` function of `Proxy.revocable`: the proxy to revoke, `None` after the first call.
    Revoke(Option<ObjId>),
}

#[derive(Default)]
struct FScope {
    vars: Vec<u32>,
    uses_args: bool,
    /// The first walk saw something that keeps the function out of frame slots (see `eval_scope.rs`): an inner function or class,
    /// `new.target`, `super` or `delete name`. `uses_this`: the function names `this`.
    nofr: bool,
    uses_this: bool,
    /// The function has a loop or a call, so one call can be long or recursive: it gets its frame at the first call. Other
    /// functions get it at the second call, so that a function that runs once pays nothing.
    eager: bool,
    /// The state of the function for frame slots: `FR_ENV` and the others (see `eval_scope.rs`).
    fr: u8,
    /// The scopes of this function in `Interp::sc_all`: `sc_lo..sc_hi`.
    sc_lo: u32,
    sc_hi: u32,
    /// The template of the frame, as a call starts: `Interp::fr_pool[fr_off..fr_off + fr_len]` (see `fr_layout`). Valid in `FR_ON`.
    fr_off: u32,
    fr_len: u32,
}

const TYPE_ERROR: usize = 1;
const RANGE_ERROR: usize = 2;
const REFERENCE_ERROR: usize = 3;
const URI_ERROR: usize = 5;
const ERR_NAMES: [&str; 7] = ["Error", "TypeError", "RangeError", "ReferenceError", "SyntaxError", "URIError", "EvalError"];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Hint {
    Default,
    Number,
    String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Bind {
    /// Initialize a binding that `declare_scope` made (let, const).
    Init,
    /// Assign (assignment expression, `var` initializer).
    Assign,
    /// Create the binding in the current environment (parameter, catch parameter).
    Declare,
}

/// A started iteration (see `iter_open` in `eval_symbols.rs`): an array-like or string by index, a Map or Set by
/// cursor, or any object through the iterator protocol. `done` is set at the end, after an error and after a close.
#[derive(Clone, Copy)]
struct It {
    src: Value,
    i: usize,
    kind: ItKind,
    done: bool,
}

/// A reference that can be read and written: a name or a property of a value.
enum Ref {
    /// A name and its site (see `eval_scope.rs`).
    Name(u32, u32),
    Prop(Value, PropertyKey),
    /// `base.#name`: the name object, the `#name` id and the binding flags.
    Priv(Value, ObjId, u32, u8),
    /// `super[key]` with this value of `this`.
    Super(Value, PropertyKey),
}

// ------------------------------------------------------------------ the interpreter

pub struct Interp<'p> {
    /// The code of the current script. See `eval_scripts.rs`.
    prog: Rc<Program<'static>>,
    cur: usize,
    scripts: Vec<Code>,
    script_source: usize,
    script_nodes: usize,
    _p: PhantomData<&'p ()>,
    /// The heap. Public so a host can read results. Do not collect it directly: use [`Interp::collect`].
    pub heap: Heap,
    limits: EvalLimits,
    steps: u64,
    step_limit: u64,
    depth: u32,
    /// Native frames since the last call began (see `in_call`). A call starts again from zero.
    frames: u32,
    /// The lowest stack address a call may start at. Below it the stack grows (see `grow_stack`).
    stack_floor: usize,
    /// Pending control frames of all synchronous calls together (see `eval_statements.rs`).
    stmt_frames: u32,
    /// Empty frame buffers of finished machine runs, reused to save an allocation per call (see `eval_statements.rs`).
    sf_pool: Vec<Vec<SFrame>>,
    ran: bool,
    // names and caches filled by the first walk
    name_map: FxMap<String, u32>,
    names: Vec<String>,
    name_strs: Vec<Option<StrId>>,
    /// For each name: the position of its property on the global object when it was last found there (a hint, checked on use).
    name_gpos: Vec<u32>,
    expr_name: Vec<u32>,
    pat_name: Vec<u32>,
    stmt_label: Vec<u32>,
    func_name: Vec<u32>,
    expr_str: Vec<Option<StrId>>,
    prop_str: Vec<Option<StrId>>,
    scopes: Vec<FScope>,
    /// Static scopes and identifier slots of the current script (see `eval_scope.rs`), and the scope the first walk is in.
    slots: Slots,
    /// The scopes of all scripts and their names (see `eval_scope.rs`). A script keeps its scopes after it ran.
    sc_all: Vec<RScope>,
    sc_pool: Vec<u32>,
    sc_cur: u32,
    sc_fn_expr: bool,
    /// The function (an index of `scopes`) and the number of class bodies that the first walk is in.
    sc_fn: usize,
    sc_class: u32,
    /// Where the frame of the running function starts in `envs[FRAME_ENV].bindings` (see `eval_scope.rs`).
    fp: usize,
    /// The frame templates of all functions, and the scratch of `fr_layout`.
    fr_pool: Vec<Binding>,
    fr_names: Vec<u32>,
    fr_ends: Vec<u32>,
    // environments and functions
    envs: Vec<Env>,
    free_envs: Vec<u32>,
    /// Heap object allocations at the end of the previous `collect` (see `trim_tables`).
    gc_allocs: u64,
    /// Accounted heap bytes at the end of the previous `collect` (see `gc_due`).
    gc_live: usize,
    live_envs: usize,
    funcs: FxMap<ObjId, Callee>,
    // run state
    env: u32,
    strict: bool,
    ret: Value,
    completion: Value,
    pending_labels: Vec<u32>,
    arg_pool: Vec<Vec<Value>>,
    /// The strings object of each tagged template site, by (script, expression). Kept for the realm's life, as in the spec.
    tpl_objs: FxMap<(u32, u32), ObjId>,
    // builtin objects
    global: ObjId,
    object_proto: ObjId,
    function_proto: ObjId,
    array_proto: ObjId,
    error_protos: [ObjId; 7],
    promise: PromiseState,
    /// JSON, RegExp and String bindings: state of `eval_builtins.rs`.
    ext: Ext,
    host: Option<Rc<dyn Host>>,
    bin: BinaryState,
    ao: AoState,
    sc: ScState,
    weak: WeakState,
    px: ProxyState,
    date: DateState,
    big: BigState,
    intl: IntlState,
    aw: AsyncState,
    // permanent keys
    k_length: PropertyKey,
    k_name: PropertyKey,
    k_message: PropertyKey,
    k_prototype: PropertyKey,
    k_constructor: PropertyKey,
    k_to_string: PropertyKey,
    k_value_of: PropertyKey,
    k_proto: PropertyKey,
    k_value: PropertyKey,
    k_writable: PropertyKey,
    k_enumerable: PropertyKey,
    k_configurable: PropertyKey,
    k_get: PropertyKey,
    k_set: PropertyKey,
    // Walk context
    cur_strict: bool,
    n_arguments: u32,
    /// Class state (see `eval_classes.rs`).
    cls: ClassState,
    /// Call frames for `Error.stack` (see `eval_stack.rs`).
    stk: StackState,
    /// `new.target` of the host constructor that is running (see `call_host`). Only set for the length of that call.
    host_nt: Option<ObjId>,
    k_stack: PropertyKey,
}

fn unsupported(what: &'static str, span: Span) -> Error {
    Error::Unsupported(Unsupported { what, span })
}

fn is_nullish(v: Value) -> bool {
    matches!(v, Value::Undefined | Value::Null)
}

impl<'p> Interp<'p> {
    /// Check the whole tree (Unsupported is reported here, before anything runs) and build the global
    /// object and the builtins in `heap`.
    pub fn new(prog: &'p Program<'p>, heap: Heap, limits: EvalLimits) -> Result<Interp<'p>, Error> {
        Self::check_code_limits(prog, &limits)?;
        let mut it = Self::with_code(Rc::new(prog.clone().into_owned()), heap, limits)?;
        it.in_call(|it| it.walk_program())?;
        it.install_builtins()?;
        it.install_sc()?;
        it.install_ext()?;
        it.install_array_object()?;
        it.install_promise()?;
        it.install_binary()?;
        it.install_reflect()?;
        it.install_date()?;
        it.install_intl()?;
        it.install_bigint()?;
        Ok(it)
    }

    fn check_code_limits(prog: &Program<'_>, limits: &EvalLimits) -> Res<()> {
        if limits.max_scripts == 0 { return Err(Error::Limit(LimitKind::Scripts)); }
        if prog.source.len() > limits.max_script_source { return Err(Error::Limit(LimitKind::ScriptSource)); }
        let stats = prog.stats();
        if stats.nodes() + stats.list_items > limits.max_script_nodes { return Err(Error::Limit(LimitKind::ScriptNodes)); }
        if limits.max_envs == 0 { return Err(Error::Limit(LimitKind::Envs)); }
        Ok(())
    }

    fn with_code(prog: Rc<Program<'static>>, mut heap: Heap, mut limits: EvalLimits) -> Result<Interp<'p>, Error> {
        let stats = prog.stats();
        Self::check_code_limits(&prog, &limits)?;
        limits.max_call_depth = limits.max_call_depth.min(MAX_CALL_DEPTH);
        limits.max_traversal_depth = limits.max_traversal_depth.min(MAX_TRAVERSAL_DEPTH);
        limits.max_statement_frames = limits.max_statement_frames.min(MAX_STATEMENT_FRAMES);
        if limits.max_envs == 0 { return Err(Error::Limit(LimitKind::Envs)); }
        macro_rules! e {
            ($x:expr) => {
                match $x {
                    Ok(v) => v,
                    Err(e) => return Err(match e {
                        HeapError::Limit(l) => Error::Limit(LimitKind::Heap(l)),
                        e => Error::Heap(e),
                    }),
                }
            };
        }
        let object_proto = e!(heap.new_object(None));
        let function_proto = e!(heap.new_object(Some(object_proto)));
        let array_proto = e!(heap.new_array(Some(object_proto)));
        let global = e!(heap.new_object(Some(object_proto)));
        let mut error_protos = [object_proto; 7];
        for p in error_protos.iter_mut() {
            *p = e!(heap.new_object(Some(object_proto)));
        }
        let ext = e!(Ext::new(&mut heap, object_proto));
        let mut key = |s: &str| heap.key_from_str(s);
        let ks = [
            e!(key("length")), e!(key("name")), e!(key("message")), e!(key("prototype")), e!(key("constructor")),
            e!(key("toString")), e!(key("valueOf")), e!(key("__proto__")), e!(key("value")), e!(key("writable")),
            e!(key("enumerable")), e!(key("configurable")), e!(key("get")), e!(key("set")), e!(key("stack")),
        ];
        let p = &prog;
        let mut it = Interp {
            cur: 0,
            scripts: vec![Code::placeholder(&prog)],
            script_source: prog.source.len(),
            script_nodes: stats.nodes() + stats.list_items,
            _p: PhantomData,
            strict: prog.strict,
            cur_strict: prog.strict,
            prog: prog.clone(),
            heap,
            limits,
            steps: 0,
            step_limit: 0,
            depth: 0,
            frames: 0,
            stack_floor: usize::MAX,
            stmt_frames: 0,
            sf_pool: Vec::new(),
            ran: false,
            name_map: FxMap::default(),
            names: vec![String::new()],
            name_strs: vec![None],
            name_gpos: vec![NONE],
            expr_name: vec![0; p.stats().exprs],
            pat_name: vec![0; p.stats().pats],
            stmt_label: vec![0; p.stats().stmts],
            func_name: vec![0; p.stats().funcs],
            expr_str: vec![None; p.stats().exprs],
            prop_str: vec![None; p.stats().props],
            scopes: (0..=p.stats().funcs).map(|_| FScope::default()).collect(),
            slots: Slots::new(&p.stats()),
            sc_all: Vec::new(),
            sc_pool: Vec::new(),
            sc_cur: NONE,
            sc_fn_expr: false,
            sc_fn: 0,
            sc_class: 0,
            fp: 0,
            fr_pool: Vec::new(),
            fr_names: Vec::new(),
            fr_ends: Vec::new(),
            envs: Vec::new(),
            free_envs: Vec::new(),
            gc_allocs: 0,
            gc_live: 0,
            live_envs: 0,
            funcs: FxMap::default(),
            env: 0,
            ret: Value::Undefined,
            completion: Value::Undefined,
            pending_labels: Vec::new(),
            arg_pool: Vec::new(),
            tpl_objs: FxMap::default(),
            global,
            object_proto,
            function_proto,
            array_proto,
            error_protos,
            promise: PromiseState::default(),
            ext,
            host: None,
            bin: BinaryState::default(),
            ao: AoState::default(),
            sc: ScState::new(object_proto, ks[0]),
            weak: WeakState::new(object_proto),
            px: ProxyState::default(),
            date: DateState::new(object_proto),
            big: BigState::new(object_proto),
            intl: IntlState::new(object_proto),
            aw: AsyncState::default(),
            k_length: ks[0],
            k_name: ks[1],
            k_message: ks[2],
            k_prototype: ks[3],
            k_constructor: ks[4],
            k_to_string: ks[5],
            k_value_of: ks[6],
            k_proto: ks[7],
            k_value: ks[8],
            k_writable: ks[9],
            k_enumerable: ks[10],
            k_configurable: ks[11],
            k_get: ks[12],
            k_set: ks[13],
            k_stack: ks[14],
            stk: StackState::default(),
            host_nt: None,
            n_arguments: 0,
            cls: ClassState::default(),
        };
        it.n_arguments = it.intern_name("arguments");
        it.cls_init();
        h!(it, it.heap.set_lazy_prototype(it.object_proto, it.k_prototype, it.k_constructor));
        // The global lexical environment: id 0, never freed.
        it.envs.push(Env { parent: NONE, bindings: Vec::new(), index: None, this: Some(Value::Object(global)), captured: true, live: true, var_scope: true });
        // Environment 1 holds the frames of functions that keep their variables in slots (`eval_scope.rs`). It is never freed
        // and no closure reaches it, and it is empty while no call runs. It is not counted in `live_envs`.
        it.envs.push(Env { parent: NONE, bindings: Vec::new(), index: None, this: None, captured: true, live: true, var_scope: true });
        it.live_envs = 1;
        Ok(it)
    }

    /// Give the heap back (the interpreter and every environment are dropped).
    pub fn into_heap(self) -> Heap {
        self.heap
    }

    pub fn global_object(&self) -> ObjId {
        self.global
    }

    pub fn stats(&self) -> EvalStats {
        use std::mem::size_of;
        let ast_bytes = self.scripts.iter().map(|c| c.prog.stats().bytes + c.prog.source.len()).sum();
        let script_cache_bytes = self.scripts.iter().map(|c| c.cache_bytes()).sum();
        let env_bytes = self.envs.capacity() * size_of::<Env>() + self.envs.iter().map(|e| e.bindings.capacity() * size_of::<Binding>() + e.index.as_ref().map_or(0, |m| m.capacity() * 9)).sum::<usize>();
        let func_bytes = self.funcs.capacity() * (size_of::<(ObjId, Callee)>() + 1);
        EvalStats { steps: self.steps, live_envs: self.live_envs, env_slots: self.envs.len(), functions: self.funcs.len(), scripts: self.scripts.len(), script_source: self.script_source, script_nodes: self.script_nodes, ast_bytes, script_cache_bytes, env_bytes, func_bytes }
    }

    /// Run the script once. Returns its completion value (see the module comment). A second call is
    /// `Unsupported` because top-level `let` would be redeclared.
    pub fn run(&mut self) -> Result<Value, Error> {
        if self.ran {
            return Err(unsupported("run called twice", Span::default()));
        }
        self.ran = true;
        self.exec_script(0)
    }

    /// Call a function value (a closure, a native or a bound function) from the host.
    pub fn call(&mut self, f: Value, this: Value, args: &[Value]) -> Result<Value, Error> {
        if self.depth == 0 {
            self.begin();
        }
        if !self.is_callable(f) {
            return Err(self.type_err("value is not a function"));
        }
        self.call_fn(f, this, args)
    }

    fn begin(&mut self) {
        self.step_limit = self.steps.saturating_add(self.limits.max_steps);
    }

    /// Every value the evaluator owns, for `Heap::collect`. Complete at any time.
    pub fn roots(&self) -> Vec<Value> {
        let mut v = vec![Value::Object(self.global), Value::Object(self.object_proto), Value::Object(self.function_proto), Value::Object(self.array_proto), self.ret, self.completion];
        v.extend(self.error_protos.iter().map(|&o| Value::Object(o)));
        v.extend(self.host_nt.map(Value::Object));
        self.ext_roots(&mut v);
        for k in [self.k_length, self.k_name, self.k_message, self.k_prototype, self.k_constructor, self.k_to_string, self.k_value_of, self.k_proto, self.k_value, self.k_writable, self.k_enumerable, self.k_configurable, self.k_get, self.k_set, self.k_stack] {
            if let PropertyKey::String(s) = k {
                v.push(Value::String(s));
            }
        }
        v.extend(self.name_strs.iter().flatten().map(|&s| Value::String(s)));
        v.extend(self.expr_str.iter().flatten().map(|&s| Value::String(s)));
        v.extend(self.prop_str.iter().flatten().map(|&s| Value::String(s)));
        v.extend(self.tpl_objs.values().map(|&o| Value::Object(o)));
        self.script_roots(&mut v);
        for e in self.envs.iter().filter(|e| e.live) {
            v.extend(e.bindings.iter().map(|b| b.value));
            v.extend(e.this);
        }
        v.extend(self.ao.array_ctor.map(Value::Object));
        self.numeric_roots(&mut v);
        self.promise_roots(&mut v);
        self.binary_roots(&mut v);
        self.promise_all_values(&mut v);
        if let Some(h) = &self.host {
            h.roots(&mut v);
        }
        self.sc_roots(&mut v);
        self.sc_all_values(&mut v);
        self.weak_roots(&mut v);
        self.weak_all_values(&mut v);
        self.date_roots(&mut v);
        self.big_roots(&mut v);
        self.date_all_values(&mut v);
        self.intl_roots(&mut v);
        self.intl_all_values(&mut v);
        self.async_all_values(&mut v);
        self.gen_all_values(&mut v);
        for (&o, c) in &self.funcs {
            v.push(Value::Object(o));
            if let Callee::Promise(pf) = c {
                pf.trace(&mut v);
            }
            if let Callee::Bound { target, this, args } = c {
                v.push(Value::Object(*target));
                v.push(*this);
                v.extend(args.iter().copied());
            }
            if let Callee::Revoke(Some(p)) = c {
                v.push(Value::Object(*p));
            }
        }
        self.proxy_roots(&mut v);
        self.cls_roots(&mut v);
        v
    }

    /// Free unreachable environments and function entries, then `Heap::collect`. `extra` are the values
    /// the host still holds (results, thrown values). Refused while a call is active.
    pub fn collect(&mut self, extra: &[Value]) -> Result<Collected, Error> {
        if self.depth != 0 || self.stmt_frames != 0 {
            return Err(unsupported("collect during a call", Span::default()));
        }
        let mut seen: FxSet<ObjId> = FxSet::default();
        let mut env_seen = vec![false; self.envs.len()];
        env_seen[FRAME_ENV as usize] = true;
        let mut work: Vec<Value> = vec![Value::Object(self.global), Value::Object(self.object_proto), Value::Object(self.function_proto), Value::Object(self.array_proto), self.ret, self.completion];
        work.extend(self.error_protos.iter().map(|&o| Value::Object(o)));
        self.ext_roots(&mut work);
        work.extend_from_slice(extra);
        work.extend(self.ao.array_ctor.map(Value::Object));
        self.numeric_roots(&mut work);
        self.promise_roots(&mut work);
        let host = self.host.clone();
        let mut host_vals: Vec<Value> = Vec::new();
        if let Some(h) = &host {
            h.roots(&mut host_vals);
            work.extend_from_slice(&host_vals);
        }
        self.binary_roots(&mut work);
        self.sc_roots(&mut work);
        self.weak_roots(&mut work);
        self.date_roots(&mut work);
        self.big_roots(&mut work);
        self.intl_roots(&mut work);
        let mut pending: FxMap<ObjId, Vec<Value>> = FxMap::default();
        let mut agg_seen: FxSet<u64> = FxSet::default();
        let mut env_work: Vec<u32> = vec![0];
        let mut act_seen = vec![false; self.aw.acts.len()];
        self.gen_roots(&mut work);
        work.extend(self.tpl_objs.values().map(|&o| Value::Object(o)));
        self.async_queue_roots(&mut work, &mut env_work, &mut act_seen);
        loop {
            if let Some(e) = env_work.pop() {
                if e == NONE || env_seen[e as usize] {
                    continue;
                }
                env_seen[e as usize] = true;
                let env = &self.envs[e as usize];
                work.extend(env.bindings.iter().map(|b| b.value));
                work.extend(env.this);
                env_work.push(env.parent);
                continue;
            }
            let Some(v) = work.pop() else {
                if !self.promise.caps.is_empty() && self.caps_trace(&seen, &mut work) {
                    continue;
                }
                break;
            };
            let Value::Object(o) = v else { continue };
            if !seen.insert(o) {
                continue;
            }
            // A host function owns no host state: the embedder's `trace` is not asked about it.
            let mut host_fn = false;
            match self.funcs.get(&o) {
                Some(Callee::Host { .. }) => host_fn = true,
                Some(Callee::Script { env, .. }) => env_work.push(*env),
                Some(Callee::Bound { target, this, args }) => {
                    work.push(Value::Object(*target));
                    work.push(*this);
                    work.extend(args.iter().copied());
                }
                Some(&Callee::Promise(pf)) => self.pfn_trace(pf, &mut agg_seen, &mut work),
                Some(&Callee::Revoke(Some(p))) => work.push(Value::Object(p)),
                _ => {}
            }
            self.promise_trace(o, &mut work);
            if let Some(h) = host.as_ref().filter(|_| !host_fn) {
                let from = host_vals.len();
                h.trace(o, &mut host_vals);
                work.extend_from_slice(&host_vals[from..]);
            }
            if let Some((buf, _, _)) = self.heap.view_parts(o) {
                work.push(Value::Object(buf));
            }
            // `px.ready` is false until the first proxy is made, so a realm without proxies pays one test for each object.
            if self.px.ready
                && self.heap.is_proxy(o)
                && let Some(Some((target, handler))) = h!(self, self.heap.proxy_parts(o))
            {
                work.push(Value::Object(target));
                work.push(Value::Object(handler));
            }
            self.sc_trace(o, &mut work);
            self.intl_trace(o, &mut work);
            self.weak_trace(o, &seen, &mut pending, &mut work);
            self.cls_trace(o, &mut work, &mut env_work);
            self.async_promise_trace(o, &mut work, &mut env_work, &mut act_seen);
            self.gen_trace(o, &mut work, &mut env_work);
            h!(self, self.heap.stored_edges(o, &mut work));
            if let Some(p) = h!(self, self.heap.stored_prototype(o)) {
                work.push(Value::Object(p));
            }
        }
        for (i, &seen) in env_seen.iter().enumerate() {
            if self.envs[i].live && !seen {
                self.free_env(i as u32);
            }
        }
        self.funcs.retain(|o, _| seen.contains(o));
        self.promise_prune(&seen);
        self.ext_prune(&seen);
        if let Some(h) = &host {
            h.prune(&|o| seen.contains(&o));
        }
        self.sc_prune(&seen);
        self.weak_prune(&seen);
        self.date_prune(&seen);
        self.intl_prune(&seen);
        self.cls_prune(&seen);
        self.async_prune(&act_seen);
        self.gen_prune(&seen);
        self.trim_tables();
        let mut roots = self.roots();
        roots.extend_from_slice(extra);
        roots.extend_from_slice(&host_vals);
        let r = h!(self, self.heap.collect(&roots));
        self.gc_allocs = self.heap.stats().total_object_allocs;
        self.gc_live = self.heap.used_bytes();
        Ok(r)
    }

    /// Has the heap grown to 1.5 times what the previous collect left (and past `GC_MIN_BYTES`), at a point where a collect can run?
    /// The driver asks at each safe point (between tasks, after each key or click), so garbage cannot pile up while one call of the host
    /// runs many events. QuickJS and SpiderMonkey use the same rule: a limit that is a factor of the live size after the last collect.
    pub fn gc_due(&self) -> bool {
        self.depth == 0 && self.stmt_frames == 0 && self.heap.used_bytes() > GC_MIN_BYTES.max(self.gc_live + self.gc_live / 2)
    }

    /// Give back the capacity of the side tables after a burst died, by the rule of `Slab::trim` (see `trim_to`).
    /// The heap objects made since the previous collect stand for the entries made since then. Free environments
    /// lose their bindings buffers, and free slots at the end of the environment table go.
    fn trim_tables(&mut self) {
        use std::mem::size_of;
        let since = (self.heap.stats().total_object_allocs - self.gc_allocs).min(usize::MAX as u64) as usize;
        if let Some(k) = trim_to(self.funcs.len(), self.funcs.capacity(), since, size_of::<(ObjId, Callee)>()) {
            self.funcs.shrink_to(k);
        }
        let pm = &mut self.promise.map;
        if let Some(k) = trim_to(pm.len(), pm.capacity(), since, size_of::<(ObjId, PromiseData)>()) {
            pm.shrink_to(k);
        }
        let dp = &mut self.promise.done_pairs;
        if let Some(k) = trim_to(dp.len(), dp.capacity(), since, size_of::<u64>()) {
            dp.shrink_to(k);
        }
        let q = &mut self.promise.queue;
        if let Some(k) = trim_to(q.len(), q.capacity(), since, size_of::<Job>()) {
            q.shrink_to(k);
        }
        let acts = &mut self.aw.acts;
        if let Some(k) = trim_to(acts.len(), acts.capacity(), since, size_of::<ActSlot>()) {
            acts.shrink_to(k);
        }
        for &i in &self.free_envs {
            self.envs[i as usize].bindings = Vec::new();
        }
        while self.envs.len() > 1 && self.envs.last().is_some_and(|e| !e.live) {
            self.envs.pop();
        }
        let n = self.envs.len() as u32;
        self.free_envs.retain(|&i| i < n);
        if let Some(k) = trim_to(self.envs.len(), self.envs.capacity(), since, size_of::<Env>()) {
            self.envs.shrink_to(k);
            self.free_envs.shrink_to(k);
        }
    }

    /// Text of a value for tools and tests: strings raw, numbers as JavaScript prints them.
    pub fn display(&self, v: Value) -> String {
        match v {
            Value::Undefined => "undefined".into(),
            Value::Null => "null".into(),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) if n == 0.0 && n.is_sign_negative() => "-0".into(),
            Value::Number(n) => number_to_string(n),
            Value::String(s) => self.heap.to_string_lossy(s).unwrap_or_default(),
            Value::Symbol(s) => self.sym_text(s),
            Value::Object(o) => if self.funcs.contains_key(&o) { "function".into() } else { "object".into() },
            Value::BigInt(_) | Value::BigHeap(_) => self.heap.bigint_text(v),
        }
    }

    /// `String(v[name])` as text, for tools and tests (an error's `name` and `message`). A failure gives "?".
    pub fn get_str(&mut self, v: Value, name: &str) -> String {
        let r = (|| {
            let key = self.heap.key_from_str(name).map_err(Error::Heap)?;
            let x = self.get_prop(v, key)?;
            let s = self.coerce_string(x)?;
            self.heap.to_string_lossy(s).map_err(Error::Heap)
        })();
        r.unwrap_or_else(|_| "?".into())
    }

    // ------------------------------------------------------------ names, keys and errors

    fn intern_name(&mut self, s: &str) -> u32 {
        if let Some(&i) = self.name_map.get(s) {
            return i;
        }
        let i = self.names.len() as u32;
        self.names.push(s.to_string());
        self.name_strs.push(None);
        self.name_gpos.push(NONE);
        self.name_map.insert(s.to_string(), i);
        i
    }

    fn name_at(&mut self, span: Span) -> u32 {
        let prog = self.prog.clone();
        self.intern_name(&prog.name(span))
    }

    fn name_str(&mut self, n: u32) -> Res<StrId> {
        if let Some(s) = self.name_strs[n as usize] {
            return Ok(s);
        }
        let s = h!(self, self.heap.intern_str(&self.names[n as usize]));
        self.name_strs[n as usize] = Some(s);
        Ok(s)
    }

    fn name_key(&mut self, n: u32) -> Res<PropertyKey> {
        let s = self.name_str(n)?;
        Ok(h!(self, self.heap.key_from_string(s)))
    }

    fn herr(&mut self, e: HeapError) -> Error {
        match e {
            HeapError::Limit(l) => Error::Limit(LimitKind::Heap(l)),
            HeapError::InvalidArrayLength => self.mk_err(RANGE_ERROR, "Invalid array length"),
            HeapError::SymbolConversion => self.mk_err(TYPE_ERROR, "Cannot convert a Symbol value to a string"),
            HeapError::SymbolToNumber => self.mk_err(TYPE_ERROR, "Cannot convert a Symbol value to a number"),
            HeapError::BigIntToNumber => self.mk_err(TYPE_ERROR, "Cannot convert a BigInt value to a number"),
            HeapError::NeedsExecution(w) => unsupported(w, Span::default()),
            HeapError::Proxy(_) => unsupported("Proxy: this operation has no trap dispatch yet (see eval_proxy.rs)", Span::default()),
            HeapError::NeedsBigInt => unsupported("a value that is not a BigInt written to a BigInt typed array by a generic method", Span::default()),
            e => Error::Heap(e),
        }
    }

    fn mk_err(&mut self, kind: usize, msg: &str) -> Error {
        let r: Result<Value, HeapError> = (|| {
            let o = self.heap.new_object(Some(self.error_protos[kind]))?;
            let m = self.heap.intern_str(msg)?;
            let d = PropertyDescriptor { value: Some(Value::String(m)), writable: Some(true), enumerable: Some(false), configurable: Some(true), ..Default::default() };
            self.heap.define_own_property(o, self.k_message, &d)?;
            Ok(Value::Object(o))
        })();
        let r = match r {
            Ok(Value::Object(o)) => match self.capture_stack(o) {
                Ok(()) => Ok(Value::Object(o)),
                Err(Error::Limit(l)) => return Error::Limit(l),
                Err(e) => return e,
            },
            r => r,
        };
        match r {
            Ok(v) => Error::Throw(v),
            Err(HeapError::Limit(l)) => Error::Limit(LimitKind::Heap(l)),
            Err(e) => Error::Heap(e),
        }
    }

    fn type_err(&mut self, msg: &str) -> Error {
        self.mk_err(TYPE_ERROR, msg)
    }

    fn key_text(&self, k: PropertyKey) -> String {
        match k {
            PropertyKey::Index(i) => i.to_string(),
            PropertyKey::String(s) => self.heap.to_string_lossy(s).unwrap_or_default(),
            PropertyKey::Symbol(s) => self.sym_text(s),
        }
    }

    /// Bound recursive tree and execution walks, including flat parser trees.
    fn descend<T, E: From<Error>>(&mut self, f: impl FnOnce(&mut Self) -> Result<T, E>) -> Result<T, E> {
        if self.frames >= self.limits.max_traversal_depth { return Err(Error::Limit(LimitKind::TraversalDepth).into()); }
        self.frames += 1;
        let result = f(self);
        self.frames -= 1;
        result
    }

    fn tick(&mut self) -> Res<()> {
        self.steps += 1;
        if self.steps > self.step_limit {
            return Err(Error::Limit(LimitKind::Steps));
        }
        Ok(())
    }

    /// Source text of an expression for an error message, as V8 prints a callee.
    fn src_text(&self, e: ExprId) -> String {
        let t = self.prog.text(self.prog.expr(e).span);
        if let Some(rest) = t.strip_prefix("super").filter(|r| r.starts_with(['.', '['])) {
            return format!("(intermediate value){rest}");
        }
        if t.len() <= 80 && !t.contains('\n') { t.to_string() } else { "expression".to_string() }
    }

    fn is_callable(&self, v: Value) -> bool {
        matches!(v, Value::Object(o) if self.funcs.contains_key(&o))
    }

    // ------------------------------------------------------------ the first walk (Unsupported + hoisting)

    fn walk_program(&mut self) -> Res<()> {
        let prog = self.prog.clone();
        let top = self.scopes.len() - 1;
        self.cur_strict = prog.strict;
        self.sc_cur = NONE;
        self.sc_fn = top;
        self.sc_class = 0;
        self.sc_begin(RKind::Root);
        for s in prog.list(prog.body) {
            self.w_stmt(s, top, usize::MAX, false)?;
        }
        Ok(())
    }

    fn w_func(&mut self, fid: FuncId, outer_args: usize) -> Res<()> {
        self.descend(|it| it.w_func_inner(fid, outer_args))
    }

    fn w_func_inner(&mut self, fid: FuncId, outer_args: usize) -> Res<()> {
        let prog = self.prog.clone();
        let f = *prog.func(fid);
        if f.is_async() || f.is_generator() {
            self.async_preflight(fid)?;
        }
        if let Some(n) = f.name {
            self.func_name[fid.index()] = self.name_at(n);
        }
        let saved = self.cur_strict;
        self.cur_strict = f.strict;
        let new_cur = if f.kind == FuncKind::Arrow { self.cls.cur } else { self.cls.elem_ctx.take() };
        let saved_cur = std::mem::replace(&mut self.cls.cur, new_cur);
        let me = fid.index();
        let args_scope = if f.kind == FuncKind::Arrow { outer_args } else { me };
        let is_expr = std::mem::take(&mut self.sc_fn_expr);
        let sc = self.sc_begin(RKind::Fn(fid, is_expr));
        let lo = self.sc_cur;
        let parent = std::mem::replace(&mut self.sc_fn, me);
        self.scopes[parent].nofr = true;
        for p in prog.list(f.params) {
            self.w_pat(p, me, args_scope)?;
        }
        match f.body {
            Body::Block(l) => {
                for s in prog.list(l) {
                    self.w_stmt(s, me, args_scope, false)?;
                }
            }
            Body::Expr(e) => self.w_expr(e, me, args_scope)?,
        }
        self.sc_end(sc);
        self.sc_fn = parent;
        self.fr_state(fid, lo, is_expr);
        self.cur_strict = saved;
        self.cls.cur = saved_cur;
        Ok(())
    }

    fn pat_names(&mut self, p: PatId, out: &mut Vec<u32>) {
        let prog = self.prog.clone();
        let pat = *prog.pat(p);
        match pat.kind {
            PatKind::Ident => {
                let n = self.name_at(pat.span);
                self.pat_name[p.index()] = n;
                out.push(n);
            }
            PatKind::Array(l) => {
                for q in prog.list(l) {
                    self.pat_names(q, out);
                }
            }
            PatKind::Object(l) => {
                for q in prog.list(l) {
                    let v = prog.pat_prop(q).value;
                    self.pat_names(v, out);
                }
            }
            PatKind::Assign { target, .. } | PatKind::Rest(target) => self.pat_names(target, out),
            PatKind::Member(_) | PatKind::Hole => {}
        }
    }

    fn w_pat(&mut self, p: PatId, fs: usize, ac: usize) -> Res<()> {
        self.descend(|it| it.w_pat_inner(p, fs, ac))
    }

    fn w_pat_inner(&mut self, p: PatId, fs: usize, ac: usize) -> Res<()> {
        let prog = self.prog.clone();
        let pat = *prog.pat(p);
        match pat.kind {
            PatKind::Ident => {
                let n = self.name_at(pat.span);
                self.pat_name[p.index()] = n;
                self.sc_site(self.pat_site(p.index()), n);
            }
            PatKind::Member(e) => self.w_expr(e, fs, ac)?,
            PatKind::Array(l) => {
                for q in prog.list(l) {
                    self.w_pat(q, fs, ac)?;
                }
            }
            PatKind::Object(l) => {
                for q in prog.list(l) {
                    let pp = *prog.pat_prop(q);
                    if let Key::Computed(e) = pp.key {
                        self.w_expr(e, fs, ac)?;
                    }
                    self.w_pat(pp.value, fs, ac)?;
                }
            }
            PatKind::Assign { target, default } => {
                self.w_pat(target, fs, ac)?;
                self.w_expr(default, fs, ac)?;
            }
            PatKind::Rest(t) => self.w_pat(t, fs, ac)?,
            PatKind::Hole => {}
        }
        Ok(())
    }

    fn w_stmt(&mut self, s: StmtId, fs: usize, ac: usize, nested: bool) -> Res<()> {
        self.descend(|it| it.w_stmt_inner(s, fs, ac, nested))
    }

    fn w_stmt_inner(&mut self, s: StmtId, fs: usize, ac: usize, nested: bool) -> Res<()> {
        let prog = self.prog.clone();
        let st = *prog.stmt(s);
        match st.kind {
            StmtKind::Expr(e) | StmtKind::Directive(e) | StmtKind::Throw(e) => self.w_expr(e, fs, ac)?,
            StmtKind::Var { kind, decls } => {
                for d in prog.list(decls) {
                    let d = *prog.decl(d);
                    if kind == VarKind::Var {
                        let mut names = Vec::new();
                        self.pat_names(d.target, &mut names);
                        self.scopes[fs].vars.extend(names);
                    }
                    self.w_pat(d.target, fs, ac)?;
                    if let Some(i) = d.init {
                        self.w_expr(i, fs, ac)?;
                    }
                }
            }
            StmtKind::Function(fid) => {
                self.w_func(fid, ac)?;
                // Annex B.3.3 is only for plain functions, not async functions or generators.
                if nested && !self.cur_strict && prog.func(fid).is_plain_ctor() {
                    let n = self.func_name[fid.index()];
                    self.scopes[fs].vars.push(n);
                }
            }
            StmtKind::Class(c) => self.w_class(c, fs, ac)?,
            StmtKind::Return(e) => {
                if let Some(e) = e {
                    self.w_expr(e, fs, ac)?;
                }
            }
            StmtKind::If { test, cons, alt } => {
                self.w_expr(test, fs, ac)?;
                self.w_stmt(cons, fs, ac, true)?;
                if let Some(a) = alt {
                    self.w_stmt(a, fs, ac, true)?;
                }
            }
            StmtKind::For { init, test, update, body } => {
                self.scopes[fs].eager = true;
                let sc = self.sc_lexical_var(if let ForInit::Var(v) = init { Some(v) } else { None });
                match init {
                    ForInit::None => {}
                    ForInit::Var(v) => self.w_stmt(v, fs, ac, true)?,
                    ForInit::Expr(e) => self.w_expr(e, fs, ac)?,
                }
                for e in [test, update].into_iter().flatten() {
                    self.w_expr(e, fs, ac)?;
                }
                self.w_stmt(body, fs, ac, true)?;
                self.sc_end(sc);
            }
            StmtKind::ForIn { left, right, body } | StmtKind::ForOf { left, right, body, is_await: false } => {
                // The right side runs before the iteration environment exists.
                self.scopes[fs].eager = true;
                self.w_expr(right, fs, ac)?;
                let sc = self.sc_lexical_var(if let ForLeft::Var(v) = left { Some(v) } else { None });
                match left {
                    ForLeft::Var(v) => self.w_stmt(v, fs, ac, true)?,
                    ForLeft::Pat(p) => self.w_pat(p, fs, ac)?,
                }
                self.w_stmt(body, fs, ac, true)?;
                self.sc_end(sc);
            }
            StmtKind::ForOf { .. } => return Err(unsupported("for await", st.span)),
            StmtKind::While { test, body } | StmtKind::DoWhile { body, test } => {
                self.scopes[fs].eager = true;
                self.w_expr(test, fs, ac)?;
                self.w_stmt(body, fs, ac, true)?;
            }
            StmtKind::Break(l) | StmtKind::Continue(l) => {
                if let Some(l) = l {
                    self.stmt_label[s.index()] = self.name_at(l);
                }
            }
            StmtKind::Try { block, handler, finalizer } => {
                self.w_stmt(block, fs, ac, true)?;
                if let Some(c) = handler {
                    let c = *prog.catch(c);
                    let sc = self.sc_begin(RKind::Catch(c.param, s));
                    if let Some(p) = c.param {
                        self.w_pat(p, fs, ac)?;
                    }
                    self.w_stmt(c.body, fs, ac, true)?;
                    self.sc_end(sc);
                }
                if let Some(f) = finalizer {
                    self.w_stmt(f, fs, ac, true)?;
                }
            }
            StmtKind::Switch { disc, cases } => {
                self.w_expr(disc, fs, ac)?;
                let lex = prog.list(cases).any(|c| self.has_lexical(prog.case(c).body));
                let sc = if lex { self.sc_begin(RKind::Switch(s)) } else { self.sc_cur };
                for c in prog.list(cases) {
                    let c = *prog.case(c);
                    if let Some(t) = c.test {
                        self.w_expr(t, fs, ac)?;
                    }
                    for s in prog.list(c.body) {
                        self.w_stmt(s, fs, ac, true)?;
                    }
                }
                self.sc_end(sc);
            }
            StmtKind::Labeled { label, body } => {
                self.stmt_label[s.index()] = self.name_at(label);
                self.w_stmt(body, fs, ac, true)?;
            }
            StmtKind::Block(l) => {
                let sc = if self.has_lexical(l) { self.sc_begin(RKind::Block(s)) } else { self.sc_cur };
                for s in prog.list(l) {
                    self.w_stmt(s, fs, ac, true)?;
                }
                self.sc_end(sc);
            }
            StmtKind::Empty | StmtKind::Debugger => {}
            StmtKind::With { .. } => return Err(unsupported("with statement", st.span)),
        }
        Ok(())
    }

    fn w_expr(&mut self, e: ExprId, fs: usize, ac: usize) -> Res<()> {
        self.descend(|it| it.w_expr_inner(e, fs, ac))
    }

    fn w_expr_inner(&mut self, e: ExprId, fs: usize, ac: usize) -> Res<()> {
        let prog = self.prog.clone();
        let ex = *prog.expr(e);
        match ex.kind {
            ExprKind::Ident => {
                let n = self.name_at(ex.span);
                self.expr_name[e.index()] = n;
                self.sc_site(e.index() as u32, n);
                if n == self.n_arguments && ac != usize::MAX {
                    self.scopes[ac].uses_args = true;
                }
            }
            ExprKind::This => {
                self.scopes[fs].uses_this = true;
                self.cls_w_this(e, ac)
            }
            ExprKind::NewTarget | ExprKind::Super => {
                self.scopes[fs].nofr = true;
                match ex.kind {
                    ExprKind::Super => self.cls_w_super(ex.span)?,
                    _ => self.cls_w_new_target(ex.span, ac)?,
                }
            }
            ExprKind::Unary { op: UnaryOp::Delete, arg } => {
                if matches!(prog.expr(arg).kind, ExprKind::Ident | ExprKind::Paren(_)) {
                    self.scopes[fs].nofr = true;
                }
                self.w_expr(arg, fs, ac)?
            }
            ExprKind::Null | ExprKind::Bool(_) | ExprKind::Number(_) | ExprKind::String | ExprKind::Hole | ExprKind::BigInt => {}
            ExprKind::RegExp => self.ext_walk_regexp(e, ex.span)?,
            ExprKind::Template(t) => {
                let t = *prog.template(t);
                for x in prog.template_exprs(&t) {
                    self.w_expr(x, fs, ac)?;
                }
            }
            ExprKind::Tagged { tag, quasi } => {
                self.scopes[fs].eager = true;
                self.w_expr(tag, fs, ac)?;
                let t = *prog.template(quasi);
                for x in prog.template_exprs(&t) {
                    self.w_expr(x, fs, ac)?;
                }
            }
            ExprKind::Array(l) | ExprKind::Seq(l) => {
                for x in prog.list(l) {
                    self.w_expr(x, fs, ac)?;
                }
            }
            ExprKind::Object(l) => {
                for p in prog.list(l) {
                    let pr = *prog.prop(p);
                    if let Key::Computed(k) = pr.key {
                        self.w_expr(k, fs, ac)?;
                    }
                    self.w_expr(pr.value, fs, ac)?;
                }
            }
            ExprKind::Function(f) => {
                self.sc_fn_expr = true;
                self.w_func(f, ac)?
            }
            ExprKind::Class(c) => self.w_class(c, fs, ac)?,
            ExprKind::Unary { arg, .. } | ExprKind::Update { arg, .. } | ExprKind::Spread(arg) | ExprKind::Paren(arg) | ExprKind::Chain(arg) => self.w_expr(arg, fs, ac)?,
            ExprKind::Binary { left, right, .. } | ExprKind::Logical { left, right, .. } => {
                self.w_expr(left, fs, ac)?;
                self.w_expr(right, fs, ac)?;
            }
            ExprKind::Assign { target, value, .. } => {
                self.w_pat(target, fs, ac)?;
                self.w_expr(value, fs, ac)?;
            }
            ExprKind::Cond { test, cons, alt } => {
                for x in [test, cons, alt] {
                    self.w_expr(x, fs, ac)?;
                }
            }
            ExprKind::Call { callee, args, .. } | ExprKind::New { callee, args } => {
                self.scopes[fs].eager = true;
                self.w_expr(callee, fs, ac)?;
                for x in prog.list(args) {
                    self.w_expr(x, fs, ac)?;
                }
            }
            ExprKind::Member { obj, .. } => self.w_expr(obj, fs, ac)?,
            ExprKind::Index { obj, index, .. } => {
                self.w_expr(obj, fs, ac)?;
                self.w_expr(index, fs, ac)?;
            }
            ExprKind::PrivateMember { obj, name, .. } => {
                self.cls_w_private(e, name);
                self.w_expr(obj, fs, ac)?;
            }
            ExprKind::PrivateIn { name, right } => {
                self.cls_w_private(e, name);
                self.w_expr(right, fs, ac)?;
            }
            ExprKind::Await(a) => {
                if !self.aw_expr(e) {
                    return Err(unsupported("await in this position", ex.span));
                }
                self.w_expr(a, fs, ac)?;
            }
            ExprKind::Yield { arg, .. } => {
                if !self.aw_expr(e) {
                    return Err(unsupported("yield in this position", ex.span));
                }
                if let Some(a) = arg {
                    self.w_expr(a, fs, ac)?;
                }
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------ builtins

    fn def(&mut self, o: ObjId, key: PropertyKey, v: Value, w: bool, e: bool, c: bool) -> Res<()> {
        let d = PropertyDescriptor { value: Some(v), writable: Some(w), enumerable: Some(e), configurable: Some(c), ..Default::default() };
        hxw!(self, self.heap.define_own_property(o, key, &d));
        Ok(())
    }

    fn native(&mut self, n: Nat, name: &str, len: u32) -> Res<ObjId> {
        let o = self.new_func_object(Callee::Native(n))?;
        let s = h!(self, self.heap.intern_str(name));
        h!(self, self.heap.init_function(o, (self.k_length, len as f64), (self.k_name, s)));
        Ok(o)
    }

    fn method(&mut self, on: ObjId, name: &str, n: Nat, len: u32) -> Res<ObjId> {
        let f = self.native(n, name, len)?;
        let k = h!(self, self.heap.key_from_str(name));
        self.def(on, k, Value::Object(f), true, false, true)?;
        Ok(f)
    }

    fn install_builtins(&mut self) -> Res<()> {
        let g = self.global;
        for (n, v) in [("undefined", Value::Undefined), ("NaN", Value::Number(f64::NAN)), ("Infinity", Value::Number(f64::INFINITY))] {
            let k = h!(self, self.heap.key_from_str(n));
            self.def(g, k, v, false, false, false)?;
        }
        let k = h!(self, self.heap.key_from_str("globalThis"));
        self.def(g, k, Value::Object(g), true, false, true)?;
        let (op, fp, ap) = (self.object_proto, self.function_proto, self.array_proto);
        // constructors: name, native, length, prototype
        let ctors: [(&str, Nat, u32, ObjId); 3] = [("Object", Nat::ObjectCtor, 1, op), ("Function", Nat::FnCtor, 1, fp), ("Array", Nat::ArrayCtor, 1, ap)];
        let mut made = Vec::new();
        for (name, n, len, proto) in ctors {
            let c = self.native(n, name, len)?;
            self.def(c, self.k_prototype, Value::Object(proto), false, false, false)?;
            self.def(proto, self.k_constructor, Value::Object(c), true, false, true)?;
            let k = h!(self, self.heap.key_from_str(name));
            self.def(g, k, Value::Object(c), true, false, true)?;
            made.push(c);
        }
        let (oc, ac) = (made[0], made[2]);
        self.method(oc, "keys", Nat::ObjectKeys, 1)?;
        self.method(oc, "defineProperty", Nat::ObjectDefineProperty, 3)?;
        self.method(oc, "getOwnPropertyDescriptor", Nat::ObjectGetOwnPropertyDescriptor, 2)?;
        self.method(oc, "create", Nat::ObjectCreate, 2)?;
        self.method(oc, "getPrototypeOf", Nat::ObjectGetPrototypeOf, 1)?;
        self.method(op, "hasOwnProperty", Nat::ObjHasOwn, 1)?;
        self.method(op, "toString", Nat::ObjToString, 0)?;
        self.method(op, "valueOf", Nat::ObjValueOf, 0)?;
        // `Function.prototype.length` is 0 and `.name` is "" (writable false, enumerable false, configurable true).
        let empty = h!(self, self.heap.intern_str(""));
        self.def(fp, self.k_length, Value::Number(0.0), false, false, true)?;
        self.def(fp, self.k_name, Value::String(empty), false, false, true)?;
        self.method(fp, "call", Nat::FnCall, 1)?;
        self.method(fp, "apply", Nat::FnApply, 2)?;
        self.method(fp, "bind", Nat::FnBind, 1)?;
        self.method(ac, "isArray", Nat::ArrayIsArray, 1)?;
        self.method(ap, "push", Nat::ArrPush, 1)?;
        self.method(ap, "pop", Nat::ArrPop, 0)?;
        self.method(ap, "forEach", Nat::ArrForEach, 1)?;
        self.method(ap, "join", Nat::ArrJoin, 1)?;
        self.method(ap, "toString", Nat::ArrJoin, 0)?;
        let mut error_ctor: Option<ObjId> = None;
        for (i, &error_name) in ERR_NAMES.iter().enumerate() {
            let proto = self.error_protos[i];
            let c = self.native(Nat::ErrorCtor(i as u8), error_name, 1)?;
            self.def(c, self.k_prototype, Value::Object(proto), false, false, false)?;
            self.def(proto, self.k_constructor, Value::Object(c), true, false, true)?;
            if let Some(base) = error_ctor {
                h!(self, self.heap.set_prototype_of(c, Some(base)));
            } else {
                error_ctor = Some(c);
            }
            let nm = h!(self, self.heap.intern_str(error_name));
            self.def(proto, self.k_name, Value::String(nm), true, false, true)?;
            let empty = h!(self, self.heap.intern_str(""));
            self.def(proto, self.k_message, Value::String(empty), true, false, true)?;
            if i == 0 {
                self.method(proto, "toString", Nat::ErrToString, 0)?;
            } else {
                let base = self.error_protos[0];
                h!(self, self.heap.set_prototype_of(proto, Some(base)));
            }
            let k = h!(self, self.heap.key_from_str(error_name));
            self.def(g, k, Value::Object(c), true, false, true)?;
        }
        for (name, n) in [
            ("String", Nat::StringFn),
            ("Number", Nat::NumberFn),
            ("Boolean", Nat::BooleanFn),
            ("isNaN", Nat::IsNaN),
            ("parseInt", Nat::ParseInt),
            ("encodeURIComponent", Nat::EncodeUriComponent),
            ("parseFloat", Nat::ParseFloat),
            ("isFinite", Nat::IsFinite),
            ("encodeURI", Nat::EncodeUri),
            ("decodeURI", Nat::DecodeUri),
            ("decodeURIComponent", Nat::DecodeUriComponent),
            ("escape", Nat::Escape),
            ("unescape", Nat::Unescape),
        ] {
            let f = self.native(n, name, if name == "parseInt" { 2 } else { 1 })?;
            let k = h!(self, self.heap.key_from_str(name));
            self.def(g, k, Value::Object(f), true, false, true)?;
        }
        Ok(())
    }

    /// Allocate a function object and register its code. Checks the function-slot limit.
    fn new_func_object(&mut self, c: Callee) -> Res<ObjId> {
        if self.funcs.len() >= self.limits.max_functions {
            return Err(Error::Limit(LimitKind::Functions));
        }
        let o = h!(self, self.heap.new_object(Some(self.function_proto)));
        self.funcs.insert(o, c);
        Ok(o)
    }

    // ------------------------------------------------------------ environments

    fn new_env(&mut self, parent: u32, var_scope: bool) -> Res<u32> {
        if self.live_envs >= self.limits.max_envs {
            return Err(Error::Limit(LimitKind::Envs));
        }
        self.live_envs += 1;
        if let Some(i) = self.free_envs.pop() {
            let e = &mut self.envs[i as usize];
            e.parent = parent;
            e.live = true;
            e.captured = false;
            e.var_scope = var_scope;
            return Ok(i);
        }
        self.envs.push(Env { parent, bindings: Vec::new(), index: None, this: None, captured: false, live: true, var_scope });
        Ok(self.envs.len() as u32 - 1)
    }

    fn free_env(&mut self, i: u32) {
        let e = &mut self.envs[i as usize];
        if !e.live || i <= FRAME_ENV {
            return;
        }
        e.live = false;
        e.bindings.clear();
        e.index = None;
        e.this = None;
        self.live_envs -= 1;
        self.free_envs.push(i);
    }

    /// Free an environment on scope exit when no closure captured it.
    fn release(&mut self, i: u32) {
        if !self.envs[i as usize].captured {
            self.free_env(i);
        }
    }

    /// A closure captures the current environment and all its parents.
    fn capture(&mut self) {
        let mut e = self.env;
        while e != NONE && !self.envs[e as usize].captured {
            self.envs[e as usize].captured = true;
            e = self.envs[e as usize].parent;
        }
    }

    fn find_in(&self, env: u32, name: u32) -> Option<usize> {
        let e = &self.envs[env as usize];
        if let Some(ix) = &e.index {
            return ix.get(&name).map(|&i| i as usize);
        }
        e.bindings.iter().position(|b| b.name == name)
    }

    fn env_declare(&mut self, env: u32, name: u32, flags: u8, value: Value) {
        if let Some(i) = self.find_in(env, name) {
            let b = &mut self.envs[env as usize].bindings[i];
            b.flags = flags;
            b.value = value;
            return;
        }
        let e = &mut self.envs[env as usize];
        e.bindings.push(Binding { name, flags, value });
        let n = e.bindings.len();
        if let Some(ix) = &mut e.index {
            ix.insert(name, n as u32 - 1);
        } else if n > 12 {
            e.index = Some(Box::new(e.bindings.iter().enumerate().map(|(i, b)| (b.name, i as u32)).collect()));
        }
    }

    /// `var` hoisting: a missing name becomes `undefined`. A function's own-name binding is replaced.
    fn declare_var(&mut self, env: u32, name: u32) {
        match self.find_in(env, name) {
            None => self.env_declare(env, name, INIT | MUT, Value::Undefined),
            Some(i) => {
                let b = &mut self.envs[env as usize].bindings[i];
                if b.flags & SELF != 0 {
                    b.flags = INIT | MUT;
                    b.value = Value::Undefined;
                }
            }
        }
    }

    fn lookup(&self, name: u32) -> Option<(u32, usize)> {
        let mut e = self.env;
        while e != NONE {
            if let Some(i) = self.find_in(e, name) {
                return Some((e, i));
            }
            e = self.envs[e as usize].parent;
        }
        None
    }

    fn lookup_this(&self) -> Value {
        let mut e = self.env;
        while e != NONE {
            if let Some(t) = self.envs[e as usize].this {
                return t;
            }
            e = self.envs[e as usize].parent;
        }
        Value::Undefined
    }

    fn declare_pat_lexical(&mut self, p: PatId, flags: u8) {
        // A plain name: the walk before the run cached its id, so no hashing and no list.
        let pat = *self.prog.pat(p);
        if let PatKind::Ident = pat.kind {
            let n = self.pat_name[p.index()];
            debug_assert_eq!(n, self.name_at(pat.span));
            self.env_declare(self.env, n, flags, Value::Undefined);
            return;
        }
        let mut names = Vec::new();
        self.pat_names(p, &mut names);
        for n in names {
            self.env_declare(self.env, n, flags, Value::Undefined);
        }
    }

    fn has_lexical(&self, l: List<Stmt>) -> bool {
        self.prog.list(l).any(|s| match self.prog.stmt(s).kind {
            StmtKind::Var { kind, .. } => kind != VarKind::Var,
            StmtKind::Function(_) | StmtKind::Class(_) => true,
            _ => false,
        })
    }

    /// Hoist the declarations of a statement list into the current environment: `let`/`const` as
    /// uninitialized (TDZ), function declarations as initialized closures. `func_top`: the list is a
    /// function or script body, where functions are var-scoped.
    fn declare_scope(&mut self, l: List<Stmt>, func_top: bool) -> Res<()> {
        let prog = self.prog.clone();
        for s in prog.list(l) {
            match prog.stmt(s).kind {
                StmtKind::Var { kind, decls } if kind != VarKind::Var => {
                    for d in prog.list(decls) {
                        let target = prog.decl(d).target;
                        self.declare_pat_lexical(target, if kind == VarKind::Let { MUT } else { 0 });
                    }
                }
                StmtKind::Function(fid) => {
                    let f = self.make_closure(fid, None, false)?;
                    let n = self.func_name[fid.index()];
                    if func_top && self.env == 0 {
                        let key = self.name_key(n)?;
                        let d = PropertyDescriptor { value: Some(f), writable: Some(true), enumerable: Some(true), configurable: Some(false), ..Default::default() };
                        hxw!(self, self.heap.define_own_property(self.global, key, &d));
                    } else {
                        self.env_declare(self.env, n, INIT | MUT, f);
                    }
                }
                StmtKind::Class(c) => self.declare_class(c),
                _ => {}
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------ functions

    fn make_closure(&mut self, fid: FuncId, name: Option<StrId>, is_expr: bool) -> Res<Value> {
        let prog = self.prog.clone();
        let f = *prog.func(fid);
        self.capture();
        let self_binding = is_expr && f.name.is_some() && f.kind == FuncKind::Normal;
        let o = self.new_func_object(Callee::Script { script: self.cur as u32, fid, env: self.env, self_binding })?;
        let nstr = match (f.name.is_some(), name) {
            (true, _) => self.name_str(self.func_name[fid.index()])?,
            (false, Some(s)) => s,
            (false, None) => h!(self, self.heap.intern_str("")),
        };
        let mut len = 0;
        for p in prog.list(f.params) {
            if matches!(prog.pat(p).kind, PatKind::Assign { .. } | PatKind::Rest(_)) {
                break;
            }
            len += 1;
        }
        // `length`, `name`, and the `prototype` that a generator or a plain function gets: one allocation of the size.
        if f.is_generator() || (f.kind == FuncKind::Normal && !f.is_async()) {
            h!(self, self.heap.reserve_props(o, 3));
        }
        h!(self, self.heap.init_function(o, (self.k_length, len as f64), (self.k_name, nstr)));
        if f.is_generator() {
            self.gen_function(o)?;
        } else if f.kind == FuncKind::Normal && !f.is_async() {
            // Most functions never use their prototype (98.6% on the staging login), so the object waits.
            h!(self, self.heap.add_lazy_prototype(o));
        }
        Ok(Value::Object(o))
    }

    fn call_fn(&mut self, f: Value, this: Value, args: &[Value]) -> Res<Value> {
        self.descend(|it| it.call_fn_inner(f, this, args))
    }

    #[cold]
    #[inline(never)]
    fn call_fn_low(&mut self, f: Value, this: Value, args: &[Value]) -> Res<Value> {
        self.grow_stack(|it| it.call_fn_inner(f, this, args))
    }

    #[cold]
    #[inline(never)]
    fn construct_low(&mut self, f: Value, args: &[Value], callee: ExprId) -> Res<Value> {
        self.grow_stack(|it| it.construct_inner(f, args, callee))
    }

    fn call_fn_inner(&mut self, f: Value, this: Value, args: &[Value]) -> Res<Value> {
        if self.depth >= self.limits.max_call_depth {
            return Err(Error::Limit(LimitKind::CallDepth));
        }
        self.tick()?;
        let Value::Object(o) = f else { return Err(self.type_err("value is not a function")) };
        enum D {
            S(u32, FuncId, u32, bool),
            N(Nat),
            B(ObjId, Value, Vec<Value>),
            P(PFn),
            H(u32),
            X,
            R,
        }
        let d = match self.funcs.get(&o) {
            Some(&Callee::Script { script, fid, env, self_binding }) => D::S(script, fid, env, self_binding),
            Some(Callee::Proxy { .. }) => D::X,
            Some(Callee::Revoke(_)) => D::R,
            Some(&Callee::Host { id, .. }) => D::H(id),
            Some(&Callee::Native(n)) => D::N(n),
            Some(Callee::Bound { target, this, args }) => D::B(*target, *this, args.clone()),
            Some(&Callee::Promise(pf)) => D::P(pf),
            Some(Callee::Class) => return Err(self.class_call_error(o)),
            None => return Err(self.type_err("value is not a function")),
        };
        // A native callee needs no stack check or frame reset: it reaches script code only through `call_fn`, which does both.
        if !matches!(d, D::N(_)) && self.low_stack() {
            return self.call_fn_low(f, this, args);
        }
        self.depth += 1;
        let r = match d {
            D::N(n) => self.call_native(n, this, args, false),
            d => self.in_frames(|it| match d {
                D::S(script, fid, env, sb) => it.call_script((script, fid), o, env, sb, this, args),
                D::P(pf) => it.call_promise(pf, this, args, false),
                D::H(id) => it.call_host(id, this, args, false, None),
                D::X => it.px_call(o, this, args),
                D::R => it.call_revoke(o),
                D::B(t, bthis, mut bargs) => {
                    bargs.extend_from_slice(args);
                    it.call_fn(Value::Object(t), bthis, &bargs)
                }
                D::N(_) => unreachable!(),
            }),
        };
        self.depth -= 1;
        r
    }

    /// Run the body of one call, or the setup walk or top level of a script (`Interp::new`, `run`, `run_script`). Two things happen here and nowhere else:
    /// - `frames` counts native frames from the start of THIS call, so `max_traversal_depth` bounds the recursion of one
    ///   activation (expressions, patterns, `exec`). How many calls are nested is `max_call_depth`.
    /// - The stack is checked: at least `STACK_RED` bytes are left below this point, enough for one activation up to
    ///   `max_traversal_depth` frames and for the native code it calls. If not, the call continues on a new stack segment.
    ///   The check is a compare of the stack pointer with `stack_floor` (see `grow_stack`).
    #[inline(always)]
    fn in_call<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        if self.low_stack() { self.grow_stack(|it| it.in_frames(f)) } else { self.in_frames(f) }
    }

    /// True when less than `STACK_RED` bytes remain (or the end of this stack is not known yet). The hot call paths test it
    /// first and, if true, run themselves again inside `grow_stack`, so the call body exists once.
    #[inline(always)]
    fn low_stack(&self) -> bool {
        let here = 0u8;
        (&here as *const u8 as usize) < self.stack_floor
    }

    /// Count the native frames of `f` from zero and restore the count of the caller afterwards.
    #[inline(always)]
    fn in_frames<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        let saved = std::mem::take(&mut self.frames);
        let r = f(self);
        self.frames = saved;
        r
    }

    /// The cold side of `in_call`: the first call on a thread, or a call near the end of the stack. The first call reads
    /// the end of the thread stack and sets the floor. A call near the end runs on a new segment (`stacker::grow`: mapped
    /// memory, committed only when touched, unmapped on return) with the floor of the segment, and the old floor comes back.
    #[cold]
    #[inline(never)]
    fn grow_stack<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        let here = 0u8;
        let sp = &here as *const u8 as usize;
        match stacker::remaining_stack() {
            None => {
                self.stack_floor = 0;
                f(self)
            }
            Some(left) if left > STACK_RED => {
                self.stack_floor = sp - left + STACK_RED;
                f(self)
            }
            Some(_) => stacker::grow(STACK_SEG, || {
                let here = 0u8;
                let left = stacker::remaining_stack().unwrap_or(STACK_SEG);
                let floor = &here as *const u8 as usize - left + STACK_RED;
                let saved = std::mem::replace(&mut self.stack_floor, floor);
                let r = f(self);
                self.stack_floor = saved;
                r
            }),
        }
    }

    fn construct(&mut self, f: Value, args: &[Value], callee: ExprId) -> Res<Value> {
        self.descend(|it| it.construct_inner(f, args, callee))
    }

    fn construct_inner(&mut self, f: Value, args: &[Value], callee: ExprId) -> Res<Value> {
        if self.low_stack() {
            return self.construct_low(f, args, callee);
        }
        let not_ctor = |s: &mut Self| {
            let d = s.src_text(callee);
            Err(s.type_err(&format!("{d} is not a constructor")))
        };
        let Value::Object(o) = f else { return not_ctor(self) };
        if self.depth >= self.limits.max_call_depth {
            return Err(Error::Limit(LimitKind::CallDepth));
        }
        self.tick()?;
        enum D {
            S(u32, FuncId, u32, bool),
            N(Nat),
            B(ObjId, Vec<Value>),
            P(PFn),
            H(u32),
            C,
            X,
        }
        let d = match self.funcs.get(&o) {
            Some(Callee::Class) => D::C,
            Some(&Callee::Proxy { ctor: true }) => D::X,
            Some(&Callee::Host { id, ctor: true, .. }) => D::H(id),
            Some(&Callee::Script { script, fid, env, self_binding }) if self.func_of(script, fid).is_plain_ctor() => D::S(script, fid, env, self_binding),
            Some(&Callee::Native(n)) if matches!(n, Nat::ObjectCtor | Nat::FnCtor | Nat::ArrayCtor | Nat::ErrorCtor(_) | Nat::StringFn | Nat::NumberFn | Nat::BooleanFn | Nat::Ext(Bi::RegExpCtor) | Nat::Sc(ScFn::IterCtor) | Nat::Px(PxFn::Ctor)) || matches!(n, Nat::Bin(b) if b.ctor_name().is_some() || b == BinFn::TaCtor) || matches!(n, Nat::Sc(f) if f.is_ctor()) || n == Nat::Date(DateFn::Ctor) || matches!(n, Nat::Intl(f) if f.is_ctor()) => D::N(n),
            Some(Callee::Bound { target, args, .. }) => D::B(*target, args.clone()),
            Some(&Callee::Promise(pf @ (PFn::Ctor | PFn::AggErrCtor))) => D::P(pf),
            _ => return not_ctor(self),
        };
        self.depth += 1;
        let r = self.in_frames(|it| match d {
            D::S(script, fid, env, sb) => (|| {
                let pv = it.get_prop(f, it.k_prototype)?;
                let proto = if let Value::Object(p) = pv { p } else { it.object_proto };
                let obj = h!(it, it.heap.new_object(Some(proto)));
                it.cls_set_nt(script, fid, o);
                it.stk.new = true;
                let r = it.call_script((script, fid), o, env, sb, Value::Object(obj), args);
                it.cls.pending_nt = None;
                let r = r?;
                Ok(if matches!(r, Value::Object(_)) { r } else { Value::Object(obj) })
            })(),
            D::C => it.construct_class_body(o, args, o),
            D::X => it.px_construct(o, args, o, callee),
            D::N(n) => it.call_native(n, Value::Undefined, args, true),
            D::P(pf) => it.call_promise(pf, Value::Undefined, args, true),
            // A plain `new F` has no different `new.target`: the `prototype` of a host constructor is read-only, so it is the default.
            D::H(id) => it.call_host(id, Value::Undefined, args, true, None),
            D::B(t, mut bargs) => {
                bargs.extend_from_slice(args);
                it.construct(Value::Object(t), &bargs, callee)
            }
        });
        self.depth -= 1;
        r
    }

    fn call_script(&mut self, (script, fid): (u32, FuncId), fobj: ObjId, cenv: u32, self_binding: bool, this: Value, args: &[Value]) -> Res<Value> {
        let new = std::mem::take(&mut self.stk.new);
        match self.func_of(script, fid).mode {
            FuncMode::Plain => {}
            FuncMode::Async => return self.call_async((script, fid), fobj, cenv, self_binding, this, args),
            FuncMode::Generator => return self.call_generator((script, fid), fobj, cenv, self_binding, this, args),
        }
        let (saved_env, saved_strict) = (self.env, self.strict);
        // The callee runs on its own script code. The caller's code comes back on every exit.
        let saved_script = self.cur;
        self.select(script as usize);
        let fs = &self.scopes[fid.index()];
        let mut first = false;
        match fs.fr {
            FR_ENV => {}
            FR_ON => return self.call_frame(fid, cenv, new, args, (saved_env, saved_strict, saved_script)),
            FR_NEW => {
                self.scopes[fid.index()].fr = FR_FIRST;
                first = true;
            }
            _ => match self.fr_gate(fid) {
                Gate::Frame => return self.call_frame(fid, cenv, new, args, (saved_env, saved_strict, saved_script)),
                Gate::Env => {}
            },
        }
        let env = match self.new_env(cenv, true) {
            Ok(e) => e,
            Err(e) => {
                self.select(saved_script);
                return Err(e);
            }
        };
        self.env = env;
        let r = self.in_frame(script, Where::Func(fid), new, |it| it.call_body(fobj, fid, env, self_binding, this, args));
        if first {
            self.scopes[fid.index()].fr = FR_WAIT;
        }
        self.select(saved_script);
        self.env = saved_env;
        self.strict = saved_strict;
        self.release(env);
        r
    }

    fn call_body(&mut self, fobj: ObjId, fid: FuncId, env: u32, self_binding: bool, this: Value, args: &[Value]) -> Res<Value> {
        self.bind_call(fobj, fid, env, self_binding, this, args)?;
        self.run_body(fid, false)
    }

    /// Bind `this`, the parameters, `arguments` and the `var`s of a call into its environment.
    fn bind_call(&mut self, fobj: ObjId, fid: FuncId, env: u32, self_binding: bool, this: Value, args: &[Value]) -> Res<()> {
        let prog = self.prog.clone();
        let f = *prog.func(fid);
        self.strict = f.strict;
        if self.cls.fflags[fid.index()] != 0 {
            self.cls_frame(env, fid, fobj);
        }
        if f.kind != FuncKind::Arrow {
            let t = match this {
                Value::Object(_) => this,
                _ if f.strict => this,
                _ if is_nullish(this) => Value::Object(self.global),
                _ => self.to_object(this)?,
            };
            self.envs[env as usize].this = Some(t);
        }
        if self_binding {
            let n = self.func_name[fid.index()];
            self.env_declare(env, n, INIT | SELF, Value::Object(fobj));
        }
        if f.simple_params {
            for (i, p) in prog.list(f.params).enumerate() {
                let n = self.pat_name[p.index()];
                self.env_declare(env, n, INIT | MUT, args.get(i).copied().unwrap_or(Value::Undefined));
            }
        } else {
            for (i, p) in prog.list(f.params).enumerate() {
                if let PatKind::Rest(t) = prog.pat(p).kind {
                    let rest = args.get(i..).unwrap_or(&[]).to_vec();
                    let arr = self.new_array_from(&rest)?;
                    self.bind_pattern(t, arr, Bind::Declare)?;
                } else {
                    self.bind_pattern(p, args.get(i).copied().unwrap_or(Value::Undefined), Bind::Declare)?;
                }
            }
        }
        if self.scopes[fid.index()].uses_args && f.kind != FuncKind::Arrow {
            let o = h!(self, self.heap.new_object(Some(self.object_proto)));
            for (i, &a) in args.iter().enumerate() {
                self.def(o, PropertyKey::Index(i as u32), a, true, true, true)?;
            }
            self.def(o, self.k_length, Value::Number(args.len() as f64), true, false, true)?;
            self.def(o, self.sc.k_iterator, Value::Object(self.sc.f_array_values), true, false, true)?;
            self.env_declare(env, self.n_arguments, INIT | MUT, Value::Object(o));
        }
        for i in 0..self.scopes[fid.index()].vars.len() {
            let n = self.scopes[fid.index()].vars[i];
            self.declare_var(env, n);
        }
        Ok(())
    }

    /// Run the body of a call. `frame`: the variables are in frame slots, so there is nothing to declare.
    fn run_body(&mut self, fid: FuncId, frame: bool) -> Res<Value> {
        let f = *self.prog.func(fid);
        match f.body {
            Body::Expr(e) => self.eval_expr(e),
            Body::Block(l) => {
                if !frame {
                    self.declare_scope(l, true)?;
                }
                match self.exec_list(l) {
                    Ok(()) => Ok(Value::Undefined),
                    Err(Ctl::Return) => Ok(std::mem::replace(&mut self.ret, Value::Undefined)),
                    Err(Ctl::Err(e)) => Err(e),
                    Err(_) => Err(unsupported("stray break or continue", f.span)),
                }
            }
        }
    }

    /// GetTemplateObject: the frozen strings array of a tagged template site, made on the first call there. Its frozen
    /// `raw` array holds the raw pieces. A cooked piece with a bad escape is `undefined`.
    fn template_object(&mut self, e: ExprId, t: Id<Template>) -> Res<Value> {
        let site = (self.cur as u32, e.index() as u32);
        if let Some(&o) = self.tpl_objs.get(&site) {
            return Ok(Value::Object(o));
        }
        let prog = self.prog.clone();
        let t = *prog.template(t);
        let strings = h!(self, self.heap.new_array(Some(self.array_proto)));
        let raw = h!(self, self.heap.new_array(Some(self.array_proto)));
        let mut u = Vec::new();
        for (i, q) in prog.template_quasis(&t).enumerate() {
            let q = prog.quasi_node(q);
            let k = PropertyKey::Index(i as u32);
            u.clear();
            let cooked = if q.bad_escape {
                Value::Undefined
            } else {
                let _ = prog.quasi(q, false, &mut u);
                Value::String(h!(self, self.heap.intern_vec(std::mem::take(&mut u))))
            };
            self.def(strings, k, cooked, false, true, false)?;
            u.clear();
            let _ = prog.quasi(q, true, &mut u);
            let r = h!(self, self.heap.intern_vec(std::mem::take(&mut u)));
            self.def(raw, k, Value::String(r), false, true, false)?;
        }
        for a in [strings, raw] {
            let d = PropertyDescriptor { writable: Some(false), ..Default::default() };
            hxw!(self, self.heap.define_own_property(a, self.k_length, &d));
        }
        h!(self, self.heap.prevent_extensions(raw));
        let d = PropertyDescriptor { value: Some(Value::Object(raw)), writable: Some(false), enumerable: Some(false), configurable: Some(false), ..Default::default() };
        let rk = PropertyKey::String(h!(self, self.heap.intern_str("raw")));
        hxw!(self, self.heap.define_own_property(strings, rk, &d));
        h!(self, self.heap.prevent_extensions(strings));
        self.tpl_objs.insert(site, strings);
        Ok(Value::Object(strings))
    }

    fn new_array_from(&mut self, items: &[Value]) -> Res<Value> {
        self.new_array_from_proto(items, self.array_proto)
    }

    fn new_array_from_proto(&mut self, items: &[Value], proto: ObjId) -> Res<Value> {
        let a = h!(self, self.heap.new_array(Some(proto)));
        for (i, &v) in items.iter().enumerate() {
            self.def(a, PropertyKey::Index(i as u32), v, true, true, true)?;
        }
        Ok(Value::Object(a))
    }

    // ------------------------------------------------------------ conversions

    /// Check temporary string growth before allocating the appended units.
    fn append_string(&self, out: &mut Vec<u16>, units: &[u16]) -> Res<()> {
        let limits = self.heap.limits();
        let len = out.len().checked_add(units.len()).ok_or(Error::Limit(LimitKind::Heap(Limit::StringLength)))?;
        if len > limits.max_string_units { return Err(Error::Limit(LimitKind::Heap(Limit::StringLength))); }
        if len > limits.max_bytes / 2 { return Err(Error::Limit(LimitKind::Heap(Limit::Bytes))); }
        out.extend_from_slice(units);
        Ok(())
    }

    fn coerce_primitive(&mut self, v: Value, hint: Hint) -> Res<Value> {
        let Value::Object(_) = v else { return Ok(v) };
        if let Some(p) = self.sc_to_primitive(v, hint)? {
            return Ok(p);
        }
        let order = if hint == Hint::String { [self.k_to_string, self.k_value_of] } else { [self.k_value_of, self.k_to_string] };
        for k in order {
            let m = self.get_prop(v, k)?;
            if self.is_callable(m) {
                let r = self.call_fn(m, v, &[])?;
                if !matches!(r, Value::Object(_)) {
                    return Ok(r);
                }
            }
        }
        Err(self.type_err("Cannot convert object to primitive value"))
    }

    fn coerce_number(&mut self, v: Value) -> Res<f64> {
        let p = self.coerce_primitive(v, Hint::Number)?;
        if let Value::Symbol(_) = p {
            return Err(self.type_err("Cannot convert a Symbol value to a number"));
        }
        Ok(h!(self, self.heap.to_number(p)))
    }

    fn coerce_string(&mut self, v: Value) -> Res<StrId> {
        let p = self.coerce_primitive(v, Hint::String)?;
        Ok(h!(self, self.heap.to_string(p)))
    }

    fn coerce_key(&mut self, v: Value) -> Res<PropertyKey> {
        let p = self.coerce_primitive(v, Hint::String)?;
        Ok(h!(self, self.heap.to_property_key(p)))
    }

    fn truthy(&self, v: Value) -> Res<bool> {
        self.heap.to_boolean(v).map_err(Error::Heap)
    }

    fn type_of(&self, v: Value) -> &'static str {
        if self.is_callable(v) {
            return "function";
        }
        self.heap.type_of(v).unwrap_or("undefined")
    }

    // ------------------------------------------------------------ property access

    fn get_prop(&mut self, base: Value, key: PropertyKey) -> Res<Value> {
        let o = match base {
            Value::Object(o) => o,
            Value::Undefined | Value::Null => {
                let t = self.display(base);
                let k = self.key_text(key);
                return Err(self.type_err(&format!("Cannot read properties of {t} (reading '{k}')")));
            }
            Value::String(s) => {
                if key == self.k_length {
                    return Ok(Value::Number(h!(self, self.heap.str_len(s)) as f64));
                }
                if let PropertyKey::Index(i) = key
                    && (i as usize) < h!(self, self.heap.str_len(s))
                {
                    return Ok(Value::String(h!(self, self.heap.substring(s, i as usize, i as usize + 1))));
                }
                self.ext.string_proto
            }
            Value::Symbol(_) => self.sc.symbol_proto,
            Value::Number(_) => self.ao.number_proto.unwrap_or(self.object_proto),
            Value::Bool(_) => self.sc.bool_proto,
            Value::BigInt(_) | Value::BigHeap(_) => return self.get_prop_big(base, key),
        };
        let got = match self.heap.get(o, key) {
            Ok(g) => g,
            Err(e) => self.get_miss(o, key, base, e)?,
        };
        match got {
            Get::Value(v) => Ok(v),
            Get::Accessor { getter: Some(g) } => self.call_fn(Value::Object(g), base, &[]),
            Get::Accessor { getter: None } => Ok(Value::Undefined),
        }
    }

    /// `get_prop` of a BigInt: the property of `BigInt.prototype`. Out of line, to keep `get_prop` small.
    #[cold]
    #[inline(never)]
    fn get_prop_big(&mut self, base: Value, key: PropertyKey) -> Res<Value> {
        let o = self.big_proto()?;
        let got = match self.heap.get(o, key) {
            Ok(g) => g,
            Err(e) => self.get_miss(o, key, base, e)?,
        };
        match got {
            Get::Value(v) => Ok(v),
            Get::Accessor { getter: Some(g) } => self.call_fn(Value::Object(g), base, &[]),
            Get::Accessor { getter: None } => Ok(Value::Undefined),
        }
    }

    /// The slow side of `get_prop` (`hx!` with its proxy arm, out of line): a host fill, a proxy, or an error. It is a function of
    /// its own so that `get_prop` stays as small as it was and the compiler keeps inlining it into the hot paths.
    #[cold]
    #[inline(never)]
    fn get_miss(&mut self, o: ObjId, key: PropertyKey, base: Value, mut e: HeapError) -> Res<Get> {
        let mut fills = 0u8;
        loop {
            match e {
                HeapError::NeedsHost { owner, index } if fills < 2 => {
                    fills += 1;
                    self.host_fill(owner, index)?;
                }
                HeapError::Proxy(_) => return Ok(Get::Value(self.o_get_slow(o, key, base)?)),
                e => return Err(self.herr(e).into()),
            }
            match self.heap.get(o, key) {
                Ok(g) => return Ok(g),
                Err(n) => e = n,
            }
        }
    }

    fn put_prop(&mut self, base: Value, key: PropertyKey, mut v: Value) -> Res<()> {
        // A write to an integer index of a view runs ToNumber (ToBigInt for a BigInt view) on the value first, even when the
        // index is out of range.
        if let (Value::Object(o), Value::Object(_)) = (base, v)
            && self.heap.has_exotics()
            && self.heap.is_view_index_key(o, key)
        {
            v = self.ta_conv(self.heap.view_type(o).unwrap_or(Elem::U8), v)?;
        }
        match base {
            Value::Object(o) => match match self.heap.set(o, key, v) {
                Ok(r) => r,
                Err(e) => self.set_miss(o, key, v, base, e)?,
            } {
                Set::Done => Ok(()),
                Set::Rejected if self.strict => {
                    let k = self.key_text(key);
                    Err(self.type_err(&format!("Cannot assign to read only property '{k}' of object")))
                }
                Set::Rejected => Ok(()),
                Set::Setter(s) => self.call_fn(Value::Object(s), base, &[v]).map(|_| ()),
            },
            Value::Undefined | Value::Null => {
                let t = self.display(base);
                let k = self.key_text(key);
                Err(self.type_err(&format!("Cannot set properties of {t} (setting '{k}')")))
            }
            _ if self.strict => {
                let k = self.key_text(key);
                let t = self.type_of(base);
                Err(self.type_err(&format!("Cannot create property '{k}' on {t}")))
            }
            _ => Ok(()),
        }
    }

    /// The slow side of `put_prop` (`hxw!` with its proxy arm, out of line). See `get_miss`. A value that is not a BigInt written
    /// to a `BigInt64Array` or `BigUint64Array` element is converted with ToBigInt (a string and a boolean convert, anything else
    /// is a `TypeError`) and written again.
    #[cold]
    #[inline(never)]
    fn set_miss(&mut self, o: ObjId, key: PropertyKey, mut v: Value, base: Value, mut e: HeapError) -> Res<Set> {
        let mut fills = 0u8;
        loop {
            match e {
                HeapError::NeedsHost { .. } | HeapError::NeedsNamed if fills < 2 => {
                    fills += 1;
                    self.host_ask(e)?;
                }
                HeapError::Proxy(_) => return Ok(if self.o_set(o, key, v, base)? { Set::Done } else { Set::Rejected }),
                HeapError::NeedsBigInt if !matches!(v, Value::BigInt(_) | Value::BigHeap(_)) => v = self.to_bigint(v)?,
                e => return Err(self.herr(e).into()),
            }
            match self.heap.set(o, key, v) {
                Ok(r) => return Ok(r),
                Err(n) => e = n,
            }
        }
    }

    fn expr_key(&mut self, e: ExprId, name: Span) -> Res<PropertyKey> {
        if let Some(s) = self.expr_str[e.index()] {
            // A name after a dot is an identifier name: it never starts with a digit, so it is never an array index. The string is
            // rooted by `expr_str`, so it is never stale.
            return Ok(PropertyKey::String(s));
        }
        let n = self.prog.name(name);
        let s = h!(self, self.heap.intern_str(&n));
        self.expr_str[e.index()] = Some(s);
        Ok(PropertyKey::String(s))
    }

    fn prop_key(&mut self, pid: Id<Prop>, key: Key) -> Res<PropertyKey> {
        let prog = self.prog.clone();
        match key {
            Key::Computed(e) => {
                let v = self.eval_expr(e)?;
                self.coerce_key(v)
            }
            Key::None | Key::Private(_) | Key::BigInt(_) => Err(unsupported("property key", Span::default())),
            _ => {
                if let Some(s) = self.prop_str[pid.index()] {
                    return Ok(h!(self, self.heap.key_from_string(s)));
                }
                let s = self.key_string(key, &prog)?;
                self.prop_str[pid.index()] = Some(s);
                Ok(h!(self, self.heap.key_from_string(s)))
            }
        }
    }

    fn key_string(&mut self, key: Key, prog: &Program<'_>) -> Res<StrId> {
        match key {
            Key::Name(sp) => Ok(h!(self, self.heap.intern_str(&prog.name(sp)))),
            Key::Str(sp) => {
                let mut u = Vec::new();
                let _ = prog.string(sp, &mut u);
                Ok(h!(self, self.heap.intern_vec(u)))
            }
            Key::Num(sp) => {
                let n = self.num_lit(sp, num_base(prog.text(sp)));
                Ok(h!(self, self.heap.intern_str(&number_to_string(n))))
            }
            Key::BigInt(sp) => self.big_key_string(sp),
            _ => Err(unsupported("property key", Span::default())),
        }
    }

    fn num_lit(&self, sp: Span, base: NumBase) -> f64 {
        let t = self.prog.text(sp).as_bytes();
        if t.len() <= 15 && t.iter().all(u8::is_ascii_digit) {
            return t.iter().fold(0u64, |a, &d| a * 10 + (d - b'0') as u64) as f64;
        }
        self.prog.number(sp, base).unwrap_or(f64::NAN)
    }

    // ------------------------------------------------------------ names (variables)

    fn tdz(&mut self, n: u32) -> Error {
        let m = format!("Cannot access '{}' before initialization", self.names[n as usize]);
        self.mk_err(REFERENCE_ERROR, &m)
    }

    fn get_name(&mut self, n: u32, site: u32) -> Res<Value> {
        if let Some((e, i)) = self.find_name(n, site) {
            let b = &self.envs[e as usize].bindings[i];
            return if b.flags & INIT != 0 { Ok(b.value) } else { Err(self.tdz(n)) };
        }
        self.get_global(n)
    }

    /// A name that no environment binds: a property of the global object.
    #[inline(never)]
    fn get_global(&mut self, n: u32) -> Res<Value> {
        if let Some(s) = self.name_strs[n as usize]
            && let Some(v) = self.heap.own_data_at(self.global, self.name_gpos[n as usize], PropertyKey::String(s))
        {
            return Ok(v);
        }
        let key = self.name_key(n)?;
        match hx!(self, self.heap.lookup(self.global, key)) {
            Some((holder, Property::Data { value, .. })) => {
                if holder == self.global {
                    self.learn_gpos(n, key);
                }
                Ok(value)
            }
            Some((_, Property::Accessor { get, .. })) => match get {
                Some(g) => self.call_fn(Value::Object(g), Value::Object(self.global), &[]),
                None => Ok(Value::Undefined),
            },
            None => {
                let m = format!("{} is not defined", self.names[n as usize]);
                Err(self.mk_err(REFERENCE_ERROR, &m))
            }
        }
    }

    fn assign_name(&mut self, n: u32, v: Value, site: u32) -> Res<()> {
        if let Some((e, i)) = self.find_name(n, site) {
            let b = &mut self.envs[e as usize].bindings[i];
            if b.flags & (INIT | MUT) == INIT | MUT {
                b.value = v;
                return Ok(());
            }
            return self.assign_name_refused(n, e, i);
        }
        self.assign_global(n, v)
    }

    /// An assignment to a binding that is in its TDZ or is not mutable.
    #[cold]
    #[inline(never)]
    fn assign_name_refused(&mut self, n: u32, e: u32, i: usize) -> Res<()> {
        let flags = self.envs[e as usize].bindings[i].flags;
        if flags & INIT == 0 {
            return Err(self.tdz(n));
        }
        if flags & SELF != 0 && !self.strict {
            return Ok(());
        }
        Err(self.type_err("Assignment to constant variable."))
    }

    #[inline(never)]
    fn assign_global(&mut self, n: u32, v: Value) -> Res<()> {
        if let Some(s) = self.name_strs[n as usize]
            && self.heap.set_own_data_at(self.global, self.name_gpos[n as usize], PropertyKey::String(s), v)
        {
            return Ok(());
        }
        let key = self.name_key(n)?;
        if self.strict && !hx!(self, self.heap.has_property(self.global, key)) {
            let m = format!("{} is not defined", self.names[n as usize]);
            return Err(self.mk_err(REFERENCE_ERROR, &m));
        }
        self.put_prop(Value::Object(self.global), key, v)?;
        self.learn_gpos(n, key);
        Ok(())
    }

    /// Keep the position of the global property `n` for the next use, but only from the second use on: a name used once pays no probe.
    fn learn_gpos(&mut self, n: u32, key: PropertyKey) {
        let h = &mut self.name_gpos[n as usize];
        *h = if *h == NONE { NONE - 1 } else { self.heap.own_pos(self.global, key).unwrap_or(NONE) };
    }

    // ------------------------------------------------------------ expressions

    fn eval_named(&mut self, e: ExprId, name: Option<StrId>) -> Res<Value> {
        let ex = *self.prog.expr(e);
        match ex.kind {
            ExprKind::Function(fid) if self.prog.func(fid).name.is_none() => self.make_closure(fid, name, true),
            ExprKind::Class(c) if self.prog.class(c).name.is_none() => self.eval_class(c, name),
            ExprKind::Paren(inner) => self.eval_named(inner, name),
            _ => self.eval_expr(e),
        }
    }

    fn eval_args(&mut self, l: List<Expr>) -> Res<Vec<Value>> {
        let prog = self.prog.clone();
        let mut out = self.arg_pool.pop().unwrap_or_default();
        out.clear();
        for a in prog.list(l) {
            if let ExprKind::Spread(inner) = prog.expr(a).kind {
                let v = self.eval_expr(inner)?;
                let mut it = self.iter_start(v, inner)?;
                while let Some(x) = self.iter_next(&mut it)? {
                    self.iter_buffer_room(out.len())?;
                    out.push(x);
                }
            } else {
                out.push(self.eval_expr(a)?);
            }
        }
        Ok(out)
    }

    fn link(&mut self, e: ExprId) -> Res<Option<(Value, Value)>> {
        self.descend(|it| it.link_inner(e))
    }

    /// `link(e)`, but a plain name is read at once: `link` of a name goes through `link_inner` and then `eval_expr_inner` (same one step).
    fn link_base(&mut self, e: ExprId) -> Res<Option<(Value, Value)>> {
        if matches!(self.prog.expr(e).kind, ExprKind::Ident) {
            self.tick()?;
            return Ok(Some((self.get_name(self.expr_name[e.index()], e.index() as u32)?, Value::Undefined)));
        }
        self.link(e)
    }

    fn link_inner(&mut self, e: ExprId) -> Res<Option<(Value, Value)>> {
        let prog = self.prog.clone();
        let ex = *prog.expr(e);
        match ex.kind {
            ExprKind::Member { obj, .. } | ExprKind::Index { obj, .. } if self.cls.super_used && is_super(&prog, obj) => self.super_link(e),
            ExprKind::Call { callee, .. } if self.cls.super_used && is_super(&prog, callee) => self.super_call(e),
            ExprKind::PrivateMember { .. } => self.link_private(e),
            ExprKind::Member { obj, name, optional } => {
                let Some((base, _)) = self.link_base(obj)? else { return Ok(None) };
                if optional && is_nullish(base) {
                    return Ok(None);
                }
                let key = self.expr_key(e, name)?;
                Ok(Some((self.get_prop(base, key)?, base)))
            }
            ExprKind::Index { obj, index, optional } => {
                let Some((base, _)) = self.link_base(obj)? else { return Ok(None) };
                if optional && is_nullish(base) {
                    return Ok(None);
                }
                let iv = self.eval_expr(index)?;
                if is_nullish(base) {
                    return Err(self.index_nullish_err(base, iv));
                }
                let key = self.coerce_key(iv)?;
                Ok(Some((self.get_prop(base, key)?, base)))
            }
            ExprKind::Call { callee, args, optional } => {
                let Some((f, this)) = self.link_base(callee)? else { return Ok(None) };
                if optional && is_nullish(f) {
                    return Ok(None);
                }
                let argv = self.eval_args(args)?;
                self.mark_call(e);
                let r = if self.is_callable(f) { self.call_fn(f, this, &argv) } else { Err(self.not_a_function(callee)) };
                self.stk.pos = 0;
                let r = r?;
                self.arg_pool.push(argv);
                Ok(Some((r, Value::Undefined)))
            }
            ExprKind::Tagged { tag, quasi } => self.link_tagged(e, tag, quasi),
            ExprKind::Paren(inner) if matches!(self.unparen(inner), ExprKind::Member { .. } | ExprKind::Index { .. } | ExprKind::PrivateMember { .. } | ExprKind::Chain(_)) => self.link(inner),
            ExprKind::Chain(inner) => Ok(Some(self.link(inner)?.unwrap_or((Value::Undefined, Value::Undefined)))),
            _ => Ok(Some((self.eval_expr(e)?, Value::Undefined))),
        }
    }

    #[cold]
    #[inline(never)]
    fn not_a_function(&mut self, callee: ExprId) -> Error {
        let m = format!("{} is not a function", self.src_text(callee));
        self.type_err(&m)
    }

    #[cold]
    #[inline(never)]
    fn index_nullish_err(&mut self, base: Value, iv: Value) -> Error {
        let t = self.display(base);
        let ks = self.coerce_string(iv).map(|s| self.heap.to_string_lossy(s).unwrap_or_default()).unwrap_or_default();
        self.type_err(&format!("Cannot read properties of {t} (reading '{ks}')"))
    }

    #[inline(never)]
    fn link_tagged(&mut self, e: ExprId, tag: ExprId, quasi: Id<Template>) -> Res<Option<(Value, Value)>> {
        let (f, this) = self.link(tag)?.unwrap_or((Value::Undefined, Value::Undefined));
        let strings = self.template_object(e, quasi)?;
        let prog = self.prog.clone();
        let t = *prog.template(quasi);
        let mut argv = self.arg_pool.pop().unwrap_or_default();
        argv.clear();
        argv.push(strings);
        for x in prog.template_exprs(&t) {
            argv.push(self.eval_expr(x)?);
        }
        self.mark_call(e);
        let r = if self.is_callable(f) { self.call_fn(f, this, &argv) } else { Err(self.not_a_function(tag)) };
        self.stk.pos = 0;
        let r = r?;
        self.arg_pool.push(argv);
        Ok(Some((r, Value::Undefined)))
    }

    fn unparen(&self, mut e: ExprId) -> ExprKind {
        while let ExprKind::Paren(i) = self.prog.expr(e).kind {
            e = i;
        }
        self.prog.expr(e).kind
    }

    fn eval_expr(&mut self, e: ExprId) -> Res<Value> {
        self.descend(|it| it.eval_expr_inner(e))
    }

    fn eval_expr_inner(&mut self, e: ExprId) -> Res<Value> {
        self.tick()?;
        let prog = self.prog.clone();
        let ex = *prog.expr(e);
        match ex.kind {
            ExprKind::Ident => self.get_name(self.expr_name[e.index()], e.index() as u32),
            ExprKind::This if self.expr_name[e.index()] == 0 => Ok(self.lookup_this()),
            ExprKind::This => self.this_binding(),
            ExprKind::Null => Ok(Value::Null),
            ExprKind::Bool(b) => Ok(Value::Bool(b)),
            ExprKind::Number(base) => Ok(Value::Number(self.num_lit(ex.span, base))),
            ExprKind::BigInt => self.big_literal(ex.span),
            ExprKind::String => match self.expr_str[e.index()] {
                Some(s) => Ok(Value::String(s)),
                None => self.string_literal(e, ex.span),
            },
            ExprKind::RegExp => self.ext_eval_regexp(e),
            ExprKind::Template(t) => self.eval_template(t),
            ExprKind::Array(l) => self.eval_array(l),
            ExprKind::Object(l) => self.eval_object(l),
            ExprKind::Function(fid) => self.make_closure(fid, None, true),
            ExprKind::Unary { op, arg } => self.eval_unary(op, arg),
            ExprKind::Update { op, prefix, arg } => self.eval_update(op, prefix, arg),
            ExprKind::Binary { op, left, right } => {
                let a = self.eval_expr(left)?;
                let b = self.eval_expr(right)?;
                self.binary(op, a, b)
            }
            ExprKind::Logical { op, left, right } => {
                let a = self.eval_expr(left)?;
                let short = match op {
                    LogicalOp::And => !self.truthy(a)?,
                    LogicalOp::Or => self.truthy(a)?,
                    LogicalOp::Nullish => !is_nullish(a),
                };
                if short { Ok(a) } else { self.eval_expr(right) }
            }
            ExprKind::Assign { op, target, value } => self.eval_assign(op, target, value),
            ExprKind::Cond { test, cons, alt } => {
                let t = self.eval_expr(test)?;
                if self.truthy(t)? { self.eval_expr(cons) } else { self.eval_expr(alt) }
            }
            ExprKind::Call { .. } | ExprKind::Tagged { .. } | ExprKind::Member { .. } | ExprKind::Index { .. } | ExprKind::PrivateMember { .. } => Ok(self.link(e)?.map_or(Value::Undefined, |p| p.0)),
            ExprKind::Class(c) => self.eval_class(c, None),
            ExprKind::PrivateIn { .. } => self.eval_private_in(e),
            ExprKind::NewTarget => self.eval_new_target(),
            ExprKind::Super => Err(self.mk_err(REFERENCE_ERROR, "Unsupported reference to 'super'")),
            ExprKind::Chain(inner) => Ok(self.link(inner)?.map_or(Value::Undefined, |p| p.0)),
            ExprKind::New { callee, args } => self.eval_new(e, callee, args),
            ExprKind::Seq(l) => self.eval_seq(l),
            ExprKind::Paren(inner) => self.eval_expr(inner),
            ExprKind::Spread(_) | ExprKind::Hole => Err(unsupported("spread or hole outside a list", ex.span)),
            // The first walk rejects these before anything runs.
            _ => Err(unsupported("expression", ex.span)),
        }
    }

    #[inline(never)]
    fn string_literal(&mut self, e: ExprId, span: Span) -> Res<Value> {
        let prog = self.prog.clone();
        let mut u = Vec::new();
        let _ = prog.string(span, &mut u);
        let s = h!(self, self.heap.intern_vec(u));
        self.expr_str[e.index()] = Some(s);
        Ok(Value::String(s))
    }

    #[inline(never)]
    fn eval_template(&mut self, t: Id<Template>) -> Res<Value> {
        let prog = self.prog.clone();
        let t = *prog.template(t);
        let mut out: Vec<u16> = Vec::new();
        let quasis: Vec<_> = prog.template_quasis(&t).collect();
        let exprs: Vec<_> = prog.template_exprs(&t).collect();
        for (i, q) in quasis.iter().enumerate() {
            let _ = prog.quasi(prog.quasi_node(*q), false, &mut out);
            if let Some(&x) = exprs.get(i) {
                let v = self.eval_expr(x)?;
                let s = self.coerce_string(v)?;
                self.append_string(&mut out, h!(self, self.heap.str_units(s)))?;
            }
        }
        Ok(Value::String(h!(self, self.heap.intern_vec(out))))
    }

    #[inline(never)]
    fn eval_array(&mut self, l: List<Expr>) -> Res<Value> {
        let prog = self.prog.clone();
        let a = h!(self, self.heap.new_array(Some(self.array_proto)));
        let mut n: u32 = 0;
        for x in prog.list(l) {
            match prog.expr(x).kind {
                ExprKind::Hole => n += 1,
                ExprKind::Spread(inner) => {
                    let v = self.eval_expr(inner)?;
                    let mut it = self.iter_start(v, inner)?;
                    while let Some(item) = self.iter_next(&mut it)? {
                        self.def(a, PropertyKey::Index(n), item, true, true, true)?;
                        n += 1;
                    }
                }
                _ => {
                    let v = self.eval_expr(x)?;
                    self.def(a, PropertyKey::Index(n), v, true, true, true)?;
                    n += 1;
                }
            }
        }
        hxw!(self, self.heap.set(a, self.k_length, Value::Number(n as f64)));
        Ok(Value::Object(a))
    }

    #[inline(never)]
    fn eval_update(&mut self, op: UpdateOp, prefix: bool, arg: ExprId) -> Res<Value> {
        let r = self.expr_ref(arg)?;
        let old = self.ref_get(&r)?;
        let n = match old {
            Value::Number(n) => n,
            _ => match self.to_numeric(old)? {
                Value::Number(n) => n,
                big => {
                    let new = self.big_step(big, op == UpdateOp::Inc)?;
                    self.ref_put(&r, new)?;
                    return Ok(if prefix { new } else { big });
                }
            },
        };
        let new = if op == UpdateOp::Inc { n + 1.0 } else { n - 1.0 };
        self.ref_put(&r, Value::Number(new))?;
        Ok(Value::Number(if prefix { new } else { n }))
    }

    #[inline(never)]
    fn eval_new(&mut self, e: ExprId, callee: ExprId, args: List<Expr>) -> Res<Value> {
        let f = self.eval_expr(callee)?;
        let argv = self.eval_args(args)?;
        self.mark_call(e);
        let r = self.construct(f, &argv, callee);
        self.stk.pos = 0;
        r
    }

    #[inline(never)]
    fn eval_seq(&mut self, l: List<Expr>) -> Res<Value> {
        let prog = self.prog.clone();
        let mut last = Value::Undefined;
        for x in prog.list(l) {
            last = self.eval_expr(x)?;
        }
        Ok(last)
    }

    fn eval_object(&mut self, l: List<Prop>) -> Res<Value> {
        let prog = self.prog.clone();
        let o = h!(self, self.heap.new_object(Some(self.object_proto)));
        if prog.list(l).len() > 0 {
            h!(self, self.heap.reserve_props(o, prog.list(l).len()));
        }
        for pid in prog.list(l) {
            self.object_prop(o, pid, None)?;
        }
        Ok(Value::Object(o))
    }

    /// Define one property of an object literal. `pre` is the already evaluated value of a spread, `key: value`
    /// or shorthand property (the async evaluator awaits it first).
    fn object_prop(&mut self, o: ObjId, pid: Id<Prop>, pre: Option<Value>) -> Res<()> {
        let prog = self.prog.clone();
        let p = *prog.prop(pid);
        match p.kind {
            PropKind::Spread => {
                let v = match pre {
                    Some(v) => v,
                    None => self.eval_expr(p.value)?,
                };
                self.copy_props(o, v, &[])?;
            }
            PropKind::Init | PropKind::Shorthand | PropKind::Method => {
                let key = self.prop_key(pid, p.key)?;
                if p.kind == PropKind::Init && !matches!(p.key, Key::Computed(_)) && key == self.k_proto {
                    let v = match pre {
                        Some(v) => v,
                        None => self.eval_expr(p.value)?,
                    };
                    match v {
                        Value::Object(po) => { h!(self, self.heap.set_prototype_of(o, Some(po))); }
                        Value::Null => { h!(self, self.heap.set_prototype_of(o, None)); }
                        _ => {}
                    }
                    return Ok(());
                }
                let name = match key {
                    PropertyKey::Symbol(s) => Some(self.symbol_fn_name(s)?),
                    k => Some(self.key_str(k)?),
                };
                let v = match pre {
                    Some(v) => v,
                    None => self.eval_named(p.value, name)?,
                };
                self.def(o, key, v, true, true, true)?;
            }
            PropKind::Get | PropKind::Set => {
                let key = self.prop_key(pid, p.key)?;
                let ExprKind::Function(fid) = prog.expr(p.value).kind else { return Err(unsupported("accessor", p.span)) };
                let Value::Object(f) = self.make_closure(fid, None, false)? else { return Err(unsupported("accessor", p.span)) };
                let d = if p.kind == PropKind::Get {
                    PropertyDescriptor { get: Some(Some(f)), enumerable: Some(true), configurable: Some(true), ..Default::default() }
                } else {
                    PropertyDescriptor { set: Some(Some(f)), enumerable: Some(true), configurable: Some(true), ..Default::default() }
                };
                hxw!(self, self.heap.define_own_property(o, key, &d));
            }
        }
        Ok(())
    }

    fn key_str(&mut self, k: PropertyKey) -> Res<StrId> {
        match k {
            PropertyKey::String(s) => Ok(s),
            PropertyKey::Index(i) => Ok(h!(self, self.heap.intern_str(&i.to_string()))),
            PropertyKey::Symbol(_) => Err(unsupported("symbol key", Span::default())),
        }
    }

    /// CopyDataProperties: own enumerable properties of `src` onto `to`, skipping `skip`.
    fn copy_props(&mut self, to: ObjId, src: Value, skip: &[PropertyKey]) -> Res<()> {
        let keys: Vec<PropertyKey> = match src {
            Value::Object(o) => self.property_keys(o)?,
            Value::String(s) => (0..h!(self, self.heap.str_len(s)) as u32).map(PropertyKey::Index).collect(),
            _ => return Ok(()),
        };
        for k in keys {
            if skip.contains(&k) {
                continue;
            }
            if let Value::Object(o) = src {
                match self.o_peek(o, k)? {
                    Some(p) if p.enumerable() => {}
                    _ => continue,
                }
            }
            let v = self.get_prop(src, k)?;
            self.def(to, k, v, true, true, true)?;
        }
        Ok(())
    }

    fn eval_unary(&mut self, op: UnaryOp, arg: ExprId) -> Res<Value> {
        match op {
            UnaryOp::Typeof => {
                if let ExprKind::Ident = self.unparen(arg) {
                    let mut a = arg;
                    while let ExprKind::Paren(i) = self.prog.expr(a).kind {
                        a = i;
                    }
                    let n = self.expr_name[a.index()];
                    if self.find_name(n, a.index() as u32).is_none() {
                        let key = self.name_key(n)?;
                        if !hx!(self, self.heap.has_property(self.global, key)) {
                            let s = h!(self, self.heap.intern_str("undefined"));
                            return Ok(Value::String(s));
                        }
                    }
                }
                let v = self.eval_expr(arg)?;
                let t = self.type_of(v);
                Ok(Value::String(h!(self, self.heap.intern_str(t))))
            }
            UnaryOp::Delete => self.eval_delete(arg),
            UnaryOp::Void => {
                self.eval_expr(arg)?;
                Ok(Value::Undefined)
            }
            UnaryOp::Not => {
                let v = self.eval_expr(arg)?;
                Ok(Value::Bool(!self.truthy(v)?))
            }
            UnaryOp::Neg => {
                let v = self.eval_expr(arg)?;
                match self.to_numeric(v)? {
                    Value::Number(n) => Ok(Value::Number(-n)),
                    big => self.big_neg(big),
                }
            }
            UnaryOp::Plus => {
                let v = self.eval_expr(arg)?;
                Ok(Value::Number(self.coerce_number(v)?))
            }
            UnaryOp::BitNot => {
                let v = self.eval_expr(arg)?;
                match self.to_numeric(v)? {
                    Value::Number(n) => Ok(Value::Number(!to_int32(n) as f64)),
                    big => self.big_not(big),
                }
            }
        }
    }

    fn eval_delete(&mut self, arg: ExprId) -> Res<Value> {
        let mut a = arg;
        while let ExprKind::Paren(i) = self.prog.expr(a).kind {
            a = i;
        }
        match self.prog.expr(a).kind {
            ExprKind::Member { .. } | ExprKind::Index { .. } => {
                let (base, key) = self.member_ref(a)?;
                let o = match base {
                    Value::Object(o) => o,
                    Value::Undefined | Value::Null => {
                        let t = self.display(base);
                        let k = self.key_text(key);
                        return Err(self.type_err(&format!("Cannot convert undefined or null to object ({t}, deleting '{k}')")));
                    }
                    _ => return Ok(Value::Bool(true)),
                };
                let ok = hxw!(self, self.heap.delete(o, key), self.px_delete(o, key)?);
                if !ok && self.strict {
                    let k = self.key_text(key);
                    return Err(self.type_err(&format!("Cannot delete property '{k}' of #<Object>")));
                }
                Ok(Value::Bool(ok))
            }
            ExprKind::Ident => {
                let n = self.expr_name[a.index()];
                if self.lookup(n).is_some() {
                    return Ok(Value::Bool(false));
                }
                let key = self.name_key(n)?;
                Ok(Value::Bool(hxw!(self, self.heap.delete(self.global, key))))
            }
            _ => {
                self.eval_expr(arg)?;
                Ok(Value::Bool(true))
            }
        }
    }

    // ------------------------------------------------------------ references and assignment

    /// Evaluate the base and key of `a.b` or `a[b]`.
    fn member_ref(&mut self, e: ExprId) -> Res<(Value, PropertyKey)> {
        let ex = *self.prog.expr(e);
        match ex.kind {
            ExprKind::Member { obj, name, .. } => {
                let base = self.eval_expr(obj)?;
                Ok((base, self.expr_key(e, name)?))
            }
            ExprKind::Index { obj, index, .. } => {
                let base = self.eval_expr(obj)?;
                let iv = self.eval_expr(index)?;
                if is_nullish(base) {
                    let t = self.display(base);
                    let ks = self.coerce_string(iv).map(|s| self.heap.to_string_lossy(s).unwrap_or_default()).unwrap_or_default();
                    return Err(self.type_err(&format!("Cannot read properties of {t} (reading '{ks}')")));
                }
                Ok((base, self.coerce_key(iv)?))
            }
            ExprKind::Paren(i) => self.member_ref(i),
            _ => Err(unsupported("assignment target", ex.span)),
        }
    }

    fn expr_ref(&mut self, e: ExprId) -> Res<Ref> {
        let ex = *self.prog.expr(e);
        match ex.kind {
            ExprKind::Ident => Ok(Ref::Name(self.expr_name[e.index()], e.index() as u32)),
            ExprKind::Paren(i) => self.expr_ref(i),
            ExprKind::Member { .. } | ExprKind::Index { .. } | ExprKind::PrivateMember { .. } => self.member_target(e),
            _ => Err(unsupported("update target", ex.span)),
        }
    }

    fn ref_get(&mut self, r: &Ref) -> Res<Value> {
        match *r {
            Ref::Name(n, site) => self.get_name(n, site),
            Ref::Prop(b, k) => self.get_prop(b, k),
            Ref::Priv(b, pn, n, fl) => self.priv_get(b, pn, n, fl),
            Ref::Super(this, k) => self.super_get(this, k),
        }
    }

    fn ref_put(&mut self, r: &Ref, v: Value) -> Res<()> {
        match *r {
            Ref::Name(n, site) => self.assign_name(n, v, site),
            Ref::Prop(b, k) => self.put_prop(b, k, v),
            Ref::Priv(b, pn, n, fl) => self.priv_put(b, pn, n, fl, v),
            Ref::Super(this, k) => self.super_set(this, k, v),
        }
    }

    fn eval_assign(&mut self, op: AssignOp, target: PatId, value: ExprId) -> Res<Value> {
        let prog = self.prog.clone();
        let pat = *prog.pat(target);
        let r = match pat.kind {
            PatKind::Ident => Ref::Name(self.pat_name[target.index()], self.pat_site(target.index())),
            PatKind::Member(me) => self.member_target(me)?,
            PatKind::Array(_) | PatKind::Object(_) if op == AssignOp::Assign => {
                let v = self.eval_expr(value)?;
                self.bind_pattern(target, v, Bind::Assign)?;
                return Ok(v);
            }
            _ => return Err(unsupported("assignment target", pat.span)),
        };
        let name = match r {
            Ref::Name(n, _) => Some(self.name_str(n)?),
            _ => None,
        };
        let v = match op {
            AssignOp::Assign => self.eval_named(value, name)?,
            AssignOp::Compound(b) => {
                let old = self.ref_get(&r)?;
                let rhs = self.eval_expr(value)?;
                self.binary(b, old, rhs)?
            }
            AssignOp::Logical(l) => {
                let old = self.ref_get(&r)?;
                let short = match l {
                    LogicalOp::And => !self.truthy(old)?,
                    LogicalOp::Or => self.truthy(old)?,
                    LogicalOp::Nullish => !is_nullish(old),
                };
                if short {
                    return Ok(old);
                }
                self.eval_named(value, name)?
            }
        };
        self.ref_put(&r, v)?;
        Ok(v)
    }

    fn bind_pattern(&mut self, p: PatId, v: Value, mode: Bind) -> Res<()> {
        self.descend(|it| it.bind_pattern_inner(p, v, mode))
    }

    fn bind_pattern_inner(&mut self, p: PatId, v: Value, mode: Bind) -> Res<()> {
        let prog = self.prog.clone();
        let pat = *prog.pat(p);
        match pat.kind {
            PatKind::Ident => {
                let n = self.pat_name[p.index()];
                match mode {
                    Bind::Assign => self.assign_name(n, v, self.pat_site(p.index())),
                    Bind::Declare => {
                        match self.pat_frame(n, self.pat_site(p.index())) {
                            Some(i) => self.envs[FRAME_ENV as usize].bindings[i].value = v,
                            None => self.env_declare(self.env, n, INIT | MUT, v),
                        }
                        Ok(())
                    }
                    Bind::Init => {
                        if let Some(i) = self.pat_frame(n, self.pat_site(p.index())) {
                            let b = &mut self.envs[FRAME_ENV as usize].bindings[i];
                            b.flags |= INIT;
                            b.value = v;
                            return Ok(());
                        }
                        match self.find_in(self.env, n) {
                            Some(i) => {
                                let b = &mut self.envs[self.env as usize].bindings[i];
                                b.flags |= INIT;
                                b.value = v;
                            }
                            None => self.env_declare(self.env, n, INIT | MUT, v),
                        }
                        Ok(())
                    }
                }
            }
            PatKind::Member(me) => {
                let r = self.member_target(me)?;
                self.ref_put(&r, v)
            }
            PatKind::Assign { target, default } => {
                let v = if matches!(v, Value::Undefined) {
                    let name = if let PatKind::Ident = prog.pat(target).kind { Some(self.name_str(self.pat_name[target.index()])?) } else { None };
                    self.eval_named(default, name)?
                } else {
                    v
                };
                self.bind_pattern(target, v, mode)
            }
            PatKind::Array(l) => self.bind_array_pattern(l, v, mode),
            PatKind::Object(l) => {
                if is_nullish(v) {
                    let t = self.display(v);
                    return Err(self.type_err(&format!("Cannot destructure '{t}' as it is {t}.")));
                }
                let mut used = Vec::new();
                for q in prog.list(l) {
                    let pp = *prog.pat_prop(q);
                    if let PatKind::Rest(t) = prog.pat(pp.value).kind {
                        let o = h!(self, self.heap.new_object(Some(self.object_proto)));
                        self.copy_props(o, v, &used)?;
                        self.bind_pattern(t, Value::Object(o), mode)?;
                        continue;
                    }
                    let key = match pp.key {
                        Key::Computed(e) => {
                            let kv = self.eval_expr(e)?;
                            self.coerce_key(kv)?
                        }
                        k => {
                            let s = self.key_string(k, &prog)?;
                            h!(self, self.heap.key_from_string(s))
                        }
                    };
                    used.push(key);
                    let x = self.get_prop(v, key)?;
                    self.bind_pattern(pp.value, x, mode)?;
                }
                Ok(())
            }
            PatKind::Rest(_) | PatKind::Hole => Err(unsupported("pattern", pat.span)),
        }
    }

    // ------------------------------------------------------------ operators

    fn binary(&mut self, op: BinaryOp, a: Value, b: Value) -> Res<Value> {
        use BinaryOp::*;
        if let (Value::Number(x), Value::Number(y)) = (a, b) {
            match op {
                Add => return Ok(Value::Number(x + y)),
                Sub => return Ok(Value::Number(x - y)),
                Mul => return Ok(Value::Number(x * y)),
                Lt => return Ok(Value::Bool(x < y)),
                Gt => return Ok(Value::Bool(x > y)),
                Le => return Ok(Value::Bool(x <= y)),
                Ge => return Ok(Value::Bool(x >= y)),
                StrictEq | Eq => return Ok(Value::Bool(x == y)),
                StrictNe | Ne => return Ok(Value::Bool(x != y)),
                _ => {}
            }
        }
        match op {
            Add => {
                let (pa, pb) = (self.coerce_primitive(a, Hint::Default)?, self.coerce_primitive(b, Hint::Default)?);
                if matches!(pa, Value::String(_)) || matches!(pb, Value::String(_)) {
                    let sa = h!(self, self.heap.to_string(pa));
                    let sb = h!(self, self.heap.to_string(pb));
                    return Ok(Value::String(h!(self, self.heap.concat(sa, sb))));
                }
                if pa.is_bigint() || pb.is_bigint() {
                    let (na, nb) = (self.numeric_of_prim(pa)?, self.numeric_of_prim(pb)?);
                    return self.big_arith(Add, na, nb);
                }
                let (x, y) = (h!(self, self.heap.to_number(pa)), h!(self, self.heap.to_number(pb)));
                Ok(Value::Number(x + y))
            }
            Sub | Mul | Div | Mod | Exp | Shl | Shr | UShr | BitAnd | BitOr | BitXor => {
                let (x, y) = if let (Value::Number(x), Value::Number(y)) = (a, b) {
                    (x, y)
                } else {
                    let (na, nb) = (self.to_numeric(a)?, self.to_numeric(b)?);
                    match (na, nb) {
                        (Value::Number(x), Value::Number(y)) => (x, y),
                        _ => return self.big_arith(op, na, nb),
                    }
                };
                Ok(Value::Number(match op {
                    Sub => x - y,
                    Mul => x * y,
                    Div => x / y,
                    Mod => x % y,
                    Exp => {
                        ieee754::pow(x, y)
                    }
                    Shl => to_int32(x).wrapping_shl(to_uint32(y) & 31) as f64,
                    Shr => (to_int32(x) >> (to_uint32(y) & 31)) as f64,
                    UShr => (to_uint32(x) >> (to_uint32(y) & 31)) as f64,
                    BitAnd => (to_int32(x) & to_int32(y)) as f64,
                    BitOr => (to_int32(x) | to_int32(y)) as f64,
                    _ => (to_int32(x) ^ to_int32(y)) as f64,
                }))
            }
            StrictEq => Ok(Value::Bool(a.strict_equals(b))),
            StrictNe => Ok(Value::Bool(!a.strict_equals(b))),
            Eq => Ok(Value::Bool(self.loose_eq(a, b)?)),
            Ne => Ok(Value::Bool(!self.loose_eq(a, b)?)),
            Lt | Gt | Le | Ge => {
                let (pa, pb) = (self.coerce_primitive(a, Hint::Number)?, self.coerce_primitive(b, Hint::Number)?);
                let r = match op {
                    Lt => self.less(pa, pb)? == Some(true),
                    Gt => self.less(pb, pa)? == Some(true),
                    Le => self.less(pb, pa)? == Some(false),
                    _ => self.less(pa, pb)? == Some(false),
                };
                Ok(Value::Bool(r))
            }
            In => {
                let Value::Object(o) = b else {
                    let t = self.display(b);
                    let k = self.coerce_string(a).map(|s| self.heap.to_string_lossy(s).unwrap_or_default()).unwrap_or_default();
                    return Err(self.type_err(&format!("Cannot use 'in' operator to search for '{k}' in {t}")));
                };
                let key = self.coerce_key(a)?;
                Ok(Value::Bool(self.o_has_property(o, key)?))
            }
            Instanceof => self.sc_instanceof(a, b),
        }
    }

    /// `a < b` on primitives: `None` is `undefined` (a NaN).
    fn less(&mut self, a: Value, b: Value) -> Res<Option<bool>> {
        if let (Value::String(x), Value::String(y)) = (a, b) {
            let (ux, uy) = (h!(self, self.heap.str_units(x)), h!(self, self.heap.str_units(y)));
            return Ok(Some(ux < uy));
        }
        if a.is_bigint() || b.is_bigint() {
            return Ok(self.big_order(a, b)?.map(|o| o.is_lt()));
        }
        let (x, y) = (h!(self, self.heap.to_number(a)), h!(self, self.heap.to_number(b)));
        Ok(if x.is_nan() || y.is_nan() { None } else { Some(x < y) })
    }

    fn loose_eq(&mut self, a: Value, b: Value) -> Res<bool> {
        match (a, b) {
            (Value::Object(_), Value::Object(_)) => Ok(a.strict_equals(b)),
            (Value::Object(_), x) if !is_nullish(x) => {
                let p = self.coerce_primitive(a, Hint::Default)?;
                self.loose_eq(p, b)
            }
            (x, Value::Object(_)) if !is_nullish(x) => {
                let p = self.coerce_primitive(b, Hint::Default)?;
                self.loose_eq(a, p)
            }
            _ => Ok(h!(self, self.heap.loose_equals(a, b))),
        }
    }

    // ------------------------------------------------------------ statements

    fn loop_step(r: Ex, labels: &[u32]) -> Result<bool, Ctl> {
        match r {
            Ok(()) => Ok(true),
            Err(Ctl::Break(l)) if l == 0 || labels.contains(&l) => Ok(false),
            Err(Ctl::Continue(l)) if l == 0 || labels.contains(&l) => Ok(true),
            Err(c) => Err(c),
        }
    }

    /// Annex B.3.3: in sloppy code a function declared in a block is also assigned to the var of the
    /// same name when the declaration is evaluated.
    fn annex_b(&mut self, fid: FuncId) -> Res<()> {
        if self.strict || self.envs[self.env as usize].var_scope || !self.prog.func(fid).is_plain_ctor() {
            return Ok(());
        }
        let n = self.func_name[fid.index()];
        let Some(i) = self.find_in(self.env, n) else { return Ok(()) };
        let v = self.envs[self.env as usize].bindings[i].value;
        let mut e = self.envs[self.env as usize].parent;
        while e != NONE && !self.envs[e as usize].var_scope {
            e = self.envs[e as usize].parent;
        }
        if e == 0 {
            let key = self.name_key(n)?;
            return self.put_prop(Value::Object(self.global), key, v);
        }
        if e != NONE
            && let Some(j) = self.find_in(e, n)
        {
            self.envs[e as usize].bindings[j].value = v;
        }
        Ok(())
    }

    /// CreatePerIterationEnvironment: a fresh copy, so a closure keeps the old iteration's values.
    fn copy_env(&mut self) -> Res<()> {
        let old = self.env;
        let parent = self.envs[old as usize].parent;
        let new = self.new_env(parent, false)?;
        let items: Vec<(u32, u8, Value)> = self.envs[old as usize].bindings.iter().map(|b| (b.name, b.flags, b.value)).collect();
        for (n, f, v) in items {
            self.env_declare(new, n, f, v);
        }
        self.env = new;
        Ok(())
    }

    fn exec_for_each(&mut self, left: ForLeft, right: ExprId, body: StmtId, is_of: bool, labels: &[u32]) -> Ex {
        let prog = self.prog.clone();
        let rv = self.eval_expr(right)?;
        let outer = self.env;
        let mut lexical: Option<(PatId, VarKind)> = None;
        // In a function with frame slots (`eval_scope.rs`): no environment for an iteration, only the TDZ again.
        let mut mark = 0;
        let target = match left {
            ForLeft::Pat(p) => p,
            ForLeft::Var(v) => {
                let StmtKind::Var { kind, decls } = prog.stmt(v).kind else { return Err(unsupported("for left", Span::default()).into()) };
                let d = prog.list(decls).next().map(|d| *prog.decl(d));
                let Some(d) = d else { return Err(unsupported("for left", Span::default()).into()) };
                if kind != VarKind::Var {
                    lexical = Some((d.target, kind));
                    mark = self.stmt_label[v.index()];
                }
                d.target
            }
        };
        let mut cur_env: Option<u32> = None;
        let mut step = |me: &mut Self, v: Value| -> Result<bool, Ctl> {
            if mark != 0 {
                me.fr_tdz(mark);
                me.bind_pattern(target, v, Bind::Init)?;
            } else if let Some((p, kind)) = lexical {
                if let Some(e) = cur_env.take() {
                    me.env = outer;
                    me.release(e);
                }
                let e = me.new_env(outer, false)?;
                cur_env = Some(e);
                me.env = e;
                me.declare_pat_lexical(p, if kind == VarKind::Let { MUT } else { 0 });
                me.bind_pattern(target, v, Bind::Init)?;
            } else {
                me.bind_pattern(target, v, Bind::Assign)?;
            }
            Self::loop_step(me.exec(body), labels)
        };
        let r: Ex = (|| {
            if is_of {
                let mut it = self.iter_start(rv, right)?;
                while let Some(v) = self.iter_next(&mut it)? {
                    match step(self, v) {
                        Ok(true) => {}
                        Ok(false) => {
                            self.iter_close(&mut it)?;
                            break;
                        }
                        Err(Ctl::Err(e)) => return Err(Ctl::Err(self.iter_close_throw(&mut it, e))),
                        Err(c) => {
                            self.iter_close(&mut it)?;
                            return Err(c);
                        }
                    }
                }
            } else {
                let (keys, proxied) = self.for_in_keys(rv)?;
                for k in keys {
                    if let Value::Object(o) = rv
                        && !(if proxied { self.px_for_in_has(o, k)? } else { self.o_has_property(o, k)? })
                    {
                        continue;
                    }
                    let kv = h!(self, self.heap.key_to_value(k));
                    if !step(self, kv)? {
                        break;
                    }
                }
            }
            Ok(())
        })();
        self.env = outer;
        if let Some(e) = cur_env {
            self.release(e);
        }
        r
    }

    /// Charge virtual view indices before materializing their key list.
    fn property_keys(&mut self, o: ObjId) -> Res<Vec<PropertyKey>> {
        // `hx!`: a host-indexed object whose host cache is stale asks to be filled first (`Host::fill_indexed`).
        let len = hx!(self, self.heap.virtual_len(o));
        if len != 0 {
            self.spend(len as u64)?;
        }
        self.o_own_keys(o)
    }

    /// The keys of a `for-in`, and whether a proxy is on the chain (then each key is checked again as V8 does: `px_for_in_has`).
    fn for_in_keys(&mut self, v: Value) -> Res<(Vec<PropertyKey>, bool)> {
        let mut o = match v {
            Value::Object(o) => o,
            Value::String(s) => return Ok(((0..h!(self, self.heap.str_len(s)) as u32).map(PropertyKey::Index).collect(), false)),
            _ => return Ok((Vec::new(), false)),
        };
        let mut seen: FxSet<PropertyKey> = FxSet::default();
        let mut out = Vec::new();
        let mut proxied = false;
        loop {
            let proxy = self.heap.is_proxy(o);
            proxied |= proxy;
            for k in self.property_keys(o)? {
                self.tick()?;
                if matches!(k, PropertyKey::Symbol(_)) || !seen.insert(k) {
                    continue;
                }
                // V8 does not ask a proxy for the enumerability of its keys here: it asks at each step of the loop.
                if proxy || matches!(self.o_peek(o, k)?, Some(p) if p.enumerable()) {
                    out.push(k);
                }
            }
            match self.o_get_prototype_of(o)? {
                Some(p) => o = p,
                None => break,
            }
        }
        Ok((out, proxied))
    }
}

type NumBase = crate::lex::NumberBase;

fn num_base(t: &str) -> NumBase {
    let b = t.as_bytes();
    if b.len() > 1 && b[0] == b'0' {
        match b[1] {
            b'x' | b'X' => return NumBase::Hex,
            b'o' | b'O' => return NumBase::Octal,
            b'b' | b'B' => return NumBase::Binary,
            _ => {}
        }
    }
    NumBase::Decimal
}

include!("eval_statements.rs");
include!("eval_proxy.rs");
include!("eval_native.rs");
include!("eval_promise.rs");
include!("eval_promise_combinators.rs");
include!("eval_binary.rs");
include!("eval_binary_search.rs");
include!("eval_typed.rs");
include!("eval_scripts.rs");
include!("eval_scope.rs");
include!("eval_stack.rs");
include!("eval_builtins.rs");
include!("eval_string_more.rs");
include!("eval_json_stringify.rs");
include!("eval_json_revive.rs");
include!("eval_host.rs");
include!("eval_array_object.rs");
include!("eval_array_more.rs");
include!("eval_numeric.rs");
include!("eval_symbols.rs");
include!("eval_collections.rs");
include!("eval_set_methods.rs");
include!("eval_iterator.rs");
include!("eval_weak.rs");
include!("eval_date.rs");
include!("eval_intl.rs");
include!("eval_reflect.rs");
include!("eval_bigint.rs");
include!("eval_object_more.rs");
include!("eval_classes.rs");
include!("eval_wrapper.rs");
include!("eval_async.rs");
include!("eval_generator.rs");
include!("eval_clone.rs");

#[cfg(test)]
#[path = "eval_class_tests.rs"]
mod class_tests;

#[cfg(test)]
#[path = "eval_interact_tests.rs"]
mod interact_tests;

#[cfg(test)]
#[path = "eval_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "eval_promise_tests.rs"]
mod promise_tests;

#[cfg(test)]
#[path = "eval_promise_combinators_tests.rs"]
mod promise_combinator_tests;

#[cfg(test)]
#[path = "eval_scripts_tests.rs"]
mod script_tests;

#[cfg(test)]
#[path = "eval_scope_tests.rs"]
mod scope_tests;
#[cfg(test)]
#[path = "eval_builtins_tests.rs"]
mod builtin_tests;

#[cfg(test)]
#[path = "eval_batch1_tests.rs"]
mod batch1_tests;

#[cfg(test)]
#[path = "eval_json_stringify_tests.rs"]
mod stringify_tests;

#[cfg(test)]
#[path = "eval_json_revive_tests.rs"]
mod revive_tests;

#[cfg(test)]
#[path = "eval_host_tests.rs"]
mod host_tests;

#[cfg(test)]
#[path = "eval_host_nt_tests.rs"]
mod host_nt_tests;

#[cfg(test)]
#[path = "eval_indexed_tests.rs"]
mod indexed_tests;
#[cfg(test)]
#[path = "eval_named_tests.rs"]
mod named_tests;

#[cfg(test)]
#[path = "eval_proxy_tests.rs"]
mod proxy_tests;

#[cfg(test)]
#[path = "eval_binary_tests.rs"]
mod binary_tests;

#[cfg(test)]
#[path = "eval_typed_tests.rs"]
mod typed_tests;

#[cfg(test)]
#[path = "eval_array_object_tests.rs"]
mod array_object_tests;

#[cfg(test)]
#[path = "eval_numeric_tests.rs"]
mod numeric_tests;

#[cfg(test)]
#[path = "eval_sc_tests.rs"]
mod sc_tests;

#[cfg(test)]
#[path = "eval_interop_tests.rs"]
mod interop_tests;

#[cfg(test)]
#[path = "eval_async_tests.rs"]
mod async_tests;

#[cfg(test)]
#[path = "eval_generator_tests.rs"]
mod generator_tests;

#[cfg(test)]
#[path = "eval_tagged_tests.rs"]
mod tagged_tests;

#[cfg(test)]
#[path = "eval_intl_tests.rs"]
mod intl_tests;

#[cfg(test)]
#[path = "eval_intl_num_tests.rs"]
mod intl_num_tests;

#[cfg(test)]
#[path = "eval_number_tests.rs"]
mod number_tests;

#[cfg(test)]
#[path = "eval_bigint_tests.rs"]
mod bigint_eval_tests;
#[cfg(test)]
#[path = "eval_iterator_tests.rs"]
mod iterator_tests;

#[cfg(test)]
#[path = "eval_stack_tests.rs"]
mod stack_tests;

#[cfg(test)]
#[path = "eval_weak_tests.rs"]
mod weak_tests;

#[cfg(test)]
#[path = "eval_date_tests.rs"]
mod date_tests;

#[cfg(test)]
#[path = "eval_reflect_tests.rs"]
mod reflect_tests;

#[cfg(test)]
#[path = "eval_wrapper_tests.rs"]
mod wrapper_tests;

#[cfg(test)]
#[path = "eval_depth_tests.rs"]
mod depth_tests;

#[cfg(test)]
#[path = "eval_recursion_tests.rs"]
mod recursion_tests;

#[cfg(test)]
#[path = "eval_statements_tests.rs"]
mod statements_tests;
