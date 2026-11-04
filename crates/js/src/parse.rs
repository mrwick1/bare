//! The JavaScript parser (ES2022 script) over [`crate::lex`]: [`parse_script`] gives a [`Program`].
//!
//! How it works, for readers who know JS only:
//! - Recursive descent for statements and bindings. Pratt (precedence climbing) for binary operators.
//! - The parser owns the lexical goals. It always lexes the next token with [`Goal::Div`]. Where an
//!   expression may start and the token is `/` or `/=`, it calls [`Lexer::rescan`] with
//!   [`Goal::RegExp`]. After the expression of a `${ ... }` it finds `}` and rescans it with
//!   [`Goal::TemplateTail`]. Nesting of templates is the recursion of the parser; the lexer keeps none.
//! - The parser holds one token, plus one peeked token when a decision needs it (`let`, `async`,
//!   `get`, `static`).
//! - Cover grammar: `(a, b) => x`, `[a, b] = c`, `{a = 1} = c` start as expressions. When `=>` or `=`
//!   follows, the expression is converted to a pattern ([`Parser::pattern_of`]). Where a `{a = 1}`
//!   was not converted, it is an error.
//! - Early errors are checked while parsing: scopes and redeclaration (a port of the rules that Acorn
//!   uses, ECMA-262 sections 8.1.x "Static Semantics: Early Errors"), strict mode, `return`/`break`/
//!   `continue`/labels, `super`, `new.target`, private names, class element rules.
//! - Limits are checked before anything grows. Recursion depth is counted, so deep input gives an error
//!   and never overflows the Rust stack.
//!
//! The parser stops at the first error. It does not recover.
//!
//! Not supported, with a precise error ([`ErrorKind::Unsupported`]): generators and `yield`, `import`/
//! `export`/`import()`/`import.meta`, class static blocks. Everything else in ES2022 script syntax is.
//! Not done here: regexp pattern grammar ([`REGEXP_PATTERN_VALIDATED`]), HTML-like comments, non-ASCII
//! identifiers (the lexer reports them), scope resolution of names.
//!
//! Specification sections used for the subtle rules (ECMA-262, 2022):
//! - 12.9.4 / B.1.2: legacy octal numbers and string escapes are errors in strict code.
//! - 12.10: automatic semicolon insertion, and the restricted productions (`return`, `break`,
//!   `continue`, `throw`, postfix `++`/`--`, `async` before `function` or arrow parameters, `=>`).
//! - 13.2.5.1, 13.15.1: `{a = 1}` is valid only where it becomes a pattern. Duplicate `__proto__`
//!   in an object literal is an error unless it becomes a pattern.
//! - 13.6: `**` does not take an unary operator as its left operand. 13.13: `??` cannot mix with `&&`/`||`
//!   without parentheses.
//! - 13.3.9: no optional chain in the tag of a template or the target of an assignment.
//! - 14.3.1.1, 14.7.5.1: `let`/`const` redeclaration; `for (let ...)`; lookahead restrictions on
//!   `let [`, `async of`.
//! - 15.2.1 (functions), 15.3.1 (arrows), 15.4.1 (methods): duplicate parameters; `"use strict"` with
//!   non-simple parameters; getter and setter parameter counts.
//! - 15.7.1: class element rules (one `constructor`, `prototype`, private names).
//! - 16.1: directive prologue.

use crate::ast::*;
use crate::lex::{self, Goal, Keyword as K, Kind, Lexer, Punct as P, TemplatePart, Token};
use std::borrow::Cow;
use std::collections::HashMap;

/// The regexp token is scanned and its flags are checked, but the pattern grammar is not. A pattern
/// such as `/(/` parses. Another component owns pattern validation; until it runs, a bad regexp is
/// not an early error. This constant is the explicit gate: it is `false`.
pub const REGEXP_PATTERN_VALIDATED: bool = false;

/// Parser limits. A program that goes past one gets [`ErrorKind::Limit`], never a panic or a stack overflow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Most source bytes.
    pub max_source: usize,
    /// Most nodes plus list entries in the whole tree. Checked before each one is added.
    pub max_nodes: u32,
    /// Most entries in one list (arguments, elements, statements in one block, ...).
    pub max_list: u32,
    /// Deepest nesting of expressions, statements and patterns. The Rust stack use is about
    /// `max_depth` times 2 KiB at worst in a release build (nested classes), so 256 needs about 0.5 MiB.
    /// A debug build uses about 6 times more. A test parses deep input on a 2 MiB (release) or 8 MiB (debug) thread.
    pub max_depth: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { max_source: 16 << 20, max_nodes: 16 << 20, max_list: 1 << 20, max_depth: 256 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitKind {
    Source,
    Nodes,
    List,
    Depth,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// The lexer failed.
    Lex(lex::ErrorKind),
    /// The token is not allowed here.
    Syntax,
    /// The text has valid shape but breaks an early error rule (redeclaration, strict mode, context).
    Early,
    /// Valid ES2022 syntax that this parser does not support yet.
    Unsupported,
    Limit(LimitKind),
}

/// The first error. `start..end` are byte offsets. `end == start` when there is no better span.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    pub msg: &'static str,
    pub start: u32,
    pub end: u32,
}

impl Error {
    /// Line (from 1) and UTF-16 column (from 0) of the start.
    pub fn location(&self, src: &str) -> lex::Location {
        lex::location(src, self.start as usize)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {} (bytes {}..{})", self.kind, self.msg, self.start, self.end)
    }
}
impl std::error::Error for Error {}

type R<T> = Result<T, Error>;

fn lex_err(e: lex::Error) -> Error {
    use lex::ErrorKind as L;
    let msg = match e.kind {
        L::SourceTooLarge => "source too large",
        L::UnexpectedChar => "unexpected character",
        L::NonAscii => "non-ASCII character outside a string, comment or regexp is not supported",
        L::UnterminatedString => "unterminated string",
        L::UnterminatedComment => "unterminated comment",
        L::UnterminatedTemplate => "unterminated template",
        L::UnterminatedRegExp => "unterminated regular expression",
        L::InvalidEscape => "invalid escape sequence",
        L::InvalidNumber => "invalid number",
        L::InvalidRegExpFlag => "invalid regular expression flag",
        L::DuplicateRegExpFlag => "duplicate regular expression flag",
        L::InvalidPrivateName => "invalid private name",
        L::ExpectedTemplateContinuation => "expected template continuation",
        L::BadToken => "bad token",
    };
    Error { kind: ErrorKind::Lex(e.kind), msg, start: e.offset, end: e.offset }
}

/// Parse `source` as a script (not a module). Borrows `source`; the [`Program`] owns its nodes.
pub fn parse_script(source: &str, limits: Limits) -> Result<Program<'_>, Error> {
    if source.len() > limits.max_source {
        let at = limits.max_source.min(u32::MAX as usize) as u32;
        return Err(Error { kind: ErrorKind::Limit(LimitKind::Source), msg: "source too large", start: at, end: at });
    }
    let mut lexer = Lexer::new(source).map_err(lex_err)?;
    let tok = lexer.next(Goal::Div).map_err(lex_err)?;
    let mut p = Parser {
        src: source,
        lex: lexer,
        tok,
        peeked: None,
        prev_end: 0,
        p: Program::new(source),
        limits,
        used: 0,
        scratch: Vec::new(),
        depth: 0,
        strict: false,
        ctx: Ctx::default(),
        labels: Vec::new(),
        label_base: 0,
        scopes: Vec::new(),
        nscopes: 0,
        privs: Vec::new(),
        arrow_at: u32::MAX,
        last_octal: None,
        cover: Cover::default(),
        no_in: false,
        spread_commas: Vec::new(),
        for_await_head: false,
        await_pos: 0,
    };
    p.parse_program()?;
    Ok(p.p)
}

// ---------------------------------------------------------------- scopes

const S_VAR: u8 = 1; // `var` stops here: a function or the script
const S_FUNC: u8 = 2; // a function: its function declarations act as `var`
const S_TOP: u8 = 4; // the script: its function declarations act as `var`
const S_CATCH: u8 = 8; // the scope of a `catch (e)` with a plain identifier

const N_VAR: u8 = 1;
const N_LEX: u8 = 2;
const N_FUNC: u8 = 4;
const N_CATCH: u8 = 8; // a lexical name that is the plain parameter of this catch
const N_PARAM: u8 = 16;

/// A scope keeps small name lists. Above `INDEX_AT` names it adds a hash index.
#[derive(Default)]
struct Scope<'a> {
    flags: u8,
    names: Vec<(Cow<'a, str>, u8)>,
    index: HashMap<Cow<'a, str>, u32>,
}
const INDEX_AT: usize = 24;

