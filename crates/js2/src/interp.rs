//! The interpreter (`docs/design-engine-rewrite.md` 6.10 items 3, 8 and 9, 6.11 item 11; plan step S2.6). It runs the bytecode of
//! `bytecode.rs` after `verify::verify` accepted it, and reads operands with no check that the verifier proved.
//!
//! **The loop.** One `loop` and one `match` on the op. The hot state is in locals: `pc`, `fp` (the index of register 0 in the stack),
//! the code, its words and its feedback vector. An op decodes with shifts from one word. `Wide` is a separate branch before the match,
//! which decodes the two words by form and runs the same arms (the `step!` macro holds the arms once and the two branches only differ in
//! how they get the fields), so the narrow word, which is nearly every op, pays nothing for it.
//!
//! **The stack.** One `Vec<Value>` for all frames. A frame is a record of [`REC`] words and then the registers of the function:
//!
//! | Word | Holds |
//! | --- | --- |
//! | `fp-10` | the function cell (a root; it keeps the context that the code closed over) |
//! | `fp-9` | `this` |
//! | `fp-8` | the current context: the closure's context at the entry, then `PushCtx`, `PopCtx` and `CopyCtx` change it |
//! | `fp-7` | the `pc` of the call in the caller (the key of its root map; -1: a frame that has no map, see below) |
//! | `fp-6` | the `pc` where the caller goes on |
//! | `fp-5` | the code index of the caller, or -1 for the frame that the host entered |
//! | `fp-4` | the number of contexts that this function pushed (the handlers need it) |
//! | `fp-3` | the register of the caller that gets the result, bit 24 for a construct call, bit 25 for `super()` |
//! | `fp-2` | `new.target` (`undefined` for a plain call) |
//! | `fp-1` | the arguments object, for a function that reads all its arguments (`Code::flags` `CF_ARGS`), else `undefined` |
//!
//! A call writes the record and copies the arguments to registers `0..nparams` (missing ones are `undefined`; extra ones are dropped,
//! except that a function with `CF_ARGS` gets all of them as the arguments object, made at the call). It does **not** fill the other
//! registers: the verifier proved that a register is written before it is read (6.10 item 4). A return writes the result into the
//! caller's register and drops the frame by moving `fp`.
//!
//! **Constructors and fields.** A construct call (`New`, `NewSpread`, `CallSuper`) makes `this` from `new.target.prototype` (not for a
//! derived class, whose `this` is the variable that `super()` sets). The fields of a base class run before its body: the call pushes the
//! frame of the constructor and, on top of it, the frame of its fields function (`SetFields`), which has the pc -1 in its record, so
//! that no handler of the constructor catches its exceptions and no root map is looked up for the constructor (its arguments are
//! also in the registers of the caller). The fields of a derived class run when `super()` returns: the return of the parent frame
//! (bit 25) pushes the fields function of the class on top of the caller, at the pc of the `CallSuper`. A getter or a setter is a frame
//! too: its result goes to the register of the op (a setter: to the dead word past the registers of the caller).
//!
//! **Collection.** An op that allocates starts with `gate!`: when `Realm::due`, it stores `(code, fp, pc)` and collects, and the
//! frames are walked in place (`walk_frames`): the record words and the registers that the root map of the safepoint names, from the top
//! frame down. The op then runs with no collection inside. `Code::roots` is keyed by the `pc` of the instruction, so the top frame
//! uses the `pc` of the op and each caller uses the `pc` of its call (`fp-5` of the callee).
//!
//! **Exceptions.** A thrown value goes to `unwind`: the first handler of the code whose range holds the `pc` of the throwing
//! instruction (a caller: of its call) gets it in its register, after the contexts are popped down to the depth of the handler. With no
//! handler the frame goes away. When the frame of the host is reached, [`Vm::call`] returns `Err(Exit::Throw(value))`.
//!
//! **Caches.** `GetField`, `SetField`, `GetCtxField`, `GetGlobal` and `SetGlobal` compare the shape id of the receiver with the record
//! of the site and then load or store at its place, inline (`interp_rt.rs`: the states and the transitions). Everything else about a
//! miss is in the realm, out of line.
//!
//! **Not here:** the generators and async functions, `with` and direct `eval`, regexp and BigInt (a literal throws a `TypeError`),
//! the iterator protocol (an array and a string are iterated, anything else throws; `for-in` walks the own keys), `Symbol`, `Map`, `Set`,
//! `JSON`, `Date` and `Promise`. The core built-ins (S2.7: `builtins.rs`) and `ToPrimitive` are here. The classes, `super`, private names, accessors, spread and rest, `arguments` and `new.target` are here
//! (`interp_ops.rs` has the runtime side).
//!
//! `unsafe` is in five places, each with a `SAFETY` comment. (1) `r!`: a register is read or written with no bounds check. (2) `word!`:
//! an instruction word is read with no bounds check. (3) The `Op` of the opcode byte is a `transmute`, so the `match` is one jump table
//! and no second table lookup. (1) to (3) rest on `verify` (registers below `nregs`, a pc that stays in the code, opcodes that exist)
//! and on the stack size that a call makes. (4) The cache hit and the element read turn a `Value` into a cell (`Obj::from_value_shape`,
//! `read_loc`, `write_loc`, `set_shape_id`, `Obj::from_value`) and (5) the helpers of `interp_rt.rs` do the same: they rest on the
//! invariant that every pointer in a register or a cell is live (the roots above keep it). The gains of (1) to (3) are measured with the
//! safe form of each, in `docs/progress.md` (S2.6): (1) and (2) together save 8 to 18% of the instructions of a micro loop, (3) another 6 to 9%.
use crate::atoms::SharedAtoms;
use crate::builtins::Outcome;
use crate::bytecode::{Asm, CF_ARGS, CF_LINK, Code, ITER_REGS, Ins, Op, STR_EMPTY, STR_SLOT, decode};
use crate::gc::Marker;
use crate::interp_rt::{ErrKind, Feedback, Got, Key, Put, R, Realm};
use crate::object::{F_DERIVED, Obj};
use crate::roots::RootMaps;
use crate::shapes::{A_C, A_W};
use crate::value::Value;
use std::cmp::Ordering;
use std::sync::Arc;

/// Words of a frame record.
pub const REC: usize = 10;
const ENTRY: i32 = -1;
const CONSTRUCT: i32 = 1 << 24;
/// A construct call made by `super()`: the return runs the fields of the class that made the call.
const AFTER_SUPER: i32 = 1 << 25;
/// The pc of a frame that has no root map: the constructor of a base class, below its fields function.
const NOPC: u32 = u32::MAX;
/// The most words of the stack (about 4 MiB).
const MAX_STACK: usize = 1 << 19;

/// The functions of a script. A function cell holds an index of this table; `Code::funcs` holds indexes too.
#[derive(Debug, Default)]
pub struct Program {
    pub codes: Vec<Code>,
}

/// Why `Vm::call` stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    /// An uncaught exception.
    Throw(Value),
    /// The budget ran out at a back edge (`Vm::fuel`).
    Fuel,
}

/// A realm with the JS stack and the feedback of the code that it runs.
pub struct Vm {
    pub realm: Realm,
    pub stack: Vec<Value>,
    pub fbs: Vec<Feedback>,
    pub prog: Program,
    /// The code indexes of the default constructors (`DefaultCtor`): a base class and a derived class.
    pub dctor: [usize; 2],
    /// Back edges that may still run. At 0 the run stops with [`Exit::Fuel`].
    pub fuel: u64,
}

fn record(stack: &[Value], fp: usize, k: usize) -> Value {
    stack[fp - REC + k]
}

/// An integer word of a frame record. The loop wrote it as `Value::int`, so the low 32 bits are the number.
#[inline(always)]
fn int_at(stack: &[Value], fp: usize, k: usize) -> i32 {
    stack[fp - REC + k].bits() as u32 as i32
}

/// The target of `SwitchI` for the table `t` (`lo`, the default, the length and the targets: `bytecode.rs`) and the value `v`. An integer
/// and a double that is an integer value (`-0` is 0, `3.0` is 3) index the table; a NaN, a fraction, a value out of range and a
/// non-number get the default (`f as i32` saturates, so the round trip tells the first group from the others).
#[inline(always)]
fn switch_int(t: &[u32], v: Value) -> usize {
    let i = v.as_int().or_else(|| v.as_f64().filter(|f| *f == f64::from(*f as i32)).map(|f| f as i32));
    // `lo + len - 1` fits an `i32` (the verifier), so a value below `lo` wraps past `len`.
    let k = i.map_or(usize::MAX, |i| i.wrapping_sub(t[0] as i32) as u32 as usize);
    (if k < t[2] as usize { t[3 + k] } else { t[1] }) as usize
}

/// The target of `SwitchA` for the table `t` and the value `v`. Out of line: the string probe is not part of the hot loop of an
/// arithmetic or call path, and a function this size inside `run` costs those paths instructions.
#[inline(never)]
fn switch_str(heap: &mut crate::heap::Heap, code: &Code, t: &[u32], v: Value) -> usize {
    let mut to = t[0];
    if let Some(s) = Realm::str_of(v) {
        // The hash of an atom or of a string that was a key before is in its cell; a rope or a new string computes it.
        let h = match s.cached_hash() {
            0 => s.hash(heap).expect("memory"),
            h => h,
        };
        let mask = t[1] as usize;
        let mut p = h as usize & mask;
        // The verifier proved an empty slot, so the probe ends.
        while let [slot_hash, atom, target] = t[2 + STR_SLOT * p..][..STR_SLOT] {
            if atom == STR_EMPTY {
                break;
            }
            let case = code.atoms[atom as usize];
            if slot_hash == h && (case == s || crate::string::Str::equals(heap, s, case).expect("memory")) {
                to = target;
                break;
            }
            p = (p + 1) & mask;
        }
    }
    to as usize
}

