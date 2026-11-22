//! The verifier of bytecode (plan step S2.4). It checks a [`Code`] before the interpreter (S2.6) may run it, so that the interpreter can
//! read operands with no checks. It is meant for debug builds, tests and a compiler that has a bug, not for every compile in a release
//! build: the data flow part keeps register sets for each instruction. It checks:
//! - every word decodes, and each instruction starts where the one before it ends;
//! - registers (and the windows of calls, literals and iterators) are below `nregs`, the special constants are 0 to 3, each index
//!   of a table (numbers, atoms, functions, recipes, sites) is in range, and a field that the op does not use is 0;
//! - recipes contain only valid keys, and template recipes are only used by templates;
//! - names, generator kinds and lazy stubs are consistent. [`verify_program`] also checks child indexes in the code table;
//! - each site has an atom, and exactly one instruction uses it;
//! - each jump target is the first word of an instruction; `JmpBack` goes back and every other jump goes forward;
//! - each switch table is inside the pool `Code::tables`, and all its targets are later instructions. A string table has a power of two
//!   slots, an empty slot (so a probe ends), atoms in range with the hash of their atom, and every filled slot is found by the probe from its hash;
//! - the loop table: headers are sorted and unique, each loop ends at a `JmpBack` that targets its header, every `JmpBack` is covered
//!   by the loop of its target, and loops are nested or apart;
//! - handlers: a valid range, an instruction as target, a register in range;
//! - source positions are sorted and inside the code;
//! - root maps: each safepoint instruction has a map, each map belongs to a safepoint instruction, and the map has the register count
//!   of the function;
//! - data flow on every path: no path runs past the last word; the context depth is the same on every path to an instruction and
//!   never below 0, and at least the depth of a handler in each instruction that can throw in its range; no instruction reads a register
//!   that some path has not written, and a root map names only registers that every path has written. Registers `0..nparams` are
//!   written at the entry, and the first matching handler writes its register. Unreachable instructions are not checked by the data flow.
//! - root-map coverage: the collector reads the map at the start of an allocating instruction and while its frame is paused at a call.
//!   The map covers every register read by the instruction or before its next write along normal, handler and resume paths.
//!   The verifier treats every live register as a possible heap value. A later map counts as a read so it cannot mark a stale pointer.
//! - suspension (`GenInit`, `Yield`, `RetGen` in a generator; `AsyncInit`, `Await`, `RetAsync` in an async function): only code with `gen_kind` that is a constructor-less function has them, and it has no `Ret`
//!   or `LoadNewTarget`. A save list is inside the pool, names registers that are written on every path to the suspension and that its
//!   root map names (a collection before the copy must not free them), and is no longer than `gen_regs`. After the suspension only the registers of
//!   the list, the sent value and the mode are written: the resume restores no other. An exception before suspension sees
//!   the original frame. A throw after resumption goes through the resume instructions.
use crate::bytecode::{CF_ARGS, CF_GEN, Code, GK_ASYNC, GK_ASYNCGEN, GK_GEN, GK_NONE, Handler, ITER_REGS, Ins, NO_NAME, NO_REG, Op, Recipes, STR_EMPTY, STR_SLOT, SaveList, YS_REGS, decode};
use std::ops::Range;

const UNREACHED: u32 = u32::MAX;

struct Access<'a> {
    reads: [u32; 4],
    read_count: usize,
    read_window: Range<u32>,
    writes: Range<u32>,
    target: Option<usize>,
    safepoint: bool,
    fallthrough: bool,
    handler: Option<&'a Handler>,
    save: Option<SaveList<'a>>,
}

impl Access<'_> {
    fn reads(&self) -> impl Iterator<Item = u32> + '_ {
        self.reads[..self.read_count].iter().copied()
    }
}

fn has(b: &[u64], r: u32) -> bool {
    b[r as usize / 64] >> (r % 64) & 1 != 0
}

fn put(b: &mut [u64], r: u32) {
    b[r as usize / 64] |= 1 << (r % 64);
}

