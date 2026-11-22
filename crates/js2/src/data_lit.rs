//! Large constant trees use a single allocation opcode and a flat recipe.
use crate::ast::{ExprId, ExprKind, Key, PropKind, Span, UnaryOp};
use crate::bytecode::Op;
use crate::compile::{CR, CompileError, Fc};
use crate::string::{Chars, Str};
use crate::interp_rt::{Key as RuntimeKey, R, Realm};
use crate::object::Obj;
use crate::value::Value;

/// Small literals stay on the register path. Bound the stack used to lower and build a recipe.
pub(crate) const MIN_BYTES: u32 = 1024;
const MAX_DEPTH: usize = 64;

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
        let index = self.data.len() as u32;
        self.data.push(DataLiteral(items.into_boxed_slice()));
        self.op(Op::NewDataLit, dst, index, 0);
        Ok(true)
    }

    fn data_items(&mut self, e: ExprId, out: &mut Vec<Item<Str>>, depth: usize) -> CR<bool> {
        let prog = self.prog;
        let x = *prog.expr(e);
        let item = match x.kind {
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
