//! The bytecode format of the new engine (`docs/design-engine-rewrite.md` 6.10 items 1, 7 and 8, 6.11 items 8 and 19; plan step S2.4).
//! Nothing here runs code. The compiler (S2.5) writes this format through [`Asm`], the interpreter (S2.6) reads it, and
//! [`crate::verify::verify`] checks a [`Code`] before the interpreter may trust it. The numbers of the opcodes are not stable until S2.6.
//!
//! **Words.** One instruction is one 32-bit word, `[op:8][a:8][b:8][c:8]` with the op in the low byte, and sometimes one more raw
//! 32-bit word `x`. The forms:
//!
//! | Form | Word | Used by |
//! | --- | --- | --- |
//! | `Abc` | three 8-bit operands | most ops |
//! | `AbcX` | `Abc`, then the word `x` | an op with four operands, and a compare of two registers with a jump (`x` is the offset) |
//! | `Abx` | `a:8`, `bx:16` unsigned | a table index |
//! | `AsBx` | `a:8`, `bx:16` signed | a small integer, a jump on one register |
//! | `AsBxX` | `AsBx`, then the word `x` | a compare of a register with a constant, and a jump |
//! | `Sj` | one 24-bit signed operand | the jumps without a register |
//!
//! An operand that does not fit uses the wide form: the prefix word `[Wide][op][a:16]`, then the other operands at 16 bits (`Abc`:
//! `[b:16][c:16]`) or 32 bits (`Abx`, `AsBx`: one word), then `x` if the form has it. This is the `Wide` prefix of V8's Ignition,
//! with words in place of bytes; [`encode`] picks the narrow form when every operand fits, so the compiler never names a width.
//! Section "Compared" of the PR text and `docs/progress.md` (S2.4) compare it with QuickJS and Ignition.
//!
//! **Constants.** Each kind has its own place, and a constant that fits in the word is not in a table:
//! - Integers: an immediate. `LoadI` (16 bits), the `...I` ops (8 bits), and the compare-jumps `J..I` (a 32-bit `x`).
//! - `null`, `undefined`, `true`, `false`: no operand. `LoadNull` and the like; `JSEqNull`, `JSNeUndef`; or the 2-bit selector `z` of
//!   `SetFieldK` (0 `undefined`, 1 `null`, 2 `false`, 3 `true`).
//! - Atoms (names and string literals): the table [`Code::atoms`], which holds shared atoms (`crate::atoms`) and no realm pointer. A
//!   property name is the name of a *site* (below), so `GetField` has no name operand.
//! - Other numbers: [`Code::numbers`] (`LoadN`).
//! - Child functions: [`Code::funcs`]. Object-literal key lists: [`Code::recipes`].
//! - Jump tables of a `switch`: [`Code::tables`], one pool of words for all the tables of the function (see below).
//!
//! **Sites.** A property op, a global access or a name access names a *site*: an index into [`Code::sites`], where each site has the
//! atom of its name. The per-realm feedback vector (6.11 item 11) has one cache record for each site, at the same index. A cache hit
//! reads the record only; the name is read on a miss. So `GetField dst, obj, site` is one word.
//!
//! **Loops.** A jump backward is always `JmpBack`, and every other jump goes forward. `JmpBack` is the back edge: the place of the
//! budget and interrupt check, and a safepoint. [`Code::loops`] lists each loop header with the last back edge that targets it, sorted
//! by header, so the JIT needs no loop discovery (6.11 item 19). [`Asm::finish`] builds it from the `JmpBack` ops.
//!
//! **Switch tables.** `SwitchI` and `SwitchA` jump on the value of one register to one of many targets. The table is in [`Code::tables`], not
//! in the instruction stream, so every instruction keeps a fixed size. The op names the table by its offset in the pool. The targets are
//! absolute `pc`s of instructions after the switch (the compiler fills them after the widths are fixed). An integer table is `lo`,
//! `default`, `len` and `len` targets: the target of `v` is `targets[v - lo]` (a gap holds `default`). A string table is `default`, `mask` and
//! `mask + 1` slots of `hash`, `atom`, `target`, probed linearly from `hash & mask`; a slot with `atom == u32::MAX` is empty and ends a
//! probe. Both ops end the basic block: they do not fall through.
//!
//! **Root maps.** [`Code::roots`] holds the root maps of S1.6. The safepoints are the instructions where [`Op::is_safepoint`] is true:
//! every op that can allocate, call JS (a getter, `valueOf`) or throw, and `JmpBack`. The key of a map is the `pc` of the first word of
//! the instruction. The verifier checks that each safepoint has a map, that each map names only registers that every path has written,
//! and that no instruction reads a register that is not written (so a frame needs no fill, 6.10 item 4).
use crate::roots::RootMaps;
use crate::string::Str;
use std::fmt::Write;

/// The operand layout of an instruction word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    Abc,
    AbcX,
    Abx,
    AsBx,
    AsBxX,
    Sj,
}

impl Form {
    /// The number of operand fields: `a`, `b`, `c` and `x` for `AbcX`, `a` and `b` for `Abx`, and so on.
    pub fn fields(self) -> usize {
        match self {
            Form::Abc | Form::AsBxX => 3,
            Form::AbcX => 4,
            Form::Abx | Form::AsBx => 2,
            Form::Sj => 1,
        }
    }

    /// Does the form have the extra word?
    pub fn has_x(self) -> bool {
        matches!(self, Form::AbcX | Form::AsBxX)
    }

    /// The widths of the fields that are in the first word (`x` is always 32 bits), narrow and wide.
    fn widths(self, wide: bool) -> &'static [u32] {
        match (self, wide) {
            (Form::Abc | Form::AbcX, false) => &[8, 8, 8],
            (Form::Abc | Form::AbcX, true) => &[16, 16, 16],
            (Form::Abx | Form::AsBx | Form::AsBxX, false) => &[8, 16],
            (Form::Abx | Form::AsBx | Form::AsBxX, true) => &[16, 32],
            (Form::Sj, false) => &[24],
            (Form::Sj, true) => &[32],
        }
    }
}