/// The size of the window of registers that `ins` reads (`R`) or writes (`W`), if it has one.
fn window(ins: &Ins, recipes: &Recipes) -> Option<u32> {
    Some(match ins.op {
        Op::Call => 2 + ins.c,
        Op::CallU | Op::New => 1 + ins.c,
        Op::Concat | Op::NewArrLit | Op::CallSuper => ins.c,
        Op::CallSpread => 3,
        Op::NewSpread => 2,
        Op::ObjRest => ins.x,
        Op::NewObjLit => recipes.get(ins.c as usize).map_or(0, |k| k.len() as u32),
        Op::IterOpen | Op::YieldOpen | Op::ForInOpen | Op::ForInNext | Op::IterNext | Op::IterClose | Op::AIterOpen | Op::AIterNext | Op::AIterClose | Op::AIterCloseT => ITER_REGS,
        Op::YieldStarStep | Op::AYsCall => YS_REGS,
        Op::ForEachStep => 3,
        _ => return None,
    })
}

/// Check the code table after the loader has resolved child references.
pub fn verify_program(codes: &[Code]) -> Result<(), String> {
    for (ci, code) in codes.iter().enumerate() {
        if let Some(child) = code.funcs.iter().find(|&&child| child as usize >= codes.len()) {
            return Err(format!("function {ci}: no child function {child} in the code table"));
        }
        verify(code).map_err(|err| format!("function {ci}: {err}"))?;
    }
    Ok(())
}

