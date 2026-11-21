//! Async functions in the compiler (`docs/design-js2-async.md` 5.2 and 6.3; plan step A1). The runtime side is `generator.rs`.
//!
//! **Entry.** `AsyncInit` is the first instruction. It makes the activation and the result promise, and the function runs on in the frame
//! of its caller, synchronously, until the first `await`. A throw before that (in a default of a parameter, say) rejects the promise: the
//! whole code after `AsyncInit` is in the range of one catch-all handler, which runs `RetAsync ex, 1`.
//!
//! **Suspension.** `await v` is `Await v` with a save list, as `yield v` is (`compile_gen.rs`). The activation is the handler of a reaction
//! on the promise of `v`. When the promise settles, the job writes the value into `sent` and the mode (0 fulfilled, 1 rejected) into
//! `mode`, and the code after the `Await` dispatches on the mode:
//!
//! ```text
//! Await v            ; save list: sent s, mode m
//! JSEqI m, 0 -> cont ; fulfilled: the value of the expression is s
//! Throw s            ; rejected: the handlers of the code around catch it
//! cont:
//! ```
//!
//! An async function returns with `RetAsync`, never with `Ret` (the verifier checks it). `new.target` is `undefined` in it.
use super::*;
use crate::bytecode::GK_ASYNC;
use crate::generator::{MODE_NEXT, MODE_THROW};

impl<'a> Fc<'a> {
    /// `AsyncInit` and the start of the range of the catch-all handler, in an async function.
    pub(crate) fn async_init(&mut self) {
        if self.gen_kind != GK_ASYNC {
            return;
        }
        self.op(Op::AsyncInit, 0, 0, 0);
        // The exception lands here. The register is below every variable and temporary, and no map names it.
        let ex = self.alloc();
        self.async_h = Some((self.asm.len(), ex, self.depth));
    }

    /// The catch-all handler of an async function: the range from after `AsyncInit` to `end`, and the block that rejects the promise.
    pub(crate) fn async_catch_all(&mut self, end: usize) {
        let Some((start, ex, depth)) = self.async_h else { return };
        let lb = self.label();
        self.mark_used(lb);
        self.bind(lb);
        self.live.iter_mut().for_each(|w| *w = 0);
        self.set_live(ex);
        self.op(Op::RetAsync, ex, 0, 1);
        self.handlers.push(HandlerRec { start, end, target: lb, reg: ex, depth });
    }

    /// `await arg` into `dst`.
    pub(crate) fn await_expr(&mut self, arg: ExprId, dst: u32) -> CR {
        let m0 = self.mark();
        let v = self.any(arg)?;
        // The settled value goes to `dst` when nothing reads `dst` after a rejection: a register that holds no value now.
        let direct = dst < m0 && !self.is_var_reg(dst) && self.live.get(dst as usize / 64).is_none_or(|w| w >> (dst % 64) & 1 == 0);
        let s = if direct { dst } else { self.alloc() };
        let m = self.alloc();
        if self.dead {
            self.free(m0);
            return Ok(());
        }
        let t = self.save_list(s, m, m0);
        self.op(Op::Await, v, t, 0);
        // A resume brings back the list, `s` and `m` only: the temporaries of the operand hold no value now.
        (m0..self.next).for_each(|r| Self::bit(&mut self.live, r, false));
        self.set_live(s);
        self.set_live(m);
        let cont = self.label();
        self.jmp_rx(Op::JSEqI, m, MODE_NEXT as u32, cont);
        debug_assert_eq!(MODE_THROW, 1);
        self.op(Op::Throw, s, 0, 0);
        self.bind(cont);
        if s != dst {
            self.op(Op::Mov, dst, s, 0);
        }
        self.free(m0);
        Ok(())
    }
}

#[cfg(test)]
#[path = "compile_async_tests.rs"]
mod tests;