/// Mark the frames, from the top (`ci`, `fp`, `pc` of its safepoint) down to the frame of the host.
fn walk_frames(m: &mut Marker, prog: &Program, stack: &[Value], mut ci: usize, mut fp: usize, mut pc: u32) {
    loop {
        let rec = fp - REC;
        m.values(&stack[rec..rec + 3]);
        m.values(&stack[rec + 8..rec + 10]);
        if pc != NOPC {
            let map = prog.codes[ci].roots.find(pc).expect("a root map for each safepoint");
            for r in map.slots() {
                m.value(stack[fp + r]);
            }
        }
        let caller = int_at(stack, fp, 5);
        if caller == ENTRY {
            return;
        }
        pc = int_at(stack, fp, 3) as u32;
        ci = caller as usize;
        fp = fp - REC - prog.codes[ci].nregs as usize;
    }
}

/// Find the handler for `e` thrown at `pc` of frame `(ci, fp)`, dropping frames that have none. On `Ok`, the frame, `pc` and the
/// handler register are set. `Err(e)` when the frame of the host has no handler either.
fn unwind(prog: &Program, stack: &mut [Value], ci: &mut usize, fp: &mut usize, pc: &mut usize, e: Value) -> Result<(), Value> {
    loop {
        if let Some(h) = prog.codes[*ci].handler_at(*pc as u32) {
            let rec = *fp - REC;
            let mut depth = stack[rec + 6].as_int().expect("depth");
            while depth > h.depth as i32 {
                let c = Realm::ctx(stack[rec + 2]).expect("a context to pop");
                stack[rec + 2] = c.get(0);
                depth -= 1;
            }
            stack[rec + 6] = Value::int(depth);
            stack[*fp + h.reg as usize] = e;
            *pc = h.target as usize;
            return Ok(());
        }
        let caller = int_at(stack, *fp, 5);
        if caller == ENTRY {
            return Err(e);
        }
        *pc = int_at(stack, *fp, 3) as usize;
        *ci = caller as usize;
        *fp -= REC + prog.codes[*ci].nregs as usize;
    }
}

impl Vm {
    pub fn new(mut prog: Program, shared: Arc<SharedAtoms>) -> Vm {
        let dctor = [prog.codes.len(), prog.codes.len() + 1];
        prog.codes.extend(default_ctors());
        scan_flags(&mut prog.codes);
        let fbs = prog.codes.iter().map(|_| Feedback::default()).collect();
        Vm { realm: Realm::new(shared), stack: Vec::new(), fbs, prog, dctor, fuel: u64::MAX }
    }

    /// A function cell for code `code` with no context.
    pub fn function(&mut self, code: usize) -> Value {
        self.realm.new_function(code, Value::UNDEFINED)
    }

    /// Call `f` with `this` and `args` from the host. The JS stack must be empty (no re-entry yet). The result is not rooted: use it
    /// before the next call into the engine.
    pub fn call(&mut self, f: Value, this: Value, args: &[Value]) -> Result<Value, Exit> {
        let Some((ci, ctx)) = self.realm.function(f) else {
            let e = self.realm.error(ErrKind::Type, "not a function");
            return Err(Exit::Throw(e));
        };
        let code = &self.prog.codes[ci];
        let fp = REC;
        let need = fp + code.nregs as usize + 8;
        if self.stack.len() < need {
            self.stack.resize(need.max(256), Value::UNDEFINED);
        }
        let args_obj = if code.flags & CF_ARGS != 0 {
            Obj::array(&mut self.realm.heap, &self.realm.shapes, self.realm.s_args, args).expect("memory").value()
        } else {
            Value::UNDEFINED
        };
        let rec = [f, this, ctx, Value::int(0), Value::int(0), Value::int(ENTRY), Value::int(0), Value::int(0), Value::UNDEFINED, args_obj];
        self.stack[..REC].copy_from_slice(&rec);
        for i in 0..code.nparams as usize {
            self.stack[fp + i] = args.get(i).copied().unwrap_or(Value::UNDEFINED);
        }
        self.run(ci, fp)
    }

    /// Run code `ci` (a function with no parameters and no context) as the main function.
    pub fn run_main(&mut self, ci: usize) -> Result<Value, Exit> {
        let f = self.function(ci);
        let g = self.realm.global.value();
        self.call(f, g, &[])
    }

    /// Collect now, as the gate does (for tests and the host between tasks: the stack must be empty or stopped at a safepoint).
    pub fn collect_idle(&mut self) {
        let Vm { realm, stack, fbs, prog, .. } = self;
        realm.collect(fbs, |_| {});
        let _ = (stack, prog);
    }