/// Check `code`. Gives the first error. The function table and the lazy compiler's source data belong to the loader.
pub fn verify(code: &Code) -> Result<(), String> {
    let n = code.words.len();
    let nregs = code.nregs as u32;
    if n == 0 {
        return Err("no code".into());
    }
    if code.nparams > code.nregs {
        return Err("more parameters than registers".into());
    }
    if code.fn_name != NO_NAME && code.fn_name as usize >= code.atoms.len() {
        return Err("the function name has no atom".into());
    }
    if code.gen_kind > GK_ASYNCGEN || code.gen_regs > code.nregs || (code.gen_kind == GK_NONE && code.gen_regs != 0) {
        return Err("invalid suspension metadata".into());
    }
    if code.gen_kind != GK_NONE && !code.noctor {
        return Err("a generator that a script can construct".into());
    }
    if code.roots.regs() != nregs as usize {
        return Err("the root maps have another register count".into());
    }
    if let Ok((i, 1)) = decode(&code.words, 0)
        && i.op == Op::Lazy
    {
        return verify_lazy(code, i);
    }
    let mut ins: Vec<(usize, Ins, usize)> = Vec::new();
    let mut at = vec![usize::MAX; n];
    let mut pc = 0;
    while pc < n {
        let (i, len) = decode(&code.words, pc).map_err(|e| format!("{pc:04}: {e:?}"))?;
        at[pc] = ins.len();
        ins.push((pc, i, len));
        pc += len;
    }
    let start = |w: i64| usize::try_from(w).is_ok_and(|w| w < n && at[w] != usize::MAX);
    let natoms = code.atoms.len() as u32;
    if let Some((i, _)) = code.sites.iter().enumerate().find(|(_, a)| **a >= natoms) {
        return Err(format!("site {i} has no atom"));
    }
    // The recipe of a `GetTemplate` is pairs (cooked, raw); a cooked string that is `undefined` is `u32::MAX`.
    let mut recipe_kind = vec![0u8; code.recipes.len()];
    for &(_, i, _) in &ins {
        let (r, kind) = match i.op {
            Op::GetTemplate => (i.b, 1),
            Op::NewObjLit => (i.c, 2),
            _ => continue,
        };
        if let Some(entry) = recipe_kind.get_mut(r as usize) {
            *entry |= kind;
        }
    }
    let bad_recipe = |i: usize, k: &[u32]| {
        let template = recipe_kind[i] == 1;
        recipe_kind[i] == 3 || k.iter().enumerate().any(|(j, a)| *a >= natoms && !(template && j % 2 == 0 && *a == u32::MAX)) || (template && !k.len().is_multiple_of(2))
    };
    if (0..code.recipes.len()).any(|i| code.recipes.get(i).is_some_and(|k| bad_recipe(i, k))) {
        return Err("a recipe names no atom, has unmatched template entries or is used as both kinds".into());
    }
    let mut site_uses = vec![0u32; code.sites.len()];
    let mut access = Vec::with_capacity(ins.len());
    let words = (code.nregs as usize).div_ceil(64).max(1);
    let mut maps = vec![0u64; ins.len() * words];
    for (k, &(pc, i, len)) in ins.iter().enumerate() {
        let err = |what: &str| Err(format!("{pc:04} {}: {what}", i.op.name()));
        if i.op == Op::Lazy {
            return err("Lazy must be the only word of its stub");
        }
        let (gen_op, async_op) = (matches!(i.op, Op::GenInit | Op::Yield | Op::YieldResult | Op::RetGen), matches!(i.op, Op::AsyncInit | Op::Await | Op::RetAsync));
        // An async generator has the ops of both: `Await` for `await`, and the generator ops.
        let gk = code.gen_kind;
        let ok = if i.op == Op::Await { gk == GK_ASYNC || gk == GK_ASYNCGEN } else if gen_op { gk == GK_GEN || gk == GK_ASYNCGEN } else { gk == GK_ASYNC };
        if (gen_op || async_op) && !ok {
            return err("an op of generator or async code in a function that is not one");
        }
        if code.gen_kind != GK_NONE && matches!(i.op, Op::Ret | Op::RetUndef | Op::RetDerived | Op::LoadNewTarget) {
            return err("not allowed in generator code (a generator returns with RetGen or RetAsync, and its frame keeps the generator where new.target is)");
        }
        let safepoint = i.op.is_safepoint();
        if safepoint {
            let map = code.roots.find(pc as u32).ok_or_else(|| format!("{pc:04} {}: a safepoint with no root map", i.op.name()))?;
            if matches!(i.op, Op::AddKey | Op::ForEachStep) && [i.b, i.c].iter().any(|&r| !map.slots().any(|s| s == r as usize)) {
                return err("an operand is missing from the root map");
            }
            let bits = &mut maps[k * words..(k + 1) * words];
            map.slots().for_each(|r| put(bits, r as u32));
        }
        let mut a = Access { reads: [0; 4], read_count: 0, read_window: 0..0, writes: 0..0, target: None,
            safepoint, fallthrough: i.op.falls_through(), handler: handler(code, i, pc, safepoint), save: save_list(code, i) };
        for k in 0..i.op.form().fields() {
            let letter = i.op.letter(k);
            let v = i.field(k);
            let in_range = |count: usize| (v as usize) < count;
            match letter {
                'r' | 'w' => {
                    if v >= nregs {
                        return err("register out of range");
                    }
                    if letter == 'r' {
                        a.reads[a.read_count] = v;
                        a.read_count += 1;
                    } else {
                        a.writes = v..v + 1;
                    }
                }
                'R' | 'W' => {
                    let size = window(&i, &code.recipes).expect("the size of a register window");
                    if i.op == Op::Concat && size == 0 {
                        return err("Concat of no strings");
                    }
                    if v as u64 + size as u64 > nregs as u64 {
                        return err("window out of range");
                    }
                    if i.op == Op::ForEachStep && [i.b, i.c].iter().any(|r| (v..v + size).contains(r)) {
                        return err("the argument window overlaps a source register");
                    }
                    if letter == 'R' {
                        a.read_window = v..v + size;
                    } else {
                        a.writes = v..v + size;
                    }
                }
                'z' if v > 3 => return err("special constant above 3"),
                'N' if !in_range(code.numbers.len()) => return err("no such number"),
                'A' if !in_range(code.atoms.len()) => return err("no such atom"),
                'F' if !in_range(code.funcs.len()) => return err("no such function"),
                'H' if !in_range(code.recipes.len()) => return err("no such recipe"),
                'D' if !in_range(code.data.len()) => return err("no such data literal"),
                'T' => {
                    if let Err(why) = check_table(code, &i, pc, &start) {
                        return err(why);
                    }
                }
                'S' => match site_uses.get_mut(v as usize) {
                    Some(u) => *u += 1,
                    None => return err("no such site"),
                },
                'j' => {
                    let t = pc as i64 + len as i64 + v as i32 as i64;
                    if !start(t) {
                        return err("jump target is not an instruction");
                    }
                    if (i.op == Op::JmpBack) != (t <= pc as i64) {
                        return err(if i.op == Op::JmpBack { "JmpBack goes forward" } else { "a jump back that is not JmpBack" });
                    }
                    a.target = Some(at[t as usize]);
                }
                '-' if v != 0 => return err(&format!("field {k} is not used and is not 0")),
                _ => {}
            }
        }
        access.push(a);
    }
    if let Some(s) = site_uses.iter().position(|u| *u != 1) {
        return Err(format!("site {s} is used {} times, not once", site_uses[s]));
    }
    check_loops(code, &ins, &at, n)?;
    for h in &code.handlers {
        let (s, e, t) = (h.start as i64, h.end as i64, h.target as i64);
        if !(s < e && start(s) && (e == n as i64 || start(e)) && start(t) && (h.reg as u32) < nregs) {
            return Err(format!("handler {s:04}..{e:04} -> {t:04} r{}: a bad range, target or register", h.reg));
        }
    }
    if code.lines.windows(2).any(|w| w[0].0 >= w[1].0) || code.lines.last().is_some_and(|l| l.0 as usize >= n) {
        return Err("source positions not sorted or past the end".into());
    }
    for &p in code.roots.pcs() {
        if !start(p as i64) || !access[at[p as usize]].safepoint {
            return Err(format!("{p:04}: a root map for a word that is not a safepoint instruction"));
        }
    }
    let state = flow(code, &ins, &at, &access, words, &maps)?;
    check_live_roots(code, &ins, &at, &access, &state, words, &maps)
}

