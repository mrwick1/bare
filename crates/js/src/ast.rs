//! The JavaScript syntax tree (ES2022 script), built by [`crate::parse::parse_script`].
//!
//! Layout, in plain words:
//! - A [`Program`] borrows the source text and owns every node. Nodes live in flat `Vec` arenas, one
//!   arena per node type. A node refers to another node by an [`Id`]: a 32-bit index that stays valid
//!   as long as the `Program` lives (nodes are never moved out or removed).
//! - A list (call arguments, statements in a block, ...) is a [`List`]: a `start` and a `len` into one
//!   shared `Vec<u32>`. Read it with [`Program::list`].
//! - Nodes do not copy text. A name, a string, a number, a regexp or a template piece is a byte [`Span`]
//!   into the source. Decode it on demand with [`Program::name`], [`Program::string`],
//!   [`Program::number`], [`Program::quasi`] (UTF-16 code units for strings, as JavaScript needs).
//! - Every node has a byte span. [`Program::location`] turns an offset into a line and UTF-16 column.
//!
//! The tree is executable structure: nothing is skipped or kept as opaque source text. A module has
//! [`Program::module`]: its code is one synthetic function, and its `import` and `export` declarations are in [`ModuleInfo`].
//!
//! Distinctions an executor needs, kept on purpose:
//! - [`ExprKind::Paren`]: `(a)` is not `a` (name inference, `(a?.b).c`, directive detection).
//! - [`ExprKind::Chain`]: wraps a whole optional chain, where `?.` short-circuits to.
//! - Assignment targets and bindings are [`Pat`] nodes, never expressions pretending to be patterns.
//! - [`StmtKind::Directive`]: a string expression statement in a directive prologue.
//! - [`Func::strict`], [`Func::simple_params`], [`FuncKind`]: code is strict, has plain parameters,
//!   and which kind of function it is.
//!
//! Scope analysis (which names a function uses or captures) is not done here: that is for the
//! compiler that walks this tree.

use crate::lex::{self, Kind, NumberBase, TemplatePart, Token};
use std::borrow::Cow;
use std::cell::OnceCell;
use std::marker::PhantomData;
use std::num::NonZeroU32;
use std::rc::Rc;

/// A typed index of a node of type `T`. `Option<Id<T>>` is 4 bytes.
pub struct Id<T>(NonZeroU32, PhantomData<fn() -> T>);

impl<T> Clone for Id<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Id<T> {}
impl<T> PartialEq for Id<T> {
    fn eq(&self, o: &Self) -> bool {
        self.0 == o.0
    }
}
impl<T> Eq for Id<T> {}
impl<T> std::hash::Hash for Id<T> {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        self.0.hash(h)
    }
}
impl<T> std::fmt::Debug for Id<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "#{}", self.0)
    }
}

impl<T> Id<T> {
    /// The id of the node at arena position `index` (the first node is position 0).
    pub(crate) fn from_index(index: usize) -> Id<T> {
        // The parser limits the node count far below u32::MAX, so `index + 1` fits and is not zero.
        Id(NonZeroU32::new(index as u32 + 1).unwrap_or(NonZeroU32::MIN), PhantomData)
    }
    fn from_raw(raw: u32) -> Id<T> {
        Id(NonZeroU32::new(raw).unwrap_or(NonZeroU32::MIN), PhantomData)
    }
    pub(crate) fn from_raw_pub(raw: u32) -> Id<T> {
        Id::from_raw(raw)
    }
    pub(crate) fn raw(self) -> u32 {
        self.0.get()
    }
    pub fn index(self) -> usize {
        self.0.get() as usize - 1
    }
}

/// A run of ids stored in [`Program`]. Read with [`Program::list`].
pub struct List<T> {
    pub(crate) start: u32,
    pub(crate) len: u32,
    _p: PhantomData<fn() -> T>,
}
impl<T> Clone for List<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for List<T> {}
impl<T> std::fmt::Debug for List<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}+{}]", self.start, self.len)
    }
}
impl<T> List<T> {
    pub(crate) fn new(start: u32, len: u32) -> List<T> {
        List { start, len, _p: PhantomData }
    }
    pub fn len(self) -> usize {
        self.len as usize
    }
    pub fn is_empty(self) -> bool {
        self.len == 0
    }
}