/// `ops!` makes the opcode enum and its tables from one list, so the name, the form and the operand letters cannot drift apart. The
/// letters say what each field is, in the order of the fields (`a`, `b`, `c`, `x`; for `Abx` and `AsBx`: `a`, `bx`; for `AsBxX`: `a`,
/// `bx`, `x`; for `Sj`: the one operand). A letter list shorter than the form means that the fields that are left are 0.
///
/// `w` a register that the op writes. `r` a register that it reads. `i` a signed immediate. `u` an unsigned number (a count, a
/// depth, a slot). `j` a jump offset in words from the word after the instruction. `z` a special constant (0 to 3, see above).
/// `N` an index of the numbers, `A` of the atoms, `F` of the child functions, `H` of the recipes, `S` of the sites, `T` the offset of a switch table in [`Code::tables`]. `-` a field that is 0.
/// `R` is the first register of a window that the op reads (a call, a literal, an iterator record), and `W` of a window that it writes:
/// the size of a window comes from the op ([`crate::verify`]).
macro_rules! ops {
    ($($name:ident $form:ident $spec:literal $doc:literal,)*) => {
        /// The opcodes.
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
    Mov Abc "w r" "`a = b`.",
    LoadUndef Abc "w" "`a = undefined`.",
    LoadNull Abc "w" "`a = null`.",
    LoadTrue Abc "w" "`a = true`.",
    LoadFalse Abc "w" "`a = false`.",
    LoadThis Abc "w" "`a = this`.",
    LoadThisS Abc "w" "`a = this` in sloppy code: `undefined` and `null` give the global object.",
    LoadHole Abc "w" "`a` = the hole value (a variable in its temporal dead zone).",
    LoadI AsBx "w i" "`a` = the integer `sbx`.",
    LoadN Abx "w N" "`a` = the number `numbers[bx]`.",
    LoadAtom Abx "w A" "`a` = the string of the atom `atoms[bx]` (a string literal).",
    CheckUnbound Abc "r" "Throw a ReferenceError when `a` is not the hole: `super()` ran twice.",
    CheckInit Abx "r A" "Throw a ReferenceError that names `atoms[bx]` when `a` is the hole (a `let` or `const` in a register, read in its dead zone).",
    GetCtx Abc "w u u" "`a` = the slot `c` of the context `b` levels up. For a variable with no dead zone.",
    GetCtxTdz AbcX "w u u A" "As `GetCtx`, and a ReferenceError that names `atoms[x]` when the slot is the hole.",
    SetCtx Abc "r u u" "The slot `c` of the context `b` levels up = `a`.",
    SetCtxTdz AbcX "r u u A" "As `SetCtx`, and a ReferenceError that names `atoms[x]` when the slot is the hole.",
    InitCtx Abc "r u u" "Initialize that slot with `a` (no dead-zone check).",
    GetCtxField AbcX "w u u S" "`a` = the field of the site `x` of the value in the slot `c` of the context `b` levels up (the top pair of the staging journey).",
    PushCtx Abx "- u" "Push a new context with `bx` slots, all the hole.",
    PopCtx Abc "" "Pop the context.",
    CopyCtx Abc "" "Replace the context with a copy of it (one iteration of a `for` with `let` that a closure captured).",
    GetGlobal Abx "w S" "`a` = the global of the site `bx`. A missing name throws a ReferenceError.",
    SetGlobal Abx "r S" "The global of the site `bx` = `a`.",
    DeleteGlobal Abx "w S" "`a = delete name` for a global name: false when the property is not configurable.",
    SetGlobalS Abx "r S" "As `SetGlobal` in strict code: a ReferenceError when the global does not exist.",
    DeclGlobal Abx "- S" "Create the global of the site `bx` with the value `undefined` when it does not exist (the hoisting of the `var` names and the function names of a script).",
    DeclLex Abx "u S" "The slot `a` of the current context is the global lexical binding of the site `bx` (a top-level `let` or `class`): other scripts of the realm read it by name. A SyntaxError when the name is declared already.",
    CheckLex Abx "- S" "A SyntaxError when the global lexical environment has the name of the site `bx` already, or the global object has a fixed property of that name (the early check of a top-level `let` that lives in a register).",
    DeclLexC Abx "u S" "As `DeclLex` for a top-level `const`: an assignment throws a TypeError.",
    TypeofGlobal Abx "w S" "`a` = `typeof` the global of the site `bx`. A missing name gives `\"undefined\"`.",
    GetName Abx "w S" "`a` = the name of the site `bx` by the full search (a function with `with` or direct `eval`).",
    SetName Abx "r S" "The name of the site `bx` = `a`, by the full search.",
    TypeofName Abx "w S" "`a` = `typeof` the name of the site `bx`, by the full search.",
    GetField Abc "w r S" "`a = b.name` for the site `c`.",
    SetField Abc "r r S" "`a.name = b` for the site `c`.",
    SetFieldK Abc "r z S" "`a.name` = the special constant `b` (0 `undefined`, 1 `null`, 2 `false`, 3 `true`).",
    SetFieldI Abc "r i S" "`a.name` = the integer `b` (8 bits, signed).",
    SetFieldA Abc "r A S" "`a.name` = the string of the atom `atoms[b]`.",
    DefField Abc "r r S" "Define the own data property of the site `c` on `a` with the value `b` (an object literal that has no recipe).",
    GetElem Abc "w r r" "`a = b[c]`.",
    GetElemI Abc "w r u" "`a = b[c]` for the index `c` (8 bits).",
    SetElem Abc "r r r" "`a[b] = c`.",
    RequireObj Abc "r" "Throw a TypeError when `a` is `undefined` or `null` (an empty object pattern takes no property, so no read throws).",
    ToKey Abc "w r r" "`a` = the property key of `c` (ToPropertyKey). A `null` or `undefined` `b` throws first, as `b[c]` does.",
    DefElem Abc "r r r" "Define the own data property with the key `b` (already a key, see `ToKey`) on `a` with the value `c` (a computed key).",
    Delete Abc "w r r" "`a = delete b[c]`.",
    NewObject Abc "w u" "`a = {}` with room for `b` properties.",
    NewObjLit Abc "w R H" "`a` = a new object with the keys of `recipes[c]` and the values in the registers from `b`, in one op (no collection can happen between the allocation and the stores).",
    NewArray Abc "w u" "`a = []` with room for `b` elements.",
    NewArrLit Abc "w R u" "`a` = a new array of the `c` values in the registers from `b`.",
    ArrayPush Abc "r r" "Append `b` to the array `a`.",
    Closure Abx "w F" "`a` = a new function for the child function `funcs[bx]`, in the current context.",
    Call Abc "w R u" "`a = b(...)`. The callee is in `b`, `this` in `b+1`, the `c` arguments from `b+2`.",
    CallU Abc "w R u" "`a = b(...)` with `this` undefined. The callee is in `b`, the `c` arguments from `b+1`.",
    New Abc "w R u" "`a = new b(...)`. The constructor is in `b`, the `c` arguments from `b+1`.",
    Add Abc "w r r" "`a = b + c`.",
    Sub Abc "w r r" "`a = b - c`.",
    Mul Abc "w r r" "`a = b * c`.",
    Div Abc "w r r" "`a = b / c`.",
    Mod Abc "w r r" "`a = b % c`.",
    Exp Abc "w r r" "`a = b ** c`.",
    BitAnd Abc "w r r" "`a = b & c`.",
    BitOr Abc "w r r" "`a = b | c`.",
    BitXor Abc "w r r" "`a = b ^ c`.",
    Shl Abc "w r r" "`a = b << c`.",
    Shr Abc "w r r" "`a = b >> c`.",
    UShr Abc "w r r" "`a = b >>> c`.",
    AddI Abc "w r i" "`a = b + c` (`c` is an 8-bit signed integer).",
    SubI Abc "w r i" "`a = b - c`.",
    MulI Abc "w r i" "`a = b * c`.",
    DivI Abc "w r i" "`a = b / c`.",
    ModI Abc "w r i" "`a = b % c`.",
    BitAndI Abc "w r i" "`a = b & c`.",
    BitOrI Abc "w r i" "`a = b | c`.",
    ShlI Abc "w r i" "`a = b << c`.",
    ShrI Abc "w r i" "`a = b >> c`.",
    UShrI Abc "w r i" "`a = b >>> c`.",
    Eq Abc "w r r" "`a = b == c`.",
    Ne Abc "w r r" "`a = b != c`.",
    StrictEq Abc "w r r" "`a = b === c`.",
    StrictNe Abc "w r r" "`a = b !== c`.",
    Lt Abc "w r r" "`a = b < c`.",
    Le Abc "w r r" "`a = b <= c`.",
    Gt Abc "w r r" "`a = b > c`.",
    Ge Abc "w r r" "`a = b >= c`.",
    In Abc "w r r" "`a = b in c`.",
    InstanceOf Abc "w r r" "`a = b instanceof c`.",
    Neg Abc "w r" "`a = -b`.",
    Plus Abc "w r" "`a = +b`.",
    Not Abc "w r" "`a = !b`.",
    BitNot Abc "w r" "`a = ~b`.",
    Typeof Abc "w r" "`a = typeof b`.",
    Inc Abc "w r" "`a = b + 1` (after ToNumeric).",
    Dec Abc "w r" "`a = b - 1` (after ToNumeric).",
    ToNumeric Abc "w r" "`a = ToNumeric(b)`.",
    ToStr Abc "w r" "`a = ToString(b)`.",
    Concat Abc "w R u" "`a` = the `c` strings in the registers from `b`, joined (a template literal).",
    Jmp Sj "j" "Jump forward.",
    JmpBack Sj "j" "Jump back to a loop header: the back edge. A safepoint.",
    JmpIf AsBx "r j" "Jump when `a` is truthy.",
    JmpIfNot AsBx "r j" "Jump when `a` is falsy.",
    JmpNullish AsBx "r j" "Jump when `a` is `undefined` or `null`.",
    JmpNotNullish AsBx "r j" "Jump when `a` is neither `undefined` nor `null`.",
    JLt AbcX "r r - j" "Jump when `a < b`.",
    JLe AbcX "r r - j" "Jump when `a <= b`.",
    JGt AbcX "r r - j" "Jump when `a > b`.",
    JGe AbcX "r r - j" "Jump when `a >= b`.",
    JnLt AbcX "r r - j" "Jump when `a < b` is false (not the same as `a >= b`: a NaN).",
    JnLe AbcX "r r - j" "Jump when `a <= b` is false.",
    JnGt AbcX "r r - j" "Jump when `a > b` is false.",
    JnGe AbcX "r r - j" "Jump when `a >= b` is false.",
    JEq AbcX "r r - j" "Jump when `a == b`.",
    JNe AbcX "r r - j" "Jump when `a != b`.",
    JSEq AbcX "r r - j" "Jump when `a === b`.",
    JSNe AbcX "r r - j" "Jump when `a !== b`.",
    JLtI AsBxX "r j i" "Jump when `a < x` (`x` is a 32-bit integer).",
    JLeI AsBxX "r j i" "Jump when `a <= x`.",
    JGtI AsBxX "r j i" "Jump when `a > x`.",
    JGeI AsBxX "r j i" "Jump when `a >= x`.",
    JnLtI AsBxX "r j i" "Jump when `a < x` is false.",
    JnLeI AsBxX "r j i" "Jump when `a <= x` is false.",
    JnGtI AsBxX "r j i" "Jump when `a > x` is false.",
    JnGeI AsBxX "r j i" "Jump when `a >= x` is false.",
    JEqI AsBxX "r j i" "Jump when `a == x`.",
    JNeI AsBxX "r j i" "Jump when `a != x`.",
    JSEqI AsBxX "r j i" "Jump when `a === x`.",
    JSNeI AsBxX "r j i" "Jump when `a !== x`.",
    JSEqA AsBxX "r j A" "Jump when `a` is the string of the atom `atoms[x]` (`===`).",
    JSNeA AsBxX "r j A" "Jump when `a` is not that string (`!==`).",
    JSEqB AsBxX "r j u" "Jump when `a === true` (`x` 1) or `a === false` (`x` 0).",
    JSNeB AsBxX "r j u" "Jump when `a !== true` (`x` 1) or `a !== false` (`x` 0).",
    JSEqNull AsBx "r j" "Jump when `a === null`.",
    JSNeNull AsBx "r j" "Jump when `a !== null`.",
    JSEqUndef AsBx "r j" "Jump when `a === undefined`.",
    JSNeUndef AsBx "r j" "Jump when `a !== undefined`.",
    SwitchI Abx "r T" "Jump by the integer table at `tables[bx..]` on the value of `a`: an integer or a double that is an integer value (`-0` is 0) in the table goes to its target; anything else goes to the default.",
    SwitchA Abx "r T" "Jump by the string table at `tables[bx..]` on the value of `a`: a string with the content of a case goes to its target; anything else goes to the default.",
    Ret Abc "r" "Return `a`.",
    RetUndef Abc "" "Return `undefined`.",
    Throw Abc "r" "Throw `a`.",
    ThrowConst Abx "- A" "Throw the TypeError of an assignment to the constant named by `atoms[bx]`.",
    IterOpen Abc "W r" "Open the iterator of `b`. The iterator record (iterator, `next`, state) is in the 3 registers from `a`.",
    ForInOpen Abc "W r" "Open the keys of `b` (`for-in`). The record is in the 3 registers from `a`, as for `IterOpen`.",
    IterNext AsBxX "w j R" "`a` = the next value of the record in the 3 registers from `x`. When it is done, jump.",
    IterClose Abc "R" "Close the iterator whose record is in the 3 registers from `a` (a `break` or an exception out of the loop).",
    CreateArgs Abc "w" "`a` = a new arguments object that holds every argument of this call (the frame keeps them, also those past `nparams`). Always unmapped: `arguments[i]` does not alias the parameter `i`. The first instruction of the function, before any parameter is written.",
    CreateRest Abc "w u" "`a` = a new array of the arguments of this call from the index `b` on (a rest parameter; `b` is its position).",
    LoadCallee Abc "w" "`a` = the function that runs (the name of a named function expression inside its own body).",
    LoadNewTarget Abc "w" "`a` = `new.target` of this call.",
    NewRegExp Abc "w A A" "`a` = a new RegExp object with the pattern `atoms[b]` and the flags `atoms[c]`.",
    LoadBigInt Abx "w A" "`a` = the BigInt whose literal text (no `n`, no `_`, with its `0x`, `0o` or `0b` prefix) is `atoms[bx]`.",
    ArraySpread Abc "r r" "Append every value that the iterator of `b` gives to the array `a` (`[...b]`, an argument `...b`).",
    ArrayHole Abc "r" "Append a hole to the array `a` (`[1, , 2]`).",
    CallSpread Abc "w R" "`a = b(...)` with the arguments in an array: the callee is in `b`, `this` in `b+1`, the array in `b+2`.",
    NewSpread Abc "w R" "`a = new b(...)` with the arguments in an array: the constructor is in `b`, the array in `b+1`.",
    CopyProps Abc "r r" "Copy the own enumerable properties of `b` to the object `a` (`{...b}`); `undefined` and `null` copy nothing.",
    ObjRest AbcX "w r R u" "`a` = a new object with the own enumerable properties of `b` except the `x` keys in the registers from `c` (`{p, ...rest} = b`). `undefined` and `null` throw.",
    SetProto Abc "r r" "Set the prototype of the object literal `a` to `b` when `b` is an object or `null` (`{__proto__: b}`).",
    DefAccessor AbcX "r r S u" "Define an accessor of the site `c` on `a` with the function `b`. `x`: bit 0 set for a setter, bit 1 set for a property that is not enumerable (a class).",
    DefAccessorElem AbcX "r r r u" "As `DefAccessor`, with the key in `b` (already a key, see `ToKey`) and the function in `c`.",
    DefMethod Abc "r r S" "Define the method `b` on `a` as the property of the site `c`, not enumerable (a class member).",
    DefMethodElem Abc "r r r" "As `DefMethod`, with the key in `b` (already a key) and the function in `c`.",
    SetHome Abc "r r" "The home object of the function `a` is `b` (it uses `super.x`).",
    GetTemplate Abc "w H S" "`a` = the template object of the tagged template at the site `c`: the same object each time. `recipes[b]` holds pairs of atoms: the cooked string and the raw string of each piece (`u32::MAX` as the cooked string: it is `undefined`).",
    MakeClass Abc "r" "`a` is the constructor of a class that has no `extends`: give it a new prototype object (its `constructor` is `a`, its home object is that object).",
    MakeDerived Abc "r r" "As `MakeClass` for a class with `extends`: `a` is the constructor, `b` the value of the `extends` clause. Throws a TypeError when it is no constructor and no `null`.",
    SetFnName Abc "r r u" "Define the `name` of the function `a` from the key `b` (already a key, see `ToKey`; for a number key, its text): with the prefix `get ` for `c` 1, `set ` for `c` 2 (`SetFunctionName`).",
    DefaultCtor Abc "w u" "`a` = the default constructor of a class (`b` 1: a derived class, which calls `super(...args)`).",
    SetFields Abc "r r" "The function `b` makes the fields of an instance of the class `a`. The constructor calls it with `this` (when it has no `super()`, at its start; else right after `super()` returns).",
    NewPrivateName Abx "w A" "`a` = a new private name, whose description is `atoms[bx]` (one for each class evaluation).",
    GetPrivate Abc "w r r" "`a = b.#name` where the register `c` holds the private name. A TypeError when `b` does not have it.",
    SetPrivate Abc "r r r" "`a.#name = c` where the register `b` holds the private name. A TypeError when `a` does not have it, or it is a method.",
    HasPrivate Abc "w r r" "`a = #name in b` where the register `c` holds the private name.",
    DefPrivate Abc "r r r" "Add the private field `b` (a private name) with the value `c` to `a`. A TypeError when `a` has it.",
    DefPrivateMethod Abc "r r r" "Add the private method `b` (a private name) with the function `c` to `a`: not writable.",
    DefPrivateAccessor AbcX "r r r u" "Add a private accessor `b` (a private name) with the function `c` to `a`. `x` 0: getter, 1: setter.",
    GetSuper Abc "w r S" "`a = super.name` for the site `c`, with `b` as `this`.",
    GetSuperElem Abc "w r r" "`a = super[c]`, with `b` as `this`.",
    SetSuper Abc "r r S" "`super.name = b` for the site `c`, with `a` as `this`.",
    SetSuperElem Abc "r r r" "`super[b] = c`, with `a` as `this`.",
    CallSuper Abc "w R u" "`a = super(...)`: the `c` arguments are in the registers from `b`. It constructs with the `new.target` of this call and runs the field initializers (`SetFields`) of the class. The compiler writes `a` to the variable `this`.",
    CallSuperSpread Abc "w r" "As `CallSuper`, with the arguments in the array `b`.",
    RetDerived Abc "r r" "Return from a derived constructor with the value `a` and the variable `this` in `b`: an object is returned as it is; `undefined` gives `this` (a ReferenceError when `this` is the hole); anything else is a TypeError.",
}

/// Words of a slot of a string switch table: the hash, the atom and the target.
pub const STR_SLOT: usize = 3;
/// The atom field of an empty slot of a string switch table.
pub const STR_EMPTY: u32 = u32::MAX;

/// The registers of an iterator record (`IterOpen`, `ForInOpen`, `IterNext`, `IterClose`).
pub const ITER_REGS: u32 = 3;

impl Op {
    /// The opcode of a byte, or `None` for a byte that is no opcode.
    pub fn from_u8(b: u8) -> Option<Op> {
        Op::ALL.get(b as usize).copied()
    }

    /// The letter of field `i` (`a`, `b`, `c`, `x`), `-` when the spec has none.
    pub fn letter(self, i: usize) -> char {
        self.spec().chars().filter(|l| *l != ' ').nth(i).unwrap_or('-')
    }

    /// The index of the field that holds the jump offset (`a` 0, `b` 1, `c` 2, `x` 3), if the op jumps.
    pub fn jump_field(self) -> Option<usize> {
        (0..self.form().fields()).find(|&i| self.letter(i) == 'j')
    }

    /// Does control go on to the next instruction? (`Jmp`, `JmpBack`, the returns and the throws do not.)
    pub fn falls_through(self) -> bool {
        !matches!(self, Op::Jmp | Op::JmpBack | Op::Ret | Op::RetUndef | Op::Throw | Op::ThrowConst | Op::RetDerived | Op::SwitchI | Op::SwitchA)
    }

    /// Can the instruction run JS, allocate or throw, so that a collection can happen in it? The compiler gives a root map to each one.
    /// Everything is a safepoint except the ops listed here: they move values, test them with no coercion, and never throw.
    pub fn is_safepoint(self) -> bool {
        use Op::*;
        !matches!(
            self,
            Nop | Mov | LoadCallee | LoadNewTarget | LoadUndef | LoadNull | LoadTrue | LoadFalse | LoadThis | LoadHole | LoadI | LoadN | LoadAtom | GetCtx | SetCtx | InitCtx | PopCtx
                | StrictEq | StrictNe | Not | Typeof | Jmp | JmpIf | JmpIfNot | JmpNullish | JmpNotNullish | JSEq | JSNe | JSEqI | JSNeI | JSEqA
                | JSNeA | JSEqB | JSNeB | JSEqNull | JSNeNull | JSEqUndef | JSNeUndef | SwitchI | SwitchA | Ret | RetUndef | Throw
        )
    }
}

/// One decoded instruction. A field that the spec marks `i` or `j` holds a signed value as two's complement in 32 bits. Which field is
/// which depends on `op.form()` and `op.spec()`: `a`, `b`, `c` are the fields in the order of the spec, `x` is the extra word.
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

    /// Field `i` in the order of the spec: `a`, `b`, `c`, then `x` for `AbcX`. For `AsBxX` the fields are `a`, `b` and `x`.
    pub fn field(&self, i: usize) -> u32 {
        match (self.op.form(), i) {
            (_, 0) => self.a,
            (_, 1) => self.b,
            (Form::AsBxX, 2) | (Form::AbcX, 3) => self.x,
            _ => self.c,
        }
    }

    fn set_field(&mut self, i: usize, v: u32) {
        match (self.op.form(), i) {
            (_, 0) => self.a = v,
            (_, 1) => self.b = v,
            (Form::AsBxX, 2) | (Form::AbcX, 3) => self.x = v,
            _ => self.c = v,
        }
    }

    /// The jump offset of a jump, in words from the word after the instruction.
    pub fn offset(&self) -> Option<i32> {
        self.op.jump_field().map(|i| self.field(i) as i32)
    }

    /// The word that a jump at `pc` with `len` words goes to, as a signed number (it can be below 0 or past the end).
    pub fn target(&self, pc: usize, len: usize) -> Option<i64> {
        self.offset().map(|o| (pc + len) as i64 + o as i64)
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
    /// A label that no `bind` placed.
    Unbound,
}

const W: u32 = Op::Wide as u32;

/// Is the value of a field with `bits` bits in range? `signed`: the value is a two's complement number.
fn fits(v: u32, bits: u32, signed: bool) -> bool {
    bits == 32 || if signed { (v as i32) >= -(1 << (bits - 1)) && (v as i32) < 1 << (bits - 1) } else { v < 1 << bits }
}

fn signed(op: Op, i: usize) -> bool {
    matches!(op.letter(i), 'i' | 'j')
}

/// Append `ins` to `words` in the narrowest form that holds its operands. Gives the number of words written.
pub fn encode(words: &mut Vec<u32>, ins: &Ins) -> Result<usize, BcError> {
    encode_in(words, ins, false)
}

/// As [`encode`]; `wide` asks for the wide form even when the narrow form fits (the assembler needs this for a jump).
pub fn encode_in(words: &mut Vec<u32>, ins: &Ins, wide: bool) -> Result<usize, BcError> {
    let op = ins.op;
    let form = op.form();
    let start = words.len();
    let f = [ins.a, ins.b, ins.c];
    let n = form.fields().min(form.widths(true).len());
    let ok = |w: bool| (0..n).all(|i| fits(f[i], form.widths(w)[i], signed(op, i)));
    let wide = wide || !ok(false);
    if wide && !ok(true) {
        return Err(BcError::Operand);
    }
    let m = |i: usize| f[i] & ((1u64 << form.widths(wide)[i]) - 1) as u32;
    let o = op as u32;
    match (form, wide) {
        (Form::Abc | Form::AbcX, false) => words.push(o | m(0) << 8 | m(1) << 16 | m(2) << 24),
        (Form::Abc | Form::AbcX, true) => words.extend([W | o << 8 | m(0) << 16, m(1) | m(2) << 16]),
        (Form::Abx | Form::AsBx | Form::AsBxX, false) => words.push(o | m(0) << 8 | m(1) << 16),
        (Form::Abx | Form::AsBx | Form::AsBxX, true) => words.extend([W | o << 8 | m(0) << 16, ins.b]),
        (Form::Sj, false) => words.push(o | m(0) << 8),
        (Form::Sj, true) => words.extend([W | o << 8, ins.a]),
    }
    if form.has_x() {
        words.push(ins.x);
    }
    Ok(words.len() - start)
}

fn sext(v: u32, bits: u32) -> u32 {
    if bits == 32 { v } else { ((v << (32 - bits)) as i32 >> (32 - bits)) as u32 }
}

/// Decode the instruction at word `pc`. Gives it and the number of words it takes.
pub fn decode(words: &[u32], pc: usize) -> Result<(Ins, usize), BcError> {
    let w = *words.get(pc).ok_or(BcError::End)?;
    let top = Op::from_u8(w as u8).ok_or(BcError::BadOp(w as u8))?;
    let (op, wide) = if top == Op::Wide {
        (Op::from_u8((w >> 8) as u8).filter(|o| *o != Op::Wide).ok_or(BcError::BadWide)?, true)
    } else {
        (top, false)
    };
    let form = op.form();
    let widths = form.widths(wide);
    let mut at = pc + 1;
    let mut next = || {
        let r = words.get(at).copied();
        at += 1;
        r
    };
    let mut f = [0u32; 3];
    if wide {
        let a = w >> 16;
        match form {
            Form::Abc | Form::AbcX => {
                let w2 = next().ok_or(BcError::BadWide)?;
                f = [a, w2 & 0xffff, w2 >> 16];
            }
            Form::Abx | Form::AsBx | Form::AsBxX => f = [a, next().ok_or(BcError::BadWide)?, 0],
            Form::Sj => f[0] = next().ok_or(BcError::BadWide)?,
        }
    } else {
        f = match form {
            Form::Abc | Form::AbcX => [w >> 8 & 255, w >> 16 & 255, w >> 24],
            Form::Abx | Form::AsBx | Form::AsBxX => [w >> 8 & 255, w >> 16, 0],
            Form::Sj => [w >> 8, 0, 0],
        };
    }
    for (i, v) in f.iter_mut().enumerate().take(widths.len()) {
        if signed(op, i) {
            *v = sext(*v, widths[i]);
        }
    }
    let mut ins = Ins { op, a: f[0], b: f[1], c: f[2], x: 0 };
    if form.has_x() {
        ins.x = next().ok_or(if wide { BcError::BadWide } else { BcError::End })?;
    }
    Ok((ins, at - pc))
}

/// The key lists of the object literals of a function. Recipe `i` is the atom indexes of its keys, in the order of the slots.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Recipes {
    ends: Vec<u32>,
    keys: Vec<u32>,
}