    #[allow(unused_assignments)]
    fn run(&mut self, mut ci: usize, mut fp: usize) -> Result<Value, Exit> {
        let Vm { realm, stack, fbs, prog, dctor, fuel } = self;
        let prog: &Program = prog;
        let mut code: &Code = &prog.codes[ci];
        let mut words: &[u32] = &code.words;
        let mut fb: &mut Feedback = &mut fbs[ci];
        if !fb.ready {
            fb.init(code);
        }
        let mut pc: usize = 0;
        let undef = Value::UNDEFINED;

        // Make the locals of the frame `(ci, fp)`.
        macro_rules! reload {
            () => {{
                code = &prog.codes[ci];
                words = &code.words;
                fb = &mut fbs[ci];
                if !fb.ready {
                    fb.init(code);
                }
            }};
        }
        // SAFETY (`r!` and `word!`): `verify` bounds every register by `nregs` and every pc by the code, and a call or `Vm::call` makes
        // the stack hold `fp + nregs` words (`need` in `call!`). `stack` is not resized inside an op that reads a register after it.
        macro_rules! r {
            ($i:expr) => {
                (*unsafe { &mut *stack.as_mut_ptr().add(fp + ($i as usize)) })
            };
        }
        macro_rules! word {
            ($i:expr) => {
                unsafe { *words.get_unchecked($i) }
            };
        }
        // Throw `e` at the current instruction.
        macro_rules! throw {
            ($e:expr) => {{
                let e: Value = $e;
                match unwind(prog, stack, &mut ci, &mut fp, &mut pc, e) {
                    Ok(()) => {
                        reload!();
                        continue;
                    }
                    Err(e) => return Err(Exit::Throw(e)),
                }
            }};
        }
        macro_rules! err {
            ($kind:expr, $($msg:tt)*) => {{
                let e = realm.error($kind, &format!($($msg)*));
                throw!(e)
            }};
        }
        macro_rules! tri {
            ($e:expr) => {
                match $e {
                    Ok(v) => v,
                    Err(e) => throw!(e),
                }
            };
        }
        // An op that allocates collects first, when the heap is past the limit.
        macro_rules! gate {
            () => {
                if realm.due() {
                    realm.cur = (ci, fp, pc as u32);
                    let (cur, st) = (realm.cur, &stack[..]);
                    realm.collect(&mut fbs[..], |m| walk_frames(m, prog, st, cur.0, cur.1, cur.2));
                    fb = &mut fbs[ci];
                }
            };
        }
        // The canonical atom of this realm for `code.atoms[ai]`.
        macro_rules! atom {
            ($ai:expr) => {{
                let ai = $ai as usize;
                match fb.names[ai] {
                    Some(s) => s,
                    None => {
                        let s = realm.atoms.adopt(code.atoms[ai]);
                        fb.names[ai] = Some(s);
                        s
                    }
                }
            }};
        }
        macro_rules! name {
            ($site:expr) => {
                atom!(code.sites[$site])
            };
        }
        // `dst = recv.name` at `site`: the cache hit, else the realm.
        macro_rules! get_field {
            ($dst:expr, $recv:expr, $site:expr, $next:expr) => {{
                let recv: Value = $recv;
                // SAFETY: the registers hold live values (see the module comment).
                if let Some((o, shape)) = unsafe { Obj::from_value_shape(recv) } {
                    let c = &fb.caches[$site];
                    if c.shape == shape {
                        // SAFETY: the entry was made by `Obj::loc` for an object of this shape id.
                        r!($dst) = unsafe { o.read_loc(c.loc) };
                        pc = $next;
                        continue;
                    }
                }
                gate!();
                let n = name!($site);
                match tri!(realm.get_field_slow(fb, $site, n, recv)) {
                    Got::Val(v) => {
                        r!($dst) = v;
                        pc = $next;
                    }
                    Got::Get(g) => call_get!(g, recv, $dst, $next),
                }
            }};
        }
        macro_rules! set_field {
            ($recv:expr, $val:expr, $site:expr, $next:expr) => {{
                let (recv, val): (Value, Value) = ($recv, $val);
                // SAFETY: the registers hold live values.
                if let Some((o, shape)) = unsafe { Obj::from_value_shape(recv) } {
                    let c = &fb.caches[$site];
                    if c.shape == shape {
                        // SAFETY: the entry was made by `Obj::loc` for an object of this shape id; an add entry has an inline slot.
                        unsafe {
                            o.write_loc(c.loc, val);
                            if c.to != 0 {
                                o.set_shape_id(c.to);
                            }
                        }
                        pc = $next;
                        continue;
                    }
                }
                gate!();
                let n = name!($site);
                match tri!(realm.set_field_slow(fb, $site, n, recv, val)) {
                    Put::Done => pc = $next,
                    Put::Set(f) => call_set!(f, recv, val, $next),
                }
            }};
        }
        // Binary arithmetic: two integers, two numbers, else the realm.
        macro_rules! arith {
            ($a:expr, $x:expr, $y:expr, $next:expr, |$i:ident, $j:ident| $int:expr, |$f:ident, $g:ident| $dbl:expr, $slow:expr) => {{
                let (x, y): (Value, Value) = ($x, $y);
                r!($a) = if let (Some($i), Some($j)) = (x.as_int(), y.as_int()) {
                    $int
                } else if let (Some($f), Some($g)) = (x.as_f64(), y.as_f64()) {
                    $dbl
                } else {
                    tri!(($slow)(&mut *realm, x, y))
                };
                pc = $next;
            }};
        }
        macro_rules! op_add {
            ($a:expr, $x:expr, $y:expr, $next:expr) => {
                arith!($a, $x, $y, $next, |i, j| i.checked_add(j).map_or_else(|| Value::number(i as f64 + j as f64), Value::int), |f, g| Value::number(f + g), Realm::add_slow)
            };
        }
        macro_rules! op_sub {
            ($a:expr, $x:expr, $y:expr, $next:expr) => {
                arith!($a, $x, $y, $next, |i, j| i.checked_sub(j).map_or_else(|| Value::number(i as f64 - j as f64), Value::int), |f, g| Value::number(f - g), |rt: &mut Realm, x, y| num2(rt, x, y, |p, q| p - q))
            };
        }
        macro_rules! op_mul {
            ($a:expr, $x:expr, $y:expr, $next:expr) => {
                arith!($a, $x, $y, $next, |i, j| imul(i, j), |f, g| Value::number(f * g), |rt: &mut Realm, x, y| num2(rt, x, y, |p, q| p * q))
            };
        }
        macro_rules! op_div {
            ($a:expr, $x:expr, $y:expr, $next:expr) => {
                arith!($a, $x, $y, $next, |i, j| Realm::num(i as f64 / j as f64), |f, g| Value::number(f / g), |rt: &mut Realm, x, y| num2(rt, x, y, |p, q| p / q))
            };
        }
        macro_rules! op_mod {
            ($a:expr, $x:expr, $y:expr, $next:expr) => {
                arith!($a, $x, $y, $next, |i, j| imod(i, j), |f, g| Value::number(f % g), |rt: &mut Realm, x, y| num2(rt, x, y, |p, q| p % q))
            };
        }
        macro_rules! op_exp {
            ($a:expr, $x:expr, $y:expr, $next:expr) => {
                arith!($a, $x, $y, $next, |i, j| Realm::num(pow(i as f64, j as f64)), |f, g| Value::number(pow(f, g)), |rt: &mut Realm, x, y| num2(rt, x, y, pow))
            };
        }
        macro_rules! bitop {
            ($a:expr, $x:expr, $y:expr, $next:expr, |$i:ident, $j:ident| $e:expr) => {{
                let (x, y): (Value, Value) = ($x, $y);
                let ($i, $j) = if let (Some(i), Some(j)) = (x.as_int(), y.as_int()) {
                    (i, j)
                } else {
                    let (p, q) = (tri!(realm.to_number(x)), tri!(realm.to_number(y)));
                    (Realm::to_int32(p), Realm::to_int32(q))
                };
                r!($a) = $e;
                pc = $next;
            }};
        }
        // The order of two values: `Some(ord)`, or `None` for a NaN.
        macro_rules! cmp {
            ($x:expr, $y:expr) => {{
                let (x, y): (Value, Value) = ($x, $y);
                if let (Some(i), Some(j)) = (x.as_int(), y.as_int()) {
                    Some(i.cmp(&j))
                } else if let (Some(f), Some(g)) = (x.as_f64(), y.as_f64()) {
                    f.partial_cmp(&g)
                } else {
                    tri!(realm.compare(x, y))
                }
            }};
        }
        macro_rules! jump {
            ($cond:expr, $next:expr, $off:expr) => {{
                pc = if $cond { ($next as isize + $off as isize) as usize } else { $next };
            }};
        }
        macro_rules! seq {
            ($x:expr, $y:expr) => {{
                let (x, y): (Value, Value) = ($x, $y);
                if let (Some(i), Some(j)) = (x.as_int(), y.as_int()) { i == j } else { realm.strict_eq(x, y) }
            }};
        }
        macro_rules! ctx_hops {
            (0) => {
                Realm::ctx(stack[fp - REC + 2]).expect("a context")
            };
            ($hops:expr) => {{
                let mut c = Realm::ctx(stack[fp - REC + 2]).expect("a context");
                for _ in 0..$hops {
                    c = Realm::ctx(c.get(0)).expect("a parent context");
                }
                c
            }};
        }
        macro_rules! tdz {
            ($v:expr, $ai:expr) => {
                if $v.is_hole() {
                    let n = realm.text(code.atoms[$ai as usize]);
                    err!(ErrKind::Reference, "cannot access '{}' before initialization", n)
                }
            };
        }
        // Make sure that the stack has `$need` words.
        macro_rules! ensure {
            ($need:expr) => {{
                let need: usize = $need;
                if need > stack.len() {
                    if need > MAX_STACK {
                        err!(ErrKind::Range, "maximum call stack size exceeded")
                    }
                    let grown = need.max(2 * stack.len()).min(MAX_STACK + 8);
                    stack.resize(grown, undef);
                }
            }};
        }
        // Push the frame of code `$ni` on top of the current one and make it the current frame (the caller sets `pc`). The `$argc`
        // arguments are at the stack index `$ab` (the registers of the caller, or values staged at the new frame). `$dstw`: the register
        // of the caller that gets the result, and the flags. `$callpc`: the key of the caller's root map. `$next`: where the caller goes on.
        macro_rules! enter {
            ($callee:expr, $ni:expr, $cctx:expr, $this:expr, $nt:expr, $dstw:expr, $callpc:expr, $next:expr, $ab:expr, $argc:expr) => {{
                let ncode = &prog.codes[$ni];
                let new_fp = fp + code.nregs as usize + REC;
                ensure!(new_fp + ncode.nregs as usize + 8);
                let (np, argc, ab) = (ncode.nparams as usize, $argc as usize, $ab as usize);
                let argsw = if ncode.flags & CF_ARGS != 0 {
                    Obj::array(&mut realm.heap, &realm.shapes, realm.s_args, &stack[ab..ab + argc]).expect("memory").value()
                } else {
                    undef
                };
                let given = np.min(argc);
                let (callee, this, nt): (Value, Value, Value) = ($callee, $this, $nt);
                let rec = &mut stack[new_fp - REC..new_fp];
                rec.copy_from_slice(&[callee, this, $cctx, Value::int($callpc as i32), Value::int($next as i32), Value::int(ci as i32), Value::int(0), Value::int($dstw as i32), nt, argsw]);
                stack.copy_within(ab..ab + given, new_fp);
                stack[new_fp + given..new_fp + np].fill(undef);
                ci = $ni;
                fp = new_fp;
                reload!();
            }};
        }
        macro_rules! call_err {
            ($callee:expr) => {{
                if realm.function_new($callee).is_some() {
                    err!(ErrKind::Type, "class constructor cannot be invoked without 'new'")
                } else {
                    err!(ErrKind::Type, "not a function")
                }
            }};
        }
        // Call the native function `$callee` (`builtins.rs`) with the `$argc` arguments at the stack index `$ab`; the result goes to
        // register `$dst`. A native may ask for a call (`Outcome::Enter`), which is a frame as for any call. Ends with `continue`.
        macro_rules! native_call {
            ($callee:expr, $this:expr, $ab:expr, $argc:expr, $dst:expr, $next:expr) => {{
                gate!();
                let (callee, this): (Value, Value) = ($callee, $this);
                let (ab, argc) = ($ab as usize, $argc as usize);
                match tri!(realm.call_native(callee, this, &stack[ab..ab + argc])) {
                    Outcome::Val(v) => {
                        r!($dst) = v;
                        pc = $next;
                    }
                    Outcome::Enter { f, this } => {
                        let Some((ni, cctx)) = realm.function(f) else { call_err!(f) };
                        let len = realm.tail_args.len();
                        let nf = fp + code.nregs as usize + REC;
                        ensure!(nf + len + 8);
                        stack[nf..nf + len].copy_from_slice(&realm.tail_args);
                        enter!(f, ni, cctx, this, undef, $dst as i32, pc, $next, nf, len);
                        pc = 0;
                    }
                }
                continue;
            }};
        }
        // Call the helper `$h` of the prelude with the arguments in `realm.tail_args`; the result goes to register `$dst`.
        macro_rules! enter_helper {
            ($h:expr, $dst:expr, $next:expr) => {{
                let f = realm.helpers[$h];
                let Some((ni, cctx)) = realm.function(f) else { call_err!(f) };
                let len = realm.tail_args.len();
                let nf = fp + code.nregs as usize + REC;
                ensure!(nf + len + 8);
                stack[nf..nf + len].copy_from_slice(&realm.tail_args);
                enter!(f, ni, cctx, undef, undef, $dst as i32, pc, $next, nf, len);
                pc = 0;
                continue;
            }};
        }
        // Call with a frame record. `$argbase`: the first argument register. `$this`: the value of `this`.
        macro_rules! call {
            ($a:expr, $b:expr, $argbase:expr, $argc:expr, $this:expr, $next:expr) => {{
                let callee: Value = r!($b);
                let Some((ni, cctx)) = realm.function(callee) else {
                    if realm.native_id(callee).is_some() {
                        native_call!(callee, $this, fp + $argbase as usize, $argc, $a, $next)
                    }
                    call_err!(callee)
                };
                enter!(callee, ni, cctx, $this, undef, $a as i32, pc, $next, fp + $argbase as usize, $argc);
                pc = 0;
                continue;
            }};
        }
        // A construct call. `this` comes from `$nt.prototype`, unless the callee is a derived class. A base class runs its fields first.
        macro_rules! construct {
            ($callee:expr, $ni:expr, $cctx:expr, $nt:expr, $dst:expr, $next:expr, $ab:expr, $argc:expr, $flag:expr) => {{
                let callee: Value = $callee;
                let derived = Realm::obj(callee).is_some_and(|f| f.has_flag(F_DERIVED));
                let this = if derived { undef } else { tri!(realm.new_this($nt)) };
                enter!(callee, $ni, $cctx, this, $nt, ($dst as i32) | CONSTRUCT | $flag, pc, $next, $ab, $argc);
                if !derived && let Some((fi, fctx, ff)) = realm.fields_of(callee) {
                    enter!(ff, fi, fctx, this, undef, code.nregs as i32, NOPC as usize, 0usize, 0usize, 0usize);
                }
                pc = 0;
                continue;
            }};
        }
        // Run the getter `$g` with `this` and go on at `$next`; its result goes to register `$dst`.
        macro_rules! call_get {
            ($g:expr, $this:expr, $dst:expr, $next:expr) => {{
                let g: Value = $g;
                let this: Value = $this;
                let Some((ni, cctx)) = realm.function(g) else { err!(ErrKind::Type, "a getter is not a function") };
                enter!(g, ni, cctx, this, undef, $dst as i32, pc, $next, fp, 0usize);
                pc = 0;
                continue;
            }};
        }
        // Run the setter `$f` with `this` and the value `$v`; the result goes to the dead word past the registers.
        macro_rules! call_set {
            ($f:expr, $this:expr, $v:expr, $next:expr) => {{
                let (f, this, v): (Value, Value, Value) = ($f, $this, $v);
                let Some((ni, cctx)) = realm.function(f) else { err!(ErrKind::Type, "a setter is not a function") };
                let nf = fp + code.nregs as usize + REC;
                ensure!(nf + 8);
                stack[nf] = v;
                enter!(f, ni, cctx, this, undef, code.nregs as i32, pc, $next, nf, 1usize);
                pc = 0;
                continue;
            }};
        }
        // The arguments of `CallSpread` and the like: the elements of the array `$arr`, staged where the new frame goes.
        macro_rules! stage_array {
            ($arr:expr) => {{
                let arr = Realm::obj($arr).expect("an array");
                let len = arr.len() as usize;
                let nf = fp + code.nregs as usize + REC;
                ensure!(nf + len + 8);
                for i in 0..len {
                    stack[nf + i] = arr.get_element(i as u32).unwrap_or(undef);
                }
                (nf, len)
            }};
        }
        // The home object of the running function (`super.x` starts at its prototype).
        macro_rules! home {
            () => {
                Realm::obj(record(stack, fp, 0)).map_or(undef, |f| f.slot(&realm.shapes, 2))
            };
        }
        macro_rules! ret {
            ($v:expr) => {{
                let mut v: Value = $v;
                let caller = int_at(stack, fp, 5);
                let dst = int_at(stack, fp, 7);
                if dst & CONSTRUCT != 0 && Realm::obj(v).is_none() {
                    v = record(stack, fp, 1);
                }
                if caller == ENTRY {
                    return Ok(v);
                }
                pc = int_at(stack, fp, 4) as usize;
                let callpc = int_at(stack, fp, 3);
                ci = caller as usize;
                fp -= REC + prog.codes[ci].nregs as usize;
                stack[fp + (dst & 0xffff) as usize] = v;
                reload!();
                if dst & AFTER_SUPER != 0
                    && let Some((class, _)) = super_info(realm, stack, fp)
                    && let Some((fi, fctx, ff)) = realm.fields_of(class)
                {
                    // The fields of the class that called `super()`: a frame on top of the caller, then the caller goes on.
                    enter!(ff, fi, fctx, v, undef, code.nregs as i32, callpc as u32 as usize, pc, 0usize, 0usize);
                    pc = 0;
                }
                continue;
            }};
        }

        // One instruction. `$a`, `$b`, `$c`: the fields; `$bx`: the 16 (wide: 32) bits of `Abx`; `$sbx`: the same signed; `$sb`, `$sc`:
        // `b`, `c` signed; `$sj`: the offset of `Sj`; `$x`: the extra word; `$n`: the words before `x`.
        macro_rules! step {
            ($op:expr, $a:expr, $b:expr, $c:expr, $bx:expr, $sbx:expr, $sb:expr, $sc:expr, $sj:expr, $x:expr, $n:expr) => {{
                // The fields are macros, not variables: each arm decodes only what it uses (a variable would be decoded before the
                // jump to the arm, for every op).
                macro_rules! a { () => { ($a) } }
                macro_rules! b { () => { ($b) } }
                macro_rules! c { () => { ($c) } }
                macro_rules! bx { () => { ($bx) } }
                macro_rules! sbx { () => { ($sbx) } }
                macro_rules! sb { () => { ($sb) } }
                macro_rules! sc { () => { ($sc) } }
                macro_rules! sj { () => { ($sj) } }
                let next: usize = pc + $n;
                let nextx: usize = next + 1;
                let _ = nextx;
                // SAFETY: `verify` accepted the code, and it rejects a byte that is no opcode; the loop reaches only the first word of
                // an instruction (the start of the code, or a verified jump target, or the word after an instruction).
                let opv: Op = unsafe { std::mem::transmute::<u8, Op>($op) };
                match opv {
                    Op::Nop => pc = next,
                    Op::Wide => unreachable!("a!() Wide prefix is decoded before the dispatch"),
                    Op::Mov => {
                        r!(a!()) = r!(b!());
                        pc = next;
                    }
                    Op::LoadUndef => {
                        r!(a!()) = undef;
                        pc = next;
                    }
                    Op::LoadNull => {
                        r!(a!()) = Value::NULL;
                        pc = next;
                    }
                    Op::LoadTrue => {
                        r!(a!()) = Value::TRUE;
                        pc = next;
                    }
                    Op::LoadFalse => {
                        r!(a!()) = Value::FALSE;
                        pc = next;
                    }
                    Op::LoadThis => {
                        r!(a!()) = record(stack, fp, 1);
                        pc = next;
                    }
                    Op::LoadHole => {
                        r!(a!()) = Value::HOLE;
                        pc = next;
                    }
                    Op::LoadI => {
                        r!(a!()) = Value::int(sbx!());
                        pc = next;
                    }
                    Op::LoadN => {
                        r!(a!()) = Realm::num(code.numbers[bx!()]);
                        pc = next;
                    }
                    Op::LoadAtom => {
                        r!(a!()) = code.atoms[bx!()].value();
                        pc = next;
                    }
                    Op::CheckInit => {
                        tdz!(r!(a!()), bx!());
                        pc = next;
                    }
                    Op::GetCtx => {
                        let v = ctx_hops!(b!()).get(1 + c!());
                        r!(a!()) = v;
                        pc = next;
                    }
                    Op::GetCtxTdz => {
                        let v = ctx_hops!(b!()).get(1 + c!());
                        tdz!(v, $x);
                        r!(a!()) = v;
                        pc = nextx;
                    }
                    Op::SetCtx => {
                        ctx_hops!(b!()).set(1 + c!(), r!(a!()));
                        pc = next;
                    }
                    Op::SetCtxTdz => {
                        let cx = ctx_hops!(b!());
                        tdz!(cx.get(1 + c!()), $x);
                        cx.set(1 + c!(), r!(a!()));
                        pc = nextx;
                    }
                    Op::InitCtx => {
                        ctx_hops!(b!()).set(1 + c!(), r!(a!()));
                        pc = next;
                    }
                    Op::GetCtxField => {
                        let v = ctx_hops!(b!()).get(1 + c!());
                        get_field!(a!(), v, $x as usize, nextx);
                    }
                    Op::PushCtx => {
                        gate!();
                        let parent = stack[fp - REC + 2];
                        let cx = crate::object::Ctx::new(&mut realm.heap, bx!() + 1, Value::HOLE).expect("memory");
                        cx.set(0, parent);
                        stack[fp - REC + 2] = cx.value();
                        let d = int_at(stack, fp, 6);
                        stack[fp - REC + 6] = Value::int(d + 1);
                        pc = next;
                    }
                    Op::PopCtx => {
                        let parent = ctx_hops!(0).get(0);
                        stack[fp - REC + 2] = parent;
                        let d = int_at(stack, fp, 6);
                        stack[fp - REC + 6] = Value::int(d - 1);
                        pc = next;
                    }
                    Op::CopyCtx => {
                        gate!();
                        let cx = ctx_hops!(0).copy(&mut realm.heap).expect("memory");
                        stack[fp - REC + 2] = cx.value();
                        pc = next;
                    }
                    Op::GetGlobal | Op::GetName => {
                        let g = realm.global.value();
                        if let Some((o, shape)) = unsafe { Obj::from_value_shape(g) } {
                            let cc = &fb.caches[bx!()];
                            if cc.shape == shape {
                                // SAFETY: as in `get_field`.
                                r!(a!()) = unsafe { o.read_loc(cc.loc) };
                                pc = next;
                                continue;
                            }
                        }
                        gate!();
                        let n = name!(bx!());
                        let v = tri!(realm.get_global_slow(fb, bx!(), n));
                        r!(a!()) = v;
                        pc = next;
                    }
                    Op::SetGlobal | Op::SetName => {
                        let g = realm.global.value();
                        set_field!(g, r!(a!()), bx!(), next);
                    }
                    Op::TypeofGlobal | Op::TypeofName => {
                        gate!();
                        let n = name!(bx!());
                        let g = realm.global.value();
                        let v = tri!(realm.get_prop(g, Key::Name(n)));
                        r!(a!()) = realm.typeof_name(v);
                        pc = next;
                    }
                    Op::GetField => get_field!(a!(), r!(b!()), c!(), next),
                    Op::SetField => set_field!(r!(a!()), r!(b!()), c!(), next),
                    Op::SetFieldK => {
                        let v = [undef, Value::NULL, Value::FALSE, Value::TRUE][b!() & 3];
                        set_field!(r!(a!()), v, c!(), next)
                    }
                    Op::SetFieldI => set_field!(r!(a!()), Value::int(sb!()), c!(), next),
                    Op::SetFieldA => set_field!(r!(a!()), code.atoms[b!()].value(), c!(), next),
                    Op::DefField => {
                        gate!();
                        let n = name!(c!());
                        let o = Realm::obj(r!(a!())).expect("an object literal");
                        tri!(realm.define(o, Key::Name(n), r!(b!())));
                        pc = next;
                    }
                    Op::GetElem | Op::GetElemI => {
                        let key = if opv == Op::GetElemI { Value::int(c!() as i32) } else { r!(c!()) };
                        let recv = r!(b!());
                        // SAFETY: the registers hold live values.
                        if let (Some(o), Some(i)) = (unsafe { Obj::from_value(recv) }, key.as_int())
                            && i >= 0
                            && o.is_indexed()
                            && let Some(v) = o.get_element(i as u32)
                        {
                            r!(a!()) = v;
                            pc = next;
                            continue;
                        }
                        gate!();
                        let k = tri!(realm.key_of(key));
                        match tri!(realm.get_prop_g(recv, k)) {
                            Got::Val(v) => {
                                r!(a!()) = v;
                                pc = next;
                            }
                            Got::Get(g) => call_get!(g, recv, a!(), next),
                        }
                    }
                    Op::SetElem => {
                        gate!();
                        let (recv, key, v) = (r!(a!()), r!(b!()), r!(c!()));
                        let k = tri!(realm.key_of(key));
                        match tri!(realm.set_prop_g(recv, k, v)) {
                            Put::Done => pc = next,
                            Put::Set(f) => call_set!(f, recv, v, next),
                        }
                    }
                    Op::ToKey => {
                        gate!();
                        let recv = r!(b!());
                        if recv.is_undefined() || recv.is_null() {
                            err!(ErrKind::Type, "cannot read properties of undefined or null")
                        }
                        r!(a!()) = match tri!(realm.key_of(r!(c!()))) {
                            Key::Idx(i) => Realm::num(i as f64),
                            Key::Name(s) => s.value(),
                        };
                        pc = next;
                    }
                    Op::DefElem => {
                        gate!();
                        let o = Realm::obj(r!(a!())).expect("an object literal");
                        let k = tri!(realm.key_of(r!(b!())));
                        tri!(realm.define(o, k, r!(c!())));
                        pc = next;
                    }
                    Op::Delete => {
                        let k = tri!(realm.key_of(r!(c!())));
                        let ok = tri!(realm.delete_prop(r!(b!()), k));
                        r!(a!()) = Value::bool(ok);
                        pc = next;
                    }
                    Op::NewObject => {
                        gate!();
                        r!(a!()) = Obj::new(&mut realm.heap, &realm.shapes, realm.s_object).expect("memory").value();
                        pc = next;
                    }
                    Op::NewObjLit => {
                        gate!();
                        let id = match fb.lits[c!()] {
                            Some(id) => id,
                            None => {
                                let keys: Vec<_> = code.recipes.get(c!()).expect("a!() recipe").iter().map(|&ai| atom!(ai)).collect();
                                let id = realm.shapes.literal(crate::cell::Layout::Object, Some(realm.object_proto.cell()), &keys).expect("a!() literal shape");
                                fb.lits[c!()] = Some(id);
                                id
                            }
                        };
                        let o = Obj::new(&mut realm.heap, &realm.shapes, id).expect("memory");
                        for i in 0..code.recipes.get(c!()).map_or(0, <[u32]>::len) {
                            o.set_slot(&realm.shapes, i as u32, r!(b!() + i));
                        }
                        r!(a!()) = o.value();
                        pc = next;
                    }
                    Op::NewArray => {
                        gate!();
                        r!(a!()) = Obj::array(&mut realm.heap, &realm.shapes, realm.s_array, &[]).expect("memory").value();
                        pc = next;
                    }
                    Op::NewArrLit => {
                        gate!();
                        let vals = &stack[fp + b!()..fp + b!() + c!()];
                        let o = Obj::array(&mut realm.heap, &realm.shapes, realm.s_array, vals).expect("memory");
                        r!(a!()) = o.value();
                        pc = next;
                    }
                    Op::ArrayPush => {
                        gate!();
                        let o = Realm::obj(r!(a!())).expect("an array");
                        o.set_element(&mut realm.heap, o.len(), r!(b!())).expect("memory");
                        pc = next;
                    }
                    Op::Closure => {
                        gate!();
                        let ctx = stack[fp - REC + 2];
                        let child = code.funcs[bx!()] as usize;
                        let f = realm.new_function(child, ctx);
                        if prog.codes[child].flags & CF_LINK != 0 {
                            // The function uses `super` (or holds one that does): it gets the home object and the constructor link.
                            let (cur, nt) = (record(stack, fp, 0), record(stack, fp, 8));
                            let (co, fo) = (Realm::obj(cur).expect("a function"), Realm::obj(f).expect("a function"));
                            fo.set_slot(&realm.shapes, 2, co.slot(&realm.shapes, 2));
                            let link = if nt.is_undefined() {
                                co.slot(&realm.shapes, 4)
                            } else {
                                Obj::array(&mut realm.heap, &realm.shapes, realm.s_array, &[cur, nt]).expect("memory").value()
                            };
                            fo.set_slot(&realm.shapes, 4, link);
                        }
                        r!(a!()) = f;
                        pc = next;
                    }
                    Op::Call => call!(a!(), b!(), b!() + 2, c!(), r!(b!() + 1), next),
                    Op::CallU => call!(a!(), b!(), b!() + 1, c!(), undef, next),
                    Op::New => {
                        gate!();
                        let callee = r!(b!());
                        let Some((ni, cctx)) = realm.function_new(callee) else {
                            if realm.native_id(callee).is_some() {
                                let (ab, argc) = (fp + b!() + 1, c!());
                                r!(a!()) = tri!(realm.construct_native(callee, &stack[ab..ab + argc], callee));
                                pc = next;
                                continue;
                            }
                            err!(ErrKind::Type, "not a constructor")
                        };
                        construct!(callee, ni, cctx, callee, a!(), next, fp + b!() + 1, c!(), 0)
                    }
                    Op::Add | Op::AddI => {
                        let y = if opv == Op::AddI { Value::int(sc!()) } else { r!(c!()) };
                        op_add!(a!(), r!(b!()), y, next)
                    }
                    Op::Sub | Op::SubI => {
                        let y = if opv == Op::SubI { Value::int(sc!()) } else { r!(c!()) };
                        op_sub!(a!(), r!(b!()), y, next)
                    }
                    Op::Mul | Op::MulI => {
                        let y = if opv == Op::MulI { Value::int(sc!()) } else { r!(c!()) };
                        op_mul!(a!(), r!(b!()), y, next)
                    }
                    Op::Div | Op::DivI => {
                        let y = if opv == Op::DivI { Value::int(sc!()) } else { r!(c!()) };
                        op_div!(a!(), r!(b!()), y, next)
                    }
                    Op::Mod | Op::ModI => {
                        let y = if opv == Op::ModI { Value::int(sc!()) } else { r!(c!()) };
                        op_mod!(a!(), r!(b!()), y, next)
                    }
                    Op::Exp => op_exp!(a!(), r!(b!()), r!(c!()), next),
                    Op::BitAnd | Op::BitAndI => {
                        let y = if opv == Op::BitAndI { Value::int(sc!()) } else { r!(c!()) };
                        bitop!(a!(), r!(b!()), y, next, |i, j| Value::int(i & j))
                    }
                    Op::BitOr | Op::BitOrI => {
                        let y = if opv == Op::BitOrI { Value::int(sc!()) } else { r!(c!()) };
                        bitop!(a!(), r!(b!()), y, next, |i, j| Value::int(i | j))
                    }
                    Op::BitXor => bitop!(a!(), r!(b!()), r!(c!()), next, |i, j| Value::int(i ^ j)),
                    Op::Shl | Op::ShlI => {
                        let y = if opv == Op::ShlI { Value::int(sc!()) } else { r!(c!()) };
                        bitop!(a!(), r!(b!()), y, next, |i, j| Value::int(i << (j & 31)))
                    }
                    Op::Shr | Op::ShrI => {
                        let y = if opv == Op::ShrI { Value::int(sc!()) } else { r!(c!()) };
                        bitop!(a!(), r!(b!()), y, next, |i, j| Value::int(i >> (j & 31)))
                    }
                    Op::UShr | Op::UShrI => {
                        let y = if opv == Op::UShrI { Value::int(sc!()) } else { r!(c!()) };
                        bitop!(a!(), r!(b!()), y, next, |i, j| Realm::num(((i as u32) >> (j & 31)) as f64))
                    }
                    Op::Eq => {
                        let e = tri!(realm.loose_eq(r!(b!()), r!(c!())));
                        r!(a!()) = Value::bool(e);
                        pc = next;
                    }
                    Op::Ne => {
                        let e = tri!(realm.loose_eq(r!(b!()), r!(c!())));
                        r!(a!()) = Value::bool(!e);
                        pc = next;
                    }
                    Op::StrictEq => {
                        r!(a!()) = Value::bool(seq!(r!(b!()), r!(c!())));
                        pc = next;
                    }
                    Op::StrictNe => {
                        r!(a!()) = Value::bool(!seq!(r!(b!()), r!(c!())));
                        pc = next;
                    }
                    Op::Lt => {
                        r!(a!()) = Value::bool(cmp!(r!(b!()), r!(c!())) == Some(Ordering::Less));
                        pc = next;
                    }
                    Op::Le => {
                        r!(a!()) = Value::bool(matches!(cmp!(r!(b!()), r!(c!())), Some(Ordering::Less | Ordering::Equal)));
                        pc = next;
                    }
                    Op::Gt => {
                        r!(a!()) = Value::bool(cmp!(r!(b!()), r!(c!())) == Some(Ordering::Greater));
                        pc = next;
                    }
                    Op::Ge => {
                        r!(a!()) = Value::bool(matches!(cmp!(r!(b!()), r!(c!())), Some(Ordering::Greater | Ordering::Equal)));
                        pc = next;
                    }
                    Op::In => {
                        let k = tri!(realm.key_of(r!(b!())));
                        let v = tri!(realm.has_prop(r!(c!()), k));
                        r!(a!()) = Value::bool(v);
                        pc = next;
                    }
                    Op::InstanceOf => {
                        let v = tri!(realm.instance_of(r!(b!()), r!(c!())));
                        r!(a!()) = Value::bool(v);
                        pc = next;
                    }
                    Op::Neg => {
                        let x = r!(b!());
                        r!(a!()) = match x.as_int() {
                            Some(i) if i != 0 && i != i32::MIN => Value::int(-i),
                            _ => Value::number(-tri!(realm.to_number(x))),
                        };
                        pc = next;
                    }
                    Op::Plus | Op::ToNumeric => {
                        let x = r!(b!());
                        r!(a!()) = if x.is_number() { x } else { Realm::num(tri!(realm.to_number(x))) };
                        pc = next;
                    }
                    Op::Not => {
                        r!(a!()) = Value::bool(!Realm::truthy(r!(b!())));
                        pc = next;
                    }
                    Op::BitNot => {
                        let x = r!(b!());
                        let i = match x.as_int() {
                            Some(i) => i,
                            None => Realm::to_int32(tri!(realm.to_number(x))),
                        };
                        r!(a!()) = Value::int(!i);
                        pc = next;
                    }
                    Op::Typeof => {
                        r!(a!()) = realm.typeof_name(r!(b!()));
                        pc = next;
                    }
                    Op::Inc | Op::Dec => {
                        let x = r!(b!());
                        let x = if x.is_number() { x } else { Realm::num(tri!(realm.to_number(x))) };
                        let d = if opv == Op::Inc { 1 } else { -1 };
                        op_add!(a!(), x, Value::int(d), next)
                    }
                    Op::ToStr => {
                        gate!();
                        let s = tri!(realm.to_str(r!(b!())));
                        r!(a!()) = s.value();
                        pc = next;
                    }
                    Op::Concat => {
                        gate!();
                        let mut acc = tri!(realm.to_str(r!(b!())));
                        for i in 1..c!() {
                            let s = tri!(realm.to_str(r!(b!() + i)));
                            acc = crate::string::Str::concat(&mut realm.heap, acc, s).expect("memory");
                        }
                        r!(a!()) = acc.value();
                        pc = next;
                    }
                    Op::Jmp => jump!(true, next, sj!()),
                    Op::JmpBack => {
                        if *fuel == 0 {
                            return Err(Exit::Fuel);
                        }
                        *fuel -= 1;
                        jump!(true, next, sj!())
                    }
                    Op::JmpIf => jump!(Realm::truthy(r!(a!())), next, sbx!()),
                    Op::JmpIfNot => jump!(!Realm::truthy(r!(a!())), next, sbx!()),
                    Op::JmpNullish => jump!(r!(a!()).is_undefined() || r!(a!()).is_null(), next, sbx!()),
                    Op::JmpNotNullish => jump!(!(r!(a!()).is_undefined() || r!(a!()).is_null()), next, sbx!()),
                    Op::JLt => jump!(cmp!(r!(a!()), r!(b!())) == Some(Ordering::Less), nextx, $x as i32),
                    Op::JLe => jump!(matches!(cmp!(r!(a!()), r!(b!())), Some(Ordering::Less | Ordering::Equal)), nextx, $x as i32),
                    Op::JGt => jump!(cmp!(r!(a!()), r!(b!())) == Some(Ordering::Greater), nextx, $x as i32),
                    Op::JGe => jump!(matches!(cmp!(r!(a!()), r!(b!())), Some(Ordering::Greater | Ordering::Equal)), nextx, $x as i32),
                    Op::JnLt => jump!(cmp!(r!(a!()), r!(b!())) != Some(Ordering::Less), nextx, $x as i32),
                    Op::JnLe => jump!(!matches!(cmp!(r!(a!()), r!(b!())), Some(Ordering::Less | Ordering::Equal)), nextx, $x as i32),
                    Op::JnGt => jump!(cmp!(r!(a!()), r!(b!())) != Some(Ordering::Greater), nextx, $x as i32),
                    Op::JnGe => jump!(!matches!(cmp!(r!(a!()), r!(b!())), Some(Ordering::Greater | Ordering::Equal)), nextx, $x as i32),
                    Op::JEq => {
                        let e = tri!(realm.loose_eq(r!(a!()), r!(b!())));
                        jump!(e, nextx, $x as i32)
                    }
                    Op::JNe => {
                        let e = tri!(realm.loose_eq(r!(a!()), r!(b!())));
                        jump!(!e, nextx, $x as i32)
                    }
                    Op::JSEq => jump!(seq!(r!(a!()), r!(b!())), nextx, $x as i32),
                    Op::JSNe => jump!(!seq!(r!(a!()), r!(b!())), nextx, $x as i32),
                    Op::JLtI => jump!(cmp!(r!(a!()), Value::int($x as i32)) == Some(Ordering::Less), nextx, sbx!()),
                    Op::JLeI => jump!(matches!(cmp!(r!(a!()), Value::int($x as i32)), Some(Ordering::Less | Ordering::Equal)), nextx, sbx!()),
                    Op::JGtI => jump!(cmp!(r!(a!()), Value::int($x as i32)) == Some(Ordering::Greater), nextx, sbx!()),
                    Op::JGeI => jump!(matches!(cmp!(r!(a!()), Value::int($x as i32)), Some(Ordering::Greater | Ordering::Equal)), nextx, sbx!()),
                    Op::JnLtI => jump!(cmp!(r!(a!()), Value::int($x as i32)) != Some(Ordering::Less), nextx, sbx!()),
                    Op::JnLeI => jump!(!matches!(cmp!(r!(a!()), Value::int($x as i32)), Some(Ordering::Less | Ordering::Equal)), nextx, sbx!()),
                    Op::JnGtI => jump!(cmp!(r!(a!()), Value::int($x as i32)) != Some(Ordering::Greater), nextx, sbx!()),
                    Op::JnGeI => jump!(!matches!(cmp!(r!(a!()), Value::int($x as i32)), Some(Ordering::Greater | Ordering::Equal)), nextx, sbx!()),
                    Op::JEqI => {
                        let e = tri!(realm.loose_eq(r!(a!()), Value::int($x as i32)));
                        jump!(e, nextx, sbx!())
                    }
                    Op::JNeI => {
                        let e = tri!(realm.loose_eq(r!(a!()), Value::int($x as i32)));
                        jump!(!e, nextx, sbx!())
                    }
                    Op::JSEqI => jump!(seq!(r!(a!()), Value::int($x as i32)), nextx, sbx!()),
                    Op::JSNeI => jump!(!seq!(r!(a!()), Value::int($x as i32)), nextx, sbx!()),
                    Op::JSEqA => jump!(seq!(r!(a!()), code.atoms[$x as usize].value()), nextx, sbx!()),
                    Op::JSNeA => jump!(!seq!(r!(a!()), code.atoms[$x as usize].value()), nextx, sbx!()),
                    Op::JSEqB => jump!(r!(a!()) == Value::bool($x == 1), nextx, sbx!()),
                    Op::JSNeB => jump!(r!(a!()) != Value::bool($x == 1), nextx, sbx!()),
                    Op::JSEqNull => jump!(r!(a!()).is_null(), next, sbx!()),
                    Op::JSNeNull => jump!(!r!(a!()).is_null(), next, sbx!()),
                    Op::JSEqUndef => jump!(r!(a!()).is_undefined(), next, sbx!()),
                    Op::JSNeUndef => jump!(!r!(a!()).is_undefined(), next, sbx!()),
                    Op::SwitchI => pc = switch_int(&code.tables[bx!()..], r!(a!())),
                    Op::SwitchA => pc = switch_str(&mut realm.heap, code, &code.tables[bx!()..], r!(a!())),
                    Op::Ret => ret!(r!(a!())),
                    Op::RetUndef => ret!(undef),
                    Op::Throw => throw!(r!(a!())),
                    Op::ThrowConst => {
                        let n = realm.text(code.atoms[bx!()]);
                        err!(ErrKind::Type, "assignment to constant variable '{}'", n)
                    }
                    Op::IterOpen => {
                        let it = r!(b!());
                        let is_array = Realm::obj(it).is_some_and(|o| realm.shapes.get(o.shape(&realm.shapes)).layout == crate::cell::Layout::Indexed);
                        if !is_array {
                            err!(ErrKind::Type, "the value is not iterable (the iterator protocol is S2.7)")
                        }
                        r!(a!()) = it;
                        r!(a!() + 1) = Value::int(0);
                        r!(a!() + 2) = Value::int(0);
                        pc = next;
                    }
                    Op::ForInOpen => {
                        gate!();
                        let keys = tri!(realm.own_keys(r!(b!())));
                        r!(a!()) = keys;
                        r!(a!() + 1) = Value::int(0);
                        r!(a!() + 2) = Value::int(1);
                        pc = next;
                    }
                    Op::IterNext => {
                        let base = $x as usize;
                        let arr = Realm::obj(r!(base)).expect("an array");
                        let i = r!(base + 1).as_int().expect("an index");
                        if (i as u32) < arr.len() {
                            r!(a!()) = arr.get_element(i as u32).unwrap_or(undef);
                            r!(base + 1) = Value::int(i + 1);
                            pc = nextx;
                        } else {
                            jump!(true, nextx, sbx!())
                        }
                    }
                    Op::IterClose => pc = next,
                    Op::DeclGlobal => {
                        gate!();
                        let n = name!(bx!());
                        let g = realm.global;
                        // An own property of the global object: an inherited one (`__proto__`) does not count.
                        if realm.get_own(g, Key::Name(n)).is_none() {
                            tri!(realm.define(g, Key::Name(n), undef));
                        }
                        pc = next;
                    }
                    Op::CreateArgs => {
                        r!(a!()) = record(stack, fp, 9);
                        pc = next;
                    }
                    Op::CreateRest => {
                        gate!();
                        let all = Realm::obj(record(stack, fp, 9)).expect("the arguments of a function with CF_ARGS");
                        let vals: Vec<Value> = (b!() as u32..all.len()).map(|i| all.get_element(i).unwrap_or(undef)).collect();
                        r!(a!()) = Obj::array(&mut realm.heap, &realm.shapes, realm.s_array, &vals).expect("memory").value();
                        pc = next;
                    }
                    Op::LoadCallee => {
                        r!(a!()) = record(stack, fp, 0);
                        pc = next;
                    }
                    Op::LoadNewTarget => {
                        r!(a!()) = record(stack, fp, 8);
                        pc = next;
                    }
                    Op::NewRegExp => err!(ErrKind::Type, "regular expression literals are not supported yet (js2 has no RegExp engine: S2.7)"),
                    Op::LoadBigInt => err!(ErrKind::Type, "BigInt literals are not supported yet (js2 has no BigInt: S2.7)"),
                    Op::GetTemplate => {
                        gate!();
                        r!(a!()) = match fb.tmpl.iter().find(|t| t.0 == c!() as u32) {
                            Some(t) => t.1,
                            None => {
                                let rec: Vec<u32> = code.recipes.get(b!()).expect("a recipe").to_vec();
                                let (mut cooked, mut raw) = (Vec::new(), Vec::new());
                                for p in rec.chunks(2) {
                                    cooked.push(if p[0] == u32::MAX { undef } else { atom!(p[0]).value() });
                                    raw.push(atom!(p[1]).value());
                                }
                                let t = realm.make_template(&cooked, &raw);
                                fb.tmpl.push((c!() as u32, t));
                                t
                            }
                        };
                        pc = next;
                    }
                    Op::ArraySpread => {
                        gate!();
                        let vals = tri!(realm.iter_values(r!(b!())));
                        let o = Realm::obj(r!(a!())).expect("an array");
                        for v in vals {
                            o.set_element(&mut realm.heap, o.len(), v).expect("memory");
                        }
                        pc = next;
                    }
                    Op::ArrayHole => {
                        gate!();
                        let o = Realm::obj(r!(a!())).expect("an array");
                        o.set_len(&mut realm.heap, o.len() + 1).expect("memory");
                        pc = next;
                    }
                    Op::CallSpread => {
                        let callee = r!(b!());
                        let Some((ni, cctx)) = realm.function(callee) else {
                            if realm.native_id(callee).is_some() {
                                gate!();
                                let (nf, len) = stage_array!(r!(b!() + 2));
                                native_call!(callee, r!(b!() + 1), nf, len, a!(), next)
                            }
                            call_err!(callee)
                        };
                        let (nf, len) = stage_array!(r!(b!() + 2));
                        enter!(callee, ni, cctx, r!(b!() + 1), undef, a!() as i32, pc, next, nf, len);
                        pc = 0;
                        continue;
                    }
                    Op::NewSpread => {
                        gate!();
                        let callee = r!(b!());
                        let Some((ni, cctx)) = realm.function_new(callee) else {
                            if realm.native_id(callee).is_some() {
                                let (nf, len) = stage_array!(r!(b!() + 1));
                                r!(a!()) = tri!(realm.construct_native(callee, &stack[nf..nf + len], callee));
                                pc = next;
                                continue;
                            }
                            err!(ErrKind::Type, "not a constructor")
                        };
                        let (nf, len) = stage_array!(r!(b!() + 1));
                        construct!(callee, ni, cctx, callee, a!(), next, nf, len, 0)
                    }
                    Op::CopyProps => {
                        gate!();
                        let o = Realm::obj(r!(a!())).expect("an object literal");
                        if tri!(realm.copy_props(o, r!(b!()))) {
                            pc = next;
                        } else {
                            // An accessor: the helper copies; it returns the object, which goes back to the register of the object.
                            enter_helper!(crate::bi_object::H_SPREAD, a!(), next);
                        }
                    }
                    Op::ObjRest => {
                        gate!();
                        let keys: Vec<Value> = (0..$x as usize).map(|i| r!(c!() + i)).collect();
                        match tri!(realm.obj_rest(r!(b!()), &keys)) {
                            Some(v) => {
                                r!(a!()) = v;
                                pc = nextx;
                            }
                            None => enter_helper!(crate::bi_object::H_REST, a!(), nextx),
                        }
                    }
                    Op::SetProto => {
                        gate!();
                        let (o, v) = (Realm::obj(r!(a!())).expect("an object literal"), r!(b!()));
                        if let Some(p) = Realm::obj(v) {
                            o.set_proto(&mut realm.shapes, Some(p.cell()));
                        } else if v.is_null() {
                            o.set_proto(&mut realm.shapes, None);
                        }
                        pc = next;
                    }
                    Op::DefAccessor => {
                        gate!();
                        let n = name!(c!());
                        let o = Realm::obj(r!(a!())).expect("an object");
                        tri!(realm.define_accessor(o, Key::Name(n), r!(b!()), $x & 1 != 0, $x & 2 == 0));
                        pc = nextx;
                    }
                    Op::DefAccessorElem => {
                        gate!();
                        let o = Realm::obj(r!(a!())).expect("an object");
                        let k = tri!(realm.key_of(r!(b!())));
                        tri!(realm.define_accessor(o, k, r!(c!()), $x & 1 != 0, $x & 2 == 0));
                        pc = nextx;
                    }
                    Op::DefMethod => {
                        gate!();
                        let n = name!(c!());
                        let o = Realm::obj(r!(a!())).expect("an object");
                        tri!(realm.define_attrs(o, Key::Name(n), A_W | A_C, r!(b!())));
                        pc = next;
                    }
                    Op::DefMethodElem => {
                        gate!();
                        let o = Realm::obj(r!(a!())).expect("an object");
                        let k = tri!(realm.key_of(r!(b!())));
                        tri!(realm.define_attrs(o, k, A_W | A_C, r!(c!())));
                        pc = next;
                    }
                    Op::SetHome => {
                        Realm::obj(r!(a!())).expect("a function").set_slot(&realm.shapes, 2, r!(b!()));
                        pc = next;
                    }
                    Op::MakeClass => {
                        gate!();
                        tri!(realm.make_class(r!(a!()), None));
                        pc = next;
                    }
                    Op::MakeDerived => {
                        gate!();
                        tri!(realm.make_class(r!(a!()), Some(r!(b!()))));
                        pc = next;
                    }
                    Op::DefaultCtor => {
                        gate!();
                        r!(a!()) = realm.new_function(dctor[b!() & 1], undef);
                        pc = next;
                    }
                    Op::SetFields => {
                        Realm::obj(r!(a!())).expect("a constructor").set_slot(&realm.shapes, 3, r!(b!()));
                        pc = next;
                    }
                    Op::NewPrivateName => {
                        gate!();
                        let d = atom!(bx!());
                        r!(a!()) = realm.new_private_name(d);
                        pc = next;
                    }
                    Op::GetPrivate => {
                        gate!();
                        match tri!(realm.private_get(r!(b!()), r!(c!()))) {
                            Got::Val(v) => {
                                r!(a!()) = v;
                                pc = next;
                            }
                            Got::Get(g) => call_get!(g, r!(b!()), a!(), next),
                        }
                    }
                    Op::SetPrivate => {
                        gate!();
                        match tri!(realm.private_set(r!(a!()), r!(b!()), r!(c!()))) {
                            Put::Done => pc = next,
                            Put::Set(f) => call_set!(f, r!(a!()), r!(c!()), next),
                        }
                    }
                    Op::HasPrivate => {
                        gate!();
                        let h = tri!(realm.private_has(r!(b!()), r!(c!())));
                        r!(a!()) = Value::bool(h);
                        pc = next;
                    }
                    Op::DefPrivate => {
                        gate!();
                        tri!(realm.private_def(r!(a!()), r!(b!()), A_W, r!(c!())));
                        pc = next;
                    }
                    Op::DefPrivateMethod => {
                        gate!();
                        tri!(realm.private_def(r!(a!()), r!(b!()), 0, r!(c!())));
                        pc = next;
                    }
                    Op::DefPrivateAccessor => {
                        gate!();
                        tri!(realm.private_def_accessor(r!(a!()), r!(b!()), r!(c!()), $x == 1));
                        pc = nextx;
                    }
                    Op::GetSuper => {
                        gate!();
                        let n = name!(c!());
                        match tri!(realm.super_get(home!(), Key::Name(n))) {
                            Got::Val(v) => {
                                r!(a!()) = v;
                                pc = next;
                            }
                            Got::Get(g) => call_get!(g, r!(b!()), a!(), next),
                        }
                    }
                    Op::GetSuperElem => {
                        gate!();
                        let k = tri!(realm.key_of(r!(c!())));
                        match tri!(realm.super_get(home!(), k)) {
                            Got::Val(v) => {
                                r!(a!()) = v;
                                pc = next;
                            }
                            Got::Get(g) => call_get!(g, r!(b!()), a!(), next),
                        }
                    }
                    Op::SetSuper => {
                        gate!();
                        let n = name!(c!());
                        match tri!(realm.super_set(home!(), r!(a!()), Key::Name(n), r!(b!()))) {
                            Put::Done => pc = next,
                            Put::Set(f) => call_set!(f, r!(a!()), r!(b!()), next),
                        }
                    }
                    Op::SetSuperElem => {
                        gate!();
                        let k = tri!(realm.key_of(r!(b!())));
                        match tri!(realm.super_set(home!(), r!(a!()), k, r!(c!()))) {
                            Put::Done => pc = next,
                            Put::Set(f) => call_set!(f, r!(a!()), r!(c!()), next),
                        }
                    }
                    Op::CallSuper | Op::CallSuperSpread => {
                        gate!();
                        let Some((class, nt)) = super_info(realm, stack, fp) else { err!(ErrKind::Reference, "'super' keyword unexpected here") };
                        let Some(pv) = Realm::obj(class).and_then(|f| realm.proto_of(f)).map(|p| p.value()) else {
                            err!(ErrKind::Type, "super constructor is not a constructor")
                        };
                        let Some((ni, cctx)) = realm.function_new(pv) else {
                            if realm.native_id(pv).is_some() {
                                // A class that extends a built-in: the built-in makes `this`, then the fields of the class run.
                                let (ab, argc) = if opv == Op::CallSuper { (fp + b!(), c!()) } else { stage_array!(r!(b!())) };
                                let v = tri!(realm.construct_native(pv, &stack[ab..ab + argc], nt));
                                r!(a!()) = v;
                                if let Some((fi, fctx, ff)) = realm.fields_of(class) {
                                    enter!(ff, fi, fctx, v, undef, code.nregs as i32, pc, next, 0usize, 0usize);
                                    pc = 0;
                                } else {
                                    pc = next;
                                }
                                continue;
                            }
                            err!(ErrKind::Type, "super constructor is not a constructor")
                        };
                        let (ab, argc) = if opv == Op::CallSuper { (fp + b!(), c!()) } else { stage_array!(r!(b!())) };
                        construct!(pv, ni, cctx, nt, a!(), next, ab, argc, AFTER_SUPER)
                    }
                    Op::RetDerived => {
                        let (v, t) = (r!(a!()), r!(b!()));
                        let res = if Realm::obj(v).is_some() {
                            v
                        } else if v.is_undefined() {
                            if t.is_hole() {
                                err!(ErrKind::Reference, "must call super constructor in derived class before returning from derived constructor")
                            }
                            t
                        } else {
                            err!(ErrKind::Type, "derived constructors may only return object or undefined")
                        };
                        ret!(res)
                    }
                }
            }};
        }
        let _ = ITER_REGS;
        loop {
            let w = word!(pc);
            let op = w as u8;
            if op != Op::Wide as u8 {
                step!(
                    op,
                    ((w >> 8) & 255) as usize,
                    ((w >> 16) & 255) as usize,
                    (w >> 24) as usize,
                    (w >> 16) as usize,
                    (w as i32) >> 16,
                    ((w as i32) << 8) >> 24,
                    (w as i32) >> 24,
                    (w as i32) >> 8,
                    word!(pc + 1),
                    1
                )
            } else {
                // `[Wide][op][a:16]`, then `[b:16][c:16]` (`Abc`) or one 32-bit word (the other forms); `x` follows (`bytecode.rs`).
                let op = (w >> 8) as u8;
                let a = (w >> 16) as usize;
                let w2 = word!(pc + 1);
                let (b, c) = ((w2 & 0xffff) as usize, (w2 >> 16) as usize);
                let (sb, sc) = (w2 as u16 as i16 as i32, (w2 >> 16) as u16 as i16 as i32);
                step!(op, a, b, c, w2 as usize, w2 as i32, sb, sc, w2 as i32, word!(pc + 2), 2)
            }
        }
    }
}

