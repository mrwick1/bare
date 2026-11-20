//! Executed op counts, behind the cargo feature `op-count` (plan step S2.9, `docs/design-js2-superinstructions.md`). With the feature off
//! this module does not exist and `Vm::run` has no call into it, so the normal build is the same code as before.
//!
//! `tick` runs once for each dispatch. It counts the op, the pair (the op before it and this op, any control flow) and two fusable
//! chains: an op that runs right after the op that comes before it in the code (the previous op fell through, or jumped to the next word)
//! and that is not the target of any jump, a handler or a switch table. Only such a chain can become one superinstruction without a
//! change to the control flow. The counts are per thread.
use crate::bytecode::{Code, Op, decode};
use std::cell::RefCell;
use std::collections::HashMap;

#[derive(Clone, Default)]
pub struct Counts {
    /// Dispatches: one for each instruction (a `Wide` prefix and its op are one dispatch).
    pub dispatches: u64,
    /// Of those, the wide ones.
    pub wide: u64,
    pub ops: Vec<u64>,
    /// `pairs[a * 256 + b]`: the op `b` ran right after the op `a`, by any control flow.
    pub pairs: Vec<u64>,
    /// The same, when `b` is the next instruction in the code after `a` and no jump lands on `b`.
    pub fpairs: Vec<u64>,
    /// `(a << 16) | (b << 8) | c`: three ops in a row in the code, with no jump target on `b` or `c`.
    pub ftriples: HashMap<u32, u64>,
}

struct State {
    counts: Counts,
    /// The last op, the code it ran in, the word after it, and the op before it when that link is fusable.
    prev: Option<(u8, usize, usize)>,
    prev2: Option<u8>,
    /// Jump targets of a code, by the address and the length of its words. A code that is freed and a new one at the same address with
    /// the same length would share an entry, so `reset` clears the map; a program that frees code while it runs is not counted right.
    targets: HashMap<(usize, usize), Vec<bool>>,
    /// The ops in order, when `record` asked for it: `op | 0x100` when the op is a fusable link after the one before it.
    trace: Option<(Vec<u16>, usize)>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State {
        counts: Counts { ops: vec![0; 256], pairs: vec![0; 65536], fpairs: vec![0; 65536], ..Counts::default() },
        prev: None,
        prev2: None,
        targets: HashMap::new(),
        trace: None,
    });
}

/// The words that a jump, a handler or a switch can reach.
fn jump_targets(code: &Code) -> Vec<bool> {
    let mut t = vec![false; code.words.len() + 1];
    t[0] = true;
    for h in &code.handlers {
        t[h.target as usize] = true;
    }
    let mut pc = 0;
    while let Ok((ins, len)) = decode(&code.words, pc) {
        if let Some(x) = ins.target(pc, len) {
            t[x as usize] = true;
        }
        for x in code.switch_targets(&ins).into_iter().flatten() {
            t[x as usize] = true;
        }
        pc += len;
    }
    t
}

/// Count one dispatch: the instruction at `pc` of `code`, with the opcode `op` (the op itself, not the `Wide` prefix).
pub fn tick(code: &Code, pc: usize, op: u8, wide: bool) {
    STATE.with(|s| {
        let s = &mut *s.borrow_mut();
        let key = code.words.as_ptr() as usize;
        let tg = s.targets.entry((key, code.words.len())).or_insert_with(|| jump_targets(code));
        let is_target = tg[pc];
        let len = 1 + usize::from(wide) + usize::from(Op::ALL[op as usize].form().has_x());
        let c = &mut s.counts;
        c.dispatches += 1;
        c.wide += u64::from(wide);
        c.ops[op as usize] += 1;
        let mut link = None;
        if let Some((p, pkey, pend)) = s.prev {
            c.pairs[p as usize * 256 + op as usize] += 1;
            if pkey == key && pend == pc && !is_target {
                c.fpairs[p as usize * 256 + op as usize] += 1;
                if let Some(q) = s.prev2 {
                    *c.ftriples.entry((q as u32) << 16 | (p as u32) << 8 | op as u32).or_default() += 1;
                }
                link = Some(p);
            }
        }
        if let Some((t, cap)) = &mut s.trace
            && t.len() < *cap
        {
            t.push(op as u16 | (u16::from(link.is_some()) << 8));
        }
        s.prev2 = link;
        s.prev = Some((op, key, pc + len));
    });
}

/// Keep the ops in order, up to `cap` of them (2 bytes each), from now on.
pub fn record(cap: usize) {
    STATE.with(|s| s.borrow_mut().trace = Some((Vec::new(), cap)));
}

/// Take the recorded ops.
pub fn take_trace() -> Vec<u16> {
    STATE.with(|s| s.borrow_mut().trace.take().map_or_else(Vec::new, |t| t.0))
}

/// A copy of the counts so far.
pub fn snapshot() -> Counts {
    STATE.with(|s| s.borrow().counts.clone())
}

/// Forget the counts and the chain.
pub fn reset() {
    STATE.with(|s| {
        let s = &mut *s.borrow_mut();
        s.counts = Counts { ops: vec![0; 256], pairs: vec![0; 65536], fpairs: vec![0; 65536], ..Counts::default() };
        s.prev = None;
        s.prev2 = None;
        s.targets.clear();
    });
}

impl Counts {
    /// The name of the op with the number `n`.
    pub fn name(n: usize) -> &'static str {
        Op::ALL.get(n).map_or("?", |o| o.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_counts_add_up() {
        let mut vm = crate::script::load(&crate::script::eval_program("var s = 0; for (var i = 0; i < 50; i++) { s += i % 7; } s")).unwrap();
        reset();
        record(1_000_000);
        vm.run_main(0).unwrap();
        assert_eq!(vm.global_text("__result"), "147");
        let (c, trace) = (snapshot(), take_trace());
        assert!(c.dispatches > 200);
        assert_eq!(c.ops.iter().sum::<u64>(), c.dispatches);
        assert_eq!(c.pairs.iter().sum::<u64>(), c.dispatches - 1);
        assert!(c.fpairs.iter().sum::<u64>() <= c.pairs.iter().sum::<u64>());
        assert_eq!(trace.len() as u64, c.dispatches);
        // The loop adds one `Inc` for each turn.
        assert_eq!(c.ops[Op::Inc as usize], 50);
    }
}