pub type ExprId = Id<Expr>;
pub type StmtId = Id<Stmt>;
pub type PatId = Id<Pat>;
pub type FuncId = Id<Func>;
pub type ClassId = Id<Class>;

/// Byte offsets `start..end` into the source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

// ---------------------------------------------------------------- expressions

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Plus,
    Not,
    BitNot,
    Typeof,
    Void,
    Delete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Exp,
    Eq,
    Ne,
    StrictEq,
    StrictNe,
    Lt,
    Gt,
    Le,
    Ge,
    Shl,
    Shr,
    UShr,
    BitAnd,
    BitOr,
    BitXor,
    In,
    Instanceof,
}

/// `&&`, `||`, `??`: the right side runs only sometimes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogicalOp {
    And,
    Or,
    Nullish,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssignOp {
    /// `=`
    Assign,
    /// `+=`, `-=`, ...
    Compound(BinaryOp),
    /// `&&=`, `||=`, `??=`: the right side runs only sometimes.
    Logical(LogicalOp),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateOp {
    Inc,
    Dec,
}

#[derive(Clone, Copy, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Copy, Debug)]
pub enum ExprKind {
    /// A name used as a value. The node span is the name.
    Ident,
    This,
    Null,
    Bool(bool),
    /// The span is the token. Value: [`Program::number`].
    Number(NumberBase),
    /// `10n`. The span includes `n`. No value is computed.
    BigInt,
    /// The span includes the quotes. Value: [`Program::string`].
    String,
    /// `/body/flags`. The span is the token. The parser checks the flags and the pattern: see [`crate::parse`].
    RegExp,
    Template(Id<Template>),
    Tagged { tag: ExprId, quasi: Id<Template> },
    /// Elements are expressions, [`ExprKind::Spread`] or [`ExprKind::Hole`].
    Array(List<Expr>),
    Object(List<Prop>),
    /// A function or arrow function expression (also the value of a method).
    Function(FuncId),
    Class(ClassId),
    Unary { op: UnaryOp, arg: ExprId },
    /// `arg` is an identifier, a member access, or either in parentheses.
    Update { op: UpdateOp, prefix: bool, arg: ExprId },
    Binary { op: BinaryOp, left: ExprId, right: ExprId },
    Logical { op: LogicalOp, left: ExprId, right: ExprId },
    Assign { op: AssignOp, target: PatId, value: ExprId },
    Cond { test: ExprId, cons: ExprId, alt: ExprId },
    /// `callee` may be [`ExprKind::Super`] (a `super(...)` call). `optional`: written `?.(`.
    Call { callee: ExprId, args: List<Expr>, optional: bool },
    New { callee: ExprId, args: List<Expr> },
    /// `obj.name` or `obj?.name`. `name` is the span of the property name.
    Member { obj: ExprId, name: Span, optional: bool },
    /// `obj[index]` or `obj?.[index]`.
    Index { obj: ExprId, index: ExprId, optional: bool },
    /// `obj.#name`. `name` includes the `#`.
    PrivateMember { obj: ExprId, name: Span, optional: bool },
    /// `#name in right`. `name` includes the `#`.
    PrivateIn { name: Span, right: ExprId },
    /// Only as the callee of a call, or the object of a member access.
    Super,
    NewTarget,
    Await(ExprId),
    /// `yield`, `yield x` or `yield* x`. Only inside a generator.
    Yield { arg: Option<ExprId>, delegate: bool },
    /// At least two expressions.
    Seq(List<Expr>),
    /// Only in an array, call arguments or `new` arguments.
    Spread(ExprId),
    /// An elision in an array: `[a, , b]`.
    Hole,
    /// `( expr )`. Kept: see the module comment.
    Paren(ExprId),
    /// The whole optional chain `a?.b.c`. Inside, some links have `optional: true`. If a `?.` link finds
    /// `null`/`undefined`, the value of this whole node is `undefined`.
    Chain(ExprId),
    /// `import(specifier)` or `import(specifier, options)`.
    ImportCall { arg: ExprId, opts: Option<ExprId> },
    /// `import.meta`. Only in a module.
    ImportMeta,
}

/// A template literal: `n` expressions and `n + 1` pieces. Read with [`Program::template_quasis`] and
/// [`Program::template_exprs`] (they share one interleaved list).
#[derive(Clone, Copy, Debug)]
pub struct Template {
    pub(crate) start: u32,
    pub(crate) n_exprs: u32,
}

/// One piece of a template. The span includes the delimiters (`` ` ``, `}`, `${`).
#[derive(Clone, Copy, Debug)]
pub struct Quasi {
    pub span: Span,
    pub part: TemplatePart,
    /// The cooked value does not exist (legal only in a tagged template).
    pub bad_escape: bool,
}

#[derive(Clone, Copy, Debug)]
pub enum Key {
    /// A spread property has no key.
    None,
    /// An identifier name (keywords included) or shorthand.
    Name(Span),
    Str(Span),
    Num(Span),
    BigInt(Span),
    /// `[expr]`
    Computed(ExprId),
    /// `#name` (class elements only). The span includes `#`.
    Private(Span),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropKind {
    /// `key: value`
    Init,
    /// `name` or `name = default` (the latter only as the target of a pattern; the value is an assignment).
    Shorthand,
    Method,
    Get,
    Set,
    /// `...value`
    Spread,
}

#[derive(Clone, Copy, Debug)]
pub struct Prop {
    pub kind: PropKind,
    pub key: Key,
    /// For `Method`, `Get`, `Set`: an [`ExprKind::Function`].
    pub value: ExprId,
    pub span: Span,
}

// ---------------------------------------------------------------- patterns

#[derive(Clone, Copy, Debug)]
pub struct Pat {
    pub kind: PatKind,
    pub span: Span,
}

#[derive(Clone, Copy, Debug)]
pub enum PatKind {
    /// A name. The node span is the name.
    Ident,
    /// `a.b` or `a[b]` as an assignment target (never in a declaration or a parameter).
    Member(ExprId),
    /// Elements: patterns, [`PatKind::Hole`], and at most one [`PatKind::Rest`] at the end.
    Array(List<Pat>),
    Object(List<PatProp>),
    /// `target = default`
    Assign { target: PatId, default: ExprId },
    Rest(PatId),
    Hole,
}

#[derive(Clone, Copy, Debug)]
pub struct PatProp {
    /// `Key::None` for a rest property.
    pub key: Key,
    /// For a rest property: the [`PatKind::Rest`].
    pub value: PatId,
    pub span: Span,
}

// ---------------------------------------------------------------- functions and classes

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FuncKind {
    Normal,
    Arrow,
    Method,
    Getter,
    Setter,
    /// A class constructor.
    Constructor,
    /// The constructor of a class with `extends`: `super(...)` is allowed.
    DerivedConstructor,
}

#[derive(Clone, Copy, Debug)]
pub enum Body {
    Block(List<Stmt>),
    /// The expression of a concise arrow `x => x + 1`.
    Expr(ExprId),
    /// A block body that the parser checked and did not keep: [`Program::lazy`] with this index says where it is.
    /// The first call parses the whole function again ([`Program::lazy_function`]) into a tree of its own.
    /// Only a parse with lazy bodies makes it (`parse_script_lazy`); the evaluator never reads the body of such a node.
    Lazy(u32),
}

#[derive(Clone, Copy, Debug)]
pub struct Func {
    pub name: Option<Span>,
    /// Patterns: identifiers, destructuring, defaults ([`PatKind::Assign`]), and a last [`PatKind::Rest`].
    pub params: List<Pat>,
    pub body: Body,
    pub kind: FuncKind,
    /// Async function or generator. One byte for both keeps `Func` at 44 bytes.
    pub mode: FuncMode,
    /// The body is strict code (inherited, a class, or a `"use strict"` directive).
    pub strict: bool,
    /// All parameters are plain identifiers (no default, rest or destructuring).
    pub simple_params: bool,
    /// Function and method values start at `(`; other functions at `function`, `async` or the first parameter.
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FuncMode {
    Plain,
    Async,
    /// `function*` or a `*` method.
    Generator,
    /// `async function*` or an `async *` method.
    AsyncGen,
    /// A plain function that always keeps its environment: a class member (the parser sets it). Its call checks nothing for frame slots.
    Env,
}

impl Func {
    pub fn is_async(&self) -> bool {
        matches!(self.mode, FuncMode::Async | FuncMode::AsyncGen)
    }

    pub fn is_generator(&self) -> bool {
        matches!(self.mode, FuncMode::Generator | FuncMode::AsyncGen)
    }

    /// An ordinary `function` (not an arrow, method, async function or generator): `new` can call it.
    pub fn is_plain_ctor(&self) -> bool {
        self.kind == FuncKind::Normal && matches!(self.mode, FuncMode::Plain | FuncMode::Env)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Class {
    pub name: Option<Span>,
    pub superclass: Option<ExprId>,
    pub elems: List<ClassElem>,
    pub span: Span,
}

#[derive(Clone, Copy, Debug)]
pub enum ElemKind {
    /// Includes the constructor (its function has [`FuncKind::Constructor`]).
    Method(FuncId),
    Getter(FuncId),
    Setter(FuncId),
    /// A field and its initializer.
    Field(Option<ExprId>),
    /// `static { ... }`: the body as a method with no parameters. The element is static and has [`Key::None`].
    StaticBlock(FuncId),
}

#[derive(Clone, Copy, Debug)]
pub struct ClassElem {
    pub kind: ElemKind,
    pub is_static: bool,
    pub key: Key,
    pub span: Span,
}

// ---------------------------------------------------------------- statements

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VarKind {
    Var,
    Let,
    Const,
}

#[derive(Clone, Copy, Debug)]
pub struct Declarator {
    pub target: PatId,
    pub init: Option<ExprId>,
    pub span: Span,
}

#[derive(Clone, Copy, Debug)]
pub enum ForInit {
    None,
    /// A [`StmtKind::Var`].
    Var(StmtId),
    Expr(ExprId),
}

#[derive(Clone, Copy, Debug)]
pub enum ForLeft {
    /// A [`StmtKind::Var`] with one declarator. Only `for (var a = 1 in o)` (sloppy) has an initializer.
    Var(StmtId),
    Pat(PatId),
}

#[derive(Clone, Copy, Debug)]
pub struct Case {
    /// `None` for `default`.
    pub test: Option<ExprId>,
    pub body: List<Stmt>,
    pub span: Span,
}

#[derive(Clone, Copy, Debug)]
pub struct Catch {
    pub param: Option<PatId>,
    /// A [`StmtKind::Block`].
    pub body: StmtId,
    pub span: Span,
}

#[derive(Clone, Copy, Debug)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Clone, Copy, Debug)]
pub enum StmtKind {
    Expr(ExprId),
    /// A string in a directive prologue (`"use strict"`). The expression is the string literal.
    Directive(ExprId),
    Var { kind: VarKind, decls: List<Declarator> },
    Function(FuncId),
    Class(ClassId),
    Return(Option<ExprId>),
    If { test: ExprId, cons: StmtId, alt: Option<StmtId> },
    For { init: ForInit, test: Option<ExprId>, update: Option<ExprId>, body: StmtId },
    ForIn { left: ForLeft, right: ExprId, body: StmtId },
    /// `is_await`: `for await (... of ...)`
    ForOf { left: ForLeft, right: ExprId, body: StmtId, is_await: bool },
    While { test: ExprId, body: StmtId },
    DoWhile { body: StmtId, test: ExprId },
    /// The label is a span of the name.
    Break(Option<Span>),
    Continue(Option<Span>),
    Throw(ExprId),
    Try { block: StmtId, handler: Option<Id<Catch>>, finalizer: Option<StmtId> },
    Switch { disc: ExprId, cases: List<Case> },
    Labeled { label: Span, body: StmtId },
    Block(List<Stmt>),
    Empty,
    Debugger,
    With { obj: ExprId, body: StmtId },
    /// `export default expr;` and the anonymous `export default function () {}` / `class {}`. The value is stored in the
    /// module binding `*default*` and gets the name `default`. Only in a module.
    ExportDefault(ExprId),
}

// ---------------------------------------------------------------- modules

/// What an import or a re-export takes from the requested module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportName {
    /// A named export (`default` for `import d from`).
    Name(Box<str>),
    /// The namespace object: `import * as ns`, `export * as ns from`.
    Namespace,
}

/// `import {name as local} from 'request'`. `local` is a span of the local binding name.
#[derive(Clone, Debug)]
pub struct ImportEntry {
    pub request: u32,
    pub name: ImportName,
    pub local: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExportSource {
    /// A binding of this module. The span is the local name.
    Local(Span),
    /// The value of `export default expr` (the binding `*default*`).
    Default,
    /// A re-export: `export {name as x} from 'request'` or `export * as x from 'request'`.
    Indirect { request: u32, name: ImportName },
}

/// `export {local as name}`. `name` is the exported name.
#[derive(Clone, Debug)]
pub struct ExportEntry {
    pub name: Box<str>,
    pub source: ExportSource,
}

/// The module facts of a [`Program`] parsed with [`crate::parse::parse_module`]. The module code is one synthetic function
/// ([`ModuleInfo::func`]) whose body is the statement list; the import and export declarations are not statements.
#[derive(Clone, Debug)]
pub struct ModuleInfo {
    pub func: FuncId,
    /// The module specifiers, in order of first appearance, once each.
    pub requests: Vec<Box<str>>,
    pub imports: Vec<ImportEntry>,
    pub exports: Vec<ExportEntry>,
    /// Requests of `export * from 'request'`.
    pub stars: Vec<u32>,
    /// The module code has an `await` outside every function (the function is then `async`).
    pub has_tla: bool,
}

// ---------------------------------------------------------------- program

/// The source text of a [`Program`]: borrowed from the caller, or shared (`Rc`) by a program and the trees of its lazy functions.
#[derive(Debug, Clone)]
pub enum Src<'a> {
    Borrowed(&'a str),
    Shared(Rc<str>),
}

impl std::ops::Deref for Src<'_> {
    type Target = str;
    fn deref(&self) -> &str {
        match self {
            Src::Borrowed(s) => s,
            Src::Shared(s) => s,
        }
    }
}

impl PartialEq<&str> for Src<'_> {
    fn eq(&self, o: &&str) -> bool {
        **self == **o
    }
}

/// Location marks, shared by a tree and the trees of its lazy functions.
pub(crate) type Marks = Rc<OnceCell<Box<[lex::Mark]>>>;

/// The lazy functions of a tree.
#[derive(Debug, Default, Clone)]
pub(crate) struct LazyInfo {
    /// The function bodies that were checked and not kept ([`Body::Lazy`]).
    pub(crate) lazies: Vec<LazyFn>,
    /// Parsed with the module goal (also a tree of a function of a module, which has no `module` of its own).
    pub(crate) module_goal: bool,
    /// The tree of one lazy function ([`Program::lazy_function`]): its source is the source of the program that holds the function.
    pub(crate) tree: bool,
}

/// A function whose body the parser checked and dropped (see [`Body::Lazy`]).
#[derive(Debug, Clone)]
pub struct LazyFn {
    /// The `(` that opens the parameters: the second parse starts here.
    pub params_at: u32,
    /// The function name and the start of the function (`Func::span.start`).
    pub name: Option<Span>,
    pub start: u32,
    pub kind: FuncKind,
    pub mode: FuncMode,
    /// The code around the function was strict (the function may become strict by a directive of its own).
    pub strict: bool,
    /// The tree of the function, made by the first [`Program::lazy_function`]. It is shared by every realm that runs this program.
    pub(crate) parsed: OnceCell<Rc<Program<'static>>>,
}

/// A parsed script. Borrows the source (or owns it after [`Program::into_owned`]), owns all nodes.
#[derive(Debug, Clone)]
pub struct Program<'a> {
    pub source: Src<'a>,
    pub body: List<Stmt>,
    /// The script starts with a `"use strict"` directive.
    pub strict: bool,
    /// Set when the source was parsed as a module (see [`ModuleInfo`]).
    pub module: Option<Box<ModuleInfo>>,
    pub(crate) exprs: Vec<Expr>,
    pub(crate) stmts: Vec<Stmt>,
    pub(crate) pats: Vec<Pat>,
    pub(crate) funcs: Vec<Func>,
    pub(crate) classes: Vec<Class>,
    pub(crate) props: Vec<Prop>,
    pub(crate) pat_props: Vec<PatProp>,
    pub(crate) templates: Vec<Template>,
    pub(crate) quasis: Vec<Quasi>,
    pub(crate) decls: Vec<Declarator>,
    pub(crate) cases: Vec<Case>,
    pub(crate) catches: Vec<Catch>,
    pub(crate) elems: Vec<ClassElem>,
    pub(crate) lists: Vec<u32>,
    /// What the lazy function bodies need ([`Body::Lazy`]); `None` for a tree that has none. One word: the realm swaps a `Program` at each call
    /// from one script into another.
    pub(crate) lz: Option<Box<LazyInfo>>,
    /// Location marks, made by the first `location` call (16 B per 4 KiB of source). The trees of the lazy functions share them: they share the source.
    marks: OnceCell<Marks>,
}

/// How many nodes of each kind a program has, and the bytes they use (capacity not counted).
/// The counts include unreachable expressions: the parser reads an assignment target, a destructuring
/// target or an arrow parameter as an expression first, then converts it to a pattern. The expression stays in the arena.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub exprs: usize,
    pub stmts: usize,
    pub pats: usize,
    pub funcs: usize,
    pub classes: usize,
    pub props: usize,
    pub pat_props: usize,
    pub templates: usize,
    pub quasis: usize,
    pub decls: usize,
    pub cases: usize,
    pub catches: usize,
    pub elems: usize,
    pub list_items: usize,
    /// Function bodies that wait for their first call (not in [`Stats::nodes`]).
    pub lazies: usize,
    pub bytes: usize,
}

impl Stats {
    pub fn nodes(&self) -> usize {
        self.exprs + self.stmts + self.pats + self.funcs + self.classes + self.props + self.pat_props
            + self.templates + self.quasis + self.decls + self.cases + self.catches + self.elems
    }
}

fn token_of(kind: Kind, span: Span, escapes: bool) -> Token {
    Token { kind, start: span.start, end: span.end, newline_before: false, escapes }
}

/// Give back the spare capacity of an arena that will not grow again.
fn tight<T>(mut v: Vec<T>) -> Vec<T> {
    v.shrink_to_fit();
    v
}

/// [`Program::name`] on a plain source text (the parser has the text before it has a stable `Program`).
pub(crate) fn name_in(source: &str, span: Span) -> Cow<'_, str> {
    let full = source.get(span.start as usize..span.end as usize).unwrap_or("");
    let t = full.strip_prefix('#').unwrap_or(full);
    if !t.as_bytes().contains(&b'\\') {
        return Cow::Borrowed(t);
    }
    let tok = token_of(Kind::Ident, Span { start: span.start + (full.len() - t.len()) as u32, end: span.end }, true);
    lex::ident_name(source, &tok).unwrap_or(Cow::Borrowed(t))
}