/// `new.target` and the class constructor of a frame that may call `super()`: the frame of the constructor itself, or an arrow
/// inside it (the arrow's function cell has the link that `Closure` made: the constructor and `new.target`).
fn super_info(realm: &Realm, stack: &[Value], fp: usize) -> Option<(Value, Value)> {
    let nt = record(stack, fp, 8);
    if !nt.is_undefined() {
        return Some((record(stack, fp, 0), nt));
    }
    let link = Realm::obj(Realm::obj(record(stack, fp, 0))?.slot(&realm.shapes, 4))?;
    Some((link.get_element(0)?, link.get_element(1)?))
}

/// The default constructors of a class: `constructor() {}` and `constructor(...args) { super(...args); }`.
fn default_ctors() -> [Code; 2] {
    let mut base = Code::new(0, 0);
    base.words = vec![Op::RetUndef as u32];
    let mut a = Asm::default();
    for ins in [Ins::new(Op::CreateRest, 0, 0, 0), Ins::new(Op::CallSuperSpread, 1, 0, 0), Ins::new(Op::LoadUndef, 2, 0, 0), Ins::new(Op::RetDerived, 2, 1, 0)] {
        a.emit(ins);
    }
    let asm = a.finish().expect("the default constructor assembles");
    let mut derived = Code::new(3, 1);
    derived.words = asm.words;
    derived.roots = RootMaps::new(3);
    derived.roots.add(asm.pcs[0], [0]);
    derived.roots.add(asm.pcs[1], [0]);
    derived.roots.add(asm.pcs[3], [1, 2]);
    [base, derived]
}

