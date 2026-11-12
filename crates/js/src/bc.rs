//! The bytecode format of the register interpreter (lane B of `docs/engine-plan.md`): opcodes, encoding, decoding, the code
//! object and a disassembler. Nothing here runs code. The code generator (step B2) writes this format and the loop (step B3)
//! reads it. The design record is in `docs/decisions.md` ("Bytecode").
//!
//! One instruction is one 32-bit word, `[op:8][a:8][b:8][c:8]` with `op` in the low byte (JS: think of a `Uint32Array`, where
//! `w & 255` is the op and `w >> 8 & 255` is the first operand). A word has one of four forms:
//! - `Abc`: three 8-bit operands. `AbcX`: the same word, then one raw 32-bit word `x` (used where an instruction has four operands).
//! - `Abx`: `a` and one 16-bit unsigned operand `bx` (a constant or a function number).
//! - `AsBx`: `a` and one 16-bit signed operand (a conditional jump, or a small integer).
//! - `Sj`: one 24-bit signed operand (an unconditional jump).
//!
//! A jump offset counts words from the word after the instruction. An operand that does not fit uses the wide form: a prefix word
//! `[Wide][op][a:16]` and then the rest of the operands in one word (`Abc`: `[b:16][c:16]`; `Abx`, `AsBx` and `Sj`: one 32-bit
//! value) and, for `AbcX`, the raw word `x` after that. This is the `Wide` prefix of Ignition and the long forms of Hermes. A
//! compiler never needs to know the width: [`encode`] picks the narrow form when every operand fits.
use crate::heap::Ic;
use std::cell::Cell;
use std::fmt::Write;

/// The operand layout of an instruction word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    Abc,
    AbcX,
    Abx,
    AsBx,
    Sj,
}

/// `ops!` makes the opcode enum and its tables from one list, so the name, the form and the operand letters cannot drift apart.
/// The letters say what each operand is, in the order a, b, c, x (for `Abx` and `AsBx`: a, then the wide operand):
/// `r` register, `k` constant number, `c` inline cache number, `f` function number, `n` plain number, `i` signed number,
/// `j` jump offset, `s` scope number (an index of the scope table of the tree walker), `-` an operand that is not used (always 0).
/// A letter list shorter than the form means the operands that are left are 0.
macro_rules! ops {
    ($($name:ident $form:ident $spec:literal $doc:literal,)*) => {
        /// The opcodes. The numbers are not stable until the loop (step B3) ships. Step B2 may add ops.
        #[repr(u8)]
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Op {
            $(#[doc = $doc] $name,)*
        }
        impl Op {
            /// Every opcode, in the order of its number.
            pub const ALL: &'static [Op] = &[$(Op::$name,)*];
            pub fn form(self) -> Form {
                match self { $(Op::$name => Form::$form,)* }
            }
            pub fn name(self) -> &'static str {
                match self { $(Op::$name => stringify!($name),)* }
            }
            /// The operand letters (see `ops!`).
            pub fn spec(self) -> &'static str {
                match self { $(Op::$name => $spec,)* }
            }
        }
    };
}