impl Recipes {
    pub fn push(&mut self, keys: &[u32]) -> usize {
        self.keys.extend_from_slice(keys);
        self.ends.push(self.keys.len() as u32);
        self.ends.len() - 1
    }

    pub fn len(&self) -> usize {
        self.ends.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }

    pub fn get(&self, i: usize) -> Option<&[u32]> {
        let end = *self.ends.get(i)? as usize;
        Some(&self.keys[i.checked_sub(1).map_or(0, |p| self.ends[p] as usize)..end])
    }
}

/// A loop: the header (the target of the back edges) and the last back edge, both as the `pc` of an instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoopHeader {
    pub head: u32,
    pub end: u32,
}

/// A range of words with a handler. When an exception is thrown at a word in `start..end`, the exception goes into register `reg` and
/// the run continues at `target`. The first handler in the table whose range holds the word is the innermost, so the compiler writes
/// inner ranges first. `depth` is the number of contexts that the function has pushed (`PushCtx` less `PopCtx`) at the handler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Handler {
    pub start: u32,
    pub end: u32,
    pub target: u32,
    pub reg: u16,
    pub depth: u16,
}

/// `Code::flags`: the function reads all its arguments (`CreateArgs`, `CreateRest`), so a call keeps them.
pub const CF_ARGS: u8 = 1;
/// `Code::flags`: the function (or a function inside it) uses `super`, so `Closure` gives the new function the home object and the
/// constructor link of the function that makes it.
pub const CF_LINK: u8 = 2;