/// Fill `Code::flags` of each code: `CF_ARGS` for a code that reads all its arguments, `CF_LINK` for one that uses `super` or holds a
/// function that does.
fn scan_flags(codes: &mut [Code]) {
    for c in codes.iter_mut() {
        let (mut pc, mut f) = (0, 0);
        while let Ok((ins, n)) = decode(&c.words, pc) {
            match ins.op {
                Op::CreateArgs | Op::CreateRest => f |= CF_ARGS,
                Op::CallSuper | Op::CallSuperSpread | Op::GetSuper | Op::GetSuperElem | Op::SetSuper | Op::SetSuperElem => f |= CF_LINK,
                _ => {}
            }
            pc += n;
        }
        c.flags = f;
    }
    loop {
        let mut changed = false;
        for i in 0..codes.len() {
            if codes[i].flags & CF_LINK == 0 && codes[i].funcs.iter().any(|&k| codes.get(k as usize).is_some_and(|c| c.flags & CF_LINK != 0)) {
                codes[i].flags |= CF_LINK;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
}

/// `x * y` on integers: an overflow or a result of -0 is a double.
fn imul(i: i32, j: i32) -> Value {
    match i.checked_mul(j) {
        Some(0) if i < 0 || j < 0 => Value::number(-0.0),
        Some(v) => Value::int(v),
        None => Value::number(i as f64 * j as f64),
    }
}

/// `x % y` on integers: the sign of the result is the sign of `x`, so a zero remainder of a negative `x` is -0.
fn imod(i: i32, j: i32) -> Value {
    if j == 0 {
        Value::number(f64::NAN)
    } else if j == -1 {
        if i < 0 { Value::number(-0.0) } else { Value::int(0) }
    } else {
        match i % j {
            0 if i < 0 => Value::number(-0.0),
            r => Value::int(r),
        }
    }
}

/// `x ** y` with the rules of JavaScript where Rust's `powf` differs.
pub(crate) fn pow(x: f64, y: f64) -> f64 {
    if y.is_nan() || (x.abs() == 1.0 && y.is_infinite()) { f64::NAN } else { x.powf(y) }
}

/// A binary numeric op on two values that are not both numbers: `ToNumber` on each.
fn num2(rt: &mut Realm, x: Value, y: Value, f: fn(f64, f64) -> f64) -> R<Value> {
    let (p, q) = (rt.to_number(x)?, rt.to_number(y)?);
    Ok(Value::number(f(p, q)))
}

#[cfg(test)]
#[path = "interp_tests.rs"]
mod tests;
