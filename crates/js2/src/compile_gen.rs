//! Generators in the compiler (`docs/design-js2-async.md` 5.1, 6.1 and 6.2; plan step G2). The runtime side is `generator.rs`.
//!
//! **Entry.** A generator binds its parameters and evaluates their defaults at the call, as any function does. Then `GenInit` makes the
//! generator object and returns it to the caller; the first `next()` goes on after it ([`Fc::gen_init`]).
//!
//! **Suspension.** `yield v` is `Yield v` with a save list: the registers that hold a value at that point (`Fc::live`, the same set as the
//! root map of the instruction), except the temporaries of the `yield` itself. A resume copies them back and writes the value that was sent
//! into `sent` and the mode (0 `next`, 1 `throw`, 2 `return`) into `mode`. Both are registers of the `yield` that nothing else keeps, so
//! a `throw()` does not change a variable that the handler reads. The code after the `Yield` dispatches on the mode:
//!
//! ```text
//! Yield v            ; save list: sent s, mode m
//! JSEqI m, 0 -> cont ; next(x): the value of the expression is s
//! JSEqI m, 1 -> thr
//!                    ; return(x): what a `return x` runs (the finalizers of the `try` statements around, the `return()` of each
//!                    ; `for-of` around), then RetGen s
//! thr: Throw s       ; throw(x): the handlers of the code around catch it
//! cont:
//! ```
//!
//! A generator returns with `RetGen`, never with `Ret` (the verifier checks it). `new.target` is `undefined` in a generator.
use super::*;
use crate::bytecode::{GK_ASYNCGEN, GK_GEN, GK_NONE, ITER_REGS, NO_REG, YS_REGS};
use crate::generator::{MODE_NEXT, MODE_RETURN, MODE_THROW};

impl<'a> Fc<'a> {
    /// The registers below `below` that hold a value now, in increasing order.
    fn live_below(&self, below: u32) -> Vec<u32> {
        let mut out = Vec::new();
        for (w, &word) in self.live.iter().enumerate() {
            let mut word = word;
            while word != 0 {
                let r = w as u32 * 64 + word.trailing_zeros();
                word &= word - 1;
                if r < below {
                    out.push(r);
                }
            }
        }
        out
    }

    /// Put a save list in the pool: the live registers below `below`, and where a resume writes the sent value and the mode. Gives its
    /// offset in `Code::tables`.
    pub(crate) fn save_list(&mut self, sent: u32, mode: u32, below: u32) -> u32 {
        let at = self.tables.len() as u32;
        let regs = self.live_below(below);
        self.gen_regs = self.gen_regs.max(regs.len() as u16);
        self.tables.extend([sent, mode, regs.len() as u32]);
        self.tables.extend(regs);
        at
    }

    /// `GenInit`: the end of the call of a generator (see the module comment).
    pub(crate) fn gen_init(&mut self) {
        if self.dead {
            return;
        }
        let t = self.save_list(NO_REG, NO_REG, u32::MAX);
        self.op(Op::GenInit, 0, t, 0);
        if self.gen_kind == GK_ASYNCGEN {
            // The body of an async generator runs in the range of a catch-all handler: a throw rejects the request (`async_catch_all`).
            let ex = self.alloc();
            self.async_h = Some((self.asm.len(), ex, self.depth));
        }
    }

