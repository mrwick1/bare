//! The text of an expression for a TypeError message (`o.f is not a function`), as V8 prints it. Cold: only the throw path comes here.
//!
//! A register has no name in the bytecode. The text comes from the op that wrote the register last, before the throwing `pc` in word
//! order: `GetField` gives `base.name` (the name is the atom of its site), `GetGlobal` the name, `Mov` the text of its source. A local
//! variable or a context slot has no name, so it prints as `(intermediate value)`, as V8 prints what it cannot name. The scan is a
//! hypothesis about control flow: it takes the last writer in word order, which is the writer of the callee for the code that the
//! compiler emits for a call, a `new` and a `for-of`.
use crate::bytecode::{Code, Op, decode};
use crate::interp_rt::Realm;
use crate::value::Value;

const UNNAMED: &str = "(intermediate value)";

impl Realm {
    /// The text of the expression that wrote register `reg` before the instruction at `pc` of `code`.
    #[cold]
    #[inline(never)]
    pub(crate) fn expr_text(&mut self, code: &Code, pc: usize, reg: usize) -> String {
        let mut ins = Vec::new();
        let mut at = 0;
        while at < pc {
            let Ok((i, n)) = decode(&code.words, at) else { break };
            ins.push(i);
            at += n;
        }
        let mut out = String::new();
        self.expr_of(code, &ins, reg, 6, &mut out);
        out
    }

    fn atom_text(&mut self, code: &Code, atom: u32) -> String {
        let s = self.atoms.adopt(code.atoms[atom as usize]);
        self.text(s)
    }

    fn expr_of(&mut self, code: &Code, ins: &[crate::bytecode::Ins], reg: usize, depth: u32, out: &mut String) {
        let Some(w) = ins.iter().rposition(|i| (0..i.op.form().fields()).any(|f| i.op.letter(f) == 'w' && i.field(f) as usize == reg)) else {
            out.push_str(UNNAMED);
            return;
        };
        let (i, before) = (ins[w], &ins[..w]);
        if depth == 0 {
            out.push_str(UNNAMED);
            return;
        }
        let d = depth - 1;
        match i.op {
            Op::GetField => {
                self.expr_of(code, before, i.b as usize, d, out);
                out.push('.');
                out.push_str(&self.atom_text(code, code.sites[i.c as usize]));
            }
            Op::GetGlobal | Op::GetName | Op::TypeofGlobal => out.push_str(&self.atom_text(code, code.sites[i.b as usize])),
            Op::GetElem => {
                self.expr_of(code, before, i.b as usize, d, out);
                match before.iter().rposition(|k| k.op.letter(0) == 'w' && k.a == i.c) {
                    Some(k) if before[k].op == Op::LoadAtom => {
                        out.push('.');
                        out.push_str(&self.atom_text(code, before[k].b));
                    }
                    _ => {
                        out.push('[');
                        self.expr_of(code, before, i.c as usize, d, out);
                        out.push(']');
                    }
                }
            }
            Op::GetElemI => {
                self.expr_of(code, before, i.b as usize, d, out);
                out.push_str(&format!("[{}]", i.c));
            }
            Op::GetCtxField => {
                out.push_str(UNNAMED);
                out.push('.');
                out.push_str(&self.atom_text(code, code.sites[i.x as usize]));
            }
            Op::Mov => self.expr_of(code, before, i.b as usize, d, out),
            Op::LoadThis | Op::LoadThisS => out.push_str("this"),
            Op::LoadUndef => out.push_str("undefined"),
            Op::LoadNull => out.push_str("null"),
            Op::LoadTrue => out.push_str("true"),
            Op::LoadFalse => out.push_str("false"),
            Op::LoadI => out.push_str(&(i.b as i32).to_string()),
            Op::LoadAtom => out.push_str(&format!("\"{}\"", self.atom_text(code, i.b))),
            Op::Call | Op::CallU | Op::CallSpread | Op::EvalCall => {
                self.expr_of(code, before, i.b as usize, d, out);
                out.push_str("(...)");
            }
            _ => out.push_str(UNNAMED),
        }
    }

    /// The TypeError `{expr} {tail}` for the expression in register `reg` (`o.f is not a function`).
    #[cold]
    #[inline(never)]
    pub(crate) fn named_error(&mut self, code: &Code, pc: usize, reg: usize, tail: &str) -> Value {
        let n = self.expr_text(code, pc, reg);
        self.error(crate::interp_rt::ErrKind::Type, &format!("{n} {tail}"))
    }

    /// The TypeError of a property access on `undefined` or `null`: `Cannot read properties of null (reading 'k')`.
    #[cold]
    #[inline(never)]
    pub(crate) fn key_error(&mut self, recv: Value, key: Value, verb: &str, ing: &str) -> Value {
        let (what, k) = (if recv.is_null() { "null" } else { "undefined" }, self.key_name(key));
        self.error(crate::interp_rt::ErrKind::Type, &format!("Cannot {verb} properties of {what} ({ing} '{k}')"))
    }

    /// The key of a property access as V8 prints it in `(reading 'k')`: a string or a number as it is, anything else by its kind.
    /// No script runs: an object key prints as `#<Object>`.
    #[cold]
    pub(crate) fn key_name(&mut self, v: Value) -> String {
        if let Some(s) = Realm::str_of(v) {
            return self.text(s);
        }
        if let Some(i) = v.as_int() {
            return i.to_string();
        }
        if let Some(f) = v.as_f64() {
            return f.to_string();
        }
        if Realm::obj(v).is_some() {
            return "#<Object>".to_string();
        }
        match Realm::sym_of(v) {
            Some(s) => format!("Symbol({})", self.text(s)),
            None if v.is_undefined() => "undefined".to_string(),
            None if v.is_null() => "null".to_string(),
            None => "?".to_string(),
        }
    }
}
