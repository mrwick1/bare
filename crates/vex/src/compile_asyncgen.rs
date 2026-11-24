//! Async generators and `for await` in the compiler (`docs/design-js2-async.md` 6.4; plan step A2). The runtime side is `asyncgen.rs`.
//!
//! **Entry.** As a generator (`GenInit` after the parameters), and as an async function the body is in the range of a catch-all handler
//! that ends in `RetGen ex, 1` (a throw rejects the request that waits, `compile_async.rs`).
//!
//! **`yield v`** waits for `v` first, then `Yield`s it; a resume dispatches on the mode as that of a generator does (`compile_gen.rs`), and a
//! `return(x)` waits for `x` before it returns:
//!
//! ```text
//! Await v            ; the operand
//! Yield aw           ; save list: sent s, mode m
//! JSEqI m, 0 -> cont
//! JSEqI m, 1 -> thr
//! Await s            ; return(x): wait for x (a rejection is thrown into the body), then run the finalizers and RetGen
//! ```
//!
//! **`return x`** waits for `x`, then returns. **`for await (x of y)`** is the loop of `for-of` with `AIterOpen` for `IterOpen`, and
//! `AIterNext`, an `await` and `AIterUnpack` for `IterNext`. A jump out of it closes the iterator with `AIterClose` and an `await` of what
//! `return()` gave ([`Fc::aiter_close`]). **`yield*`** is a loop as in `compile_gen.rs`, with `AYsCall`, an `await` and `AYsUnpack` for the step.
use super::*;
use crate::bytecode::{ITER_REGS, YS_REGS};
use crate::generator::{MODE_NEXT, MODE_RETURN, MODE_THROW};

/// The mode that `AYsCall` writes when the iterator has no `throw`.
const MODE_NO_THROW: u32 = 3;

impl<'a> Fc<'a> {
    /// The value of `e`, waited for, in a new register: the operand of `return` in an async generator.
    pub(crate) fn await_operand(&mut self, e: ExprId) -> CR<u32> {
        let t = self.alloc();
        self.to(e, t)?;
        let m0 = self.mark();
        self.await_reg(t, t, m0, true);
        self.free(m0);
        Ok(t)
    }

    /// `yield arg` and `yield* arg` of an async generator, into `dst`.
    pub(crate) fn agen_yield_expr(&mut self, arg: Option<ExprId>, delegate: bool, dst: u32) -> CR {
        if delegate {
            return self.agen_yield_star(arg.expect("yield* has an operand"), dst);
        }
        let m_outer = self.mark();
        // The waited value: a register below the temporaries of the operand, so that the wait writes it in place.
        let aw = self.alloc();
        let m0 = self.mark();
        match arg {
            Some(e) => {
                let v = self.any(e)?;
                self.await_reg(v, aw, m0, true);
            }
            None => {
                let t = self.alloc();
                self.op(Op::LoadUndef, t, 0, 0);
                self.set_live(t);
                self.await_reg(t, aw, m0, true);
            }
        }
        self.free(m0);
        self.set_live(aw);
        let direct = dst < m_outer && !self.is_var_reg(dst) && self.live.get(dst as usize / 64).is_none_or(|w| w >> (dst % 64) & 1 == 0);
        let s = if direct { dst } else { self.alloc() };
        let m = self.alloc();
        if self.dead {
            self.free(m_outer);
            return Ok(());
        }
        let t = self.save_list(s, m, m_outer);
        self.op(Op::Yield, aw, t, 0);
        (m_outer..self.next).for_each(|r| Self::bit(&mut self.live, r, false));
        self.set_live(s);
        self.set_live(m);
        let (cont, thr) = (self.label(), self.label());
        self.jmp_rx(Op::JSEqI, m, MODE_NEXT as u32, cont);
        self.jmp_rx(Op::JSEqI, m, MODE_THROW as u32, thr);
        // `return(x)`: wait for `x`; a rejection is thrown in the body.
        let m1 = self.mark();
        self.await_reg(s, s, m1, true);
        self.free(m1);
        self.unwind(0)?;
        self.op(Op::RetGen, s, 0, 0);
        self.bind(thr);
        self.op(Op::Throw, s, 0, 0);
        self.bind(cont);
        if s != dst {
            self.op(Op::Mov, dst, s, 0);
        }
        self.free(m_outer);
        Ok(())
    }