impl<'a> Scope<'a> {
    fn reset(&mut self, flags: u8) {
        self.flags = flags;
        self.names.clear();
        self.index.clear();
    }
    fn find(&self, n: &str) -> Option<usize> {
        if self.names.len() > INDEX_AT {
            self.index.get(n).map(|&i| i as usize)
        } else {
            self.names.iter().position(|(k, _)| k == n)
        }
    }
    fn flags_of(&self, n: &str) -> u8 {
        self.find(n).map_or(0, |i| self.names[i].1)
    }
    fn add(&mut self, n: Cow<'a, str>, bits: u8) {
        if let Some(i) = self.find(&n) {
            self.names[i].1 |= bits;
            return;
        }
        self.names.push((n, bits));
        if self.names.len() == INDEX_AT + 1 {
            for (i, (k, _)) in self.names.iter().enumerate() {
                self.index.insert(k.clone(), i as u32);
            }
        } else if self.names.len() > INDEX_AT + 1 {
            let last = self.names.len() - 1;
            self.index.insert(self.names[last].0.clone(), last as u32);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Bind {
    Var,
    Lex,
    Func,
    SimpleCatch,
}

struct Label<'a> {
    name: Option<Cow<'a, str>>,
    kind: u8, // 0 none, 1 loop, 2 switch
    stmt_start: u32,
}
const L_LOOP: u8 = 1;
const L_SWITCH: u8 = 2;

#[derive(Default)]
struct PrivScope<'a> {
    declared: HashMap<Cow<'a, str>, u8>,
    used: Vec<(Cow<'a, str>, Span)>,
}
const PV_PLAIN: u8 = 1; // a field or a method
const PV_GET: u8 = 2;
const PV_SET: u8 = 4;
const PV_STATIC: u8 = 8;

/// What the function that is being parsed knows that its body needs.
#[derive(Clone, Copy)]
struct BodyInfo {
    params: List<Pat>,
    name: Option<Span>,
    simple: bool,
    first_dup: Option<Span>,
}

/// Facts about the enclosing function that rules depend on.
#[derive(Clone, Copy, Default)]
struct Ctx {
    in_func: bool,    // `return` is allowed
    is_async: bool,   // `await` is an operator
    new_target: bool, // `new.target` is allowed
    super_prop: bool, // `super.x` is allowed
    super_call: bool, // `super()` is allowed
    field_init: bool, // inside a class field initializer: `arguments` is not allowed
}

/// Pending cover-grammar facts (`{a = 1}` and duplicate `__proto__`): counts of what a pattern must still consume.
#[derive(Clone, Copy, Default)]
struct Cover {
    shorthand: u32,
    first_shorthand: u32,
    proto: u32,
    first_proto: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sc {
    /// A statement-list item: declarations are allowed.
    Top,
    /// The body of `if`.
    If,
    /// The body of a label that is not inside `if`.
    Label,
    /// The body of a loop, `with`, or a label in such a place.
    Other,
}

enum Op {
    B(BinaryOp),
    L(LogicalOp),
}

struct Parser<'a> {
    src: &'a str,
    lex: Lexer<'a>,
    tok: Token,
    peeked: Option<Token>,
    prev_end: u32,
    p: Program<'a>,
    limits: Limits,
    used: u32,
    scratch: Vec<u32>,
    depth: u32,
    strict: bool,
    ctx: Ctx,
    labels: Vec<Label<'a>>,
    label_base: usize,
    scopes: Vec<Scope<'a>>,
    nscopes: usize,
    privs: Vec<PrivScope<'a>>,
    /// Start of the assignment expression where an arrow function may start (Acorn: `potentialArrowAt`).
    arrow_at: u32,
    last_octal: Option<u32>,
    cover: Cover,
    /// The `no_in` flag of the assignment expression that is being parsed (an arrow body inherits it).
    no_in: bool,
    /// Start offsets of arrays and objects that have a comma after a final `...x` (invalid as a pattern).
    spread_commas: Vec<u32>,
    /// True while the head of a `for await (` is parsed: `async of` is then not an arrow (Acorn: `potentialArrowInForAwait`).
    for_await_head: bool,
    /// Start + 1 of the first `await` expression since the last reset; 0 when none (Acorn: `awaitPos`).
    await_pos: u32,
}

/// Add a node to an arena. The caller has already checked the node limit.
fn push_node<T>(arena: &mut Vec<T>, v: T) -> Id<T> {
    let id = Id::from_index(arena.len());
    arena.push(v);
    id
}

macro_rules! mk {
    ($s:ident, $arena:ident, $val:expr) => {{
        $s.grow()?;
        let v = $val;
        push_node(&mut $s.p.$arena, v)
    }};
}

fn is_reserved_word(n: &str) -> bool {
    matches!(
        n,
        "break" | "case" | "catch" | "class" | "const" | "continue" | "debugger" | "default" | "delete" | "do"
            | "else" | "enum" | "export" | "extends" | "false" | "finally" | "for" | "function" | "if" | "import"
            | "in" | "instanceof" | "new" | "null" | "return" | "super" | "switch" | "this" | "throw" | "true"
            | "try" | "typeof" | "var" | "void" | "while" | "with"
    )
}

fn is_strict_reserved(n: &str) -> bool {
    matches!(n, "implements" | "interface" | "let" | "package" | "private" | "protected" | "public" | "static" | "yield")
}

fn assign_op(p: P) -> Option<AssignOp> {
    use AssignOp::{Compound as C, Logical as L};
    Some(match p {
        P::Assign => AssignOp::Assign,
        P::PlusAssign => C(BinaryOp::Add),
        P::MinusAssign => C(BinaryOp::Sub),
        P::StarAssign => C(BinaryOp::Mul),
        P::SlashAssign => C(BinaryOp::Div),
        P::PercentAssign => C(BinaryOp::Mod),
        P::StarStarAssign => C(BinaryOp::Exp),
        P::ShlAssign => C(BinaryOp::Shl),
        P::ShrAssign => C(BinaryOp::Shr),
        P::UShrAssign => C(BinaryOp::UShr),
        P::AmpAssign => C(BinaryOp::BitAnd),
        P::PipeAssign => C(BinaryOp::BitOr),
        P::CaretAssign => C(BinaryOp::BitXor),
        P::AmpAmpAssign => L(LogicalOp::And),
        P::PipePipeAssign => L(LogicalOp::Or),
        P::QQAssign => L(LogicalOp::Nullish),
        _ => return None,
    })
}

/// The binary operator at a token and its precedence (higher binds tighter).
fn bin_op(kind: Kind, no_in: bool) -> Option<(Op, u8)> {
    use BinaryOp as B;
    Some(match kind {
        Kind::Punct(p) => match p {
            P::QQ => (Op::L(LogicalOp::Nullish), 1),
            P::PipePipe => (Op::L(LogicalOp::Or), 1),
            P::AmpAmp => (Op::L(LogicalOp::And), 2),
            P::Pipe => (Op::B(B::BitOr), 3),
            P::Caret => (Op::B(B::BitXor), 4),
            P::Amp => (Op::B(B::BitAnd), 5),
            P::EqEq => (Op::B(B::Eq), 6),
            P::NotEq => (Op::B(B::Ne), 6),
            P::EqEqEq => (Op::B(B::StrictEq), 6),
            P::NotEqEq => (Op::B(B::StrictNe), 6),
            P::Lt => (Op::B(B::Lt), 7),
            P::Gt => (Op::B(B::Gt), 7),
            P::LtEq => (Op::B(B::Le), 7),
            P::GtEq => (Op::B(B::Ge), 7),
            P::Shl => (Op::B(B::Shl), 8),
            P::Shr => (Op::B(B::Shr), 8),
            P::UShr => (Op::B(B::UShr), 8),
            P::Plus => (Op::B(B::Add), 9),
            P::Minus => (Op::B(B::Sub), 9),
            P::Star => (Op::B(B::Mul), 10),
            P::Slash => (Op::B(B::Div), 10),
            P::Percent => (Op::B(B::Mod), 10),
            P::StarStar => (Op::B(B::Exp), 11),
            _ => return None,
        },
        Kind::Keyword(K::Instanceof) => (Op::B(B::Instanceof), 7),
        Kind::Keyword(K::In) if !no_in => (Op::B(B::In), 7),
        _ => return None,
    })
}

const PREC_REL: u8 = 7;

impl<'a> Parser<'a> {
    // ------------------------------------------------------------ tokens and errors

    fn err(&self, kind: ErrorKind, msg: &'static str, start: u32, end: u32) -> Error {
        Error { kind, msg, start, end }
    }
    fn syntax(&self, msg: &'static str, span: Span) -> Error {
        self.err(ErrorKind::Syntax, msg, span.start, span.end)
    }
    fn early(&self, msg: &'static str, span: Span) -> Error {
        self.err(ErrorKind::Early, msg, span.start, span.end)
    }
    fn unsupported(&self, msg: &'static str, start: u32, end: u32) -> Error {
        self.err(ErrorKind::Unsupported, msg, start, end)
    }
    fn tspan(&self) -> Span {
        Span { start: self.tok.start, end: self.tok.end }
    }
    fn unexpected(&self) -> Error {
        let msg = if self.tok.kind == Kind::Eof { "unexpected end of input" } else { "unexpected token" };
        self.syntax(msg, self.tspan())
    }

    fn advance(&mut self) -> R<()> {
        self.prev_end = self.tok.end;
        self.tok = match self.peeked.take() {
            Some(t) => t,
            None => self.lex.next(Goal::Div).map_err(lex_err)?,
        };
        Ok(())
    }

    /// The token after the current one (lexed with `Div`).
    fn peek(&mut self) -> R<Token> {
        if let Some(t) = self.peeked {
            return Ok(t);
        }
        let t = self.lex.next(Goal::Div).map_err(lex_err)?;
        self.peeked = Some(t);
        Ok(t)
    }

    /// Scan the current token again with another goal.
    fn rescan(&mut self, goal: Goal) -> R<()> {
        self.peeked = None;
        self.tok = self.lex.rescan(&self.tok, goal).map_err(lex_err)?;
        Ok(())
    }

    fn is(&self, p: P) -> bool {
        self.tok.kind == Kind::Punct(p)
    }
    fn is_kw(&self, k: K) -> bool {
        self.tok.kind == Kind::Keyword(k)
    }
    fn text(&self) -> &'a str {
        &self.src[self.tok.start as usize..self.tok.end as usize]
    }
    /// A context word such as `async`, `of`, `let`: an `Ident` without escapes.
    fn is_word(&self, w: &str) -> bool {
        self.tok.kind == Kind::Ident && !self.tok.escapes && self.text() == w
    }
    fn eat(&mut self, p: P) -> R<bool> {
        if self.is(p) {
            self.advance()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
    fn expect(&mut self, p: P) -> R<()> {
        if self.is(p) {
            self.advance()
        } else {
            Err(self.unexpected())
        }
    }
    fn semicolon(&mut self) -> R<()> {
        if self.eat(P::Semi)? || self.is(P::RBrace) || self.tok.kind == Kind::Eof || self.tok.newline_before {
            Ok(())
        } else {
            Err(self.unexpected())
        }
    }

    // ------------------------------------------------------------ limits, arenas, lists

    fn limit(&self, k: LimitKind, msg: &'static str) -> Error {
        self.err(ErrorKind::Limit(k), msg, self.tok.start, self.tok.end)
    }
    fn grow(&mut self) -> R<()> {
        if self.used >= self.limits.max_nodes {
            return Err(self.limit(LimitKind::Nodes, "too many syntax tree nodes"));
        }
        self.used += 1;
        Ok(())
    }
    fn enter(&mut self) -> R<()> {
        if self.depth >= self.limits.max_depth {
            return Err(self.limit(LimitKind::Depth, "nesting too deep"));
        }
        self.depth += 1;
        Ok(())
    }

    fn lbegin(&self) -> usize {
        self.scratch.len()
    }
    fn lpush(&mut self, base: usize, raw: u32) -> R<()> {
        if self.scratch.len() - base >= self.limits.max_list as usize {
            return Err(self.limit(LimitKind::List, "list too long"));
        }
        self.grow()?;
        self.scratch.push(raw);
        Ok(())
    }
    fn lend<T>(&mut self, base: usize) -> List<T> {
        let start = self.p.lists.len() as u32;
        let len = (self.scratch.len() - base) as u32;
        self.p.lists.extend_from_slice(&self.scratch[base..]);
        self.scratch.truncate(base);
        List::new(start, len)
    }

    fn mk_expr(&mut self, kind: ExprKind, start: u32) -> R<ExprId> {
        let end = self.prev_end;
        self.mk_expr_at(kind, start, end)
    }
    fn mk_expr_at(&mut self, kind: ExprKind, start: u32, end: u32) -> R<ExprId> {
        Ok(mk!(self, exprs, Expr { kind, span: Span { start, end } }))
    }
    fn mk_stmt(&mut self, kind: StmtKind, start: u32) -> R<StmtId> {
        let end = self.prev_end;
        Ok(mk!(self, stmts, Stmt { kind, span: Span { start, end } }))
    }
    fn mk_pat(&mut self, kind: PatKind, start: u32, end: u32) -> R<PatId> {
        Ok(mk!(self, pats, Pat { kind, span: Span { start, end } }))
    }
    fn ex(&self, id: ExprId) -> Expr {
        self.p.exprs[id.index()]
    }
    fn pt(&self, id: PatId) -> Pat {
        self.p.pats[id.index()]
    }

    // ------------------------------------------------------------ names and scopes

    fn name_of(&self, span: Span) -> Cow<'a, str> {
        self.p.name(span)
    }

    fn push_scope(&mut self, flags: u8) {
        if self.nscopes == self.scopes.len() {
            self.scopes.push(Scope::default());
        }
        self.scopes[self.nscopes].reset(flags);
        self.nscopes += 1;
    }
    fn pop_scope(&mut self) {
        self.nscopes -= 1;
    }
    fn funcs_as_var(&self, i: usize) -> bool {
        self.scopes[i].flags & (S_FUNC | S_TOP) != 0
    }

    /// Port of Acorn's `declareName`. `at` is the name, for the error.
    fn declare(&mut self, name: Cow<'a, str>, bind: Bind, at: Span) -> R<()> {
        let top = self.nscopes - 1;
        let mut redeclared = false;
        match bind {
            Bind::Lex => {
                let s = &mut self.scopes[top];
                redeclared = s.flags_of(&name) & (N_LEX | N_FUNC | N_VAR) != 0;
                s.add(name, N_LEX);
            }
            Bind::SimpleCatch => self.scopes[top].add(name, N_LEX | N_CATCH),
            Bind::Func => {
                let as_var = self.funcs_as_var(top);
                let s = &mut self.scopes[top];
                let f = s.flags_of(&name);
                redeclared = if as_var { f & N_LEX != 0 } else { f & (N_LEX | N_VAR) != 0 };
                s.add(name, N_FUNC);
            }
            Bind::Var => {
                for i in (0..=top).rev() {
                    let as_var = self.funcs_as_var(i);
                    let s = &mut self.scopes[i];
                    let f = s.flags_of(&name);
                    if (f & N_LEX != 0 && f & N_CATCH == 0) || (!as_var && f & N_FUNC != 0) {
                        redeclared = true;
                        break;
                    }
                    s.add(name.clone(), N_VAR);
                    if s.flags & S_VAR != 0 {
                        break;
                    }
                }
            }
        }
        if redeclared {
            return Err(self.early("identifier has already been declared", at));
        }
        Ok(())
    }

    /// Reserved words and context rules for a name used as a reference or a binding.
    fn check_word(&self, span: Span) -> R<()> {
        let text = self.p.text(span);
        let escaped = text.as_bytes().contains(&b'\\');
        if !(escaped || self.strict || self.ctx.is_async) {
            return Ok(());
        }
        let name = self.name_of(span);
        if escaped && is_reserved_word(&name) {
            return Err(self.early("keyword must not contain escaped characters", span));
        }
        if self.strict && is_strict_reserved(&name) {
            return Err(self.early("reserved word in strict mode", span));
        }
        if self.ctx.is_async && name == "await" {
            return Err(self.early("'await' is not an identifier in an async function", span));
        }
        Ok(())
    }

    /// A name that is bound (declared, a parameter, an assignment target in a pattern).
    fn check_binding_name(&self, span: Span) -> R<()> {
        self.check_word(span)?;
        if self.strict {
            let n = self.name_of(span);
            if n == "eval" || n == "arguments" {
                return Err(self.early("cannot bind 'eval' or 'arguments' in strict mode", span));
            }
        }
        Ok(())
    }

    /// Call `f` with each bound name in a pattern.
    fn each_name(&mut self, pat: PatId, f: &mut dyn FnMut(&mut Self, Span) -> R<()>) -> R<()> {
        let p = self.pt(pat);
        match p.kind {
            PatKind::Ident => f(self, p.span),
            PatKind::Member(_) | PatKind::Hole => Ok(()),
            PatKind::Assign { target, .. } => self.each_name(target, f),
            PatKind::Rest(inner) => self.each_name(inner, f),
            PatKind::Array(l) => {
                for i in 0..l.len {
                    let id = Id::from_raw_pub(self.p.lists[(l.start + i) as usize]);
                    self.each_name(id, f)?;
                }
                Ok(())
            }
            PatKind::Object(l) => {
                for i in 0..l.len {
                    let pp = self.p.pat_props[Id::<PatProp>::from_raw_pub(self.p.lists[(l.start + i) as usize]).index()];
                    self.each_name(pp.value, f)?;
                }
                Ok(())
            }
        }
    }

    fn declare_pat(&mut self, pat: PatId, bind: Bind) -> R<()> {
        self.each_name(pat, &mut |s, span| {
            s.check_binding_name(span)?;
            let n = s.name_of(span);
            s.declare(n, bind, span)
        })
    }

    /// Declare parameters in the function scope. Returns the first duplicate, if duplicates are allowed here.
    fn declare_params(&mut self, params: List<Pat>, allow_dup: bool) -> R<Option<Span>> {
        let mut first_dup = None;
        for i in 0..params.len {
            let id = Id::from_raw_pub(self.p.lists[(params.start + i) as usize]);
            self.each_name(id, &mut |s, span| {
                s.check_binding_name(span)?;
                let n = s.name_of(span);
                let top = s.nscopes - 1;
                if s.scopes[top].flags_of(&n) & N_PARAM != 0 {
                    if !allow_dup {
                        return Err(s.early("duplicate parameter name not allowed here", span));
                    }
                    first_dup.get_or_insert(span);
                }
                s.scopes[top].add(n, N_VAR | N_PARAM);
                Ok(())
            })?;
        }
        Ok(first_dup)
    }

    // ------------------------------------------------------------ program and statement lists

    fn parse_program(&mut self) -> R<()> {
        self.push_scope(S_VAR | S_TOP);
        let base = self.lbegin();
        let mut prologue = true;
        let mut octal = None;
        while self.tok.kind != Kind::Eof {
            let start = self.tok.start;
            let st = self.parse_item(prologue)?;
            if prologue {
                match self.p.stmts[st.index()].kind {
                    StmtKind::Directive(e) => self.on_directive(e, start, &mut octal, None)?,
                    _ => prologue = false,
                }
            }
            self.lpush(base, st.raw())?;
        }
        self.p.body = self.lend(base);
        self.p.strict = self.strict;
        self.pop_scope();
        Ok(())
    }

    /// A directive in a prologue. `"use strict"` makes the rest of the code strict.
    fn on_directive(&mut self, e: ExprId, stmt_start: u32, octal: &mut Option<u32>, info: Option<BodyInfo>) -> R<()> {
        if let Some(o) = self.last_octal.take()
            && o >= stmt_start
        {
            octal.get_or_insert(o);
        }
        let span = self.ex(e).span;
        let t = self.p.text(span);
        if t != "\"use strict\"" && t != "'use strict'" {
            return Ok(());
        }
        if self.strict {
            return Ok(());
        }
        if let Some(i) = info
            && !i.simple
        {
            return Err(self.early("'use strict' directive in a function with non-simple parameters", span));
        }
        if let Some(o) = *octal {
            return Err(self.early("octal escape sequences are not allowed in strict mode", Span { start: o, end: o + 1 }));
        }
        self.strict = true;
        if let Some(i) = info {
            // The function name and parameters were read as sloppy code. Check them again.
            if let Some(n) = i.name {
                self.check_binding_name(n)?;
            }
            for k in 0..i.params.len {
                let id = Id::from_raw_pub(self.p.lists[(i.params.start + k) as usize]);
                self.each_name(id, &mut |s, span| s.check_binding_name(span))?;
            }
            if let Some(d) = i.first_dup {
                return Err(self.early("duplicate parameter name not allowed in strict mode", d));
            }
        }
        Ok(())
    }

    /// One statement-list item. `prologue`: a string expression statement is a directive.
    fn parse_item(&mut self, prologue: bool) -> R<StmtId> {
        self.enter()?;
        let r = self.parse_statement(Sc::Top, prologue);
        self.depth -= 1;
        r
    }

    fn parse_stmt(&mut self, sc: Sc) -> R<StmtId> {
        self.enter()?;
        let r = self.parse_statement(sc, false);
        self.depth -= 1;
        r
    }

    fn parse_statement(&mut self, sc: Sc, prologue: bool) -> R<StmtId> {
        let start = self.tok.start;
        match self.tok.kind {
            Kind::Keyword(k) => match k {
                K::Var => self.parse_var_stmt(VarKind::Var),
                K::Const => {
                    if sc != Sc::Top {
                        return Err(self.unexpected());
                    }
                    self.parse_var_stmt(VarKind::Const)
                }
                K::Function => {
                    if sc != Sc::Top && (self.strict || !matches!(sc, Sc::If | Sc::Label)) {
                        return Err(self.syntax("function declaration is not allowed here", self.tspan()));
                    }
                    self.parse_function_decl(start, false, sc != Sc::Top)
                }
                K::Class => {
                    if sc != Sc::Top {
                        return Err(self.unexpected());
                    }
                    let id = self.parse_class(start, true)?;
                    self.mk_stmt(StmtKind::Class(id), start)
                }
                K::If => self.parse_if(),
                K::For => self.parse_for(),
                K::While => {
                    self.advance()?;
                    self.expect(P::LParen)?;
                    let test = self.parse_expression(false)?;
                    self.expect(P::RParen)?;
                    let body = self.parse_loop_body()?;
                    self.mk_stmt(StmtKind::While { test, body }, start)
                }
                K::Do => {
                    self.advance()?;
                    let body = self.parse_loop_body()?;
                    if !self.is_kw(K::While) {
                        return Err(self.unexpected());
                    }
                    self.advance()?;
                    self.expect(P::LParen)?;
                    let test = self.parse_expression(false)?;
                    self.expect(P::RParen)?;
                    self.eat(P::Semi)?;
                    self.mk_stmt(StmtKind::DoWhile { body, test }, start)
                }
                K::Return => {
                    if !self.ctx.in_func {
                        return Err(self.early("'return' outside of a function", self.tspan()));
                    }
                    self.advance()?;
                    let arg = if self.is(P::Semi) || self.is(P::RBrace) || self.tok.kind == Kind::Eof || self.tok.newline_before {
                        None
                    } else {
                        Some(self.parse_expression(false)?)
                    };
                    self.semicolon()?;
                    self.mk_stmt(StmtKind::Return(arg), start)
                }
                K::Break | K::Continue => self.parse_break_continue(k == K::Break),
                K::Throw => {
                    self.advance()?;
                    if self.tok.newline_before {
                        return Err(self.syntax("illegal newline after throw", self.tspan()));
                    }
                    let arg = self.parse_expression(false)?;
                    self.semicolon()?;
                    self.mk_stmt(StmtKind::Throw(arg), start)
                }
                K::Try => self.parse_try(),
                K::Switch => self.parse_switch(),
                K::With => {
                    if self.strict {
                        return Err(self.early("'with' in strict mode", self.tspan()));
                    }
                    self.advance()?;
                    self.expect(P::LParen)?;
                    let obj = self.parse_expression(false)?;
                    self.expect(P::RParen)?;
                    let body = self.parse_stmt(Sc::Other)?;
                    self.mk_stmt(StmtKind::With { obj, body }, start)
                }
                K::Debugger => {
                    self.advance()?;
                    self.semicolon()?;
                    self.mk_stmt(StmtKind::Debugger, start)
                }
                K::Import => {
                    let n = self.peek()?;
                    if matches!(n.kind, Kind::Punct(P::LParen) | Kind::Punct(P::Dot)) {
                        self.parse_expr_statement(start, prologue, sc)
                    } else {
                        Err(self.unsupported("import declarations (modules) are not supported", self.tok.start, self.tok.end))
                    }
                }
                K::Export => Err(self.unsupported("export declarations (modules) are not supported", self.tok.start, self.tok.end)),
                _ => self.parse_expr_statement(start, prologue, sc),
            },
            Kind::Punct(P::LBrace) => {
                self.push_scope(0);
                let list = self.parse_block_list()?;
                self.pop_scope();
                self.mk_stmt(StmtKind::Block(list), start)
            }
            Kind::Punct(P::Semi) => {
                self.advance()?;
                self.mk_stmt(StmtKind::Empty, start)
            }
            Kind::Ident if !self.tok.escapes => {
                if self.text() == "let" && self.let_is_decl(sc)? {
                    if sc != Sc::Top {
                        return Err(self.syntax("lexical declaration cannot appear in a single-statement context", self.tspan()));
                    }
                    return self.parse_var_stmt(VarKind::Let);
                }
                if self.text() == "async" {
                    let n = self.peek()?;
                    if n.kind == Kind::Keyword(K::Function) && !n.newline_before {
                        if sc != Sc::Top {
                            return Err(self.unexpected());
                        }
                        self.advance()?;
                        return self.parse_function_decl(start, true, false);
                    }
                }
                self.parse_expr_statement(start, prologue, sc)
            }
            _ => self.parse_expr_statement(start, prologue, sc),
        }
    }

    /// Is the `let` at the current token the start of a declaration? (Acorn's `isLet`.)
    fn let_is_decl(&mut self, sc: Sc) -> R<bool> {
        let n = self.peek()?;
        Ok(match n.kind {
            Kind::Punct(P::LBracket) => true,
            _ if sc != Sc::Top => false,
            Kind::Punct(P::LBrace) | Kind::Ident => true,
            Kind::Keyword(K::In | K::Instanceof) => false,
            Kind::Keyword(_) => true,
            _ => false,
        })
    }

    fn parse_expr_statement(&mut self, start: u32, prologue: bool, sc: Sc) -> R<StmtId> {
        let e = self.parse_expression(false)?;
        let ex = self.ex(e);
        if matches!(ex.kind, ExprKind::Ident) && self.is(P::Colon) {
            return self.parse_labeled(start, ex.span, sc);
        }
        self.semicolon()?;
        if prologue && matches!(ex.kind, ExprKind::String) {
            return self.mk_stmt(StmtKind::Directive(e), start);
        }
        self.mk_stmt(StmtKind::Expr(e), start)
    }

    fn parse_labeled(&mut self, start: u32, label: Span, sc: Sc) -> R<StmtId> {
        self.check_word(label)?;
        let name = self.name_of(label);
        if self.labels[self.label_base..].iter().any(|l| l.name.as_deref() == Some(&*name)) {
            return Err(self.early("label is already declared", label));
        }
        self.advance()?; // `:`
        let kind = match self.tok.kind {
            Kind::Keyword(K::For | K::While | K::Do) => L_LOOP,
            Kind::Keyword(K::Switch) => L_SWITCH,
            _ => 0,
        };
        // `a: b: while (...)`: the earlier labels on the same statement take the same kind.
        let here = self.tok.start;
        for l in self.labels[self.label_base..].iter_mut().rev() {
            if l.stmt_start == start {
                l.stmt_start = here;
                l.kind = kind;
            } else {
                break;
            }
        }
        self.labels.push(Label { name: Some(name), kind, stmt_start: here });
        // A label body allows a sloppy function declaration, but not inside `if` or a loop body.
        let r = self.parse_stmt(if matches!(sc, Sc::Top | Sc::Label) { Sc::Label } else { Sc::Other });
        self.labels.pop();
        let body = r?;
        self.mk_stmt(StmtKind::Labeled { label, body }, start)
    }

    fn parse_loop_body(&mut self) -> R<StmtId> {
        self.labels.push(Label { name: None, kind: L_LOOP, stmt_start: u32::MAX });
        let r = self.parse_stmt(Sc::Other);
        self.labels.pop();
        r
    }

    fn parse_break_continue(&mut self, is_break: bool) -> R<StmtId> {
        let start = self.tok.start;
        self.advance()?;
        let label = if self.tok.kind == Kind::Ident && !self.tok.newline_before {
            let s = self.tspan();
            self.advance()?;
            Some(s)
        } else {
            None
        };
        self.semicolon()?;
        let name = label.map(|s| self.name_of(s));
        let mut found = false;
        for l in self.labels[self.label_base..].iter().rev() {
            if name.is_none() || l.name.as_deref() == name.as_deref() {
                if l.kind != 0 && (is_break || l.kind == L_LOOP) {
                    found = true;
                    break;
                }
                if name.is_some() && is_break {
                    found = true;
                    break;
                }
            }
        }
        if !found {
            let span = Span { start, end: self.prev_end };
            return Err(self.early(if is_break { "'break' without a matching loop, switch or label" } else { "'continue' without a matching loop or label" }, span));
        }
        self.mk_stmt(if is_break { StmtKind::Break(label) } else { StmtKind::Continue(label) }, start)
    }

    fn parse_if(&mut self) -> R<StmtId> {
        let start = self.tok.start;
        self.advance()?;
        self.expect(P::LParen)?;
        let test = self.parse_expression(false)?;
        self.expect(P::RParen)?;
        let cons = self.parse_stmt(Sc::If)?;
        let alt = if self.is_kw(K::Else) {
            self.advance()?;
            Some(self.parse_stmt(Sc::If)?)
        } else {
            None
        };
        self.mk_stmt(StmtKind::If { test, cons, alt }, start)
    }

    /// `{ ... }` with the current token at `{`. The caller opens the scope.
    fn parse_block_list(&mut self) -> R<List<Stmt>> {
        self.expect(P::LBrace)?;
        let base = self.lbegin();
        while !self.is(P::RBrace) {
            if self.tok.kind == Kind::Eof {
                return Err(self.unexpected());
            }
            let st = self.parse_item(false)?;
            self.lpush(base, st.raw())?;
        }
        self.advance()?;
        Ok(self.lend(base))
    }

    fn parse_try(&mut self) -> R<StmtId> {
        let start = self.tok.start;
        self.advance()?;
        if !self.is(P::LBrace) {
            return Err(self.unexpected());
        }
        let bstart = self.tok.start;
        self.push_scope(0);
        let list = self.parse_block_list()?;
        self.pop_scope();
        let block = self.mk_stmt(StmtKind::Block(list), bstart)?;
        let mut handler = None;
        if self.is_kw(K::Catch) {
            let cstart = self.tok.start;
            self.advance()?;
            let (param, simple);
            if self.eat(P::LParen)? {
                let pat = self.parse_binding_atom()?;
                simple = matches!(self.pt(pat).kind, PatKind::Ident);
                self.push_scope(if simple { S_CATCH } else { 0 });
                self.declare_pat(pat, if simple { Bind::SimpleCatch } else { Bind::Lex })?;
                self.expect(P::RParen)?;
                param = Some(pat);
            } else {
                self.push_scope(0);
                param = None;
            }
            if !self.is(P::LBrace) {
                return Err(self.unexpected());
            }
            let b2 = self.tok.start;
            let list = self.parse_block_list()?; // the catch parameter and the block share one scope
            self.pop_scope();
            let body = self.mk_stmt(StmtKind::Block(list), b2)?;
            let id = mk!(self, catches, Catch { param, body, span: Span { start: cstart, end: self.prev_end } });
            handler = Some(id);
        }
        let mut finalizer = None;
        if self.is_kw(K::Finally) {
            self.advance()?;
            if !self.is(P::LBrace) {
                return Err(self.unexpected());
            }
            let fstart = self.tok.start;
            self.push_scope(0);
            let list = self.parse_block_list()?;
            self.pop_scope();
            finalizer = Some(self.mk_stmt(StmtKind::Block(list), fstart)?);
        }
        if handler.is_none() && finalizer.is_none() {
            return Err(self.syntax("missing catch or finally clause", self.tspan()));
        }
        self.mk_stmt(StmtKind::Try { block, handler, finalizer }, start)
    }

    fn parse_switch(&mut self) -> R<StmtId> {
        let start = self.tok.start;
        self.advance()?;
        self.expect(P::LParen)?;
        let disc = self.parse_expression(false)?;
        self.expect(P::RParen)?;
        self.expect(P::LBrace)?;
        self.push_scope(0);
        self.labels.push(Label { name: None, kind: L_SWITCH, stmt_start: u32::MAX });
        let cbase = self.lbegin();
        let mut seen_default = false;
        while !self.is(P::RBrace) {
            let cstart = self.tok.start;
            let test = if self.is_kw(K::Case) {
                self.advance()?;
                Some(self.parse_expression(false)?)
            } else if self.is_kw(K::Default) {
                if seen_default {
                    return Err(self.syntax("multiple default clauses", self.tspan()));
                }
                seen_default = true;
                self.advance()?;
                None
            } else {
                return Err(self.unexpected());
            };
            self.expect(P::Colon)?;
            let sbase = self.lbegin();
            while !(self.is(P::RBrace) || self.is_kw(K::Case) || self.is_kw(K::Default)) {
                if self.tok.kind == Kind::Eof {
                    return Err(self.unexpected());
                }
                let st = self.parse_item(false)?;
                self.lpush(sbase, st.raw())?;
            }
            let body = self.lend(sbase);
            let id: Id<Case> = mk!(self, cases, Case { test, body, span: Span { start: cstart, end: self.prev_end } });
            self.lpush(cbase, id.raw())?;
        }
        self.advance()?;
        let cases = self.lend(cbase);
        self.labels.pop();
        self.pop_scope();
        self.mk_stmt(StmtKind::Switch { disc, cases }, start)
    }

    // ------------------------------------------------------------ var, for

    fn parse_var_stmt(&mut self, kind: VarKind) -> R<StmtId> {
        let start = self.tok.start;
        self.advance()?;
        let (decls, _) = self.parse_var_decls(kind, false)?;
        self.semicolon()?;
        self.mk_stmt(StmtKind::Var { kind, decls }, start)
    }

    /// Declarators after `var`/`let`/`const`. In a `for` head a missing initializer is returned
    /// (it is an error only if the head turns out to be a plain `for`).
    fn parse_var_decls(&mut self, kind: VarKind, for_head: bool) -> R<(List<Declarator>, Option<Span>)> {
        let base = self.lbegin();
        let mut missing = None;
        loop {
            let dstart = self.tok.start;
            let target = self.parse_binding_atom()?;
            if kind != VarKind::Var {
                self.each_name(target, &mut |s, span| {
                    if s.name_of(span) == "let" {
                        return Err(s.early("'let' is not allowed as a lexically bound name", span));
                    }
                    Ok(())
                })?;
            }
            self.declare_pat(target, if kind == VarKind::Var { Bind::Var } else { Bind::Lex })?;
            let init = if self.eat(P::Assign)? { Some(self.parse_assign(for_head, false)?) } else { None };
            if init.is_none() && (kind == VarKind::Const || !matches!(self.pt(target).kind, PatKind::Ident)) {
                let span = self.pt(target).span;
                if for_head {
                    missing.get_or_insert(span);
                } else {
                    return Err(self.syntax("missing initializer in declaration", span));
                }
            }
            let id: Id<Declarator> = mk!(self, decls, Declarator { target, init, span: Span { start: dstart, end: self.prev_end } });
            self.lpush(base, id.raw())?;
            if !self.eat(P::Comma)? {
                break;
            }
        }
        Ok((self.lend(base), missing))
    }

    fn parse_for(&mut self) -> R<StmtId> {
        let start = self.tok.start;
        self.advance()?;
        let is_await = if self.is_word("await") {
            if !self.ctx.is_async {
                return Err(self.unexpected());
            }
            self.advance()?;
            true
        } else {
            false
        };
        self.expect(P::LParen)?;
        self.push_scope(0);
        let r = self.parse_for_rest(start, is_await);
        self.pop_scope();
        r
    }

    fn parse_for_rest(&mut self, start: u32, is_await: bool) -> R<StmtId> {
        // The head.
        if self.is(P::Semi) {
            if is_await {
                return Err(self.unexpected());
            }
            return self.parse_for_tail(start, ForInit::None);
        }
        let is_let = self.is_word("let") && self.let_is_decl(Sc::Top)?;
        let vkind = match self.tok.kind {
            Kind::Keyword(K::Var) => Some(VarKind::Var),
            Kind::Keyword(K::Const) => Some(VarKind::Const),
            Kind::Ident if is_let => Some(VarKind::Let),
            _ => None,
        };
        if let Some(kind) = vkind {
            let vstart = self.tok.start;
            self.advance()?;
            let (decls, missing) = self.parse_var_decls(kind, true)?;
            let var = self.mk_stmt(StmtKind::Var { kind, decls }, vstart)?;
            let of = self.is_word("of");
            if of || self.is_kw(K::In) {
                if decls.len != 1 {
                    return Err(self.syntax("only one declaration is allowed in a for-in or for-of head", Span { start: vstart, end: self.prev_end }));
                }
                let d = *self.p.decl(Id::from_raw_pub(self.p.lists[decls.start as usize]));
                if d.init.is_some() {
                    // Annex B.3.5: `for (var a = 1 in o)` in sloppy code.
                    let simple = matches!(self.pt(d.target).kind, PatKind::Ident);
                    if of || self.strict || kind != VarKind::Var || !simple {
                        return Err(self.syntax("for-in or for-of loop variable declaration may not have an initializer", d.span));
                    }
                }
                return self.parse_for_in_of(start, ForLeft::Var(var), of, is_await);
            }
            if is_await {
                return Err(self.unexpected());
            }
            if let Some(span) = missing {
                return Err(self.syntax("missing initializer in declaration", span));
            }
            return self.parse_for_tail(start, ForInit::Var(var));
        }
        // An expression.
        let estart = self.tok.start;
        let outer = std::mem::take(&mut self.cover);
        self.for_await_head = is_await;
        let first = self.parse_assign(true, true);
        self.for_await_head = false;
        let first = first?;
        let of = self.is_word("of");
        if of || self.is_kw(K::In) {
            let ex = self.ex(first);
            if of && !is_await && matches!(ex.kind, ExprKind::Ident) && self.p.text(ex.span) == "async" {
                return Err(self.syntax("the left-hand side of a for-of loop may not be 'async'", ex.span));
            }
            if is_await && !of {
                return Err(self.unexpected());
            }
            if matches!(ex.kind, ExprKind::Assign { .. }) {
                return Err(self.syntax("invalid left-hand side in a for-in or for-of head", ex.span));
            }
            let pat = self.pattern_of(first, false)?;
            self.finish_cover()?;
            self.cover = outer;
            return self.parse_for_in_of(start, ForLeft::Pat(pat), of, is_await);
        }
        if is_await {
            return Err(self.unexpected());
        }
        self.finish_cover()?;
        self.cover = outer;
        let mut init = first;
        if self.is(P::Comma) {
            let base = self.lbegin();
            self.lpush(base, first.raw())?;
            while self.eat(P::Comma)? {
                let e = self.parse_assign(true, false)?;
                self.lpush(base, e.raw())?;
            }
            let list = self.lend(base);
            init = self.mk_expr(ExprKind::Seq(list), estart)?;
        }
        self.parse_for_tail(start, ForInit::Expr(init))
    }

    fn parse_for_tail(&mut self, start: u32, init: ForInit) -> R<StmtId> {
        self.expect(P::Semi)?;
        let test = if self.is(P::Semi) { None } else { Some(self.parse_expression(false)?) };
        self.expect(P::Semi)?;
        let update = if self.is(P::RParen) { None } else { Some(self.parse_expression(false)?) };
        self.expect(P::RParen)?;
        let body = self.parse_loop_body()?;
        self.mk_stmt(StmtKind::For { init, test, update, body }, start)
    }

    fn parse_for_in_of(&mut self, start: u32, left: ForLeft, of: bool, is_await: bool) -> R<StmtId> {
        self.advance()?; // `of` or `in`
        let right = if of { self.parse_assign(false, false)? } else { self.parse_expression(false)? };
        self.expect(P::RParen)?;
        let body = self.parse_loop_body()?;
        if of {
            self.mk_stmt(StmtKind::ForOf { left, right, body, is_await }, start)
        } else {
            self.mk_stmt(StmtKind::ForIn { left, right, body }, start)
        }
    }

    // ------------------------------------------------------------ binding patterns

    fn binding_ident(&mut self) -> R<PatId> {
        if self.tok.kind != Kind::Ident {
            return Err(self.unexpected());
        }
        let span = self.tspan();
        self.check_binding_name(span)?;
        self.advance()?;
        self.mk_pat(PatKind::Ident, span.start, span.end)
    }

    fn parse_binding_atom(&mut self) -> R<PatId> {
        self.enter()?;
        let r = self.binding_atom_inner();
        self.depth -= 1;
        r
    }

    fn binding_atom_inner(&mut self) -> R<PatId> {
        let start = self.tok.start;
        match self.tok.kind {
            Kind::Ident => self.binding_ident(),
            Kind::Punct(P::LBracket) => {
                self.advance()?;
                let base = self.lbegin();
                while !self.is(P::RBracket) {
                    if self.is(P::Comma) {
                        let s = self.tok.start;
                        let hole = self.mk_pat(PatKind::Hole, s, s)?;
                        self.lpush(base, hole.raw())?;
                        self.advance()?;
                        continue;
                    }
                    if self.is(P::Ellipsis) {
                        let rs = self.tok.start;
                        self.advance()?;
                        let inner = self.parse_binding_atom()?;
                        let rest = self.mk_pat(PatKind::Rest(inner), rs, self.prev_end)?;
                        self.lpush(base, rest.raw())?;
                        if !self.is(P::RBracket) {
                            return Err(self.syntax("rest element must be last", self.tspan()));
                        }
                        break;
                    }
                    let el = self.parse_binding_element()?;
                    self.lpush(base, el.raw())?;
                    if !self.is(P::RBracket) {
                        self.expect(P::Comma)?;
                    }
                }
                self.advance()?;
                let list = self.lend(base);
                self.mk_pat(PatKind::Array(list), start, self.prev_end)
            }
            Kind::Punct(P::LBrace) => {
                self.advance()?;
                let base = self.lbegin();
                while !self.is(P::RBrace) {
                    let pstart = self.tok.start;
                    if self.is(P::Ellipsis) {
                        self.advance()?;
                        let inner = self.binding_ident()?;
                        let rest = self.mk_pat(PatKind::Rest(inner), pstart, self.prev_end)?;
                        let pp: Id<PatProp> = mk!(self, pat_props, PatProp { key: Key::None, value: rest, span: Span { start: pstart, end: self.prev_end } });
                        self.lpush(base, pp.raw())?;
                        if !self.is(P::RBrace) {
                            return Err(self.syntax("rest element must be last", self.tspan()));
                        }
                        break;
                    }
                    let kt = self.tok;
                    let (key, plain) = self.parse_prop_key()?;
                    let value = if self.eat(P::Colon)? {
                        self.parse_binding_element()?
                    } else {
                        if !plain {
                            return Err(self.syntax("unexpected token in shorthand binding", Span { start: kt.start, end: kt.end }));
                        }
                        let span = Span { start: kt.start, end: kt.end };
                        self.check_binding_name(span)?;
                        let id = self.mk_pat(PatKind::Ident, span.start, span.end)?;
                        self.with_default(id)?
                    };
                    let pp = mk!(self, pat_props, PatProp { key, value, span: Span { start: pstart, end: self.prev_end } });
                    self.lpush(base, pp.raw())?;
                    if !self.is(P::RBrace) {
                        self.expect(P::Comma)?;
                    }
                }
                self.advance()?;
                let list = self.lend(base);
                self.mk_pat(PatKind::Object(list), start, self.prev_end)
            }
            _ => Err(self.unexpected()),
        }
    }

    fn parse_binding_element(&mut self) -> R<PatId> {
        let atom = self.parse_binding_atom()?;
        self.with_default(atom)
    }

    fn with_default(&mut self, target: PatId) -> R<PatId> {
        if !self.is(P::Assign) {
            return Ok(target);
        }
        self.advance()?;
        let default = self.parse_assign(false, false)?;
        let start = self.pt(target).span.start;
        self.mk_pat(PatKind::Assign { target, default }, start, self.prev_end)
    }

    // ------------------------------------------------------------ cover grammar: expression -> pattern

    /// Raise the first pending `{a = 1}` or duplicate `__proto__` that no pattern took.
    fn finish_cover(&mut self) -> R<()> {
        if self.cover.shorthand > 0 {
            let at = self.cover.first_shorthand;
            return Err(self.syntax("shorthand property assignment is valid only in a destructuring pattern", Span { start: at, end: at + 1 }));
        }
        if self.cover.proto > 0 {
            let at = self.cover.first_proto;
            return Err(self.early("duplicate __proto__ in an object literal", Span { start: at, end: at + 1 }));
        }
        Ok(())
    }

    fn check_assign_name(&self, span: Span) -> R<()> {
        if self.strict {
            let n = self.name_of(span);
            if n == "eval" || n == "arguments" {
                return Err(self.early("cannot assign to 'eval' or 'arguments' in strict mode", span));
            }
        }
        Ok(())
    }

    /// Is the expression an identifier or a member access (parentheses allowed)? For `x++`, `x += 1`.
    fn check_simple_target(&self, e: ExprId, msg: &'static str) -> R<()> {
        let ex = self.ex(e);
        match ex.kind {
            ExprKind::Paren(inner) => self.check_simple_target(inner, msg),
            ExprKind::Ident => self.check_assign_name(ex.span),
            ExprKind::Member { optional: false, .. } | ExprKind::Index { optional: false, .. } | ExprKind::PrivateMember { optional: false, .. } => Ok(()),
            _ => Err(self.syntax(msg, ex.span)),
        }
    }

    /// Convert an expression to an assignment target (`binding = false`) or a binding (`binding = true`).
    fn pattern_of(&mut self, e: ExprId, binding: bool) -> R<PatId> {
        self.enter()?;
        let r = self.pattern_of_inner(e, binding);
        self.depth -= 1;
        r
    }

    fn pattern_of_inner(&mut self, e: ExprId, binding: bool) -> R<PatId> {
        let ex = self.ex(e);
        let (s, en) = (ex.span.start, ex.span.end);
        match ex.kind {
            ExprKind::Ident => {
                if !binding {
                    self.check_assign_name(ex.span)?;
                }
                self.mk_pat(PatKind::Ident, s, en)
            }
            ExprKind::Member { optional: false, .. } | ExprKind::Index { optional: false, .. } | ExprKind::PrivateMember { optional: false, .. } if !binding => {
                self.mk_pat(PatKind::Member(e), s, en)
            }
            ExprKind::Paren(inner) if !binding => {
                let mut i = inner;
                while let ExprKind::Paren(x) = self.ex(i).kind {
                    i = x;
                }
                match self.ex(i).kind {
                    ExprKind::Ident | ExprKind::Member { optional: false, .. } | ExprKind::Index { optional: false, .. } | ExprKind::PrivateMember { optional: false, .. } => {
                        self.pattern_of(i, false)
                    }
                    _ => Err(self.syntax("invalid parenthesized assignment pattern", ex.span)),
                }
            }
            ExprKind::Array(list) => {
                let base = self.lbegin();
                let n = list.len;
                for i in 0..n {
                    let el = Id::from_raw_pub(self.p.lists[(list.start + i) as usize]);
                    let elx = self.ex(el);
                    let pat = match elx.kind {
                        ExprKind::Hole => self.mk_pat(PatKind::Hole, elx.span.start, elx.span.end)?,
                        ExprKind::Spread(arg) => {
                            if i + 1 != n || self.spread_commas.contains(&s) {
                                return Err(self.syntax("rest element must be last", elx.span));
                            }
                            if matches!(self.ex(arg).kind, ExprKind::Assign { .. }) {
                                return Err(self.syntax("rest element cannot have a default", elx.span));
                            }
                            let inner = self.pattern_of(arg, binding)?;
                            self.mk_pat(PatKind::Rest(inner), elx.span.start, elx.span.end)?
                        }
                        _ => self.pattern_of(el, binding)?,
                    };
                    self.lpush(base, pat.raw())?;
                }
                let l = self.lend(base);
                self.mk_pat(PatKind::Array(l), s, en)
            }
            ExprKind::Object(list) => {
                let base = self.lbegin();
                let n = list.len;
                let (mut shorthand, mut protos) = (0u32, 0u32);
                for i in 0..n {
                    let pr = *self.p.prop(Id::from_raw_pub(self.p.lists[(list.start + i) as usize]));
                    let pp: Id<PatProp> = match pr.kind {
                        PropKind::Init | PropKind::Shorthand => {
                            if pr.kind == PropKind::Shorthand && matches!(self.ex(pr.value).kind, ExprKind::Assign { .. }) {
                                shorthand += 1;
                            }
                            if pr.kind == PropKind::Init && self.is_proto_key(pr.key) {
                                protos += 1;
                            }
                            let value = self.pattern_of(pr.value, binding)?;
                            mk!(self, pat_props, PatProp { key: pr.key, value, span: pr.span })
                        }
                        PropKind::Spread => {
                            if i + 1 != n || self.spread_commas.contains(&s) {
                                return Err(self.syntax("rest element must be last", pr.span));
                            }
                            let arg = self.pattern_of(pr.value, binding)?;
                            if !matches!(self.pt(arg).kind, PatKind::Ident | PatKind::Member(_)) {
                                return Err(self.syntax("invalid rest element in an object pattern", pr.span));
                            }
                            let rest = self.mk_pat(PatKind::Rest(arg), pr.span.start, pr.span.end)?;
                            mk!(self, pat_props, PatProp { key: Key::None, value: rest, span: pr.span })
                        }
                        _ => return Err(self.syntax("an object pattern cannot contain a method, getter or setter", pr.span)),
                    };
                    self.lpush(base, pp.raw())?;
                }
                // This object takes its `{a = 1}` and duplicate `__proto__` out of the pending list.
                self.cover.shorthand = self.cover.shorthand.saturating_sub(shorthand);
                self.cover.proto = self.cover.proto.saturating_sub(protos.saturating_sub(1));
                let l = self.lend(base);
                self.mk_pat(PatKind::Object(l), s, en)
            }
            ExprKind::Assign { op: AssignOp::Assign, target, value } => {
                if binding {
                    self.check_binding_pat(target)?;
                }
                self.mk_pat(PatKind::Assign { target, default: value }, s, en)
            }
            _ => Err(self.syntax(if binding { "invalid destructuring or parameter target" } else { "invalid assignment target" }, ex.span)),
        }
    }

    /// A pattern built for assignment must not have member targets if it becomes a binding.
    fn check_binding_pat(&mut self, pat: PatId) -> R<()> {
        let p = self.pt(pat);
        match p.kind {
            PatKind::Member(_) => Err(self.syntax("invalid binding target", p.span)),
            PatKind::Assign { target, .. } => self.check_binding_pat(target),
            PatKind::Rest(i) => self.check_binding_pat(i),
            PatKind::Array(l) => {
                for i in 0..l.len {
                    self.check_binding_pat(Id::from_raw_pub(self.p.lists[(l.start + i) as usize]))?;
                }
                Ok(())
            }
            PatKind::Object(l) => {
                for i in 0..l.len {
                    let pp = self.p.pat_props[Id::<PatProp>::from_raw_pub(self.p.lists[(l.start + i) as usize]).index()];
                    self.check_binding_pat(pp.value)?;
                }
                Ok(())
            }
            PatKind::Ident | PatKind::Hole => Ok(()),
        }
    }

    fn is_proto_key(&self, key: Key) -> bool {
        match key {
            Key::Name(s) => self.name_of(s) == "__proto__",
            Key::Str(s) => {
                let t = self.p.text(s);
                if !t.as_bytes().contains(&b'\\') {
                    &t[1..t.len() - 1] == "__proto__"
                } else {
                    let mut v = Vec::new();
                    self.p.string(s, &mut v).is_ok() && String::from_utf16_lossy(&v) == "__proto__"
                }
            }
            _ => false,
        }
    }

    // ------------------------------------------------------------ expressions

    fn parse_expression(&mut self, no_in: bool) -> R<ExprId> {
        let start = self.tok.start;
        let first = self.parse_assign(no_in, false)?;
        if !self.is(P::Comma) {
            return Ok(first);
        }
        let base = self.lbegin();
        self.lpush(base, first.raw())?;
        while self.eat(P::Comma)? {
            let e = self.parse_assign(no_in, false)?;
            self.lpush(base, e.raw())?;
        }
        let list = self.lend(base);
        self.mk_expr(ExprKind::Seq(list), start)
    }

    fn is_arrow(&self, e: ExprId) -> bool {
        matches!(self.ex(e).kind, ExprKind::Function(f) if self.p.func(f).kind == FuncKind::Arrow)
    }

    /// AssignmentExpression. `pass`: this is an element of an array or object, or an item that may become
    /// arrow parameters; pending cover errors go to the caller, who owns them.
    fn parse_assign(&mut self, no_in: bool, pass: bool) -> R<ExprId> {
        self.enter()?;
        let outer = if pass { None } else { Some(std::mem::take(&mut self.cover)) };
        let saved_no_in = std::mem::replace(&mut self.no_in, no_in);
        let r = self.assign_inner(no_in, pass);
        self.no_in = saved_no_in;
        self.depth -= 1;
        let e = r?;
        if let Some(o) = outer {
            self.finish_cover()?;
            self.cover = o;
        }
        Ok(e)
    }

    fn assign_inner(&mut self, no_in: bool, _pass: bool) -> R<ExprId> {
        let start = self.tok.start;
        if matches!(self.tok.kind, Kind::Punct(P::LParen) | Kind::Ident) {
            self.arrow_at = start;
        }
        let left = self.parse_cond(no_in)?;
        if self.is_arrow(left) {
            return Ok(left);
        }
        let Kind::Punct(p) = self.tok.kind else { return Ok(left) };
        let Some(op) = assign_op(p) else { return Ok(left) };
        let target = if op == AssignOp::Assign {
            self.pattern_of(left, false)?
        } else {
            self.check_simple_target(left, "invalid assignment target")?;
            self.pattern_of(left, false)?
        };
        self.advance()?;
        let value = self.parse_assign(no_in, false)?;
        self.mk_expr(ExprKind::Assign { op, target, value }, start)
    }

    fn parse_cond(&mut self, no_in: bool) -> R<ExprId> {
        let start = self.tok.start;
        let test = self.parse_binary(0, no_in)?;
        if self.is_arrow(test) || !self.is(P::Question) {
            return Ok(test);
        }
        self.advance()?;
        let cons = self.parse_assign(false, false)?;
        self.expect(P::Colon)?;
        let alt = self.parse_assign(no_in, false)?;
        self.mk_expr(ExprKind::Cond { test, cons, alt }, start)
    }

    fn parse_binary(&mut self, min: u8, no_in: bool) -> R<ExprId> {
        let start = self.tok.start;
        let mut left = if self.tok.kind == Kind::PrivateName {
            // `#x in obj`
            if no_in || min >= PREC_REL || self.privs.is_empty() {
                return Err(self.unexpected());
            }
            let name = self.tspan();
            self.use_private(name)?;
            self.advance()?;
            if !self.is_kw(K::In) {
                return Err(self.unexpected());
            }
            self.advance()?;
            let right = self.parse_binary(PREC_REL, no_in)?;
            self.mk_expr(ExprKind::PrivateIn { name, right }, start)?
        } else {
            self.parse_unary()?
        };
        if self.is_arrow(left) {
            return Ok(left);
        }
        while let Some((op, prec)) = bin_op(self.tok.kind, no_in) {
            if prec <= min {
                break;
            }
            let is_exp = matches!(op, Op::B(BinaryOp::Exp));
            if is_exp && matches!(self.ex(left).kind, ExprKind::Unary { .. } | ExprKind::Await(_)) {
                return Err(self.syntax("unary operator before '**' needs parentheses", self.ex(left).span));
            }
            self.advance()?;
            let right = if is_exp {
                // Right associative: `a ** b ** c` is `a ** (b ** c)`. This recursion is counted.
                self.enter()?;
                let r = self.parse_binary(prec - 1, no_in);
                self.depth -= 1;
                r?
            } else {
                self.parse_binary(prec, no_in)?
            };
            let kind = match op {
                Op::B(op) => ExprKind::Binary { op, left, right },
                Op::L(op) => {
                    for side in [left, right] {
                        if let ExprKind::Logical { op: o, .. } = self.ex(side).kind
                            && (o == LogicalOp::Nullish) != (op == LogicalOp::Nullish)
                        {
                            return Err(self.syntax("'??' cannot be mixed with '&&' or '||' without parentheses", self.ex(side).span));
                        }
                    }
                    ExprKind::Logical { op, left, right }
                }
            };
            left = self.mk_expr(kind, start)?;
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> R<ExprId> {
        self.enter()?;
        let r = self.unary_inner();
        self.depth -= 1;
        r
    }

    fn unary_inner(&mut self) -> R<ExprId> {
        let start = self.tok.start;
        let op = match self.tok.kind {
            Kind::Punct(P::Bang) => Some(UnaryOp::Not),
            Kind::Punct(P::Tilde) => Some(UnaryOp::BitNot),
            Kind::Punct(P::Plus) => Some(UnaryOp::Plus),
            Kind::Punct(P::Minus) => Some(UnaryOp::Neg),
            Kind::Keyword(K::Typeof) => Some(UnaryOp::Typeof),
            Kind::Keyword(K::Void) => Some(UnaryOp::Void),
            Kind::Keyword(K::Delete) => Some(UnaryOp::Delete),
            _ => None,
        };
        if let Some(op) = op {
            self.advance()?;
            let arg = self.parse_unary()?;
            if op == UnaryOp::Delete {
                let mut a = arg;
                while let ExprKind::Paren(i) | ExprKind::Chain(i) = self.ex(a).kind {
                    a = i;
                }
                let ax = self.ex(a);
                if self.strict && matches!(self.ex(arg).kind, ExprKind::Ident) {
                    return Err(self.early("deleting a variable in strict mode", ax.span));
                }
                if self.strict {
                    let mut p = arg;
                    while let ExprKind::Paren(i) = self.ex(p).kind {
                        p = i;
                    }
                    if matches!(self.ex(p).kind, ExprKind::Ident) {
                        return Err(self.early("deleting a variable in strict mode", ax.span));
                    }
                }
                if matches!(ax.kind, ExprKind::PrivateMember { .. }) {
                    return Err(self.early("private fields cannot be deleted", ax.span));
                }
            }
            return self.mk_expr(ExprKind::Unary { op, arg }, start);
        }
        if matches!(self.tok.kind, Kind::Punct(P::PlusPlus | P::MinusMinus)) {
            let op = if self.is(P::PlusPlus) { UpdateOp::Inc } else { UpdateOp::Dec };
            self.advance()?;
            let arg = self.parse_unary()?;
            self.check_simple_target(arg, "invalid operand of a prefix update")?;
            return self.mk_expr(ExprKind::Update { op, prefix: true, arg }, start);
        }
        if self.ctx.is_async && self.is_word("await") {
            if self.await_pos == 0 {
                self.await_pos = start + 1;
            }
            self.advance()?;
            let arg = self.parse_unary()?;
            return self.mk_expr(ExprKind::Await(arg), start);
        }
        let e = self.parse_lhs()?;
        if self.is_arrow(e) {
            return Ok(e);
        }
        if matches!(self.tok.kind, Kind::Punct(P::PlusPlus | P::MinusMinus)) && !self.tok.newline_before {
            let op = if self.is(P::PlusPlus) { UpdateOp::Inc } else { UpdateOp::Dec };
            self.check_simple_target(e, "invalid operand of a postfix update")?;
            self.advance()?;
            return self.mk_expr(ExprKind::Update { op, prefix: false, arg: e }, start);
        }
        Ok(e)
    }

    /// A primary expression and its calls, member accesses and templates.
    fn parse_lhs(&mut self) -> R<ExprId> {
        let start = self.tok.start;
        let base = self.parse_primary(false)?;
        if self.is_arrow(base) {
            return Ok(base);
        }
        self.parse_subscripts(base, start, false)
    }

    fn use_private(&mut self, name: Span) -> R<()> {
        if self.privs.is_empty() {
            return Err(self.early("private name outside of a class", name));
        }
        self.grow()?;
        let n = self.name_of(name);
        if let Some(top) = self.privs.last_mut() {
            top.used.push((n, name));
        }
        Ok(())
    }

    fn parse_args(&mut self) -> R<List<Expr>> {
        self.expect(P::LParen)?;
        let base = self.lbegin();
        while !self.is(P::RParen) {
            let e = self.parse_arg(true)?;
            self.lpush(base, e.raw())?;
            if !self.is(P::RParen) {
                self.expect(P::Comma)?;
            }
        }
        self.advance()?;
        Ok(self.lend(base))
    }

    /// One call argument or `async (` item: `...x` or an assignment expression.
    fn parse_arg(&mut self, pass: bool) -> R<ExprId> {
        if self.is(P::Ellipsis) {
            let s = self.tok.start;
            self.advance()?;
            let arg = self.parse_assign(false, pass)?;
            return self.mk_expr(ExprKind::Spread(arg), s);
        }
        self.parse_assign(false, pass)
    }

    fn parse_subscripts(&mut self, mut base: ExprId, start: u32, no_call: bool) -> R<ExprId> {
        let mut chain = false;
        loop {
            match self.tok.kind {
                Kind::Punct(P::Dot) => {
                    self.advance()?;
                    base = self.member_name(base, start, false)?;
                }
                Kind::Punct(P::QDot) => {
                    if no_call {
                        return Err(self.syntax("optional chain in a new expression", self.tspan()));
                    }
                    if matches!(self.ex(base).kind, ExprKind::Super) {
                        return Err(self.syntax("'super' cannot start an optional chain", self.tspan()));
                    }
                    chain = true;
                    self.advance()?;
                    match self.tok.kind {
                        Kind::Punct(P::LParen) => {
                            let args = self.parse_args()?;
                            base = self.mk_expr(ExprKind::Call { callee: base, args, optional: true }, start)?;
                        }
                        Kind::Punct(P::LBracket) => {
                            self.advance()?;
                            let index = self.parse_expression(false)?;
                            self.expect(P::RBracket)?;
                            base = self.mk_expr(ExprKind::Index { obj: base, index, optional: true }, start)?;
                        }
                        Kind::Template { .. } => {
                            return Err(self.syntax("optional chain in the tag of a template", self.tspan()));
                        }
                        _ => base = self.member_name(base, start, true)?,
                    }
                }
                Kind::Punct(P::LBracket) => {
                    self.advance()?;
                    let index = self.parse_expression(false)?;
                    self.expect(P::RBracket)?;
                    base = self.mk_expr(ExprKind::Index { obj: base, index, optional: false }, start)?;
                }
                Kind::Punct(P::LParen) if !no_call => {
                    let args = self.parse_args()?;
                    base = self.mk_expr(ExprKind::Call { callee: base, args, optional: false }, start)?;
                }
                Kind::Template { .. } => {
                    if chain {
                        return Err(self.syntax("optional chain in the tag of a template", self.tspan()));
                    }
                    let quasi = self.parse_template(true)?;
                    base = self.mk_expr(ExprKind::Tagged { tag: base, quasi }, start)?;
                }
                _ => break,
            }
        }
        if chain {
            base = self.mk_expr(ExprKind::Chain(base), start)?;
        }
        Ok(base)
    }

    /// After `.` or `?.`: a name or a private name.
    fn member_name(&mut self, obj: ExprId, start: u32, optional: bool) -> R<ExprId> {
        let name = self.tspan();
        match self.tok.kind {
            Kind::Ident | Kind::Keyword(_) => {
                self.advance()?;
                self.mk_expr(ExprKind::Member { obj, name, optional }, start)
            }
            Kind::PrivateName => {
                if matches!(self.ex(obj).kind, ExprKind::Super) {
                    return Err(self.syntax("unexpected private field after 'super'", name));
                }
                self.use_private(name)?;
                self.advance()?;
                self.mk_expr(ExprKind::PrivateMember { obj, name, optional }, start)
            }
            _ => Err(self.unexpected()),
        }
    }

    fn parse_new(&mut self) -> R<ExprId> {
        let start = self.tok.start;
        self.advance()?; // `new`
        if self.is(P::Dot) {
            self.advance()?;
            if !self.is_word("target") {
                return Err(self.unexpected());
            }
            if !self.ctx.new_target {
                return Err(self.early("'new.target' outside of a function", Span { start, end: self.tok.end }));
            }
            self.advance()?;
            return self.mk_expr(ExprKind::NewTarget, start);
        }
        let cstart = self.tok.start;
        let base = if self.is_kw(K::New) {
            self.enter()?;
            let r = self.parse_new();
            self.depth -= 1;
            r?
        } else {
            self.parse_primary(true)?
        };
        if self.is_arrow(base) {
            return Err(self.unexpected());
        }
        let callee = self.parse_subscripts(base, cstart, true)?;
        let args = if self.is(P::LParen) { self.parse_args()? } else { List::new(0, 0) };
        self.mk_expr(ExprKind::New { callee, args }, start)
    }

    fn check_number(&mut self, base: lex::NumberBase) -> R<()> {
        if self.strict && matches!(base, lex::NumberBase::LegacyOctal | lex::NumberBase::NonOctalDecimal) {
            return Err(self.early("legacy octal or leading-zero decimal literal in strict mode", self.tspan()));
        }
        Ok(())
    }

    fn parse_primary(&mut self, in_new: bool) -> R<ExprId> {
        let t = self.tok;
        let start = t.start;
        match t.kind {
            Kind::Ident => self.parse_ident_primary(in_new),
            Kind::Keyword(k) => match k {
                K::This => {
                    self.advance()?;
                    self.mk_expr(ExprKind::This, start)
                }
                K::Null => {
                    self.advance()?;
                    self.mk_expr(ExprKind::Null, start)
                }
                K::True | K::False => {
                    self.advance()?;
                    self.mk_expr(ExprKind::Bool(k == K::True), start)
                }
                K::Function => {
                    self.advance()?;
                    self.parse_function_expr(start, false)
                }
                K::Class => {
                    let id = self.parse_class(start, false)?;
                    self.mk_expr(ExprKind::Class(id), start)
                }
                K::New => self.parse_new(),
                K::Super => self.parse_super(in_new),
                K::Import => Err(self.unsupported("import() and import.meta (modules) are not supported", t.start, t.end)),
                _ => Err(self.unexpected()),
            },
            Kind::Number(base) => {
                self.check_number(base)?;
                self.advance()?;
                self.mk_expr(ExprKind::Number(base), start)
            }
            Kind::BigInt(_) => {
                self.advance()?;
                self.mk_expr(ExprKind::BigInt, start)
            }
            Kind::String { legacy_octal } => {
                if legacy_octal {
                    if self.strict {
                        return Err(self.early("octal escape sequence in strict mode", self.tspan()));
                    }
                    self.last_octal = Some(start);
                }
                self.advance()?;
                self.mk_expr(ExprKind::String, start)
            }
            Kind::Template { .. } => {
                let q = self.parse_template(false)?;
                self.mk_expr(ExprKind::Template(q), start)
            }
            Kind::Punct(P::Slash | P::SlashAssign) => {
                self.rescan(Goal::RegExp)?;
                self.advance()?;
                self.mk_expr(ExprKind::RegExp, start)
            }
            Kind::Punct(P::LParen) => self.parse_paren(),
            Kind::Punct(P::LBracket) => self.parse_array(),
            Kind::Punct(P::LBrace) => self.parse_object(),
            _ => Err(self.unexpected()),
        }
    }

    fn parse_super(&mut self, in_new: bool) -> R<ExprId> {
        let t = self.tok;
        self.advance()?;
        let ok = match self.tok.kind {
            Kind::Punct(P::LParen) => self.ctx.super_call && !in_new,
            Kind::Punct(P::Dot | P::LBracket) => self.ctx.super_prop,
            _ => false,
        };
        if !ok {
            return Err(self.syntax("'super' is not allowed here", Span { start: t.start, end: t.end }));
        }
        self.mk_expr(ExprKind::Super, t.start)
    }

    fn parse_ident_primary(&mut self, in_new: bool) -> R<ExprId> {
        let t = self.tok;
        let start = t.start;
        let can_arrow = self.arrow_at == start && !in_new;
        if self.is_word("async") {
            let n = self.peek()?;
            if n.kind == Kind::Keyword(K::Function) && !n.newline_before {
                self.advance()?; // async
                self.advance()?; // function
                return self.parse_function_expr(start, true);
            }
            if can_arrow && !n.newline_before {
                if n.kind == Kind::Ident && !(self.for_await_head && self.p.text(Span { start: n.start, end: n.end }) == "of") {
                    // `async x => ...`
                    self.advance()?;
                    let param = self.tspan();
                    self.advance()?;
                    if !self.is(P::Arrow) || self.tok.newline_before {
                        return Err(self.unexpected());
                    }
                    self.check_binding_name(param)?; // checked again with the arrow's own rules in declare
                    let pat = self.mk_pat(PatKind::Ident, param.start, param.end)?;
                    let base = self.lbegin();
                    self.lpush(base, pat.raw())?;
                    let params = self.lend(base);
                    return self.parse_arrow(start, params, true);
                }
                if n.kind == Kind::Punct(P::LParen) {
                    self.advance()?; // async
                    return self.parse_async_call_or_arrow(start, t);
                }
            }
        }
        // A plain name.
        let span = self.tspan();
        self.check_word(span)?;
        if self.ctx.field_init && self.text() == "arguments" {
            return Err(self.early("'arguments' is not allowed in a class field initializer", span));
        }
        self.advance()?;
        if can_arrow && self.is(P::Arrow) && !self.tok.newline_before {
            let pat = self.mk_pat(PatKind::Ident, span.start, span.end)?;
            let base = self.lbegin();
            self.lpush(base, pat.raw())?;
            let params = self.lend(base);
            return self.parse_arrow(start, params, false);
        }
        self.mk_expr(ExprKind::Ident, start)
    }

    /// `async (...)`: a call of a function named `async`, or the parameters of an async arrow.
    fn parse_async_call_or_arrow(&mut self, start: u32, async_tok: Token) -> R<ExprId> {
        let old_await = std::mem::take(&mut self.await_pos);
        self.expect(P::LParen)?;
        let base = self.lbegin();
        let mut trailing_comma = false;
        while !self.is(P::RParen) {
            let e = self.parse_arg(true)?;
            self.lpush(base, e.raw())?;
            trailing_comma = false;
            if !self.is(P::RParen) {
                self.expect(P::Comma)?;
                trailing_comma = true;
            }
        }
        self.advance()?; // `)`
        let arrow = self.is(P::Arrow) && !self.tok.newline_before;
        if arrow && self.await_pos != 0 {
            return Err(self.early("an arrow function parameter contains an await expression", Span { start, end: self.prev_end }));
        }
        if self.await_pos == 0 || arrow {
            self.await_pos = old_await;
        }
        if arrow {
            // ECMA-262 15.3: no comma after a final rest parameter.
            if trailing_comma && base < self.scratch.len() {
                let last = Id::<Expr>::from_raw_pub(self.scratch[self.scratch.len() - 1]);
                if matches!(self.ex(last).kind, ExprKind::Spread(_)) {
                    return Err(self.syntax("rest parameter must be last", self.ex(last).span));
                }
            }
            let params = self.items_to_params(base)?;
            return self.parse_arrow(start, params, true);
        }
        let args = self.lend(base);
        let callee = self.mk_expr_at(ExprKind::Ident, async_tok.start, async_tok.end)?;
        self.mk_expr(ExprKind::Call { callee, args, optional: false }, start)
    }

    /// Convert the expressions pushed since `base` on the scratch stack into parameter patterns.
    fn items_to_params(&mut self, base: usize) -> R<List<Pat>> {
        // The items are on the scratch stack above `base`. New patterns are pushed above them.
        let n = self.scratch.len() - base;
        let out = self.scratch.len();
        for i in 0..n {
            let id = Id::<Expr>::from_raw_pub(self.scratch[base + i]);
            let ex = self.ex(id);
            let pat = match ex.kind {
                ExprKind::Spread(arg) => {
                    if i + 1 != n {
                        return Err(self.syntax("rest parameter must be last", ex.span));
                    }
                    if matches!(self.ex(arg).kind, ExprKind::Assign { .. }) {
                        return Err(self.syntax("rest parameter cannot have a default", ex.span));
                    }
                    let inner = self.pattern_of(arg, true)?;
                    self.mk_pat(PatKind::Rest(inner), ex.span.start, ex.span.end)?
                }
                _ => self.pattern_of(id, true)?,
            };
            self.lpush(out, pat.raw())?;
        }
        // Drop the expression items; keep the patterns.
        let list: List<Pat> = {
            let start = self.p.lists.len() as u32;
            self.p.lists.extend_from_slice(&self.scratch[out..]);
            List::new(start, (self.scratch.len() - out) as u32)
        };
        self.scratch.truncate(base);
        Ok(list)
    }

    fn parse_paren(&mut self) -> R<ExprId> {
        let start = self.tok.start;
        let can_arrow = self.arrow_at == start;
        let old_await = std::mem::take(&mut self.await_pos);
        self.advance()?; // `(`
        let base = self.lbegin();
        let mut rest: Option<PatId> = None;
        let mut trailing_comma = false;
        let mut first_comma_end = 0;
        while !self.is(P::RParen) {
            if self.is(P::Ellipsis) {
                let rs = self.tok.start;
                self.advance()?;
                let inner = self.parse_binding_atom()?;
                rest = Some(self.mk_pat(PatKind::Rest(inner), rs, self.prev_end)?);
                if !self.is(P::RParen) {
                    return Err(self.syntax("rest parameter must be last", self.tspan()));
                }
                break;
            }
            let e = self.parse_assign(false, true)?;
            self.lpush(base, e.raw())?;
            trailing_comma = false;
            if !self.is(P::RParen) {
                self.expect(P::Comma)?;
                trailing_comma = true;
                if first_comma_end == 0 {
                    first_comma_end = self.prev_end;
                }
            }
        }
        let close = self.tok;
        self.advance()?; // `)`
        let is_arrow = self.is(P::Arrow) && !self.tok.newline_before;
        if is_arrow {
            if !can_arrow {
                return Err(self.unexpected());
            }
            // ECMA-262 15.3.1: arrow parameters contain no AwaitExpression.
            if self.await_pos != 0 {
                return Err(self.early("an arrow function parameter contains an await expression", Span { start, end: close.end }));
            }
            self.await_pos = old_await;
            let params = match rest {
                None => self.items_to_params(base)?,
                Some(r) => {
                    // Items first, then the rest pattern.
                    let n = self.scratch.len() - base;
                    let l = self.items_to_params(base)?;
                    let _ = n;
                    let b2 = self.lbegin();
                    for i in 0..l.len {
                        let raw = self.p.lists[(l.start + i) as usize];
                        self.lpush(b2, raw)?;
                    }
                    self.lpush(b2, r.raw())?;
                    self.lend(b2)
                }
            };
            return self.parse_arrow(start, params, false);
        }
        if self.await_pos == 0 {
            self.await_pos = old_await;
        }
        if rest.is_some() || trailing_comma {
            return Err(self.syntax("unexpected token", Span { start: close.start, end: close.end }));
        }
        let n = self.scratch.len() - base;
        if n == 0 {
            return Err(self.syntax("unexpected token", Span { start: close.start, end: close.end }));
        }
        let inner = if n == 1 {
            let id = Id::from_raw_pub(self.scratch[base]);
            self.scratch.truncate(base);
            id
        } else {
            let first = Id::<Expr>::from_raw_pub(self.scratch[base]);
            let last = Id::<Expr>::from_raw_pub(self.scratch[self.scratch.len() - 1]);
            let (s, e) = (self.ex(first).span.start, self.ex(last).span.end);
            let list = self.lend(base);
            self.mk_expr_at(ExprKind::Seq(list), s, e)?
        };
        self.mk_expr(ExprKind::Paren(inner), start)
    }

    fn parse_array(&mut self) -> R<ExprId> {
        let start = self.tok.start;
        self.advance()?;
        let base = self.lbegin();
        while !self.is(P::RBracket) {
            if self.is(P::Comma) {
                let s = self.tok.start;
                let hole = self.mk_expr_at(ExprKind::Hole, s, s)?;
                self.lpush(base, hole.raw())?;
                self.advance()?;
                continue;
            }
            let spread = self.is(P::Ellipsis);
            let e = self.parse_arg(true)?;
            self.lpush(base, e.raw())?;
            if !self.is(P::RBracket) {
                self.expect(P::Comma)?;
                if spread && self.is(P::RBracket) {
                    self.grow()?;
                    self.spread_commas.push(start);
                }
            }
        }
        self.advance()?;
        let list = self.lend(base);
        self.mk_expr(ExprKind::Array(list), start)
    }

    fn parse_object(&mut self) -> R<ExprId> {
        let start = self.tok.start;
        self.advance()?;
        let base = self.lbegin();
        let mut protos = 0u32;
        while !self.is(P::RBrace) {
            let pstart = self.tok.start;
            let prop = if self.is(P::Ellipsis) {
                self.advance()?;
                let value = self.parse_assign(false, true)?;
                let span = Span { start: pstart, end: self.prev_end };
                mk!(self, props, Prop { kind: PropKind::Spread, key: Key::None, value, span })
            } else {
                self.parse_property(pstart, &mut protos)?
            };
            self.lpush(base, prop.raw())?;
            if !self.is(P::RBrace) {
                let spread = matches!(self.p.prop(prop).kind, PropKind::Spread);
                self.expect(P::Comma)?;
                if spread && self.is(P::RBrace) {
                    self.grow()?;
                    self.spread_commas.push(start);
                }
            }
        }
        self.advance()?;
        if protos > 1 {
            if self.cover.proto == 0 {
                self.cover.first_proto = start;
            }
            self.cover.proto += protos - 1;
        }
        let list = self.lend(base);
        self.mk_expr(ExprKind::Object(list), start)
    }

    /// Tokens that, after `async`, `get` or `set` in an object or class, mean the word is the key itself.
    fn ends_key_word(k: Kind) -> bool {
        matches!(k, Kind::Punct(P::Comma | P::Colon | P::LParen | P::RBrace | P::Assign | P::Semi) | Kind::Eof)
    }

    fn parse_property(&mut self, pstart: u32, protos: &mut u32) -> R<Id<Prop>> {
        let mut is_async = false;
        let mut accessor = None;
        if self.is_word("async") {
            let n = self.peek()?;
            if !n.newline_before && !Self::ends_key_word(n.kind) {
                is_async = true;
                self.advance()?;
            }
        }
        if self.is(P::Star) {
            return Err(self.unsupported("generators are not supported", self.tok.start, self.tok.end));
        }
        if !is_async && (self.is_word("get") || self.is_word("set")) {
            let n = self.peek()?;
            if !Self::ends_key_word(n.kind) {
                accessor = Some(if self.text() == "get" { PropKind::Get } else { PropKind::Set });
                self.advance()?;
            }
        }
        let kt = self.tok;
        let (key, plain) = self.parse_prop_key()?;
        if is_async || accessor.is_some() || self.is(P::LParen) {
            let (kind, fkind) = match accessor {
                Some(PropKind::Get) => (PropKind::Get, FuncKind::Getter),
                Some(_) => (PropKind::Set, FuncKind::Setter),
                None => (PropKind::Method, FuncKind::Method),
            };
            let fstart = self.tok.start;
            let f = self.parse_function_rest(fstart, None, fkind, is_async)?;
            let fend = self.prev_end;
            let value = self.mk_expr_at(ExprKind::Function(f), fstart, fend)?;
            let span = Span { start: pstart, end: self.prev_end };
            return Ok(mk!(self, props, Prop { kind, key, value, span }));
        }
        if self.eat(P::Colon)? {
            let value = self.parse_assign(false, true)?;
            if self.is_proto_key(key) {
                *protos += 1;
            }
            let span = Span { start: pstart, end: self.prev_end };
            return Ok(mk!(self, props, Prop { kind: PropKind::Init, key, value, span }));
        }
        // Shorthand `{ a }` or `{ a = 1 }` (the latter only becomes valid in a pattern).
        if !plain {
            return Err(self.unexpected());
        }
        let span = Span { start: kt.start, end: kt.end };
        self.check_word(span)?;
        if self.ctx.field_init && self.p.text(span) == "arguments" {
            return Err(self.early("'arguments' is not allowed in a class field initializer", span));
        }
        let ident = self.mk_expr_at(ExprKind::Ident, span.start, span.end)?;
        let value = if self.is(P::Assign) {
            self.check_assign_name(span)?;
            let target = self.mk_pat(PatKind::Ident, span.start, span.end)?;
            self.advance()?;
            let default = self.parse_assign(false, false)?;
            if self.cover.shorthand == 0 {
                self.cover.first_shorthand = kt.start;
            }
            self.cover.shorthand += 1;
            self.mk_expr(ExprKind::Assign { op: AssignOp::Assign, target, value: default }, kt.start)?
        } else {
            ident
        };
        let span = Span { start: pstart, end: self.prev_end };
        Ok(mk!(self, props, Prop { kind: PropKind::Shorthand, key, value, span }))
    }

    /// A property key. The bool is true for an identifier token that is not a keyword (a shorthand candidate).
    fn parse_prop_key(&mut self) -> R<(Key, bool)> {
        let t = self.tok;
        let span = Span { start: t.start, end: t.end };
        match t.kind {
            Kind::Ident => {
                self.advance()?;
                Ok((Key::Name(span), true))
            }
            Kind::Keyword(_) => {
                self.advance()?;
                Ok((Key::Name(span), false))
            }
            Kind::String { legacy_octal } => {
                if legacy_octal {
                    if self.strict {
                        return Err(self.early("octal escape sequence in strict mode", span));
                    }
                    self.last_octal = Some(t.start);
                }
                self.advance()?;
                Ok((Key::Str(span), false))
            }
            Kind::Number(base) => {
                self.check_number(base)?;
                self.advance()?;
                Ok((Key::Num(span), false))
            }
            Kind::BigInt(_) => {
                self.advance()?;
                Ok((Key::BigInt(span), false))
            }
            Kind::Punct(P::LBracket) => {
                self.advance()?;
                let e = self.parse_assign(false, false)?;
                self.expect(P::RBracket)?;
                Ok((Key::Computed(e), false))
            }
            _ => Err(self.unexpected()),
        }
    }

    fn parse_template(&mut self, tagged: bool) -> R<Id<Template>> {
        let base = self.lbegin();
        let mut n_exprs = 0u32;
        loop {
            let Kind::Template { part, bad_escape } = self.tok.kind else { return Err(self.unexpected()) };
            if bad_escape && !tagged {
                return Err(self.syntax("bad escape sequence in an untagged template", self.tspan()));
            }
            let span = self.tspan();
            let q: Id<Quasi> = mk!(self, quasis, Quasi { span, part, bad_escape });
            self.lpush(base, q.raw())?;
            self.advance()?;
            if matches!(part, TemplatePart::NoSubstitution | TemplatePart::Tail) {
                break;
            }
            let e = self.parse_expression(false)?;
            self.lpush(base, e.raw())?;
            n_exprs += 1;
            if !self.is(P::RBrace) {
                return Err(self.unexpected());
            }
            self.rescan(Goal::TemplateTail)?;
        }
        let list: List<u32> = self.lend(base);
        Ok(mk!(self, templates, Template { start: list.start, n_exprs }))
    }

    // ------------------------------------------------------------ functions

    fn parse_function_decl(&mut self, start: u32, is_async: bool, hanging: bool) -> R<StmtId> {
        self.advance()?; // `function`
        if self.is(P::Star) {
            return Err(self.unsupported("generators are not supported", self.tok.start, self.tok.end));
        }
        if self.tok.kind != Kind::Ident {
            return Err(self.unexpected());
        }
        let name = self.tspan();
        self.check_binding_name(name)?;
        if !hanging {
            let bind = if self.strict || is_async {
                if self.funcs_as_var(self.nscopes - 1) { Bind::Var } else { Bind::Lex }
            } else {
                Bind::Func
            };
            let n = self.name_of(name);
            self.declare(n, bind, name)?;
        }
        self.advance()?;
        let fstart = start;
        let f = self.parse_function_rest(fstart, Some(name), FuncKind::Normal, is_async)?;
        self.mk_stmt(StmtKind::Function(f), start)
    }

    fn parse_function_expr(&mut self, start: u32, is_async: bool) -> R<ExprId> {
        if self.is(P::Star) {
            return Err(self.unsupported("generators are not supported", self.tok.start, self.tok.end));
        }
        let mut name = None;
        if self.tok.kind == Kind::Ident {
            let s = self.tspan();
            let saved = self.ctx.is_async;
            self.ctx.is_async = is_async;
            let r = self.check_binding_name(s);
            self.ctx.is_async = saved;
            r?;
            name = Some(s);
            self.advance()?;
        }
        let f = self.parse_function_rest(start, name, FuncKind::Normal, is_async)?;
        self.mk_expr(ExprKind::Function(f), start)
    }

    fn is_simple_params(&self, params: List<Pat>) -> bool {
        (0..params.len).all(|i| {
            matches!(self.pt(Id::from_raw_pub(self.p.lists[(params.start + i) as usize])).kind, PatKind::Ident)
        })
    }

    /// Parameters and body. The current token is `(`.
    fn parse_function_rest(&mut self, start: u32, name: Option<Span>, kind: FuncKind, is_async: bool) -> R<FuncId> {
        let (saved_ctx, saved_strict, saved_lb, saved_arrow) = (self.ctx, self.strict, self.label_base, self.arrow_at);
        self.ctx = Ctx {
            in_func: true,
            is_async,
            new_target: true,
            super_prop: kind != FuncKind::Normal,
            super_call: kind == FuncKind::DerivedConstructor,
            field_init: false,
        };
        self.label_base = self.labels.len();
        self.push_scope(S_VAR | S_FUNC);
        let old_await = std::mem::take(&mut self.await_pos);
        let r = self.function_inner(start, name, kind, is_async);
        self.await_pos = old_await;
        self.pop_scope();
        self.labels.truncate(self.label_base);
        let strict = self.strict;
        (self.ctx, self.strict, self.label_base, self.arrow_at) = (saved_ctx, saved_strict, saved_lb, saved_arrow);
        let (params, body, simple) = r?;
        let span = Span { start, end: self.prev_end };
        Ok(mk!(self, funcs, Func { name, params, body, kind, is_async, strict, simple_params: simple, span }))
    }

    fn function_inner(&mut self, _start: u32, name: Option<Span>, kind: FuncKind, is_async: bool) -> R<(List<Pat>, Body, bool)> {
        let _ = is_async;
        let pstart = self.tok.start;
        self.expect(P::LParen)?;
        let base = self.lbegin();
        while !self.is(P::RParen) {
            if self.is(P::Ellipsis) {
                let rs = self.tok.start;
                self.advance()?;
                let inner = self.parse_binding_atom()?;
                let rest = self.mk_pat(PatKind::Rest(inner), rs, self.prev_end)?;
                self.lpush(base, rest.raw())?;
                if !self.is(P::RParen) {
                    return Err(self.syntax("rest parameter must be last", self.tspan()));
                }
                break;
            }
            let el = self.parse_binding_element()?;
            self.lpush(base, el.raw())?;
            if !self.is(P::RParen) {
                self.expect(P::Comma)?;
            }
        }
        let pend = self.tok;
        self.advance()?; // `)`
        // ECMA-262 15.8.1: async function parameters contain no AwaitExpression.
        if self.await_pos != 0 {
            return Err(self.early("a parameter contains an await expression", Span { start: pstart, end: pend.end }));
        }
        let params: List<Pat> = self.lend(base);
        let simple = self.is_simple_params(params);
        match kind {
            FuncKind::Getter if params.len != 0 => {
                return Err(self.syntax("a getter must not have parameters", Span { start: pstart, end: pend.end }));
            }
            FuncKind::Setter => {
                if params.len != 1 {
                    return Err(self.syntax("a setter must have exactly one parameter", Span { start: pstart, end: pend.end }));
                }
                let id = Id::from_raw_pub(self.p.lists[params.start as usize]);
                if matches!(self.pt(id).kind, PatKind::Rest(_)) {
                    return Err(self.syntax("a setter cannot have a rest parameter", Span { start: pstart, end: pend.end }));
                }
            }
            _ => {}
        }
        let allow_dup = !self.strict && simple && kind == FuncKind::Normal;
        let first_dup = self.declare_params(params, allow_dup)?;
        let info = BodyInfo { params, name, simple, first_dup };
        let body = self.parse_function_body(info)?;
        Ok((params, Body::Block(body), simple))
    }

    /// `{ ... }` of a function, with the directive prologue. May turn on strict mode (the caller restores it).
    fn parse_function_body(&mut self, info: BodyInfo) -> R<List<Stmt>> {
        if !self.is(P::LBrace) {
            return Err(self.unexpected());
        }
        self.advance()?;
        let base = self.lbegin();
        let mut prologue = true;
        let mut octal = None;
        while !self.is(P::RBrace) {
            if self.tok.kind == Kind::Eof {
                return Err(self.unexpected());
            }
            let start = self.tok.start;
            let st = self.parse_item(prologue)?;
            if prologue {
                match self.p.stmts[st.index()].kind {
                    StmtKind::Directive(e) => self.on_directive(e, start, &mut octal, Some(info))?,
                    _ => prologue = false,
                }
            }
            self.lpush(base, st.raw())?;
        }
        self.advance()?;
        Ok(self.lend(base))
    }

    /// An arrow function. The current token is `=>`. `params` were converted from the cover grammar.
    fn parse_arrow(&mut self, start: u32, params: List<Pat>, is_async: bool) -> R<ExprId> {
        let no_in = self.no_in;
        let old_await = std::mem::take(&mut self.await_pos);
        self.advance()?; // `=>`
        let (saved_ctx, saved_strict, saved_lb, saved_arrow) = (self.ctx, self.strict, self.label_base, self.arrow_at);
        // An arrow keeps `this`, `super`, `new.target` and the field-initializer rule of the code around it.
        self.ctx.in_func = true;
        self.ctx.is_async = is_async;
        self.label_base = self.labels.len();
        self.push_scope(S_VAR | S_FUNC);
        let simple = self.is_simple_params(params);
        let r = (|| -> R<Body> {
            let first_dup = self.declare_params(params, false)?;
            if self.is(P::LBrace) {
                let info = BodyInfo { params, name: None, simple, first_dup };
                Ok(Body::Block(self.parse_function_body(info)?))
            } else {
                Ok(Body::Expr(self.parse_assign(no_in, false)?))
            }
        })();
        self.pop_scope();
        self.labels.truncate(self.label_base);
        self.await_pos = old_await;
        let strict = self.strict;
        (self.ctx, self.strict, self.label_base, self.arrow_at) = (saved_ctx, saved_strict, saved_lb, saved_arrow);
        let body = r?;
        let span = Span { start, end: self.prev_end };
        let f = mk!(self, funcs, Func { name: None, params, body, kind: FuncKind::Arrow, is_async, strict, simple_params: simple, span });
        self.mk_expr(ExprKind::Function(f), start)
    }

    // ------------------------------------------------------------ classes

    fn parse_class(&mut self, start: u32, is_decl: bool) -> R<ClassId> {
        self.advance()?; // `class`
        let saved_strict = self.strict;
        self.strict = true; // all parts of a class are strict code
        let r = self.class_inner(start, is_decl);
        self.strict = saved_strict;
        r
    }

    fn class_inner(&mut self, start: u32, is_decl: bool) -> R<ClassId> {
        let mut name = None;
        if self.tok.kind == Kind::Ident {
            let s = self.tspan();
            self.check_binding_name(s)?;
            if is_decl {
                let n = self.name_of(s);
                self.declare(n, Bind::Lex, s)?;
            }
            name = Some(s);
            self.advance()?;
        } else if is_decl {
            return Err(self.unexpected());
        }
        let mut superclass = None;
        if self.is_kw(K::Extends) {
            self.advance()?;
            let s = self.tok.start;
            let base = self.parse_primary(false)?;
            superclass = Some(if self.is_arrow(base) { base } else { self.parse_subscripts(base, s, false)? });
        }
        self.expect(P::LBrace)?;
        self.grow()?;
        self.privs.push(PrivScope::default());
        let base = self.lbegin();
        let mut ctor_seen = false;
        while !self.is(P::RBrace) {
            if self.tok.kind == Kind::Eof {
                return Err(self.unexpected());
            }
            if self.eat(P::Semi)? {
                continue;
            }
            let el = self.parse_class_elem(superclass.is_some(), &mut ctor_seen)?;
            self.lpush(base, el.raw())?;
        }
        self.advance()?; // `}`
        let elems = self.lend(base);
        // Names that no member of this class declares go to the class around it.
        let scope = self.privs.pop().unwrap_or_default();
        for (n, span) in scope.used {
            if scope.declared.contains_key(&n) {
                continue;
            }
            match self.privs.last_mut() {
                Some(outer) => outer.used.push((n, span)),
                None => return Err(self.early("private name is not declared in an enclosing class", span)),
            }
        }
        Ok(mk!(self, classes, Class { name, superclass, elems, span: Span { start, end: self.prev_end } }))
    }

    fn declare_private(&mut self, name: Span, bits: u8) -> R<()> {
        let n = self.name_of(name);
        if &*n == "constructor" {
            return Err(self.early("a private name cannot be '#constructor'", name));
        }
        self.grow()?;
        let Some(top) = self.privs.last_mut() else { return Ok(()) };
        match top.declared.get_mut(&n) {
            None => {
                top.declared.insert(n, bits);
            }
            Some(prev) => {
                let pair = (bits & PV_GET != 0 && *prev & !PV_STATIC == PV_SET || bits & PV_SET != 0 && *prev & !PV_STATIC == PV_GET)
                    && (bits & PV_STATIC) == (*prev & PV_STATIC);
                if !pair {
                    return Err(self.early("private name has already been declared", name));
                }
                *prev |= bits;
            }
        }
        Ok(())
    }

    /// The text of a non-computed key that is a name or a string, if it is one.
    fn key_text(&self, key: Key) -> Option<Cow<'a, str>> {
        match key {
            Key::Name(s) => Some(self.name_of(s)),
            Key::Str(s) => {
                let t = self.p.text(s);
                if !t.as_bytes().contains(&b'\\') {
                    Some(Cow::Borrowed(&t[1..t.len() - 1]))
                } else {
                    let mut v = Vec::new();
                    self.p.string(s, &mut v).ok()?;
                    Some(Cow::Owned(String::from_utf16_lossy(&v)))
                }
            }
            _ => None,
        }
    }

    fn parse_class_elem(&mut self, derived: bool, ctor_seen: &mut bool) -> R<Id<ClassElem>> {
        let start = self.tok.start;
        let mut is_static = false;
        if self.is_word("static") {
            let n = self.peek()?;
            if n.kind == Kind::Punct(P::LBrace) {
                return Err(self.unsupported("class static blocks are not supported", self.tok.start, n.end));
            }
            if !Self::ends_key_word(n.kind) {
                is_static = true;
                self.advance()?;
            }
        }
        let mut is_async = false;
        if self.is_word("async") {
            let n = self.peek()?;
            if !n.newline_before && !Self::ends_key_word(n.kind) {
                is_async = true;
                self.advance()?;
            }
        }
        if self.is(P::Star) {
            return Err(self.unsupported("generators are not supported", self.tok.start, self.tok.end));
        }
        let mut accessor = None;
        if !is_async && (self.is_word("get") || self.is_word("set")) {
            let n = self.peek()?;
            // `get` + newline + `*g(){}` is a field `get` and a generator (ASI).
            if !(Self::ends_key_word(n.kind) || n.kind == Kind::Punct(P::Star) && n.newline_before) {
                accessor = Some(if self.text() == "get" { PV_GET } else { PV_SET });
                self.advance()?;
            }
        }
        let kt = self.tspan();
        let key = if self.tok.kind == Kind::PrivateName {
            self.advance()?;
            Key::Private(kt)
        } else {
            self.parse_prop_key()?.0
        };
        let text = self.key_text(key);
        let is_ctor_name = text.as_deref() == Some("constructor");
        if is_static && text.as_deref() == Some("prototype") {
            return Err(self.early("a class cannot have a static member named 'prototype'", kt));
        }
        let static_bit = if is_static { PV_STATIC } else { 0 };
        if self.is(P::LParen) {
            // A method, getter or setter.
            let mut fkind = match accessor {
                Some(PV_GET) => FuncKind::Getter,
                Some(_) => FuncKind::Setter,
                None => FuncKind::Method,
            };
            if let Key::Private(s) = key {
                self.declare_private(s, accessor.unwrap_or(PV_PLAIN) | static_bit)?;
            }
            if is_ctor_name && !is_static {
                if accessor.is_some() || is_async {
                    return Err(self.early("a class constructor cannot be a getter, setter or async", kt));
                }
                if *ctor_seen {
                    return Err(self.early("a class can have only one constructor", kt));
                }
                *ctor_seen = true;
                fkind = if derived { FuncKind::DerivedConstructor } else { FuncKind::Constructor };
            }
            let fstart = self.tok.start;
            let f = self.parse_function_rest(fstart, None, fkind, is_async)?;
            let kind = match fkind {
                FuncKind::Getter => ElemKind::Getter(f),
                FuncKind::Setter => ElemKind::Setter(f),
                _ => ElemKind::Method(f),
            };
            let span = Span { start, end: self.prev_end };
            return Ok(mk!(self, elems, ClassElem { kind, is_static, key, span }));
        }
        // A field.
        if is_async || accessor.is_some() {
            return Err(self.unexpected());
        }
        if is_ctor_name {
            return Err(self.early("a class field cannot be named 'constructor'", kt));
        }
        if let Key::Private(s) = key {
            self.declare_private(s, PV_PLAIN | static_bit)?;
        }
        let init = if self.eat(P::Assign)? {
            let saved = (self.ctx, self.label_base, self.arrow_at);
            self.ctx = Ctx { in_func: false, is_async: false, new_target: true, super_prop: true, super_call: false, field_init: true };
            self.label_base = self.labels.len();
            let r = self.parse_assign(false, false);
            (self.ctx, self.label_base, self.arrow_at) = saved;
            Some(r?)
        } else {
            None
        };
        self.semicolon()?;
        let span = Span { start, end: self.prev_end };
        Ok(mk!(self, elems, ClassElem { kind: ElemKind::Field(init), is_static, key, span }))
    }
}

#[cfg(test)]
#[path = "parse_tests.rs"]
mod tests;