/// The code of one function. It holds no pointer to a realm's heap: its atoms are shared atoms, so all realms of a session can use it.
#[derive(Debug)]
pub struct Code {
    pub words: Vec<u32>,
    /// The registers of a frame. Registers `0..nparams` hold the arguments at the entry; the others are not written.
    pub nregs: u16,
    pub nparams: u16,
    /// The numbers that do not fit an immediate.
    pub numbers: Vec<f64>,
    /// The names and the string literals.
    pub atoms: Vec<Str>,
    /// The child functions, as numbers of the script.
    pub funcs: Vec<u32>,
    pub recipes: Recipes,
    /// The jump tables of `SwitchI` and `SwitchA` (see the module comment for the layout).
    pub tables: Vec<u32>,
    /// The name of each site, as an index of `atoms`. Each site has exactly one instruction.
    pub sites: Vec<u32>,
    /// Sorted by `head`, one entry for each header.
    pub loops: Vec<LoopHeader>,
    pub handlers: Vec<Handler>,
    pub roots: RootMaps,
    /// Pairs `(pc, source offset)`, sorted by `pc`. The offset of a word is that of the last pair at or before it.
    pub lines: Vec<(u32, u32)>,
    /// Facts about the code that the interpreter finds when it loads a program (`CF_ARGS`, `CF_LINK`); the compiler leaves it 0.
    pub flags: u8,
    /// The `name` of the function as an index of `atoms` ([`NO_NAME`]: the empty string) and its `length` (the parameters before the first
    /// default or rest parameter). The realm defines both properties of a function cell at the first look at its own properties.
    pub fn_name: u32,
    pub fn_len: u16,
    /// The function is strict code: a write that fails (a read-only property, a primitive, `delete` of a fixed property) throws.
    pub strict: bool,
    /// The function is an arrow, a method, a getter or a setter: `new` on it throws a `TypeError`.
    pub noctor: bool,
}