impl<'a> Program<'a> {
    pub(crate) fn new(source: &'a str) -> Program<'a> {
        Program {
            source: Src::Borrowed(source),
            body: List::new(0, 0),
            strict: false,
            module: None,
            exprs: Vec::new(),
            stmts: Vec::new(),
            pats: Vec::new(),
            funcs: Vec::new(),
            classes: Vec::new(),
            props: Vec::new(),
            pat_props: Vec::new(),
            templates: Vec::new(),
            quasis: Vec::new(),
            decls: Vec::new(),
            cases: Vec::new(),
            catches: Vec::new(),
            elems: Vec::new(),
            lists: Vec::new(),
            lz: None,
            marks: OnceCell::new(),
        }
    }

    /// The same tree with an owned copy of the source. It holds no borrow, so a host can keep it
    /// after the source text is gone. Node ids and spans are unchanged.
    pub fn into_owned(self) -> Program<'static> {
        let src: Rc<str> = match &self.source {
            Src::Shared(s) => s.clone(),
            Src::Borrowed(s) => Rc::from(*s),
        };
        self.with_shared_source(src)
    }

    /// The same tree over the shared source `src` (the same text as the source of this tree: the ids and spans are unchanged).
    pub(crate) fn with_shared_source(self, src: Rc<str>) -> Program<'static> {
        Program {
            source: Src::Shared(src),
            body: self.body, strict: self.strict, module: self.module, exprs: tight(self.exprs), stmts: tight(self.stmts), pats: tight(self.pats),
            funcs: tight(self.funcs), classes: tight(self.classes), props: tight(self.props), pat_props: tight(self.pat_props),
            templates: tight(self.templates), quasis: tight(self.quasis), decls: tight(self.decls), cases: tight(self.cases),
            catches: tight(self.catches), elems: tight(self.elems), lists: tight(self.lists), lz: self.lz.map(|mut l| {
                l.lazies.shrink_to_fit();
                l
            }), marks: self.marks,
        }
    }

    pub(crate) fn lazies(&self) -> &[LazyFn] {
        self.lz.as_ref().map_or(&[], |l| &l.lazies)
    }

    pub(crate) fn lz_mut(&mut self) -> &mut LazyInfo {
        self.lz.get_or_insert_default()
    }

    /// The tree of one lazy function ([`Program::lazy_function`]).
    pub(crate) fn is_lazy_tree(&self) -> bool {
        self.lz.as_ref().is_some_and(|l| l.tree)
    }

    /// The marks that this tree shares with its lazy functions (made empty at the first call).
    fn shared_marks(&self) -> &Marks {
        self.marks.get_or_init(Default::default)
    }

    pub(crate) fn share_marks(&mut self, marks: &Marks) {
        self.marks = OnceCell::from(marks.clone());
    }

    /// The tree of the lazy function `idx` ([`Body::Lazy`]): the function with its parameters and body, as the last function of the
    /// tree. The first call parses it, bounded by `limits`. A tree that an earlier call made is given back as it is. The count is `Some(nodes)`
    /// of the parse that made the tree now, `None` for a tree that was there.
    pub fn lazy_function(&self, idx: u32, limits: crate::parse::Limits) -> Result<(Rc<Program<'static>>, FuncId, Option<u32>), crate::parse::Error> {
        let lz = &self.lazies()[idx as usize];
        let (prog, used) = match lz.parsed.get() {
            Some(p) => (p.clone(), None),
            None => {
                let Src::Shared(src) = &self.source else {
                    return Err(crate::parse::Error { kind: crate::parse::ErrorKind::Unsupported, msg: "lazy function of an unshared source", start: lz.start, end: lz.start });
                };
                let (p, used) = crate::parse::parse_lazy_function(src, self.shared_marks(), self.lz.as_ref().is_some_and(|l| l.module_goal), lz, limits)?;
                let p = Rc::new(p);
                let _ = lz.parsed.set(p.clone());
                (p, Some(used))
            }
        };
        let fid = Id::from_index(prog.funcs.len() - 1);
        Ok((prog, fid, used))
    }

    /// The lazy function `idx` of this tree ([`Body::Lazy`]).
    pub fn lazy(&self, idx: u32) -> &LazyFn {
        &self.lazies()[idx as usize]
    }

    pub fn expr(&self, id: ExprId) -> &Expr {
        &self.exprs[id.index()]
    }
    pub fn stmt(&self, id: StmtId) -> &Stmt {
        &self.stmts[id.index()]
    }
    pub fn pat(&self, id: PatId) -> &Pat {
        &self.pats[id.index()]
    }
    pub fn func(&self, id: FuncId) -> &Func {
        &self.funcs[id.index()]
    }
    pub fn class(&self, id: ClassId) -> &Class {
        &self.classes[id.index()]
    }
    pub fn prop(&self, id: Id<Prop>) -> &Prop {
        &self.props[id.index()]
    }
    pub fn pat_prop(&self, id: Id<PatProp>) -> &PatProp {
        &self.pat_props[id.index()]
    }
    pub fn template(&self, id: Id<Template>) -> &Template {
        &self.templates[id.index()]
    }
    pub fn quasi_node(&self, id: Id<Quasi>) -> &Quasi {
        &self.quasis[id.index()]
    }
    pub fn decl(&self, id: Id<Declarator>) -> &Declarator {
        &self.decls[id.index()]
    }
    pub fn case(&self, id: Id<Case>) -> &Case {
        &self.cases[id.index()]
    }
    pub fn catch(&self, id: Id<Catch>) -> &Catch {
        &self.catches[id.index()]
    }
    pub fn elem(&self, id: Id<ClassElem>) -> &ClassElem {
        &self.elems[id.index()]
    }

    /// The ids in a list.
    pub fn list<T>(&self, l: List<T>) -> impl ExactSizeIterator<Item = Id<T>> + Clone + '_ {
        self.lists[l.start as usize..(l.start + l.len) as usize].iter().map(|&r| Id::from_raw(r))
    }

    /// The pieces of a template, in order.
    pub fn template_quasis(&self, t: &Template) -> impl Iterator<Item = Id<Quasi>> + '_ {
        let t = *t;
        (0..=t.n_exprs as usize).map(move |i| Id::from_raw(self.lists[t.start as usize + 2 * i]))
    }

    /// The expressions between the pieces of a template, in order.
    pub fn template_exprs(&self, t: &Template) -> impl ExactSizeIterator<Item = ExprId> + '_ {
        let t = *t;
        (0..t.n_exprs as usize).map(move |i| Id::from_raw(self.lists[t.start as usize + 2 * i + 1]))
    }

    /// The source text of a span.
    pub fn text(&self, span: Span) -> &str {
        self.source.get(span.start as usize..span.end as usize).unwrap_or("")
    }

    /// The name in a span (an identifier, a property name, or a `#private` name without the `#`):
    /// `\u` escapes decoded. Borrowed from the source when there is no escape.
    pub fn name(&self, span: Span) -> Cow<'_, str> {
        name_in(&self.source, span)
    }

    /// Append the value of a string literal (a [`ExprKind::String`] span or a [`Key::Str`] span).
    pub fn string(&self, span: Span, out: &mut Vec<u16>) -> Result<(), lex::Error> {
        lex::decode_string(&self.source, &token_of(Kind::String { legacy_octal: false }, span, true), out)
    }

    /// The value of a number literal.
    pub fn number(&self, span: Span, base: NumberBase) -> Option<f64> {
        lex::number_value(&self.source, &token_of(Kind::Number(base), span, false))
    }

    /// Append the cooked (`raw = false`) or raw value of a template piece. A cooked piece with a bad
    /// escape fails.
    pub fn quasi(&self, q: &Quasi, raw: bool, out: &mut Vec<u16>) -> Result<(), lex::Error> {
        let kind = Kind::Template { part: q.part, bad_escape: q.bad_escape };
        lex::decode_template(&self.source, &token_of(kind, q.span, true), raw, out)
    }

    /// The pattern and flags of a regexp literal.
    pub fn regexp(&self, span: Span) -> Option<(&str, &str)> {
        lex::regexp_parts(&self.source, &token_of(Kind::RegExp, span, false))
    }

    /// Line (from 1) and UTF-16 column (from 0) of a byte offset. In a source longer than `lex::MARK_STEP`, the first
    /// call makes the marks, so later calls read at most `lex::MARK_STEP` bytes. A shorter source needs none.
    pub fn location(&self, byte: u32) -> lex::Location {
        if self.source.len() <= lex::MARK_STEP {
            return lex::location(&self.source, byte as usize);
        }
        lex::location_in(&self.source, self.shared_marks().get_or_init(|| lex::location_marks(&self.source)), byte as usize)
    }

    /// The most source bytes the next `location` call reads: the whole source before the marks exist.
    pub fn location_cost(&self) -> usize {
        if self.marks.get().is_some_and(|m| m.get().is_some()) { lex::MARK_STEP } else { self.source.len() }
    }

    pub fn stats(&self) -> Stats {
        use std::mem::size_of_val;
        Stats {
            exprs: self.exprs.len(),
            stmts: self.stmts.len(),
            pats: self.pats.len(),
            funcs: self.funcs.len(),
            classes: self.classes.len(),
            props: self.props.len(),
            pat_props: self.pat_props.len(),
            templates: self.templates.len(),
            quasis: self.quasis.len(),
            decls: self.decls.len(),
            cases: self.cases.len(),
            catches: self.catches.len(),
            elems: self.elems.len(),
            list_items: self.lists.len(),
            lazies: self.lazies().len(),
            bytes: size_of_val(&*self.exprs) + size_of_val(&*self.stmts) + size_of_val(&*self.pats)
                + size_of_val(&*self.funcs) + size_of_val(&*self.classes) + size_of_val(&*self.props)
                + size_of_val(&*self.pat_props) + size_of_val(&*self.templates) + size_of_val(&*self.quasis)
                + size_of_val(&*self.decls) + size_of_val(&*self.cases) + size_of_val(&*self.catches)
                + size_of_val(&*self.elems) + size_of_val(&*self.lists) + size_of_val(self.lazies()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::size_of;

    /// The node sizes are part of the design (fast, light). A change here must be on purpose.
    #[test]
    fn node_sizes() {
        assert_eq!(size_of::<ExprId>(), 4);
        assert_eq!(size_of::<Option<ExprId>>(), 4);
        assert_eq!(size_of::<List<Expr>>(), 8);
        assert!(size_of::<ExprKind>() <= 16, "ExprKind {}", size_of::<ExprKind>());
        assert!(size_of::<Expr>() <= 24, "Expr {}", size_of::<Expr>());
        assert!(size_of::<Stmt>() <= 32, "Stmt {}", size_of::<Stmt>());
        assert!(size_of::<Pat>() <= 24, "Pat {}", size_of::<Pat>());
        assert!(size_of::<Func>() <= 44, "Func {}", size_of::<Func>());
    }
}
