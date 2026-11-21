//! The verifier of bytecode (plan step S2.4). It checks a [`Code`] before the interpreter (S2.6) may run it, so that the interpreter can
//! read operands with no checks. It is meant for debug builds, tests and a compiler that has a bug, not for every compile in a release
//! build: the data flow part keeps one register set for each instruction. It checks:
//! - every word decodes, and each instruction starts where the one before it ends;
//! - registers (and the windows of calls, literals and iterators) are below `nregs`, the special constants are 0 to 3, each index
//!   of a table (numbers, atoms, functions, recipes, sites) is in range, and a field that the op does not use is 0;
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
//!   written at the entry, and a handler writes its register. Unreachable instructions are not checked by the data flow.
//! - suspension (`GenInit`, `Yield`, `RetGen`): only code with `gen_kind` that is a constructor-less function has them, and it has no `Ret`
//!   or `LoadNewTarget`. A save list is inside the pool, names registers that are written on every path to the suspension and that its
//!   root map names (a collection before the copy must not free them), and is no longer than `gen_regs`. After the suspension only the registers of
//!   the list, the sent value and the mode are written: the resume restores no other. A handler whose range holds a suspension is
//!   entered with that smaller set too.
use crate::bytecode::{Code, GK_NONE, ITER_REGS, Ins, NO_REG, Op, Recipes, STR_EMPTY, STR_SLOT, decode};

type Bits = Vec<u64>;

fn has(b: &Bits, r: u32) -> bool {
    b[r as usize / 64] >> (r % 64) & 1 != 0
}

fn put(b: &mut Bits, r: u32) {
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
        Op::IterOpen | Op::ForInOpen | Op::IterNext | Op::IterClose => ITER_REGS,
        _ => return None,
    })
}

/// The window base of `ins` (field of the letter `R` or `W`).
fn window_base(ins: &Ins) -> Option<u32> {
    (0..ins.op.form().fields()).find(|&i| matches!(ins.op.letter(i), 'R' | 'W')).map(|i| ins.field(i))
}

