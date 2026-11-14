//! The verifier of bytecode (step B2 of `docs/engine-plan.md`). It checks a `Code` before the loop (step B3) may run it, so the
//! loop can trust what it reads:
//! - every word decodes, and each instruction starts where the one before it ends;
//! - registers are below `nregs` (the argument registers of `Call` and `New` and the strings of `Concat` too), constants and inline
//!   caches exist, a name operand is a name constant, and each inline cache has one instruction;
//! - each jump and each handler target is the first word of an instruction, and each handler range starts and ends at one;
//! - the environment depth (`PushEnv` adds one, `PopEnv` removes one) is the same on every path to an instruction, never below 0,
//!   and at a handler target it is the handler's `depth`, which is at most the depth at each instruction in its range that can
//!   throw;
//! - no path runs past the last word;
//! - the source positions are sorted by word;
//! - a register function (stage 2) has `nparams <= nlocals <= nregs`; its ops read only registers and the environments outside it.
use crate::bc::{decode, Code, Const, Form, Ins, Op};

/// Check `code`. Gives the first error.
pub(super) fn verify(code: &Code) -> Result<(), String> {
    let n = code.ops.len();
    // The instructions, and for each word the index of the instruction that starts there.
    let mut ins: Vec<(usize, Ins, usize)> = Vec::new();
    let mut at = vec![usize::MAX; n];
    let mut pc = 0;
    while pc < n {
        let (i, len) = decode(&code.ops, pc).map_err(|e| format!("{pc:04}: {e:?}"))?;
        at[pc] = ins.len();
        ins.push((pc, i, len));
        pc += len;
    }
    let start = |w: usize| w < n && at[w] != usize::MAX;
    // The target of a jump at `pc` that takes `len` words (`usize::MAX` when it is before word 0).
    let target = |pc: usize, len: usize, i: &Ins| usize::try_from((pc + len) as i64 + i.offset() as i64).unwrap_or(usize::MAX);
    let nregs = code.nregs as u32;
    if code.regfn && !(code.nparams <= code.nlocals && code.nlocals <= code.nregs) {
        return Err("a register function: parameters, locals and registers do not fit".into());
    }
    let mut ic_used = vec![false; code.ics.len()];
    for &(pc, i, len) in &ins {
        let err = |what: &str| Err(format!("{pc:04} {}: {what}", i.op.name()));
        let vals = match i.op.form() {
            Form::Abc => vec![i.a, i.b, i.c],
            Form::AbcX => vec![i.a, i.b, i.c, i.x],
            Form::Abx | Form::AsBx => vec![i.a, i.b],
            Form::Sj => vec![i.a],
        };
        let mut letters = i.op.spec().chars().filter(|l| *l != ' ');
        for (k, v) in vals.into_iter().enumerate() {
            match letters.next() {
                Some('r') if v >= nregs => return err("register out of range"),
                Some('k') => match code.consts.get(v as usize) {
                    None => return err("no such constant"),
                    Some(Const::Name(_)) => {}
                    Some(_) if i.op != Op::LoadK => return err("the constant is not a name"),
                    Some(_) => {}
                },
                Some('q') => match code.consts.get(v as usize) {
                    Some(Const::Number(_)) => {}
                    _ => return err("the constant is not a number"),
                },
                Some('c') => match ic_used.get_mut(v as usize) {
                    Some(u) if !*u => *u = true,
                    _ => return err("no such inline cache, or a second use of it"),
                },
                Some('j') if !start(target(pc, len, &i)) => return err("jump target"),
                Some('-') | None if v != 0 => return err(&format!("operand {k} is not used and is not 0")),
                _ => {}
            }
        }
        let regs_ok = match i.op {
            Op::Call | Op::CallR => i.b + 2 + i.c <= nregs,
            Op::New => i.b + 1 + i.c <= nregs,
            Op::Concat => i.c >= 1 && i.b + i.c <= nregs,
            _ => true,
        };
        if !regs_ok {
            return err("argument registers out of range");
        }
    }
    if ic_used.contains(&false) {
        return Err("an inline cache that no instruction uses".into());
    }
    for h in &code.handlers {
        let (s, e, t) = (h.start as usize, h.end as usize, h.target as usize);
        if !(s < e && start(s) && (e == n || start(e)) && start(t) && (h.reg as u32) < nregs) {
            return Err(format!("handler {s:04}..{e:04} -> {t:04} r{}: a bad range, target or register", h.reg));
        }
    }
    if code.lines.windows(2).any(|w| w[0].0 >= w[1].0) || code.lines.last().is_some_and(|l| l.0 as usize >= n) {
        return Err("source positions not sorted or past the end".into());
    }
    // The environment depth on every path.
    let mut depth: Vec<Option<u32>> = vec![None; n];
    let mut work: Vec<(usize, u32)> = vec![(0, 0)];
    work.extend(code.handlers.iter().map(|h| (h.target as usize, h.depth as u32)));
    while let Some((pc, d)) = work.pop() {
        if pc >= n {
            return Err(format!("{pc:04}: a path runs past the end"));
        }
        match depth[pc] {
            Some(x) if x == d => continue,
            Some(x) => return Err(format!("{pc:04}: environment depth {x} and {d} on two paths")),
            None => depth[pc] = Some(d),
        }
        let (_, i, len) = ins[at[pc]];
        let d = match i.op {
            Op::PushEnv => d + 1,
            Op::PopEnv => d.checked_sub(1).ok_or(format!("{pc:04}: PopEnv at depth 0"))?,
            _ => d,
        };
        match i.op {
            Op::Jmp => work.push((target(pc, len, &i), d)),
            Op::JmpIf | Op::JmpIfNot | Op::JmpNullish | Op::JmpNotNullish => work.extend([(target(pc, len, &i), d), (pc + len, d)]),
            op if op.is_fused_jump() => work.extend([(target(pc, len, &i), d), (pc + len, d)]),
            Op::Ret | Op::RetUndef | Op::Throw => {}
            _ => work.push((pc + len, d)),
        }
    }
    // The exit code of a `break`, `continue` or `return` in a `try` block pops environments before its jump. It cannot throw.
    let safe = |w: usize| matches!(ins[at[w]].1.op, Op::PopEnv | Op::Mov | Op::LoadI | Op::Jmp);
    for h in &code.handlers {
        for w in h.start as usize..h.end as usize {
            if depth[w].is_some_and(|d| d < h.depth as u32) && !safe(w) {
                return Err(format!("{w:04}: depth below the depth of its handler {}", h.depth));
            }
        }
    }
    Ok(())
}