fn verify_lazy(code: &Code, i: Ins) -> Result<(), String> {
    let args = code.flags & CF_ARGS != 0;
    if code.words.len() != 1 || i.a != args as u32 || i.b != 0 || i.c != 0
        || code.flags & !(CF_ARGS | CF_GEN) != 0 || (code.flags & CF_GEN != 0 && code.gen_kind == GK_NONE)
        || (args && (code.nparams != 0 || code.nregs != 0))
        || (!args && (code.nparams > 64 || code.nregs != code.nparams.max(1) || code.fn_len != code.nparams))
        || !code.funcs.is_empty() || !code.sites.is_empty() || !code.recipes.is_empty() || !code.data.is_empty()
        || !code.numbers.is_empty() || !code.tables.is_empty() || !code.loops.is_empty() || !code.handlers.is_empty()
        || !code.lines.is_empty() || code.gen_regs != 0 || !code.roots.pcs().is_empty()
    {
        return Err("invalid lazy stub metadata".into());
    }
    Ok(())
}

/// Check the switch table of `i` (at `pc`). A target must be an instruction after the switch.
fn check_table(code: &Code, i: &Ins, pc: usize, start: &dyn Fn(i64) -> bool) -> Result<(), &'static str> {
    let ok = |t: u32| t as usize > pc && start(t as i64);
    match i.op {
        Op::SwitchI => {
            let (lo, default, targets) = code.int_table(i.b).ok_or("the table is not inside the pool")?;
            if targets.is_empty() {
                return Err("an empty table");
            }
            if lo as i64 + targets.len() as i64 - 1 > i32::MAX as i64 {
                return Err("the table runs past the largest integer");
            }
            if !ok(default) || !targets.iter().all(|&t| ok(t)) {
                return Err("a target is not a later instruction");
            }
        }
        Op::SwitchA => {
            let (default, slots) = code.str_table(i.b).ok_or("the table is not inside the pool or its slot count is not a power of two")?;
            let mask = slots.len() / STR_SLOT - 1;
            if !ok(default) {
                return Err("a target is not a later instruction");
            }
            if !slots.chunks(STR_SLOT).any(|c| c[1] == STR_EMPTY) {
                return Err("the table has no empty slot");
            }
            for (p, c) in slots.chunks(STR_SLOT).enumerate().filter(|(_, c)| c[1] != STR_EMPTY) {
                let atom = code.atoms.get(c[1] as usize).ok_or("a slot names no atom")?;
                if atom.cached_hash() != c[0] {
                    return Err("the hash of a slot is not the hash of its atom");
                }
                if !ok(c[2]) {
                    return Err("a target is not a later instruction");
                }
                let mut q = c[0] as usize & mask;
                while q != p {
                    if slots[q * STR_SLOT + 1] == STR_EMPTY {
                        return Err("the probe from the hash of a slot meets an empty slot first");
                    }
                    q = (q + 1) & mask;
                }
            }
        }
        Op::GenInit | Op::Yield | Op::YieldResult | Op::Await => {
            let nregs = code.nregs as u32;
            let l = code.save_list(i.b).ok_or("the save list is not inside the pool")?;
            if l.regs.len() > code.gen_regs as usize {
                return Err("the save list is longer than gen_regs");
            }
            if l.sent != NO_REG && l.sent >= nregs || l.mode != NO_REG && l.mode >= nregs || l.regs.iter().any(|r| *r >= nregs) {
                return Err("the save list names a register out of range");
            }
            if l.sent != NO_REG && l.sent == l.mode || l.regs.iter().enumerate().any(|(k, r)| l.regs[..k].contains(r)) {
                return Err("the save list names a register twice");
            }
            if i.op == Op::Yield && code.gen_kind == GK_ASYNCGEN && (l.sent == NO_REG || l.mode == NO_REG) {
                return Err("async generator Yield needs both resume registers");
            }
            if let Some(map) = code.roots.find(pc as u32)
                && l.regs.iter().any(|r| !map.slots().any(|m| m == *r as usize))
            {
                return Err("the save list names a register that the root map of the suspension does not");
            }
        }
        _ => {}
    }
    Ok(())
}