/// Check `code`. Gives the first error.
pub fn verify(code: &Code) -> Result<(), String> {
    let n = code.words.len();
    let nregs = code.nregs as u32;
    if n == 0 {
        return Err("no code".into());
    }
    if code.nparams > code.nregs {
        return Err("more parameters than registers".into());
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
    let mut templates = vec![false; code.recipes.len()];
    for &(_, i, _) in &ins {
        if i.op == Op::GetTemplate && let Some(t) = templates.get_mut(i.b as usize) {
            *t = true;
        }
    }
    let bad_recipe = |i: usize, k: &[u32]| {
        k.iter().enumerate().any(|(j, a)| *a >= natoms && !(templates[i] && j % 2 == 0 && *a == u32::MAX)) || (templates[i] && !k.len().is_multiple_of(2))
    };
    if (0..code.recipes.len()).any(|i| code.recipes.get(i).is_some_and(|k| bad_recipe(i, k))) {
        return Err("a recipe names no atom".into());
    }
    let mut site_uses = vec![0u32; code.sites.len()];
    if code.gen_kind != GK_NONE && !code.noctor {
        return Err("a generator that a script can construct".into());
    }
    for &(pc, i, len) in &ins {
        let err = |what: &str| Err(format!("{pc:04} {}: {what}", i.op.name()));
        let suspends = matches!(i.op, Op::GenInit | Op::Yield | Op::RetGen);
        if suspends && code.gen_kind == GK_NONE {
            return err("an op of generator code in a function that is not one");
        }
        if code.gen_kind != GK_NONE && matches!(i.op, Op::Ret | Op::RetUndef | Op::RetDerived | Op::LoadNewTarget) {
            return err("not allowed in generator code (a generator returns with RetGen, and its frame keeps the generator where new.target is)");
        }
        for k in 0..i.op.form().fields() {
            let v = i.field(k);
            let in_range = |count: usize| (v as usize) < count;
            match i.op.letter(k) {
                'w' | 'r' if v >= nregs => return err("register out of range"),
                'z' if v > 3 => return err("special constant above 3"),
                'N' if !in_range(code.numbers.len()) => return err("no such number"),
                'A' if !in_range(code.atoms.len()) => return err("no such atom"),
                'F' if !in_range(code.funcs.len()) => return err("no such function"),
                'H' if !in_range(code.recipes.len()) => return err("no such recipe"),
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
                    let t = i.target(pc, len).expect("a jump has an offset");
                    if !start(t) {
                        return err("jump target is not an instruction");
                    }
                    if (i.op == Op::JmpBack) != (t <= pc as i64) {
                        return err(if i.op == Op::JmpBack { "JmpBack goes forward" } else { "a jump back that is not JmpBack" });
                    }
                }
                '-' if v != 0 => return err(&format!("field {k} is not used and is not 0")),
                _ => {}
            }
        }
        if let (Some(base), Some(size)) = (window_base(&i), window(&i, &code.recipes)) {
            if i.op == Op::Concat && size == 0 {
                return err("Concat of no strings");
            }
            if base as u64 + size as u64 > nregs as u64 {
                return err("window out of range");
            }
        }
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
    if code.roots.regs() != nregs as usize {
        return Err("the root maps have another register count".into());
    }
    for &(pc, i, _) in &ins {
        if i.op.is_safepoint() && code.roots.find(pc as u32).is_none() {
            return Err(format!("{pc:04} {}: a safepoint with no root map", i.op.name()));
        }
    }
    for &p in code.roots.pcs() {
        if !start(p as i64) || !ins[at[p as usize]].1.op.is_safepoint() {
            return Err(format!("{p:04}: a root map for a word that is not a safepoint instruction"));
        }
    }
    flow(code, &ins, &at)
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
        Op::GenInit | Op::Yield => {
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

/// The data flow: the context depth and the set of registers that every path has written.
fn flow(code: &Code, ins: &[(usize, Ins, usize)], at: &[usize]) -> Result<(), String> {
    let nregs = code.nregs as u32;
    let words = (nregs as usize).div_ceil(64).max(1);
    let mut state: Vec<Option<(u32, Bits)>> = vec![None; ins.len()];
    let mut work: Vec<usize> = Vec::new();
    // Meet `s` into instruction `to`. Depths must agree; the register sets are intersected.
    let meet = |state: &mut Vec<Option<(u32, Bits)>>, work: &mut Vec<usize>, to: usize, from: usize, depth: u32, set: &Bits| -> Result<(), String> {
        match &mut state[to] {
            None => {
                state[to] = Some((depth, set.clone()));
                work.push(to);
            }
            Some((d, cur)) => {
                if *d != depth {
                    return Err(format!("{:04}: context depth {d} and {depth} on two paths (from {:04})", ins[to].0, ins[from].0));
                }
                let mut changed = false;
                for (c, s) in cur.iter_mut().zip(set) {
                    changed |= *c & !s != 0;
                    *c &= s;
                }
                if changed {
                    work.push(to);
                }
            }
        }
        Ok(())
    };
    let mut entry = vec![0u64; words];
    (0..code.nparams as u32).for_each(|r| put(&mut entry, r));
    state[0] = Some((0, entry));
    work.push(0);
    while let Some(k) = work.pop() {
        let (pc, i, len) = ins[k];
        let (depth, mut set) = state[k].clone().expect("queued with a state");
        let err = |what: String| Err(format!("{pc:04} {}: {what}", i.op.name()));
        // Reads.
        let base = window_base(&i);
        let size = window(&i, &code.recipes);
        let reads_window = (0..i.op.form().fields()).any(|f| i.op.letter(f) == 'R');
        for f in 0..i.op.form().fields() {
            if i.op.letter(f) == 'r' && !has(&set, i.field(f)) {
                return err(format!("reads r{}, which a path has not written", i.field(f)));
            }
        }
        if let (true, Some(b), Some(s)) = (reads_window, base, size)
            && let Some(r) = (b..b + s).find(|r| !has(&set, *r))
        {
            return err(format!("reads r{r} of its window, which a path has not written"));
        }
        if let Some(map) = code.roots.find(pc as u32)
            && let Some(r) = map.slots().find(|r| !has(&set, *r as u32))
        {
            return err(format!("the root map names r{r}, which a path has not written"));
        }
        // Depth, and the handlers that cover this instruction.
        for h in &code.handlers {
            if h.start as usize == pc {
                let mut hs = set.clone();
                put(&mut hs, h.reg as u32);
                meet(&mut state, &mut work, at[h.target as usize], k, h.depth as u32, &hs)?;
            }
            if (h.start as usize..h.end as usize).contains(&pc) && i.op.is_safepoint() && depth < h.depth as u32 {
                return err(format!("context depth {depth} is below the depth {} of its handler", h.depth));
            }
        }
        let depth = match i.op {
            Op::PushCtx => depth + 1,
            Op::PopCtx => depth.checked_sub(1).ok_or(format!("{pc:04}: PopCtx at depth 0"))?,
            _ => depth,
        };
        // A suspension: a resume restores the registers of the list and writes the sent value and the mode, and nothing else.
        if matches!(i.op, Op::GenInit | Op::Yield) {
            let l = code.save_list(i.b).expect("a save list that the checks accepted");
            if let Some(r) = l.regs.iter().find(|r| !has(&set, **r)) {
                return err(format!("the save list names r{r}, which a path has not written"));
            }
            let mut kept = vec![0u64; words];
            l.regs.iter().chain([&l.sent, &l.mode]).filter(|r| **r != NO_REG).for_each(|r| put(&mut kept, *r));
            for (c, k) in set.iter_mut().zip(&kept) {
                *c &= k;
            }
            for r in [l.sent, l.mode].into_iter().filter(|r| *r != NO_REG) {
                put(&mut set, r);
            }
            for h in &code.handlers {
                if (h.start as usize..h.end as usize).contains(&pc) {
                    let mut hs = set.clone();
                    put(&mut hs, h.reg as u32);
                    meet(&mut state, &mut work, at[h.target as usize], k, h.depth as u32, &hs)?;
                }
            }
        }
        // Writes.
        for f in 0..i.op.form().fields() {
            if i.op.letter(f) == 'w' {
                put(&mut set, i.field(f));
            }
        }
        if let (Some(b), Some(s)) = (base, size)
            && (0..i.op.form().fields()).any(|f| i.op.letter(f) == 'W')
        {
            (b..b + s).for_each(|r| put(&mut set, r));
        }
        // Successors.
        if let Some(t) = i.target(pc, len) {
            meet(&mut state, &mut work, at[t as usize], k, depth, &set)?;
        }
        for t in code.switch_targets(&i).into_iter().flatten() {
            meet(&mut state, &mut work, at[t as usize], k, depth, &set)?;
        }
        if i.op.falls_through() {
            if k + 1 == ins.len() {
                return err("a path runs past the end".into());
            }
            meet(&mut state, &mut work, k + 1, k, depth, &set)?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "verify_tests.rs"]
mod tests;
