//! Large constant trees use a single allocation opcode and a flat recipe.
use crate::ast::{ExprId, ExprKind, Key, PropKind, Span, UnaryOp};
use crate::bytecode::Op;
use crate::compile::{CR, CompileError, Fc};
use crate::string::{Chars, Str};
use crate::interp_rt::{Key as RuntimeKey, R, Realm};
use crate::object::Obj;
use crate::value::Value;

use crate::parse::data_scan::{MAX_DEPTH, Part, Scan};
pub(crate) use crate::parse::data_scan::MIN_BYTES;

/// A prefix tree. A container header is followed by its children in source order. Each object child has a string key first.
#[derive(Debug)]
enum Item<A> {
    Object(u32),
    Array(u32),
    String(A),
    Scalar(Value),
}

/// Built only from the validated syntax tree. Its strings are shared atoms, with the same lifetime as `Code::atoms`.
#[derive(Debug)]
pub(crate) struct DataLiteral<A = Str>(Box<[Item<A>]>);

impl<A: Copy> DataLiteral<A> {
    pub(crate) fn map_atoms<B>(&self, mut map: impl FnMut(A) -> B) -> DataLiteral<B> {
        DataLiteral(self.0.iter().map(|item| match *item {
            Item::Object(n) => Item::Object(n),
            Item::Array(n) => Item::Array(n),
            Item::String(a) => Item::String(map(a)),
            Item::Scalar(v) => Item::Scalar(v),
        }).collect())
    }

    pub(crate) fn bytes(&self) -> usize {
        size_of_val(&*self.0)
    }
}

impl<'a> Fc<'a> {
    #[cold]
    pub(crate) fn try_data_literal(&mut self, e: ExprId, dst: u32) -> CR<bool> {
        if self.dead {
            return Ok(false);
        }
        let mut items = Vec::new();
        if !self.data_items(e, &mut items, 0)? {
            return Ok(false);
        }
        self.emit_data_items(items, dst);
        Ok(true)
    }

    fn emit_data_items(&mut self, items: Vec<Item<Str>>, dst: u32) {
        let index = self.data.len() as u32;
        self.data.push(DataLiteral(items.into_boxed_slice()));
        self.op(Op::NewDataLit, dst, index, 0);
    }

    pub(crate) fn emit_data_span(&mut self, span: Span, dst: u32) -> CR {
        if self.dead {
            return Ok(());
        }
        let mut items = Vec::new();
        if !self.span_items(span, &mut items, 0)? {
            return Err(CompileError::Syntax("invalid checked data literal".into()));
        }
        self.emit_data_items(items, dst);
        Ok(())
    }

    fn span_items(&mut self, span: Span, out: &mut Vec<Item<Str>>, depth: usize) -> CR<bool> {
        let prog = self.prog;
        let source = prog.text(span);
        let mut scan = Scan::new(source, u32::MAX);
        let mut opens = Vec::new();
        let mut error = Ok(());
        let built = scan.value(MAX_DEPTH - depth, &mut |part| {
            let item = match part {
                Part::Open(object) => {
                    opens.push(out.len());
                    if object { Item::Object(0) } else { Item::Array(0) }
                }
                Part::Close(len) => {
                    let at = opens.pop().expect("an open data container");
                    match &mut out[at] {
                        Item::Object(n) | Item::Array(n) => *n = len,
                        _ => unreachable!("a data container"),
                    }
                    return Some(());
                }
                Part::String(s) => {
                    match self.data_string(Span { start: span.start + s.start, end: span.start + s.end }) {
                        Ok(s) => Item::String(s),
                        Err(e) => {
                            error = Err(e);
                            return None;
                        }
                    }
                }
                Part::Number(s) => {
                    let n = crate::lex::decimal_value(prog.text(Span { start: span.start + s.start, end: span.start + s.end }), &mut String::new());
                    Item::Scalar(Realm::num(n.unwrap_or(f64::NAN)))
                }
                Part::Bool(b) => Item::Scalar(Value::bool(b)),
                Part::Null => Item::Scalar(Value::NULL),
            };
            out.push(item);
            Some(())
        });
        error?;
        Ok(built.is_some() && scan.at == source.len())
    }