    /// `yield arg` and `yield* arg` into `dst`.
    pub(crate) fn yield_expr(&mut self, arg: Option<ExprId>, delegate: bool, dst: u32) -> CR {
        if self.gen_kind == GK_ASYNCGEN {
            return self.agen_yield_expr(arg, delegate, dst);
        }
        if delegate {
            return self.yield_star(arg.expect("yield* has an operand"), dst);
        }
        let m0 = self.mark();
        let v = match arg {
            Some(e) => self.any(e)?,
            None => {
                let t = self.alloc();
                self.op(Op::LoadUndef, t, 0, 0);
                self.set_live(t);
                t
            }
        };
        // The sent value goes to `dst` when nothing reads `dst` after a `throw()`: a register that holds no value now.
        let direct = dst < m0 && !self.is_var_reg(dst) && self.live.get(dst as usize / 64).is_none_or(|w| w >> (dst % 64) & 1 == 0);
        let s = if direct { dst } else { self.alloc() };
        let m = self.alloc();
        if self.dead {
            self.free(m0);
            return Ok(());
        }
        let t = self.save_list(s, m, m0);
        self.op(Op::Yield, v, t, 0);
        // A resume brings back the list, `s` and `m` only: the temporaries of the operand hold no value now.
        (m0..self.next).for_each(|r| Self::bit(&mut self.live, r, false));
        self.set_live(s);
        self.set_live(m);
        let (cont, thr) = (self.label(), self.label());
        self.jmp_rx(Op::JSEqI, m, MODE_NEXT as u32, cont);
        self.jmp_rx(Op::JSEqI, m, MODE_THROW as u32, thr);
        self.unwind(0)?;
        self.op(Op::RetGen, s, 0, 0);
        self.bind(thr);
        self.op(Op::Throw, s, 0, 0);
        self.bind(cont);
        if s != dst {
            self.op(Op::Mov, dst, s, 0);
        }
        self.free(m0);
        Ok(())
    }

    /// `yield* arg` into `dst` (see the module comment). The window holds the iterator record, the sent value and the mode; each turn of the
    /// loop sends them on with `YieldStarStep`, which yields what the inner iterator gave and ends with its value (or its return).
    fn yield_star(&mut self, arg: ExprId, dst: u32) -> CR {
        let m0 = self.mark();
        let w = self.window(YS_REGS);
        let v = self.any(arg)?;
        self.op(Op::IterOpen, w, v, 1);
        // `v` is dead now, and a resume does not bring it back.
        self.free(w + YS_REGS);
        (0..ITER_REGS).for_each(|i| self.set_live(w + i));
        let (sent, mode) = (w + ITER_REGS, w + ITER_REGS + 1);
        self.op(Op::LoadUndef, sent, 0, 0);
        self.op(Op::LoadI, mode, MODE_NEXT as u32, 0);
        self.set_live(sent);
        self.set_live(mode);
        let out = self.alloc();
        let (head, done, ret, end) = (self.label(), self.label(), self.label(), self.label());
        self.bind(head);
        self.mark_used(head);
        self.jmp_rx(Op::YieldStarStep, out, w, done);
        self.set_live(out);
        if self.dead {
            self.free(m0);
            return Ok(());
        }
        let t = self.save_list(sent, mode, sent);
        self.op(Op::YieldResult, out, t, 0);
        // A resume does not bring `out` back: it holds no value at the back edge.
        Self::bit(&mut self.live, out, false);
        self.jmp_back(head);
        self.bind(done);
        self.set_live(out);
        self.jmp_rx(Op::JSEqI, mode, MODE_RETURN as u32, ret);
        self.op(Op::Mov, dst, out, 0);
        self.jmp(end);
        self.bind(ret);
        self.unwind(0)?;
        self.op(Op::RetGen, out, 0, 0);
        self.bind(end);
        self.free(m0);
        Ok(())
    }

    /// The return of the function with the value in `r`: `RetGen` in a generator, else `Ret`.
    pub(crate) fn ret_op(&mut self, r: u32) {
        self.op(
            match self.gen_kind {
                GK_NONE => Op::Ret,
                GK_GEN | GK_ASYNCGEN => Op::RetGen,
                _ => Op::RetAsync,
            },
            r,
            0,
            0,
        );
    }

    /// The return of `undefined`.
    pub(crate) fn ret_undef(&mut self) {
        if self.gen_kind == GK_NONE {
            self.op(Op::RetUndef, 0, 0, 0);
            return;
        }
        let m = self.mark();
        let t = self.alloc();
        self.op(Op::LoadUndef, t, 0, 0);
        self.set_live(t);
        self.ret_op(t);
        self.free(m);
    }

    /// `new.target`: `undefined` in a generator (it is called, never constructed), else the word of the frame.
    pub(crate) fn load_new_target(&mut self, dst: u32) {
        self.op(if self.gen_kind != 0 { Op::LoadUndef } else { Op::LoadNewTarget }, dst, 0, 0);
    }
}

#[cfg(test)]
#[path = "compile_gen_tests.rs"]
mod tests;