fn check_loops(code: &Code, ins: &[(usize, Ins, usize)], at: &[usize], n: usize) -> Result<(), String> {
    let starts = |w: u32| (w as usize) < n && at[w as usize] != usize::MAX;
    let mut open: Vec<u32> = Vec::new();
    let mut last_head = None;
    for l in &code.loops {
        if last_head.is_some_and(|h| h >= l.head) {
            return Err("loop headers are not sorted and unique".into());
        }
        last_head = Some(l.head);
        if !starts(l.head) || !starts(l.end) || l.end < l.head {
            return Err(format!("loop {:04}..{:04}: a bad header or end", l.head, l.end));
        }
        let (pc, i, len) = ins[at[l.end as usize]];
        if i.op != Op::JmpBack || i.target(pc, len) != Some(l.head as i64) {
            return Err(format!("loop {:04}..{:04}: the end is not a JmpBack to the header", l.head, l.end));
        }
        while open.last().is_some_and(|e| *e < l.head) {
            open.pop();
        }
        if open.last().is_some_and(|e| l.end > *e) {
            return Err(format!("loop {:04}..{:04} crosses the end of another loop", l.head, l.end));
        }
        open.push(l.end);
    }
    for &(pc, i, len) in ins {
        if i.op == Op::JmpBack {
            let t = i.target(pc, len).expect("a jump has an offset") as u32;
            if !code.loops.iter().any(|l| l.head == t && pc as u32 <= l.end) {
                return Err(format!("{pc:04}: a back edge that no loop covers"));
            }
        }
    }
    Ok(())
}