    fn data_items(&mut self, e: ExprId, out: &mut Vec<Item<Str>>, depth: usize) -> CR<bool> {
        let prog = self.prog;
        let x = *prog.expr(e);
        let item = match x.kind {
            ExprKind::Data if depth < MAX_DEPTH => return self.span_items(x.span, out, depth),
            ExprKind::Null => Item::Scalar(Value::NULL),
            ExprKind::Bool(b) => Item::Scalar(Value::bool(b)),
            ExprKind::Number(base) => Item::Scalar(Realm::num(prog.number(x.span, base).unwrap_or(f64::NAN))),
            ExprKind::Unary { op: UnaryOp::Neg, arg } => {
                let x = prog.expr(arg);
                let ExprKind::Number(base) = x.kind else { return Ok(false) };
                Item::Scalar(Realm::num(-prog.number(x.span, base).unwrap_or(f64::NAN)))
            }
            ExprKind::String => Item::String(self.data_string(x.span)?),
            ExprKind::Array(list) if depth < MAX_DEPTH => {
                out.push(Item::Array(list.len() as u32));
                for child in prog.list(list) {
                    if !self.data_items(child, out, depth + 1)? {
                        return Ok(false);
                    }
                }
                return Ok(true);
            }
            ExprKind::Object(list) if depth < MAX_DEPTH => {
                out.push(Item::Object(list.len() as u32));
                for p in prog.list(list).map(|p| *prog.prop(p)) {
                    if p.kind != PropKind::Init {
                        return Ok(false);
                    }
                    let key = match p.key {
                        Key::Name(s) => {
                            let n = self.atom(&prog.name(s))?;
                            self.atom_tab[n as usize]
                        }
                        Key::Str(s) => self.data_string(s)?,
                        Key::Num(s) => {
                            let n = self.atom(&Realm::num_to_string(self.num_key(s)))?;
                            self.atom_tab[n as usize]
                        }
                        _ => return Ok(false),
                    };
                    if matches!(key.chars(), Some(Chars::One(b"__proto__"))) {
                        return Ok(false);
                    }
                    out.push(Item::String(key));
                    if !self.data_items(p.value, out, depth + 1)? {
                        return Ok(false);
                    }
                }
                return Ok(true);
            }
            _ => return Ok(false),
        };
        out.push(item);
        Ok(true)
    }

    fn data_string(&mut self, span: Span) -> CR<Str> {
        let text = self.prog.text(span).as_bytes();
        let body = &text[1..text.len() - 1];
        if body.is_ascii() && !body.contains(&b'\\') {
            return self.atoms.intern(Chars::One(body)).map_err(|_| CompileError::Memory);
        }
        let i = self.string_atom(span)?;
        Ok(self.atom_tab[i as usize])
    }
}

impl DataLiteral {
    /// The VM collects before this op. The builder runs no user code and has no safepoint of its own.
    pub(crate) fn build(&self, realm: &mut Realm) -> R<Value> {
        Self::value(&mut self.0.iter(), realm, &mut Vec::new())
    }

    fn value(items: &mut std::slice::Iter<'_, Item<Str>>, realm: &mut Realm, elems: &mut Vec<Value>) -> R<Value> {
        match *items.next().expect("a data value") {
            Item::Scalar(v) => Ok(v),
            Item::String(s) => Ok(s.value()),
            Item::Array(len) => {
                let base = elems.len();
                elems.reserve(len as usize);
                for _ in 0..len {
                    let v = Self::value(items, realm, elems)?;
                    elems.push(v);
                }
                let array = realm.new_array(&elems[base..]);
                elems.truncate(base);
                Ok(array)
            }
            Item::Object(len) => {
                let o = Obj::new_plain(realm);
                for _ in 0..len {
                    let Some(&Item::String(key)) = items.next() else { unreachable!("a data key") };
                    let v = Self::value(items, realm, elems)?;
                    // The ordinary literal path also stores indices of plain objects as decimal atom names.
                    realm.define(o, RuntimeKey::Name(realm.atoms.adopt(key)), v)?;
                }
                Ok(o.value())
            }
        }
    }
}

#[cfg(test)]
#[path = "data_lit_span_tests.rs"]
mod span_tests;