/// `Code::fn_name` of a function that has no name.
pub const NO_NAME: u32 = u32::MAX;

impl Code {
    pub fn new(nregs: u16, nparams: u16) -> Code {
        Code {
            words: Vec::new(),
            nregs,
            nparams,
            numbers: Vec::new(),
            atoms: Vec::new(),
            funcs: Vec::new(),
            recipes: Recipes::default(),
            tables: Vec::new(),
            sites: Vec::new(),
            loops: Vec::new(),
            handlers: Vec::new(),
            roots: RootMaps::new(nregs as usize),
            lines: Vec::new(),
            flags: 0,
            fn_name: NO_NAME,
            fn_len: 0,
            strict: false,
            noctor: false,
        }
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

    /// The integer table at `t` of `tables`: `lo`, the default target and the targets. `None` when it is not inside the pool.
    pub fn int_table(&self, t: u32) -> Option<(i32, u32, &[u32])> {
        let rest = self.tables.get(t as usize..)?;
        let &[lo, default, len] = rest.get(..3)? else { return None };
        Some((lo as i32, default, rest.get(3..)?.get(..len as usize)?))
    }

    /// The string table at `t`: the default target and the slots (`STR_SLOT` words each). `None` when it is not inside the pool or its
    /// slot count is not a power of two.
    pub fn str_table(&self, t: u32) -> Option<(u32, &[u32])> {
        let rest = self.tables.get(t as usize..)?;
        let &[default, mask] = rest.get(..2)? else { return None };
        let slots = (mask as usize).checked_add(1).filter(|n| n.is_power_of_two())?;
        Some((default, rest.get(2..)?.get(..slots.checked_mul(STR_SLOT)?)?))
    }

    /// Every target of `ins` when it is a switch (the default included), `None` for another op or a table that is not in the pool.
    pub fn switch_targets(&self, ins: &Ins) -> Option<Vec<u32>> {
        match ins.op {
            Op::SwitchI => self.int_table(ins.b).map(|(_, d, t)| t.iter().copied().chain([d]).collect()),
            Op::SwitchA => self.str_table(ins.b).map(|(d, s)| s.chunks(STR_SLOT).filter(|c| c[1] != STR_EMPTY).map(|c| c[2]).chain([d]).collect()),
            _ => None,
        }
    }

    /// Bytes of the code and its tables (not the root maps, which `RootMaps::bytes` counts).
    pub fn bytes(&self) -> usize {
        self.words.len() * 4
            + self.numbers.len() * 8
            + self.atoms.len() * 8
            + self.funcs.len() * 4
            + (self.recipes.ends.len() + self.recipes.keys.len()) * 4
            + self.tables.len() * 4
            + self.sites.len() * 4
            + self.loops.len() * 8
            + self.handlers.len() * 12
            + self.lines.len() * 8
    }

    /// The code as text: the word number, the op (`.w` for a wide form) and the operands, one instruction in each line.
    pub fn disassemble(&self) -> String {
        let mut s = format!("code nregs={} nparams={} words={} sites={}\n", self.nregs, self.nparams, self.words.len(), self.sites.len());
        let mut pc = 0;
        while pc < self.words.len() {
            let (ins, n) = match decode(&self.words, pc) {
                Ok(d) => d,
                Err(e) => {
                    let _ = writeln!(s, "{pc:04}  <{e:?}>");
                    break;
                }
            };
            let wide = if self.words[pc] as u8 == Op::Wide as u8 { ".w" } else { "" };
            let name = format!("{}{wide}", ins.op.name());
            let _ = write!(s, "{pc:04}  {name:<12}{}", if name.len() >= 12 { " " } else { "" });
            let parts: Vec<String> = (0..ins.op.form().fields())
                .filter(|&i| ins.op.letter(i) != '-')
                .map(|i| {
                    let v = ins.field(i);
                    match ins.op.letter(i) {
                        'w' | 'r' => format!("r{v}"),
                        'i' => format!("{}", v as i32),
                        'j' => format!("-> {:04}", (pc + n) as i64 + v as i32 as i64),
                        l @ ('N' | 'A' | 'F' | 'H' | 'S' | 'T') => format!("{}{v}", l.to_ascii_lowercase()),
                        _ => format!("{v}"),
                    }
                })
                .collect();
            let _ = writeln!(s, "{}", parts.join(", "));
            match ins.op {
                Op::SwitchI => {
                    if let Some((lo, default, targets)) = self.int_table(ins.b) {
                        let list: Vec<String> = targets.iter().enumerate().filter(|(_, t)| **t != default).map(|(k, t)| format!("{} -> {t:04}", lo as i64 + k as i64)).collect();
                        let _ = writeln!(s, "      [{}{}default -> {default:04}]", list.join(", "), if list.is_empty() { "" } else { ", " });
                    }
                }
                Op::SwitchA => {
                    if let Some((default, slots)) = self.str_table(ins.b) {
                        let mut list: Vec<(u32, u32)> = slots.chunks(STR_SLOT).filter(|c| c[1] != STR_EMPTY).map(|c| (c[1], c[2])).collect();
                        list.sort_unstable();
                        let list: Vec<String> = list.iter().map(|(a, t)| format!("a{a} -> {t:04}")).collect();
                        let _ = writeln!(s, "      [{}{}default -> {default:04}]", list.join(", "), if list.is_empty() { "" } else { ", " });
                    }
                }
                _ => {}
            }
            pc += n;
        }
        s
    }
}

/// A place in the code that a jump goes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Label(u32);

/// The result of [`Asm::finish`].
#[derive(Debug)]
pub struct Assembled {
    pub words: Vec<u32>,
    /// The `pc` of each instruction, by the number that [`Asm::emit`] gave. One more entry: the length of the code.
    pub pcs: Vec<u32>,
    /// The `pc` of each label, by the number of the label (a label that was bound at the end has the length of the code; a label that
    /// no `bind` placed has `u32::MAX`, and a jump to it is an error).
    pub label_pcs: Vec<u32>,
    /// One entry for each header of a `JmpBack`, sorted by header.
    pub loops: Vec<LoopHeader>,
}

/// An assembler: the compiler emits instructions and labels, and [`Asm::finish`] chooses the width of each instruction (a jump may
/// need a wide offset in a long function) and fills the jump offsets. Offsets and widths depend on each other, so the widths only grow
/// until they stop changing.
#[derive(Default)]
pub struct Asm {
    ins: Vec<(Ins, Option<Label>)>,
    at: Vec<Option<u32>>,
}

impl Asm {
    /// Add an instruction. Gives its number. A jump instruction keeps the offset that `ins` holds (use [`Asm::jump`] for a label).
    pub fn emit(&mut self, ins: Ins) -> usize {
        self.ins.push((ins, None));
        self.ins.len() - 1
    }