ops! {
    Nop Abc "" "Do nothing.",
    Wide Abc "" "The prefix of a wide instruction. Never an instruction of its own.",
    Mov Abc "r r" "`a = b`.",
    LoadUndef Abc "r" "`a = undefined`.",
    LoadNull Abc "r" "`a = null`.",
    LoadTrue Abc "r" "`a = true`.",
    LoadFalse Abc "r" "`a = false`.",
    LoadThis Abc "r" "`a = this`.",
    LoadI AsBx "r i" "`a` = the small integer `sbx`.",
    LoadK Abx "r k" "`a` = constant `bx`.",
    GetEnv Abc "r n n" "`a` = the binding `c` of the environment `b` levels up. A TDZ binding throws.",
    SetEnv Abc "r n n" "The binding `c` of the environment `b` levels up = `a`. Checks TDZ and `const`.",
    InitEnv Abc "r n n" "Initialize that binding with `a`. No TDZ check.",
    PushEnv Abx "- s" "Push a new environment with the bindings of scope `bx`, in the order and with the flags of `bc_binds`. A `let`, `const` or class binding starts in its TDZ.",
    DeclEnv Abx "- s" "Add to the current environment the bindings of scope `bx` from its current count on (the `let`, `const` and function names of a function body, after `bind_call`).",
    PopEnv Abc "" "Go back to the parent environment.",
    CopyEnv Abc "" "Replace the current environment with a copy of it if a closure captured it (one iteration of a `for` with `let`, as `copy_env`).",
    GetGlobal Abx "r k" "`a` = the global named by constant `bx`. A missing name throws a ReferenceError.",
    SetGlobal Abx "r k" "The global named by constant `bx` = `a`.",
    TypeofGlobal Abx "r k" "`a` = `typeof` the global named by constant `bx`. A missing name gives `\"undefined\"`.",
    GetName Abx "r k" "`a` = the name of constant `bx`, by the full search of the tree walker (environments, then globals). A missing name throws a ReferenceError.",
    SetName Abx "r k" "The name of constant `bx` = `a`, by the full search.",
    TypeofName Abx "r k" "`a` = `typeof` the name of constant `bx`, by the full search. A missing name gives `\"undefined\"`.",
    GetField AbcX "r r c k" "`a = b.name`. `c` is the inline cache, `x` the constant of the name.",
    SetField AbcX "r r c k" "`a.name = b`. `c` is the inline cache, `x` the constant of the name.",
    GetElem Abc "r r r" "`a = b[c]`.",
    SetElem Abc "r r r" "`a[b] = c`.",
    ToKey Abc "r r r" "`a` = the property key of `c` (ToPropertyKey). A `null` or `undefined` `b` throws first, as `b[c]` does.",
    DefField Abc "r r k" "Define the own data property named by constant `c` on `a` with the value `b` (an object literal).",
    ArrayPush Abc "r r" "Append `b` to the array `a` (an array literal).",
    NewObject Abc "r" "`a = {}`.",
    NewArray Abc "r n" "`a = []`. `b` is the expected length.",
    Closure Abx "r f" "`a` = a new function for the function number `bx` of the script, in the current environment.",
    ClosureName AbcX "r k - f" "`a` = a new function for the function number `x`, named by constant `b` (an anonymous function that takes the name of its binding or key).",
    Call Abc "r r n" "`a = b(...)`. The callee is in `b`, `this` in `b+1`, the `c` arguments in `b+2` and after.",
    New Abc "r r n" "`a = new b(...)`. The constructor is in `b` and the `c` arguments in `b+1` and after.",
    Add Abc "r r r" "`a = b + c`.",
    Sub Abc "r r r" "`a = b - c`.",
    Mul Abc "r r r" "`a = b * c`.",
    Div Abc "r r r" "`a = b / c`.",
    Mod Abc "r r r" "`a = b % c`.",
    Exp Abc "r r r" "`a = b ** c`.",
    BitAnd Abc "r r r" "`a = b & c`.",
    BitOr Abc "r r r" "`a = b | c`.",
    BitXor Abc "r r r" "`a = b ^ c`.",
    Shl Abc "r r r" "`a = b << c`.",
    Shr Abc "r r r" "`a = b >> c`.",
    UShr Abc "r r r" "`a = b >>> c`.",
    Eq Abc "r r r" "`a = b == c`.",
    Ne Abc "r r r" "`a = b != c`.",
    StrictEq Abc "r r r" "`a = b === c`.",
    StrictNe Abc "r r r" "`a = b !== c`.",
    Lt Abc "r r r" "`a = b < c`.",
    Le Abc "r r r" "`a = b <= c`.",
    Gt Abc "r r r" "`a = b > c`.",
    Ge Abc "r r r" "`a = b >= c`.",
    In Abc "r r r" "`a = b in c`.",
    InstanceOf Abc "r r r" "`a = b instanceof c`.",
    Neg Abc "r r" "`a = -b`.",
    Plus Abc "r r" "`a = +b`.",
    Not Abc "r r" "`a = !b`.",
    BitNot Abc "r r" "`a = ~b`.",
    Typeof Abc "r r" "`a = typeof b`.",
    Inc Abc "r r" "`a = b + 1` (after ToNumeric).",
    Dec Abc "r r" "`a = b - 1` (after ToNumeric).",
    ToNumeric Abc "r r" "`a = ToNumeric(b)`.",
    ToStr Abc "r r" "`a = ToString(b)`.",
    Concat Abc "r r n" "`a` = the strings in the `c` registers from `b`, joined (a template literal).",
    Delete Abc "r r r" "`a = delete b[c]`.",
    Jmp Sj "j" "Jump.",
    JmpIf AsBx "r j" "Jump when `a` is truthy.",
    JmpIfNot AsBx "r j" "Jump when `a` is falsy.",
    JmpNullish AsBx "r j" "Jump when `a` is `undefined` or `null`.",
    JmpNotNullish AsBx "r j" "Jump when `a` is neither `undefined` nor `null`.",
    Ret Abc "r" "Return `a`.",
    RetUndef Abc "" "Return `undefined`.",
    Throw Abc "r" "Throw `a`.",
}

