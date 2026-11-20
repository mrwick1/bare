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
//! | `fp-8` | the function cell (a root; it keeps the context that the code closed over) |
//! | `fp-7` | `this` |
//! | `fp-6` | the current context: the closure's context at the entry, then `PushCtx`, `PopCtx` and `CopyCtx` change it |
//! | `fp-5` | the `pc` of the call in the caller (the key of its root map) |
//! | `fp-4` | the `pc` where the caller goes on |
//! | `fp-3` | the code index of the caller, or -1 for the frame that the host entered |
//! | `fp-2` | the number of contexts that this function pushed (the handlers need it) |
//! | `fp-1` | the register of the caller that gets the result, and bit 24 for a `New` |
//!
//! A call writes the record and copies the arguments to registers `0..nparams` (missing ones are `undefined`; extra ones are dropped,
//! because there is no `arguments` yet). It does **not** fill the other registers: the verifier proved that a register is written
//! before it is read (6.10 item 4). A return writes the result into the caller's register and drops the frame by moving `fp`.
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
//! **Not here:** the generators, `arguments`, accessors and `ToPrimitive`, the iterator protocol (an array is iterated, anything else
//! throws; `for-in` walks the own keys), and every built-in. They come with S2.7 and the compiler (S2.5).
//!
//! `unsafe` is in five places, each with a `SAFETY` comment. (1) `r!`: a register is read or written with no bounds check. (2) `word!`:
//! an instruction word is read with no bounds check. (3) The `Op` of the opcode byte is a `transmute`, so the `match` is one jump table
//! and no second table lookup. (1) to (3) rest on `verify` (registers below `nregs`, a pc that stays in the code, opcodes that exist)
//! and on the stack size that a call makes. (4) The cache hit and the element read turn a `Value` into a cell (`Obj::from_value_shape`,
//! `read_loc`, `write_loc`, `set_shape_id`, `Obj::from_value`) and (5) the helpers of `interp_rt.rs` do the same: they rest on the
//! invariant that every pointer in a register or a cell is live (the roots above keep it). The gains of (1) to (3) are measured with the
//! safe form of each, in `docs/progress.md` (S2.6): (1) and (2) together save 8 to 18% of the instructions of a micro loop, (3) another 6 to 9%.
use crate::atoms::SharedAtoms;
use crate::bytecode::{Code, ITER_REGS, Op};
use crate::gc::Marker;
use crate::interp_rt::{ErrKind, Feedback, Key, R, Realm};
use crate::object::Obj;
use crate::value::Value;
use std::cmp::Ordering;
use std::sync::Arc;

/// Words of a frame record.
pub const REC: usize = 8;
const ENTRY: i32 = -1;
const CONSTRUCT: i32 = 1 << 24;
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

/// Mark the frames, from the top (`ci`, `fp`, `pc` of its safepoint) down to the frame of the host.
fn walk_frames(m: &mut Marker, prog: &Program, stack: &[Value], mut ci: usize, mut fp: usize, mut pc: u32) {
    loop {
        let rec = fp - REC;
        m.values(&stack[rec..rec + 3]);
        let map = prog.codes[ci].roots.find(pc).expect("a root map for each safepoint");
        for r in map.slots() {
            m.value(stack[fp + r]);
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
    pub fn new(prog: Program, shared: Arc<SharedAtoms>) -> Vm {
        let fbs = prog.codes.iter().map(|_| Feedback::default()).collect();
        Vm { realm: Realm::new(shared), stack: Vec::new(), fbs, prog, fuel: u64::MAX }
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
        let rec = [f, this, ctx, Value::int(0), Value::int(0), Value::int(ENTRY), Value::int(0), Value::int(0)];
        self.stack[..REC].copy_from_slice(&rec);
        for i in 0..code.nparams as usize {
            self.stack[fp + i] = args.get(i).copied().unwrap_or(Value::UNDEFINED);
        }
        self.run(ci, fp)
    }

    /// Run code `ci` (a function with no parameters and no context) as the main function.
    pub fn run_main(&mut self, ci: usize) -> Result<Value, Exit> {
        let f = self.function(ci);
        self.call(f, Value::UNDEFINED, &[])
    }

    /// Collect now, as the gate does (for tests and the host between tasks: the stack must be empty or stopped at a safepoint).
    pub fn collect_idle(&mut self) {
        let Vm { realm, stack, fbs, prog, .. } = self;
        realm.collect(fbs, |_| {});
        let _ = (stack, prog);
    }

    #[allow(unused_assignments)]
    fn run(&mut self, mut ci: usize, mut fp: usize) -> Result<Value, Exit> {
        let Vm { realm, stack, fbs, prog, fuel } = self;
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
                let v = tri!(realm.get_field_slow(fb, $site, n, recv));
                r!($dst) = v;
                pc = $next;
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
                tri!(realm.set_field_slow(fb, $site, n, recv, val));
                pc = $next;
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
        // Call with a frame record. `$argbase`: the first argument register. `$this`: the value of `this`.
        macro_rules! call {
            ($a:expr, $b:expr, $argbase:expr, $argc:expr, $this:expr, $flag:expr, $next:expr) => {{
                let callee: Value = r!($b);
                let Some((ni, cctx)) = realm.function(callee) else { err!(ErrKind::Type, "not a function") };
                let ncode = &prog.codes[ni];
                let new_fp = fp + code.nregs as usize + REC;
                let need = new_fp + ncode.nregs as usize + 8;
                if need > stack.len() {
                    if need > MAX_STACK {
                        err!(ErrKind::Range, "maximum call stack size exceeded")
                    }
                    let grown = need.max(2 * stack.len()).min(MAX_STACK + 8);
                    stack.resize(grown, undef);
                }
                let (np, argc, ab) = (ncode.nparams as usize, $argc as usize, fp + $argbase as usize);
                let this: Value = $this;
                let rec = &mut stack[new_fp - REC..new_fp];
                rec.copy_from_slice(&[callee, this, cctx, Value::int(pc as i32), Value::int($next as i32), Value::int(ci as i32), Value::int(0), Value::int($a as i32 | $flag)]);
                let given = np.min(argc);
                stack.copy_within(ab..ab + given, new_fp);
                stack[new_fp + given..new_fp + np].fill(undef);
                ci = ni;
                fp = new_fp;
                reload!();
                pc = 0;
                continue;
            }};
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
                ci = caller as usize;
                fp -= REC + prog.codes[ci].nregs as usize;
                stack[fp + (dst & 0xffff) as usize] = v;
                reload!();
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
                        r!(a!()) = tri!(realm.get_prop(recv, k));
                        pc = next;
                    }
                    Op::SetElem => {
                        gate!();
                        let (recv, key, v) = (r!(a!()), r!(b!()), r!(c!()));
                        let k = tri!(realm.key_of(key));
                        tri!(realm.set_prop(recv, k, v));
                        pc = next;
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
                        r!(a!()) = realm.new_function(code.funcs[bx!()] as usize, ctx);
                        pc = next;
                    }
                    Op::Call => call!(a!(), b!(), b!() + 2, c!(), r!(b!() + 1), 0, next),
                    Op::CallU => call!(a!(), b!(), b!() + 1, c!(), undef, 0, next),
                    Op::New => {
                        gate!();
                        let this = tri!(realm.new_this(r!(b!())));
                        call!(a!(), b!(), b!() + 1, c!(), this, CONSTRUCT, next)
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
fn pow(x: f64, y: f64) -> f64 {
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