    /// Add a jump to `to`. The offset field of `ins` is filled by `finish`.
    pub fn jump(&mut self, ins: Ins, to: Label) -> usize {
        debug_assert!(ins.op.jump_field().is_some());
        self.ins.push((ins, Some(to)));
        self.ins.len() - 1
    }

    /// The number of instructions emitted so far (the number that the next [`Asm::emit`] gives).
    pub fn len(&self) -> usize {
        self.ins.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ins.is_empty()
    }

    pub fn label(&mut self) -> Label {
        self.at.push(None);
        Label(self.at.len() as u32 - 1)
    }

    /// Put `l` before the next instruction.
    pub fn bind(&mut self, l: Label) {
        self.at[l.0 as usize] = Some(self.ins.len() as u32);
    }

    pub fn finish(self) -> Result<Assembled, BcError> {
        let n = self.ins.len();
        let mut cur: Vec<Ins> = self.ins.iter().map(|x| x.0).collect();
        let mut wide = vec![false; n];
        let mut pcs = vec![0u32; n + 1];
        let mut scratch = Vec::new();
        loop {
            for i in 0..n {
                scratch.clear();
                let len = encode_in(&mut scratch, &cur[i], wide[i])? as u32;
                wide[i] |= scratch[0] as u8 == Op::Wide as u8;
                pcs[i + 1] = pcs[i] + len;
            }
            let mut changed = false;
            for (i, (_, to)) in self.ins.iter().enumerate() {
                if let Some(l) = to {
                    let idx = self.at[l.0 as usize].ok_or(BcError::Unbound)? as usize;
                    let off = pcs[idx] as i64 - pcs[i + 1] as i64;
                    let f = cur[i].op.jump_field().expect("a jump has an offset field");
                    if cur[i].field(f) != off as i32 as u32 {
                        cur[i].set_field(f, off as i32 as u32);
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        let mut words = Vec::with_capacity(pcs[n] as usize);
        for i in 0..n {
            encode_in(&mut words, &cur[i], wide[i])?;
        }
        let label_pcs: Vec<u32> = self.at.iter().map(|a| a.map_or(u32::MAX, |i| pcs[i as usize])).collect();
        let mut loops: Vec<LoopHeader> = Vec::new();
        for (i, (ins, to)) in self.ins.iter().enumerate() {
            if let (Op::JmpBack, Some(l)) = (ins.op, to) {
                let head = label_pcs[l.0 as usize];
                match loops.iter_mut().find(|h| h.head == head) {
                    Some(h) => h.end = h.end.max(pcs[i]),
                    None => loops.push(LoopHeader { head, end: pcs[i] }),
                }
            }
        }
        loops.sort_by_key(|h| h.head);
        Ok(Assembled { words, pcs, label_pcs, loops })
    }
}

#[cfg(test)]
#[path = "bytecode_tests.rs"]
mod tests;