/// Propagate the context depth and the registers that every incoming path has written.
fn flow(code: &Code, ins: &[(usize, Ins, usize)], at: &[usize], access: &[Access<'_>], words: usize, maps: &[u64]) -> Result<Vec<u32>, String> {
    let mut depths = vec![UNREACHED; ins.len()];
    let mut written = vec![0; ins.len() * words];
    let mut work = vec![0];
    let mut set = vec![0; words];
    let mut scratch = vec![0; words];
    (0..code.nparams as u32).for_each(|r| put(&mut written, r));
    depths[0] = 0;
    while let Some(k) = work.pop() {
        let (pc, i, _) = ins[k];
        let a = &access[k];
        let depth = depths[k];
        set.copy_from_slice(&written[k * words..(k + 1) * words]);
        let err = |what: String| Err(format!("{pc:04} {}: {what}", i.op.name()));
        if let Some(r) = a.reads().find(|r| !has(&set, *r)) {
            return err(format!("reads r{r}, which a path has not written"));
        }
        if let Some(r) = a.read_window.clone().find(|r| !has(&set, *r)) {
            return err(format!("reads r{r} of its window, which a path has not written"));
        }
        if a.safepoint
            && let Some(r) = missing(&maps[k * words..(k + 1) * words], &set)
        {
            return err(format!("the root map names r{r}, which a path has not written"));
        }
        // Depths agree at a join. The register sets are intersected.
        let mut meet = |to: usize, depth: u32, incoming: &[u64]| -> Result<(), String> {
            let old = depths[to];
            let cur = &mut written[to * words..(to + 1) * words];
            if old == UNREACHED {
                depths[to] = depth;
                cur.copy_from_slice(incoming);
                work.push(to);
            } else if old != depth {
                return Err(format!("{:04}: context depth {old} and {depth} on two paths (from {pc:04})", ins[to].0));
            } else {
                let mut changed = false;
                for (c, s) in cur.iter_mut().zip(incoming) {
                    changed |= *c & !s != 0;
                    *c &= s;
                }
                if changed {
                    work.push(to);
                }
            }
            Ok(())
        };
        if let Some(h) = a.handler {
            if depth < h.depth as u32 {
                return err(format!("context depth {depth} is below the depth {} of its handler", h.depth));
            }
            scratch.copy_from_slice(&set);
            put(&mut scratch, h.reg as u32);
            meet(at[h.target as usize], h.depth as u32, &scratch)?;
        }
        let depth = match i.op {
            Op::PushCtx => depth + 1,
            Op::PopCtx => depth.checked_sub(1).ok_or_else(|| format!("{pc:04}: PopCtx at depth 0"))?,
            _ => depth,
        };
        if let Some(l) = a.save {
            if let Some(r) = l.regs.iter().find(|r| !has(&set, **r)) {
                return err(format!("the save list names r{r}, which a path has not written"));
            }
            set.fill(0);
            l.regs.iter().chain([&l.sent, &l.mode]).filter(|r| **r != NO_REG).for_each(|r| put(&mut set, *r));
        }
        if let Some(t) = a.target.filter(|_| skips_write(i.op)) {
            meet(t, depth, &set)?;
        }
        a.writes.clone().for_each(|r| put(&mut set, r));
        if let Some(t) = a.target.filter(|_| !skips_write(i.op)) {
            meet(t, depth, &set)?;
        }
        if matches!(i.op, Op::SwitchA | Op::SwitchI) {
            for t in code.switch_targets(&i).expect("a checked switch table") {
                meet(at[t as usize], depth, &set)?;
            }
        }
        if a.fallthrough {
            if k + 1 == ins.len() {
                return err("a path runs past the end".into());
            }
            meet(k + 1, depth, &set)?;
        }
    }
    Ok(depths)
}

/// A taken branch can leave the destination untouched. AYsCall writes it on only one of its two taken paths.
fn skips_write(op: Op) -> bool {
    matches!(op, Op::IterNext | Op::ForInNext | Op::AIterClose | Op::AIterCloseT | Op::AYsCall)
}

fn handler(code: &Code, i: Ins, pc: usize, safepoint: bool) -> Option<&Handler> {
    let throws = match i.op {
        Op::Throw => true,
        Op::RetDerived | Op::AsyncInit | Op::JmpBack | Op::AIterCloseT => false,
        Op::IterClose => i.c == 0,
        Op::Yield | Op::RetGen => code.gen_kind == GK_ASYNCGEN,
        _ => safepoint,
    };
    if throws { code.handler_at(pc as u32) } else { None }
}

fn save_list(code: &Code, i: Ins) -> Option<SaveList<'_>> {
    if matches!(i.op, Op::GenInit | Op::Yield | Op::YieldResult | Op::Await) { code.save_list(i.b) } else { None }
}

