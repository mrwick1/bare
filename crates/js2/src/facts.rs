//! What the pre-parser knows about the functions of one source text (design 6.5 and 6.9 items 4 and 9).
//!
//! A lazy parse ([`crate::parse::parse_script_lazy`]) reads the top level into a tree. Each function inside it is only checked: the
//! parser reads the body for syntax and early errors, keeps no tree for it, and writes one [`FnFacts`] entry. The entry has the byte
//! range of the function (the skip range), the outer names that it uses (its captures) and a few flags. The compiler (S2.5) reads the
//! facts of the children of the function that it compiles, to keep the other variables in registers.
//!
//! The entries are in post-order: a function is written when it ends, so its descendants are the entries just before it
//! (`fns[f.sub..own index]`). A later full parse of one function ([`crate::parse::parse_function`]) finds its inner functions by their
//! start offset ([`Facts::find`]) and jumps over their bodies. It writes no entry: each function is pre-parsed at most once
//! ([`Facts::preparsed`] counts the bodies that were read in check mode).
//!
//! Names are byte spans of the source. A name with a `\u` escape must be decoded ([`crate::ast::name_in`]); two spans of one
//! name can differ in the text, so the parser keeps one span for each distinct decoded name.

use crate::ast::{FuncKind, FuncMode, Span};

/// The index of a function in [`Facts::fns`].
pub type FnId = u32;

/// The function uses `this`, itself or in an arrow inside it. An arrow passes it to the function around it.
pub const THIS: u16 = 1;
/// The function uses `arguments` and no declaration of the function binds it. An arrow lists `arguments` in its captures too.
/// A function that is not an arrow binds it itself, so it does not list it.
pub const ARGS: u16 = 2;
/// `super.x` or `super()`, itself or in an arrow inside it.
pub const SUPER: u16 = 4;
/// `new.target`, itself or in an arrow inside it.
pub const NEW_TARGET: u16 = 8;
/// A direct `eval(...)` call in the body (not in a function inside it). The code of the eval can read and write every name that is in
/// scope, and can use `this`, `arguments` and `new.target`: the compiler keeps all variables of this function in the environment.
pub const EVAL: u16 = 16;
/// A function inside this one, at any depth, has [`EVAL`] or [`INNER_EVAL`]: all variables of this function are visible to it.
pub const INNER_EVAL: u16 = 32;
/// A `with` statement in the body (not in a function inside it). A name in its body may be a property of the object.
pub const WITH: u16 = 64;
/// The function is inside the body of a `with`, in this function or in a function around it: every name it uses may be a property of
/// the object, so the compiler looks names up by scope chain.
pub const IN_WITH: u16 = 128;
/// A class field initializer or a `static { }` block. It is a function for the rules of names, but its text stays in the tree of the
/// function around it: [`FnFacts::end`] is not a skip range and no [`crate::ast::Body::Skipped`] points here.
pub const INLINE: u16 = 256;
/// With [`INLINE`]: the initializer of a field. `start` is the `=`.
pub const FIELD_INIT: u16 = 512;
/// With [`INLINE`]: a static block. `start` is `static`.
pub const STATIC_BLOCK: u16 = 1024;

/// The bits of [`FnFacts::ctx`]: the context at the start of the function. An arrow takes `this`, `super` and the rest from there. The
/// parser needs them when it parses the arrow again.
pub const C_IN_FUNC: u8 = 1;
pub const C_ASYNC: u8 = 2;
pub const C_GEN: u8 = 4;
pub const C_NEW_TARGET: u8 = 8;
pub const C_SUPER_PROP: u8 = 16;
pub const C_SUPER_CALL: u8 = 32;
pub const C_FIELD_INIT: u8 = 64;
pub const C_STATIC_BLOCK: u8 = 128;

/// One function that the pre-parser read.
#[derive(Clone, Debug)]
pub struct FnFacts {
    /// The start of the function (`Func::span.start`): the key of [`Facts::find`]. An arrow starts at its first parameter (or `async`);
    /// a method starts at its `(`.
    pub start: u32,
    /// Where a full parse of this function starts. The `(` of the parameters, or the start of the arrow.
    pub parse_at: u32,
    /// The byte after the last byte of the function.
    pub end: u32,
    pub name: Option<Span>,
    pub kind: FuncKind,
    pub mode: FuncMode,
    /// `THIS` and the other flags of this module.
    pub flags: u16,
    /// The code around the function was strict (the function can become strict by a directive of its own).
    pub strict: bool,
    /// The strict state after the body (a directive of the body can set it; the code after the function does not inherit it).
    pub strict_out: bool,
    /// All parameters are plain identifiers.
    pub simple: bool,
    /// The `in` operator was not allowed at the start of an arrow (the head of a `for`): its body inherits that.
    pub no_in: bool,
    /// The context at the start (`C_*` bits).
    pub ctx: u8,
    /// How many parameters before the first default or rest: the `length` of the function.
    pub length: u32,
    /// The entries `fns[sub..own index]` are the functions inside this one, at any depth.
    pub sub: u32,
    /// The names that this function uses and does not bind: `Facts::caps[caps..caps + ncaps]`. They include the captures of the
    /// functions inside it that it does not bind. One entry for each distinct name.
    pub caps: u32,
    pub ncaps: u32,
}

/// The facts of one source text.
#[derive(Clone, Debug, Default)]
pub struct Facts {
    /// The functions, in post-order (see the module comment).
    pub fns: Vec<FnFacts>,
    /// The spans of the captured names. Read them through [`Facts::captures`].
    pub caps: Vec<Span>,
    /// The ids in the order of `start`, for [`Facts::find`]. Made when the lazy parse ends.
    pub by_start: Vec<FnId>,
    /// How many function bodies were read in check mode (a full parse of a function does not count). It is the length of
    /// [`Facts::fns`] when no function was read twice, except that a field initializer or a static block is no check-mode body.
    pub preparsed: u32,
    /// The text was read as a module.
    pub module: bool,
}

impl Facts {
    pub fn new() -> Facts {
        Facts::default()
    }

    pub fn get(&self, id: FnId) -> &FnFacts {
        &self.fns[id as usize]
    }

    /// The spans of the names that function `id` captures.
    pub fn captures(&self, id: FnId) -> &[Span] {
        let f = self.get(id);
        &self.caps[f.caps as usize..(f.caps + f.ncaps) as usize]
    }

    /// The entry that starts at byte `start`.
    pub fn find(&self, start: u32) -> Option<FnId> {
        let i = self.by_start.binary_search_by_key(&start, |&id| self.fns[id as usize].start).ok()?;
        Some(self.by_start[i])
    }

    /// The functions directly inside function `id` (`None`: the top level of the text), last first. An arrow in the head of another arrow
    /// is inside the outer arrow. A function in a class field initializer or a static block is inside the function that holds the class.
    pub fn children(&self, id: Option<FnId>) -> impl Iterator<Item = FnId> + '_ {
        let (lo, mut at) = match id {
            Some(i) => (self.fns[i as usize].sub, i),
            None => (0, self.fns.len() as u32),
        };
        std::iter::from_fn(move || {
            if at <= lo {
                return None;
            }
            at -= 1;
            let c = at;
            at = self.fns[c as usize].sub;
            Some(c)
        })
    }

    /// Sort `by_start`. The parser calls it at the end of a lazy parse.
    pub(crate) fn finish(&mut self) {
        self.by_start = (0..self.fns.len() as u32).collect();
        let fns = &self.fns;
        self.by_start.sort_unstable_by_key(|&i| fns[i as usize].start);
    }
}