    /// `yield* arg` of an async generator into `dst` (see the module comment). The window holds the iterator record, the sent value and the mode.
    fn agen_yield_star(&mut self, arg: ExprId, dst: u32) -> CR {
        let m0 = self.mark();
        let w = self.window(YS_REGS);
        let v = self.any(arg)?;
        self.op(Op::AIterOpen, w, v, 0);
        self.free(w + YS_REGS);
        (0..ITER_REGS).for_each(|i| self.set_live(w + i));
        let (sent, mode) = (w + ITER_REGS, w + ITER_REGS + 1);
        self.op(Op::LoadUndef, sent, 0, 0);
        self.op(Op::LoadI, mode, MODE_NEXT as u32, 0);
        self.set_live(sent);
        self.set_live(mode);
        let out = self.alloc();
        let (head, special, done, ret, end, back) = (self.label(), self.label(), self.label(), self.label(), self.label(), self.label());
        self.bind(head);
        self.mark_used(head);
        // The no-throw branch of AYsCall leaves out untouched.
        self.op(Op::LoadUndef, out, 0, 0);
        self.set_live(out);
        self.jmp_rx(Op::AYsCall, out, w, special);
        let m1 = self.mark();
        self.await_reg(out, out, m1, true);
        self.free(m1);
        self.jmp_rx(Op::AYsUnpack, out, out, done);
        self.set_live(out);
        if self.dead {
            self.free(m0);
            return Ok(());
        }
        let t = self.save_list(sent, mode, sent);
        self.op(Op::Yield, out, t, 0);
        // A resume does not bring `out` back.
        Self::bit(&mut self.live, out, false);
        // `return(x)` while the iterator runs: wait for `x` (a rejection becomes a throw into the iterator), then ask the iterator.
        self.jmp_rx(Op::JSNeI, mode, MODE_RETURN as u32, back);
        let m1 = self.mark();
        let (s2, m2) = (self.alloc(), self.alloc());
        self.await_raw(sent, s2, m2, m1);
        self.op(Op::Mov, sent, s2, 0);
        self.jmp_rx(Op::JSEqI, m2, MODE_NEXT as u32, back);
        self.op(Op::LoadI, mode, MODE_THROW as u32, 0);
        self.free(m1);
        self.bind(back);
        self.jmp_back(head);
        // The iterator has no `throw` (mode 3), or no `return` (`out` holds the sent value).
        self.bind(special);
        self.set_live(out);
        let nothrow = self.label();
        self.jmp_rx(Op::JSEqI, mode, MODE_NO_THROW, nothrow);
        let m1 = self.mark();
        self.await_reg(out, out, m1, true);
        self.free(m1);
        self.unwind(0)?;
        self.op(Op::RetGen, out, 0, 0);
        self.bind(nothrow);
        // Close the iterator, then fail: a rejection of the close wins over the TypeError.
        self.aiter_close(w, false)?;
        let t = self.alloc();
        self.op(Op::LoadNull, t, 0, 0);
        self.set_live(t);
        self.op(Op::AIterCheck, t, 1, 0);
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

    /// One step of a `for await` loop: `t` = the value of the next result of the record `rec`, or a jump to `brk` at the end.
    pub(crate) fn aiter_step(&mut self, rec: u32, t: u32, brk: Lb) -> CR {
        self.op(Op::AIterNext, t, rec, 0);
        self.set_live(t);
        let m0 = self.mark();
        self.await_reg(t, t, m0, true);
        self.free(m0);
        self.jmp_rx(Op::AIterUnpack, t, t, brk);
        Ok(())
    }

    /// The close of the iterator of the record `rec` that a jump out of a loop runs: `IterClose`, or for `for await` the close that waits
    /// for what `return()` gave (`throwing`: an exception leaves the loop, so every error of the close is dropped).
    pub(crate) fn iter_close_op(&mut self, rec: u32, throwing: bool) -> CR {
        if self.await_recs.contains(&rec) {
            return self.aiter_close(rec, throwing);
        }
        self.op(Op::IterClose, rec, 0, throwing as u32);
        Ok(())
    }

    /// `AsyncIteratorClose` of the record `rec`: call `return()`, wait for the result, and (unless `throwing`) check that it is an object.
    pub(crate) fn aiter_close(&mut self, rec: u32, throwing: bool) -> CR {
        if self.dead {
            return Ok(());
        }
        let m0 = self.mark();
        let t = self.alloc();
        let skip = self.label();
        self.jmp_rx(if throwing { Op::AIterCloseT } else { Op::AIterClose }, t, rec, skip);
        self.set_live(t);
        let m1 = self.mark();
        self.await_reg(t, t, m1, !throwing);
        self.free(m1);
        if !throwing {
            self.op(Op::AIterCheck, t, 0, 0);
        }
        self.bind(skip);
        self.free(m0);
        Ok(())
    }
}