#[derive(Clone, Copy)]
enum Edge {
    Write,
    Keep,
    Throw(u32),
}

/// Return the first register in `needed` that `available` does not cover.
fn missing(needed: &[u64], available: &[u64]) -> Option<usize> {
    needed.iter().zip(available).enumerate().find_map(|(w, (&need, &have))| {
        let absent = need & !have;
        (absent != 0).then(|| w * 64 + absent.trailing_zeros() as usize)
    })
}

fn check_live_roots(code: &Code, ins: &[(usize, Ins, usize)], at: &[usize], access: &[Access<'_>], depths: &[u32], words: usize, maps: &[u64]) -> Result<(), String> {
    let mut preds = vec![usize::MAX; ins.len()];
    let mut edges = Vec::new();
    // The collector itself reads each named slot. Seed those uses so an omitted pointer cannot be marked later.
    let mut live = maps.to_vec();
    let mut kills = vec![0u64; ins.len() * words];
    let mut work = Vec::new();
    let mut queued = vec![false; ins.len()];
    for (k, (&(_, i, _), a)) in ins.iter().zip(access).enumerate() {
        if depths[k] == UNREACHED {
            continue;
        }
        // Store the incoming edges in one array. Fallthrough is implicit.
        let mut edge = |to: usize, kind| {
            edges.push((k, kind, preds[to]));
            preds[to] = edges.len() - 1;
        };
        if let Some(t) = a.target {
            edge(t, if skips_write(i.op) { Edge::Keep } else { Edge::Write });
        }
        if matches!(i.op, Op::SwitchA | Op::SwitchI) {
            for t in code.switch_targets(&i).expect("a checked switch table") {
                edge(at[t as usize], Edge::Write);
            }
        }
        if let Some(h) = a.handler {
            edge(at[h.target as usize], Edge::Throw(h.reg as u32));
        }
        let kill = &mut kills[k * words..(k + 1) * words];
        let uses = &mut live[k * words..(k + 1) * words];
        a.reads().chain(a.read_window.clone()).for_each(|r| put(uses, r));
        a.writes.clone().for_each(|r| put(kill, r));
        if let Some(list) = a.save {
            for r in [list.sent, list.mode].into_iter().filter(|r| *r != NO_REG) {
                put(kill, r);
            }
            // Only saved slots can be needed after the resume (the forward pass checks this).
            list.regs.iter().for_each(|r| put(uses, *r));
        }
        work.push(k);
        queued[k] = true;
    }
    let mut current = vec![0; words];
    while let Some(k) = work.pop() {
        queued[k] = false;
        let (pc, i, _) = ins[k];
        current.copy_from_slice(&live[k * words..(k + 1) * words]);
        if access[k].safepoint
            && let Some(r) = missing(&current, &maps[k * words..(k + 1) * words])
        {
            return Err(format!("{pc:04} {}: root map misses live r{r}", i.op.name()));
        }
        let mut propagate = |pred: usize, edge| {
            let mut changed = false;
            for (w, old) in live[pred * words..(pred + 1) * words].iter_mut().enumerate() {
                let mut new = current[w];
                match edge {
                    Edge::Write => new &= !kills[pred * words + w],
                    Edge::Throw(r) if r as usize / 64 == w => new &= !(1 << (r % 64)),
                    _ => {}
                }
                changed |= new & !*old != 0;
                *old |= new;
            }
            if changed && !queued[pred] {
                work.push(pred);
                queued[pred] = true;
            }
        };
        if k > 0 && depths[k - 1] != UNREACHED && access[k - 1].fallthrough {
            propagate(k - 1, Edge::Write);
        }
        let mut edge = preds[k];
        while edge != usize::MAX {
            let (pred, kind, next) = edges[edge];
            propagate(pred, kind);
            edge = next;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "verify_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "verify_global_site_tests.rs"]
mod global_site_tests;

#[cfg(test)]
#[path = "verify_roots_tests.rs"]
mod roots_tests;