impl Op {
    /// The opcode of a byte, or `None` for a byte that is no opcode.
    pub fn from_u8(b: u8) -> Option<Op> {
        Op::ALL.get(b as usize).copied()
    }
}

/// One decoded instruction. What each field means depends on `op.form()`:
/// - `Abc`: `a`, `b`, `c`. `AbcX`: the same and `x`.
/// - `Abx`: `a` and `b` (the 16-bit or 32-bit operand). `AsBx`: `a` and `b`, where `b as i32` is the signed operand.
/// - `Sj`: `a`, where `a as i32` is the signed operand. `b` and `c` are 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ins {
    pub op: Op,
    pub a: u32,
    pub b: u32,
    pub c: u32,
    pub x: u32,
}

impl Ins {
    pub fn new(op: Op, a: u32, b: u32, c: u32) -> Ins {
        Ins { op, a, b, c, x: 0 }
    }
    pub fn with_x(self, x: u32) -> Ins {
        Ins { x, ..self }
    }
    /// The signed operand of a jump (`AsBx` and `Sj`), in words from the word after the instruction.
    pub fn offset(&self) -> i32 {
        if self.op.form() == Form::Sj { self.a as i32 } else { self.b as i32 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BcError {
    /// An operand is too large for the widest form of the instruction.
    Operand,
    /// The position is past the end of the code.
    End,
    /// The byte is no opcode.
    BadOp(u8),
    /// A `Wide` prefix has no instruction after it, or one that cannot be wide, or is cut off.
    BadWide,
}

const W: u32 = Op::Wide as u32;

/// Append `ins` to `ops` in the narrowest form that holds its operands. Gives the number of words written.
pub fn encode(ops: &mut Vec<u32>, ins: &Ins) -> Result<usize, BcError> {
    let Ins { op, a, b, c, x } = *ins;
    let o = op as u32;
    let start = ops.len();
    match op.form() {
        Form::Abc | Form::AbcX => {
            if a < 256 && b < 256 && c < 256 {
                ops.push(o | a << 8 | b << 16 | c << 24);
            } else if a < 65536 && b < 65536 && c < 65536 {
                ops.extend([W | o << 8 | a << 16, b | c << 16]);
            } else {
                return Err(BcError::Operand);
            }
            if op.form() == Form::AbcX {
                ops.push(x);
            }
        }
        Form::Abx => {
            if a < 256 && b < 65536 {
                ops.push(o | a << 8 | b << 16);
            } else if a < 65536 {
                ops.extend([W | o << 8 | a << 16, b]);
            } else {
                return Err(BcError::Operand);
            }
        }
        Form::AsBx => {
            let s = b as i32;
            if a < 256 && s >= i16::MIN as i32 && s <= i16::MAX as i32 {
                ops.push(o | a << 8 | (s as u32 & 0xffff) << 16);
            } else if a < 65536 {
                ops.extend([W | o << 8 | a << 16, b]);
            } else {
                return Err(BcError::Operand);
            }
        }
        Form::Sj => {
            let s = a as i32;
            if (-(1 << 23)..1 << 23).contains(&s) {
                ops.push(o | (s as u32) << 8);
            } else {
                ops.extend([W | o << 8, a]);
            }
        }
    }
    Ok(ops.len() - start)
}

/// Decode the instruction at word `pc`. Gives it and the number of words it takes.
pub fn decode(ops: &[u32], pc: usize) -> Result<(Ins, usize), BcError> {
    let w = *ops.get(pc).ok_or(BcError::End)?;
    let op = Op::from_u8(w as u8).ok_or(BcError::BadOp(w as u8))?;
    if op == Op::Wide {
        let inner = Op::from_u8((w >> 8) as u8).filter(|o| *o != Op::Wide).ok_or(BcError::BadWide)?;
        let a = w >> 16;
        let w2 = *ops.get(pc + 1).ok_or(BcError::BadWide)?;
        return Ok(match inner.form() {
            Form::Abc => (Ins::new(inner, a, w2 & 0xffff, w2 >> 16), 2),
            Form::AbcX => (Ins::new(inner, a, w2 & 0xffff, w2 >> 16).with_x(*ops.get(pc + 2).ok_or(BcError::BadWide)?), 3),
            Form::Abx | Form::AsBx => (Ins::new(inner, a, w2, 0), 2),
            Form::Sj => (Ins::new(inner, w2, 0, 0), 2),
        });
    }
    let (a, b, c) = (w >> 8 & 255, w >> 16 & 255, w >> 24);
    Ok(match op.form() {
        Form::Abc => (Ins::new(op, a, b, c), 1),
        Form::AbcX => (Ins::new(op, a, b, c).with_x(*ops.get(pc + 1).ok_or(BcError::End)?), 2),
        Form::Abx => (Ins::new(op, a, w >> 16, 0), 1),
        Form::AsBx => (Ins::new(op, a, (w as i32 >> 16) as u32, 0), 1),
        Form::Sj => (Ins::new(op, (w as i32 >> 8) as u32, 0, 0), 1),
    })
}

/// A constant of a function. Strings are UTF-16 code units, as the syntax tree decodes them (`Program::string`).
#[derive(Clone, Debug, PartialEq)]
pub enum Const {
    Number(f64),
    String(Box<[u16]>),
    /// An interned name of the interpreter (a property name or a global name).
    Name(u32),
}

/// A range of words with a handler. When an exception is thrown at a word in `start..end`, the exception goes into register `reg`
/// and the run continues at `target`. A `finally` block uses a completion register and ordinary jumps. The first handler in the
/// table whose range holds the word is the innermost, so the code generator writes inner ranges first. `depth` is the number of
/// environments that the code of the function pushed (`PushEnv`) at the handler: the loop pops the environments above it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Handler {
    pub start: u32,
    pub end: u32,
    pub target: u32,
    pub reg: u16,
    pub depth: u16,
}

/// The code of one function (or of a script body).
pub struct Code {
    /// The instruction words.
    pub ops: Vec<u32>,
    pub consts: Vec<Const>,
    /// One inline cache for each property instruction. The operand `c` of `GetField` and `SetField` is the index.
    pub ics: Box<[Cell<Ic>]>,
    pub handlers: Vec<Handler>,
    /// Pairs `(word, source offset)`, sorted by word. The offset of a word is that of the last pair at or before it.
    pub lines: Vec<(u32, u32)>,
    /// The number of registers of a frame.
    pub nregs: u16,
    /// Pairs `(word, expression)` for each `Call` and `New`, sorted by word: the call or `new` node, for the position of
    /// `Error.stack` and for the text of a callee in an error.
    pub calls: Vec<(u32, u32)>,
}

impl Code {
    pub fn new(nregs: u16) -> Code {
        Code { ops: Vec::new(), consts: Vec::new(), ics: Box::new([]), handlers: Vec::new(), lines: Vec::new(), nregs, calls: Vec::new() }
    }

    /// The handler for an exception thrown at word `pc`.
    pub fn handler_at(&self, pc: u32) -> Option<&Handler> {
        self.handlers.iter().find(|h| h.start <= pc && pc < h.end)
    }

    /// The source offset of word `pc`.
    pub fn source_at(&self, pc: u32) -> Option<u32> {
        let i = self.lines.partition_point(|&(w, _)| w <= pc);
        i.checked_sub(1).map(|i| self.lines[i].1)
    }

    /// The code as text, one instruction in each line: the word number, the op (`.w` for a wide form), the operands, and a note for
    /// a constant or a jump target. Handlers follow.
    pub fn disassemble(&self) -> String {
        let mut s = format!("code nregs={} words={} consts={} ics={}\n", self.nregs, self.ops.len(), self.consts.len(), self.ics.len());
        let mut pc = 0;
        while pc < self.ops.len() {
            let (ins, n) = match decode(&self.ops, pc) {
                Ok(d) => d,
                Err(e) => {
                    let _ = writeln!(s, "{pc:04}  <{e:?}>");
                    break;
                }
            };
            let name = if self.ops[pc] as u8 == Op::Wide as u8 {
                format!("{}.w", ins.op.name())
            } else {
                ins.op.name().to_string()
            };
            let _ = write!(s, "{pc:04}  {name:<14}");
            let form = ins.op.form();
            let vals = match form {
                Form::Abc => vec![ins.a, ins.b, ins.c],
                Form::AbcX => vec![ins.a, ins.b, ins.c, ins.x],
                Form::Abx | Form::AsBx => vec![ins.a, ins.b],
                Form::Sj => vec![ins.a],
            };
            let mut note = String::new();
            let parts: Vec<String> = ins
                .op
                .spec()
                .chars()
                .filter(|l| *l != ' ')
                .zip(vals)
                .filter(|(l, _)| *l != '-')
                .map(|(l, v)| match l {
                    'r' => format!("r{v}"),
                    'c' => format!("ic{v}"),
                    'f' => format!("fn{v}"),
                    's' => format!("s{v}"),
                    'i' => format!("{}", v as i32),
                    'j' => {
                        let t = (pc + n) as i64 + v as i32 as i64;
                        format!("-> {t:04}")
                    }
                    'k' => {
                        let _ = write!(note, "  ; {}", self.const_text(v));
                        format!("k{v}")
                    }
                    _ => format!("{v}"),
                })
                .collect();
            let _ = writeln!(s, "{}{note}", parts.join(", "));
            pc += n;
        }
        for h in &self.handlers {
            let _ = writeln!(s, "handler {:04}..{:04} -> {:04} r{} depth {}", h.start, h.end, h.target, h.reg, h.depth);
        }
        s
    }

    fn const_text(&self, k: u32) -> String {
        match self.consts.get(k as usize) {
            Some(Const::Number(n)) => format!("{n}"),
            Some(Const::String(u)) => format!("{:?}", String::from_utf16_lossy(u)),
            Some(Const::Name(n)) => format!("name#{n}"),
            None => "<no such constant>".to_string(),
        }
    }
}

#[cfg(test)]
#[path = "bc_tests.rs"]
mod tests;
