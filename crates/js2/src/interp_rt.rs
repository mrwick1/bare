//! Runtime services of the interpreter (`interp.rs`, plan step S2.6): the realm, the feedback vectors with their caches, and the slow
//! paths that the loop calls: conversions, arithmetic that is not on integers or doubles, property access, errors.
//!
//! **Realm.** One heap, one shape table, one atom table, the global object and the prototypes. Every pointer that the realm keeps is
//! walked as a root by [`Realm::collect`] (`consts`) or pinned as a shape (`pins`).
//!
//! **Collection.** The loop calls [`Realm::gate`] at the start of each op that allocates. It collects when the heap has passed the
//! task limit (`Heap::task_due`), with the frames of the JS stack as roots, and then the op runs with no collection inside. So an op
//! may keep a cell in a Rust local after the gate, and the slow paths here never collect. A task may pass the limit by one op, and the
//! slack of the limit (8 MiB, `heap.rs`) is far above what one op allocates.
//!
//! **Feedback.** Each code has one [`Feedback`] for the realm: a [`Cache`] for each site (same index as `Code::sites`), the atoms of
//! the code made canonical for this realm, and the shapes of its object literals. The code holds no realm pointer (design 6.11 item 11).
//!
//! Cache states of a property site (`Cache::state`), and the transitions:
//!
//! | State | Meaning | On a miss |
//! | --- | --- | --- |
//! | `FRESH` | never ran | the slow path runs and the state becomes `SEEN`; no entry is made, so code that runs once costs no entry |
//! | `SEEN` | ran once | the slow path runs; for a cacheable shape the record gets the entry and the state becomes `MONO` |
//! | `MONO` | one entry in the record: (shape, place, shape after an add) | for another cacheable shape: the entries go to a pool, the record keeps the first, state `POLY` |
//! | `POLY` | 2 to 4 entries; the pool holds all, the record the first | the pool is scanned in the slow path; a fifth shape makes the state `MEGA` |
//! | `MEGA` | too many shapes | the lookup runs with no cache work. The record has shape 0, which no object has |
//!
//! A hit is the compare of the shape id of the receiver with `Cache::shape`, then one load or store at `Cache::loc` (`Obj::loc`).
//! A set that adds a property is a hit too: the entry has the shape after the add, and the hit stores the value and the new shape id.
//! That entry is made only when the new slot is inline, so no allocation is needed. A dictionary shape (`Shapes::cacheable`) never
//! gets an entry.
//!
//! **Prototype entries** (`docs/design-js2-proto-ic.md`). A read that finds a data property on the prototype chain makes an entry too.
//! The record keeps `shape` 0 (the own-property hit never matches), the shape of the receiver in `to` (a shape id is never 0), and the
//! index of a [`ProtoHit`] in `Feedback::protos` in `loc`. The hit compares the shape of the receiver, then the entry's epoch with
//! `Shapes::proto_epoch`, then reads the slot of the holder. The shape of the receiver fixes its prototype, so the chain below the
//! receiver changes only when an object on it changes: `fill_proto` marks the holder and each link with `F_PROTO`, and each shape
//! change of a marked object bumps the epoch (`Obj::set_shape`). A read of a string, a number or a boolean uses the keys `K_STR`,
//! `K_NUM` and `K_BOOL` in `to`, and the slow path only. A collection bumps the epoch too, so an entry never outlives a collection
//! and no pointer of an entry is a root. A site that is stale `PROTO_REFILLS` times goes to `MEGA`.
//!
//! After a collection that freed a shape (`Shapes::epoch` changed) [`Realm::collect`] clears every record, because an id may name
//! another shape now.
//!
//! **Not here:** `Symbol`, `BigInt`, the full `Number` to string rules (a number prints with Rust's rules, so `1e21` differs), and the
//! built-ins past the core (`Map`, `Set`, `JSON`, `Date`, `Promise`, `RegExp`). `ToPrimitive` calls only native `valueOf` and `toString`
//! (`builtins.rs`).
use crate::atoms::{Atoms, SharedAtoms};
use crate::bytecode::Code;
use crate::cell::Layout;
use crate::gc::{Marker, collect};
use crate::heap::Heap;
use crate::codes::Codes;
use crate::interp::LazyCompile;
use crate::reenter::Machine;
use crate::builtins::{Hint, Native};
use crate::object::{Ctx, F_ARGS, F_CLASS, F_ELEMS_FIXED, F_ELEMS_RO, F_HOST, F_LAZY, F_LAZYM, F_LEN_RO, F_NATIVE, F_NOEXT, F_PROTO, F_SLOWIDX, K_WRAP, Obj};
use crate::shapes::{A_ACC, A_C, A_E, A_W, Entry, ShapeId, Shapes};
use crate::string::{Chars, Str};
use crate::value::{Kind, Value};
use std::rc::Rc;
use std::collections::HashMap;
use std::sync::Arc;

/// A thrown value (the result of a failed op).
pub type R<T> = Result<T, Value>;

pub const FRESH: u8 = 0;
pub const SEEN: u8 = 1;
pub const MONO: u8 = 2;
pub const POLY: u8 = 3;
pub const MEGA: u8 = 4;
/// Entries of a polymorphic site.
const POLY_MAX: usize = 4;
/// Bit of `to` in a pool entry of a read site: the entry is a prototype entry, and the other bits are its index in `Feedback::protos`.
const PROTO: u32 = 1 << 31;
/// The receiver keys of a prototype entry for a string, a number and a boolean. A shape id is below `SHAPE_CAP`, so none collides.
const K_STR: u32 = u32::MAX - 1;
const K_NUM: u32 = u32::MAX - 2;
const K_BOOL: u32 = u32::MAX - 3;
/// Times that a prototype entry may be made again after its epoch changed (between two collections) before the site goes to `MEGA`.
const PROTO_REFILLS: u32 = 16;

/// The hot part of a feedback record (design 6.10 item 12): 16 bytes. `shape` is the shape id of the receiver (0: no entry). `loc` is
/// the place of the slot (`Obj::loc`). `to` is the shape id after an add (0: the property is there already). `poly` is the index of
/// the pool entry in the state `POLY`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Cache {
    pub shape: u32,
    pub loc: u32,
    pub to: u32,
    pub state: u8,
    pub poly: u16,
}

impl Cache {
    /// A site of `GetGlobal` or `SetGlobal` holds in `shape` the tag of the global object (`Shapes::watch_tag`, never a shape id) and in
    /// `loc` and `to` the address of the slot of the global (low and high 32 bits), so a hit is one compare and one load or store.
    #[inline]
    pub fn global_slot(&self) -> *mut Value {
        std::ptr::with_exposed_provenance_mut((self.loc as u64 | (self.to as u64) << 32) as usize)
    }

    fn global_entry(tag: u32, slot: *mut Value) -> Cache {
        let a = slot.expose_provenance() as u64;
        Cache { shape: tag, loc: a as u32, to: (a >> 32) as u32, state: MONO, poly: 0 }
    }

    /// The record's entry as a pool entry (`Poly::e`): `(shape or receiver key, loc, to)`.
    fn tuple(&self) -> (u32, u32, u32) {
        if self.shape == 0 && self.to != 0 { (self.to, 0, PROTO | self.loc) } else { (self.shape, self.loc, self.to) }
    }
}

/// A prototype entry: the object that holds the data property, the place of its slot (`Obj::loc_now`) and the epoch of `Shapes` at which
/// the entry was made. It is valid while the epoch is the current one (see the module comment), and then the holder is alive.
#[derive(Clone, Copy, Debug)]
pub struct ProtoHit {
    pub holder: Obj,
    pub loc: u32,
    pub epoch: u64,
    refills: u32,
}

#[derive(Clone, Copy, Debug, Default)]
struct Poly {
    n: u8,
    e: [(u32, u32, u32); POLY_MAX],
}

/// The feedback vector of one code for one realm.
#[derive(Debug, Default)]
pub struct Feedback {
    pub caches: Box<[Cache]>,
    /// `Code::atoms[i]` as the canonical atom of this realm, made at the first use.
    pub names: Box<[Option<Str>]>,
    /// The shape of each object literal recipe, made at the first use. It is pinned.
    pub lits: Box<[Option<ShapeId>]>,
    poly: Vec<Poly>,
    /// The prototype entries of the sites. Nothing here is a root; an entry is read only while its epoch is current.
    pub protos: Vec<ProtoHit>,
    /// The template object of each tagged template site (`GetTemplate`): the site and the object.
    pub tmpl: Vec<(u32, Value)>,
    pub ready: bool,
}

impl Feedback {
    pub fn init(&mut self, code: &Code) {
        self.caches = vec![Cache::default(); code.sites.len()].into();
        self.names = vec![None; code.atoms.len()].into();
        self.lits = vec![None; code.recipes.len()].into();
        self.ready = true;
    }

    fn clear(&mut self) {
        self.caches.iter_mut().for_each(|c| *c = Cache::default());
        self.poly.clear();
        self.protos.clear();
    }

    /// A collection bumped the epoch: every entry is stale, and a site may fill again.
    fn collected(&mut self) {
        self.protos.iter_mut().for_each(|p| p.refills = 0);
    }

    /// Mark what the vector keeps alive: its atoms and literal shapes.
    pub fn walk(&self, m: &mut Marker) {
        self.names.iter().flatten().for_each(|s| m.value(s.value()));
        self.lits.iter().flatten().for_each(|s| m.shape(*s));
        self.tmpl.iter().for_each(|t| m.value(t.1));
    }

    /// Entries in the pool for the site, for tests.
    pub fn entries(&self, site: usize) -> usize {
        let c = &self.caches[site];
        match c.state {
            MONO => 1,
            POLY => self.poly[c.poly as usize].n as usize,
            _ => 0,
        }
    }

    /// Record an entry after a slow path found `(shape, loc, to)` for the site. See the table in the module comment.
    fn fill(&mut self, site: usize, shape: u32, loc: u32, to: u32) {
        let c = &mut self.caches[site];
        match c.state {
            FRESH => c.state = SEEN,
            SEEN => *c = if to & PROTO != 0 { Cache { shape: 0, loc: to & !PROTO, to: shape, state: MONO, poly: 0 } } else { Cache { shape, loc, to, state: MONO, poly: 0 } },
            MONO => match u16::try_from(self.poly.len()) {
                Ok(i) => {
                    let mut p = Poly { n: 2, ..Poly::default() };
                    p.e[0] = c.tuple();
                    p.e[1] = (shape, loc, to);
                    self.poly.push(p);
                    c.poly = i;
                    c.state = POLY;
                }
                Err(_) => *c = Cache { state: MEGA, ..Cache::default() },
            },
            POLY => {
                let p = &mut self.poly[c.poly as usize];
                if (p.n as usize) < POLY_MAX {
                    p.e[p.n as usize] = (shape, loc, to);
                    p.n += 1;
                } else {
                    *c = Cache { state: MEGA, ..Cache::default() };
                }
            }
            _ => {}
        }
    }

    /// The index in `protos` of the prototype entry of the site for the receiver key `key` (a shape id or `K_STR`, ...).
    #[inline]
    fn proto_idx(&self, site: usize, key: u32) -> Option<usize> {
        let c = &self.caches[site];
        if c.shape == 0 && c.to == key {
            return Some(c.loc as usize);
        }
        if c.state != POLY {
            return None;
        }
        let p = &self.poly[c.poly as usize];
        p.e[..p.n as usize].iter().find(|e| e.0 == key && e.2 & PROTO != 0).map(|e| (e.2 & !PROTO) as usize)
    }

    /// Record a prototype entry for the receiver key `key` of a read site (the site is `SEEN` or has entries).
    fn add_proto(&mut self, site: usize, key: u32, hit: ProtoHit) {
        let i = self.protos.len() as u32;
        if i < PROTO >> 1 {
            self.protos.push(hit);
            self.fill(site, key, 0, PROTO | i);
        }
    }

    /// The pool entry of a `POLY` site for `shape`.
    fn poly_find(&self, site: usize, shape: u32) -> Option<(u32, u32)> {
        let c = &self.caches[site];
        if c.state != POLY {
            return None;
        }
        let p = &self.poly[c.poly as usize];
        p.e[..p.n as usize].iter().find(|e| e.0 == shape).map(|e| (e.1, e.2))
    }
}

/// The result of a property read that may need a call: the value, or the getter that the interpreter must call with the receiver.
#[derive(Clone, Copy, Debug)]
pub enum Got {
    Val(Value),
    Get(Value),
}

/// The result of a property write that may need a call: done, or the setter that the interpreter must call with the receiver and the
/// value.
#[derive(Clone, Copy, Debug)]
pub enum Put {
    Done,
    Set(Value),
    /// The write did not happen (a read-only property, an accessor with no setter, a primitive): strict code throws a TypeError.
    Fail,
}

/// What a property lookup found: a data value, or the getter and the setter of an accessor.
#[derive(Clone, Copy, Debug)]
pub enum Slot {
    Data(Value),
    Acc(Value, Value),
}

/// A property key: an array index or a name (a canonical atom of the realm).
#[derive(Clone, Copy, Debug)]
pub enum Key {
    Idx(u32),
    Name(Str),
}

impl Key {
    /// Encode a converted key in a register for a later property access.
    pub(crate) fn as_value(self) -> Value {
        match self {
            Key::Idx(i) => Realm::num(i as f64),
            Key::Name(s) => s.key_value(),
        }
    }
}

/// The kinds of error that the interpreter throws. The value is the index in `Realm::error_protos`.
#[derive(Clone, Copy, Debug)]
pub enum ErrKind {
    Type,
    Reference,
    Range,
    Syntax,
    Plain,
}

/// The realm: heap, tables, global object and prototypes.
pub struct Realm {
    pub heap: Heap,
    pub shapes: Shapes,
    pub atoms: Atoms,
    pub global: Obj,
    pub object_proto: Obj,
    pub function_proto: Obj,
    pub array_proto: Obj,
    pub(crate) array_ctor: Value,
    pub error_proto: Obj,
    pub s_object: ShapeId,
    pub s_array: ShapeId,
    pub s_func: ShapeId,
    /// The shape of an arguments object: an array with the prototype of `Object`.
    s_args: Option<ShapeId>,
    /// `%Array.prototype.values%`, the value of `Symbol.iterator` of each arguments object; made at the first one (`Realm::new_args`).
    pub(crate) args_iter: Value,
    pub k_constructor: Str,
    pub k_raw: Str,
    /// Counter of the private names (`NewPrivateName`).
    pub private_n: u32,
    /// The raw id of `s_func` and the places of its two hidden slots (the code index and the context): the call reads them with no
    /// shape lookup.
    func_shape: u32,
    loc_code: u32,
    loc_ctx: u32,
    pub k_prototype: Str,
    /// The hidden property that holds the `[[PrimitiveValue]]` of a wrapper object (`bi_wrap.rs`).
    pub k_prim: Str,
    pub k_length: Str,
    pub k_name: Str,
    pub k_message: Str,
    pub k_value_of: Str,
    pub k_to_string: Str,
    /// The `Promise` built-ins, the job queue and the rejections (`promise.rs`).
    pub(crate) pr: crate::promise::Promises,
    /// The module records and the `import()` calls that wait for the host (`module.rs`).
    pub(crate) md: crate::module::Modules,
    pub string_proto: Obj,
    pub number_proto: Obj,
    pub boolean_proto: Obj,
    /// `Symbol.prototype`, and the well-known symbols in the order of `bi_symbol::WK` (`bi_symbol.rs`). The members of `Symbol` and of
    /// the prototype are made at the first use of either (`Realm::fn_props`).
    pub symbol_proto: Obj,
    pub symbol_ctor: Option<Obj>,
    /// The lazy tables of the built-in objects (`lazy.rs`).
    pub(crate) ltab: crate::lazy::Lazy,
    pub(crate) wk: [Str; crate::bi_symbol::WK],
    /// The registry of `Symbol.for`: an object with no prototype, made at the first call. It is a root.
    pub(crate) registry: Option<Obj>,
    /// `TypeError.prototype` and the others, by `ErrKind`.
    pub error_protos: [Obj; 7],
    /// The shape of a built-in function cell: slot 0 is the index in `natives`, slots 1 to 3 are free for the function (a bound function).
    pub s_native: ShapeId,
    /// The built-ins written in Rust (`builtins.rs`).
    pub natives: Vec<Native>,
    /// The shared atom of the `name` and the `length` of each code (`Code::fn_name`, `Code::fn_len`), for `Realm::fn_props`.
    pub fn_info: Vec<(Option<Str>, u16)>,
    /// The generator prototype, the shape of an iterator result and the names of the hidden slots of a generator (`gen.rs`).
    pub(crate) genr: crate::generator::GenRealm,
    /// The index of the native that runs a bound function.
    pub(crate) bound_id: i32,
    /// The slots of the prelude (`prelude_pre.rs`): `undefined` until the first read. The first slots are the helpers that natives call
    /// through the tail channel (`bi_object.rs`).
    pub(crate) pre: [Value; crate::prelude_pre::NPRE],
    /// While a native runs: the function cell that was called, and `new.target` (`undefined` for a plain call).
    pub callee: Value,
    pub nt: Value,
    /// A native that asks the interpreter to call a function (`call`, `apply`, a bound function): the function and `this`. The
    /// arguments are in `tail_args`.
    pub tail: Option<(Value, Value)>,
    pub tail_args: Vec<Value>,
    /// Nesting of conversions that call built-ins (`toString` of an array of arrays): a cycle ends with a `RangeError`.
    pub(crate) depth: u32,
    /// The state of `Math.random` (0: not seeded yet).
    pub(crate) rng: u64,
    /// `typeof` results: undefined, object, boolean, number, string, function, symbol, bigint.
    tnames: [Str; 8],
    /// Every value that the realm keeps and the collector must mark.
    pub(crate) consts: Vec<Value>,
    /// The global lexical environment: the top-level `let`, `const` and `class` names of the scripts that ran, each with its context
    /// and slot (`DeclLex`). A global read that the global object does not answer looks here. The collector marks the names and contexts.
    pub(crate) lex: HashMap<u64, (Str, Value, u32, bool)>,
    /// Shapes that no object may have yet.
    pub(crate) pins: Vec<ShapeId>,
    /// The top frame when a collection runs: code index, frame pointer, `pc` of the safepoint.
    pub cur: (usize, usize, u32),
    /// The program that the realm runs, the code indexes of its default constructors, and the back edges that may still run (`Vm` sets
    /// the first two, and copies the third at each call from the host). A native that calls a script function needs all three.
    pub(crate) prog: Rc<Codes>,
    pub(crate) dctor: [usize; 2],
    pub(crate) fuel: u64,
    /// Machines (a JS stack and a feedback vector) that no nested call uses now, and the nesting of nested calls: `reenter.rs`.
    pub(crate) pool: Vec<Machine>,
    pub(crate) nest: u32,
    /// The state of the interface to the host (`api.rs`).
    pub(crate) api: crate::api::ApiState,
    /// The compiler of the stubs of the program, if it has any (`Op::Lazy`, in any run: `Realm::compile_stub`).
    pub lazy: Option<Box<dyn LazyCompile>>,
    epoch: u32,
    pub collections: u32,
    /// What the last collection counted.
    pub last_gc: crate::gc::Stats,
    /// `RegExp`: `None` until the first use (`regexp_rt.rs`).
    pub(crate) rx: Option<Box<crate::regexp_rt::Rx>>,
    /// `Map`, `Set`, `WeakMap` and `WeakSet` (`bi_collections.rs`).
    pub(crate) coll: Option<Box<crate::bi_collections::Colls>>,
}

const TYPEOF: [&str; 8] = ["undefined", "object", "boolean", "number", "string", "function", "symbol", "bigint"];

impl Realm {
    /// Make an arguments object with own properties for its length and iterator.
    #[inline(never)]
    pub(crate) fn new_args(&mut self, args: &[Value]) -> Value {
        if self.args_iter.is_undefined() {
            self.args_iter = self.array_values();
        }
        let shape = match self.s_args {
            Some(shape) => shape,
            None => {
                let shape = self.shapes.root(Layout::Indexed, Some(self.object_proto.cell()), 2);
                let shape = self.shapes.add(shape, self.k_length, crate::builtins::HIDDEN).expect("shape");
                let key = self.wk[crate::bi_symbol::ITERATOR];
                let shape = self.shapes.add(shape, key, crate::builtins::HIDDEN).expect("shape");
                self.pins.push(shape);
                self.s_args = Some(shape);
                shape
            }
        };
        let o = Obj::array(&mut self.heap, &self.shapes, shape, args).expect("memory");
        o.set_slot(&self.shapes, 0, Realm::num(args.len() as f64));
        o.set_slot(&self.shapes, 1, self.args_iter);
        o.set_flag(F_ARGS);
        o.value()
    }

    pub fn new(shared: Arc<SharedAtoms>) -> Realm {
        let mut heap = Heap::default();
        let mut shapes = Shapes::default();
        // The names that `typeof` gives are shared atoms, as the string literals of the code are (`LoadAtom` loads the shared atom), so a
        // comparison with a literal is a pointer test whatever the order of compile and `Realm::new`.
        let tnames = TYPEOF.map(|s| shared.intern(Chars::One(s.as_bytes())).expect("memory"));
        let mut atoms = Atoms::new(shared);
        let mut atom = |s: &str| atoms.intern(&mut heap, Chars::One(s.as_bytes())).expect("memory");
        let (k_code, k_ctx, k_prototype, k_length, k_name, k_message) = (atom("\0code"), atom("\0ctx"), atom("prototype"), atom("length"), atom("name"), atom("message"));
        let (k_home, k_fields, k_link, k_constructor, k_raw) = (atom("\0home"), atom("\0fields"), atom("\0link"), atom("constructor"), atom("raw"));
        let (k_value_of, k_to_string, k_prim) = (atom("valueOf"), atom("toString"), atom("\0prim"));
        let (k_then, k_pstate, k_presult) = (atom("then"), atom("\0pstate"), atom("\0presult"));
        let hidden = ["\0native", "\0n1", "\0n2", "\0n3"].map(&mut atom);
        let (k_value, k_done) = (atom("value"), atom("done"));
        let gkeys = ["\0gstate", "\0gframe", "\0gsent", "\0gmode", "\0gpromise", "\0gentry"].map(&mut atom);
        let empty = atom("");
        let root = shapes.root(Layout::Object, None, 4);
        let object_proto = Obj::new(&mut heap, &shapes, root).expect("memory");
        let proto = Some(object_proto.cell());
        let s_object = shapes.root(Layout::Object, proto, 4);
        let function_proto_shape = shapes.add(s_object, hidden[0], 0).expect("shape");
        let function_proto = Obj::new(&mut heap, &shapes, function_proto_shape).expect("memory");
        let array_proto_shape = shapes.root(Layout::Indexed, proto, 2);
        let array_proto = Obj::new(&mut heap, &shapes, array_proto_shape).expect("memory");
        let error_proto = Obj::new(&mut heap, &shapes, s_object).expect("memory");
        let global = Obj::new(&mut heap, &shapes, s_object).expect("memory");
        shapes.watch(global.cell().as_ptr() as usize);
        let s_array = shapes.root(Layout::Indexed, Some(array_proto.cell()), 0);
        let f = shapes.root(Layout::Object, Some(function_proto.cell()), 4);
        let f = shapes.add(f, k_code, 0).expect("shape");
        let f = shapes.add(f, k_ctx, 0).expect("shape");
        let f = shapes.add(f, k_home, 0).expect("shape");
        let f = shapes.add(f, k_fields, 0).expect("shape");
        let s_func = shapes.add(f, k_link, 0).expect("shape");
        let wrapper = shapes.add(s_object, k_prim, 0).expect("shape");
        let [string_proto, number_proto, boolean_proto] =
            [empty.value(), Value::int(0), Value::bool(false)].map(|v| Obj::new_wrapper(&mut heap, &shapes, wrapper, v));
        let symbol_proto = Obj::new(&mut heap, &shapes, s_object).expect("memory");
        let wk = crate::bi_symbol::well_known(&mut heap);
        let pr = crate::promise::Promises::new(&mut heap, &mut shapes, s_object, k_then, k_pstate, k_presult);
        // The prototypes of `TypeError` and the others inherit from `Error.prototype`.
        let eshape = shapes.root(Layout::Object, Some(error_proto.cell()), 4);
        let mut error_protos = [error_proto; 7];
        for (i, p) in error_protos.iter_mut().enumerate() {
            if i != 4 {
                *p = Obj::new(&mut heap, &shapes, eshape).expect("memory");
            }
        }
        let s_native = hidden.iter().fold(shapes.root(Layout::Object, Some(function_proto.cell()), 4), |f, k| shapes.add(f, *k, 0).expect("shape"));
        let gen_proto = Obj::new(&mut heap, &shapes, s_object).expect("memory");
        let s_iter = {
            let s = shapes.root(Layout::Object, proto, 2);
            let s = shapes.add(s, k_value, A_W | A_E | A_C).expect("shape");
            shapes.add(s, k_done, A_W | A_E | A_C).expect("shape")
        };
        let mut consts: Vec<Value> = [object_proto, function_proto, array_proto, error_proto, global, string_proto, number_proto, boolean_proto, symbol_proto].iter().map(|o| o.value()).collect();
        consts.extend(error_protos.iter().map(|o| o.value()));
        consts.extend(hidden.iter().chain(&gkeys).chain([&k_value, &k_done]).map(|s| s.value()));
        consts.push(gen_proto.value());
        consts.extend([k_prim, k_code, k_ctx, k_prototype, k_length, k_name, k_message, k_home, k_fields, k_link, k_constructor, k_raw, k_value_of, k_to_string].iter().map(|s| s.value()));
        consts.extend(tnames.iter().map(|s| s.value()));
        consts.extend(wk.iter().map(|s| s.key_value()));
        let pins = vec![s_object, s_array, s_func, s_native, s_iter, pr.shape];
        let epoch = shapes.epoch();
        let probe = Obj::new(&mut heap, &shapes, s_func).expect("memory");
        let (func_shape, loc_code, loc_ctx) = (s_func.raw(), probe.loc(&shapes, 0), probe.loc(&shapes, 1));
        assert!(loc_code >> 9 == 0 && loc_ctx >> 9 == 0, "the code and the context of a function are inline slots");
        let mut rt = Realm {
            heap,
            shapes,
            atoms,
            global,
            object_proto,
            function_proto,
            array_proto,
            array_ctor: Value::UNDEFINED,
            error_proto,
            s_object,
            s_array,
            s_func,
            s_args: None,
            args_iter: Value::UNDEFINED,
            k_constructor,
            k_raw,
            private_n: 0,
            func_shape,
            loc_code,
            loc_ctx,
            k_prototype,
            k_prim,
            k_length,
            k_name,
            k_message,
            k_value_of,
            k_to_string,
            pr,
            md: Default::default(),
            string_proto,
            number_proto,
            boolean_proto,
            symbol_proto,
            symbol_ctor: None,
            ltab: Default::default(),
            wk,
            registry: None,
            error_protos,
            s_native,
            genr: crate::generator::GenRealm { proto: gen_proto, s_iter, keys: gkeys, func_proto: None, async_proto: None, async_shape: None, ag: Default::default() },
            natives: Vec::new(),
            fn_info: Vec::new(),
            bound_id: 0,
            pre: [Value::UNDEFINED; crate::prelude_pre::NPRE],
            callee: Value::UNDEFINED,
            nt: Value::UNDEFINED,
            tail: None,
            tail_args: Vec::new(),
            depth: 0,
            rng: 0,
            tnames,
            consts,
            lex: HashMap::new(),
            pins,
            cur: (0, 0, 0),
            prog: Rc::default(),
            dctor: [0, 0],
            fuel: u64::MAX,
            pool: Vec::new(),
            nest: 0,
            api: Default::default(),
            lazy: None,
            epoch,
            collections: 0,
            last_gc: Default::default(),
            rx: None,
            coll: None,
        };
        rt.install_builtins();
        rt
    }

    /// Does an op that allocates have to collect first? See the module comment.
    #[inline(always)]
    pub fn due(&self) -> bool {
        self.heap.task_due()
    }

    /// Collect with the frames of the stack as roots. `walk_frames` marks them (see `interp.rs`); the realm adds its own.
    pub fn collect(&mut self, fbs: &mut [Feedback], walk_frames: impl FnOnce(&mut Marker)) {
        // A nested call holds the collector (`Realm::call_function`), so no machine is in use now: the idle ones keep their feedback.
        debug_assert!(self.nest == 0, "a collection inside a nested call");
        let Realm { heap, shapes, atoms, consts, pins, lex, pool, pr, api, rx, .. } = self;
        let mut walk_frames = Some(walk_frames);
        let stats = {
            // The promise state is marked in the walk; its weak list is cleared after the marking.
            struct Rr<'a, F: FnMut(&mut Marker)>(F, &'a mut crate::promise::Promises, &'a mut crate::api::ApiState);
            impl<F: FnMut(&mut Marker)> crate::gc::Roots for Rr<'_, F> {
                fn walk(&mut self, m: &mut Marker) {
                    (self.0)(m);
                    self.1.walk(m);
                    self.2.mark(m);
                }
                fn owners(&mut self, live: &crate::gc::Live, round: u32, out: &mut Vec<Value>) -> usize {
                    self.2.trace_owners(live, round, out)
                }
                fn weak(&mut self, live: &crate::gc::Live) {
                    // SAFETY: the list holds cells of this heap that the last collection did not free: a dead one is dropped here.
                    self.1.weak_list().retain(|v| unsafe { live.value(*v) });
                    self.2.prune_owners(live);
                }
            }
            let roots = |m: &mut Marker| {
                consts.iter().for_each(|v| m.value(*v));
                lex.values().for_each(|(k, c, _, _)| {
                    m.value(k.value());
                    m.value(*c);
                });
                pins.iter().for_each(|s| m.shape(*s));
                if let Some(rx) = rx {
                    rx.mark(m);
                }
                fbs.iter().for_each(|f| f.walk(m));
                pool.iter().flat_map(|p| p.fbs.iter()).for_each(|f| f.walk(m));
                if let Some(w) = walk_frames.take() {
                    w(m);
                }
            };
            pr.collected();
            collect(heap, shapes, atoms, &mut Rr(roots, pr, api))
        };
        self.last_gc = stats;
        self.collections += 1;
        // A holder may be gone: every prototype entry is stale from now on (the module comment).
        self.shapes.bump_proto_epoch();
        fbs.iter_mut().for_each(Feedback::collected);
        self.pool.iter_mut().flat_map(|p| p.fbs.iter_mut()).for_each(Feedback::collected);
        if self.shapes.epoch() != self.epoch {
            self.epoch = self.shapes.epoch();
            self.ltab.array_iter_spot = None;
            fbs.iter_mut().for_each(Feedback::clear);
            self.pool.iter_mut().flat_map(|p| p.fbs.iter_mut()).for_each(Feedback::clear);
        }
    }

    // ---- values to cells ----

    /// The object of a value. The interpreter's invariant is that every pointer in a register, a context or an object is a live cell of
    /// this heap: the collector marks them all, and a stale pointer is a bug of the roots (`roots.rs`, rules).
    #[inline]
    pub fn obj(v: Value) -> Option<Obj> {
        // SAFETY: by the invariant above.
        unsafe { Obj::from_value(v) }
    }

    #[inline]
    pub fn str_of(v: Value) -> Option<Str> {
        // SAFETY: by the invariant above.
        unsafe { Str::from_value(v) }
    }

    /// The program blob of a value, if it is one (`regexp_rt.rs`).
    #[inline]
    pub(crate) fn blob_of(v: Value) -> Option<crate::blob::Blob> {
        // SAFETY: by the invariant of `Realm::obj`.
        unsafe { crate::blob::Blob::from_value(v) }
    }

    /// The symbol of a value of kind `Symbol`.
    #[inline]
    pub fn sym_of(v: Value) -> Option<Str> {
        // SAFETY: by the invariant above.
        unsafe { Str::from_symbol(v) }
    }

    #[inline]
    pub fn ctx(v: Value) -> Option<Ctx> {
        // SAFETY: by the invariant above.
        unsafe { Ctx::from_value(v) }
    }

    // ---- errors ----

    pub fn error(&mut self, kind: ErrKind, msg: &str) -> Value {
        let m = Str::from_str(&mut self.heap, msg).expect("memory");
        self.make_error(self.error_protos[kind as usize], Some(m.value()))
    }

    pub fn type_error<T>(&mut self, msg: &str) -> R<T> {
        Err(self.error(ErrKind::Type, msg))
    }

    /// The text of an error value, for tests and for the host: `Name: message`.
    pub fn error_text(&self, v: Value) -> String {
        let Some(o) = Realm::obj(v) else { return format!("{v:?}") };
        let s = |k| match self.lookup_ro(o, k) {
            Some(Slot::Data(x)) => Realm::str_of(x).map(|s| self.text(s)).unwrap_or_default(),
            _ => String::new(),
        };
        format!("{}: {}", s(self.k_name), s(self.k_message))
    }

    /// Is `v` an object that has `Error.prototype` in its chain?
    pub fn is_error(&self, v: Value) -> bool {
        let Some(mut o) = Realm::obj(v) else { return false };
        // The prototype objects of the errors are ordinary objects.
        if o == self.error_proto || self.error_protos.contains(&o) {
            return false;
        }
        while let Some(p) = self.proto_of(o) {
            if p == self.error_proto {
                return true;
            }
            o = p;
        }
        false
    }

    // ---- strings ----

    pub fn intern(&mut self, s: &str) -> Str {
        if s.is_ascii() {
            self.atoms.intern(&mut self.heap, Chars::One(s.as_bytes())).expect("memory")
        } else {
            let u: Vec<u16> = s.encode_utf16().collect();
            self.atoms.intern(&mut self.heap, Chars::Two(&u)).expect("memory")
        }
    }

    pub fn string(&mut self, s: &str) -> Value {
        Str::from_str(&mut self.heap, s).expect("memory").value()
    }

    /// The text of a string (a rope is flattened).
    pub fn text(&self, s: Str) -> String {
        match s.resolve().chars() {
            Some(Chars::One(b)) => b.iter().map(|&c| c as char).collect(),
            Some(Chars::Two(u)) => String::from_utf16_lossy(u),
            None => "<rope>".into(),
        }
    }

    pub(crate) fn flat(&mut self, s: Str) -> Str {
        s.flatten(&mut self.heap).expect("memory")
    }

    pub fn typeof_name(&self, v: Value) -> Value {
        let i = match v.as_pointer() {
            _ if v.is_undefined() => 0,
            _ if v.is_null() => 1,
            _ if v.as_bool().is_some() => 2,
            _ if v.is_number() => 3,
            Some((Kind::String, _)) => 4,
            Some((Kind::Symbol, _)) => 6,
            Some((Kind::BigInt, _)) => 7,
            Some((Kind::Object, _)) => Realm::obj(v).map_or(1, |o| if o.is_callable() { 5 } else { 1 }),
            None => 0,
        };
        self.tnames[i].value()
    }

    // ---- conversions ----

    pub fn truthy(v: Value) -> bool {
        if let Some(b) = v.as_bool() {
            b
        } else if let Some(i) = v.as_int() {
            i != 0
        } else if let Some(f) = v.as_f64() {
            f != 0.0 && !f.is_nan()
        } else if v.is_undefined() || v.is_null() {
            false
        } else if let Some(s) = Realm::str_of(v) {
            !s.is_empty()
        } else {
            true
        }
    }

    /// `ToNumber`.
    pub fn to_number(&mut self, v: Value) -> R<f64> {
        if let Some(f) = v.as_f64() {
            return Ok(f);
        }
        if v.is_undefined() {
            return Ok(f64::NAN);
        }
        if v.is_null() {
            return Ok(0.0);
        }
        if let Some(b) = v.as_bool() {
            return Ok(b as u8 as f64);
        }
        if let Some(s) = Realm::str_of(v) {
            let s = self.flat(s);
            return Ok(crate::bi_number::string_to_number(&self.text(s)));
        }
        if Realm::obj(v).is_some() {
            let p = self.to_primitive(v, Hint::Number)?;
            return self.to_number(p);
        }
        let what = if Realm::sym_of(v).is_some() { "a Symbol value" } else { "a value" };
        self.type_error(&format!("cannot convert {what} to a number"))
    }

    pub fn to_int32(f: f64) -> i32 {
        if f.is_finite() { (f.trunc() % 4294967296.0) as i64 as i32 } else { 0 }
    }

    /// A number as a value: an integer when it is one (and not -0).
    #[inline]
    pub fn num(f: f64) -> Value {
        let i = f as i32;
        if i as f64 == f && (i != 0 || f.is_sign_positive()) { Value::int(i) } else { Value::number(f) }
    }

    pub fn num_to_string(f: f64) -> String {
        if f.is_nan() {
            "NaN".into()
        } else if f.is_infinite() {
            if f > 0.0 { "Infinity".into() } else { "-Infinity".into() }
        } else if f == 0.0 {
            "0".into()
        } else if (1e-6..1e21).contains(&f.abs()) {
            format!("{f}")
        } else {
            // The exponent form of Number::toString: the shortest digits, `n` is the place of the decimal point.
            let e = format!("{:e}", f.abs());
            let (m, x) = e.split_once('e').unwrap_or((&e, "0"));
            let digits: String = m.chars().filter(|c| *c != '.').collect();
            let n: i32 = x.parse::<i32>().unwrap_or(0) + 1;
            let (sign, ex) = (if f < 0.0 { "-" } else { "" }, n - 1);
            let es = format!("e{}{}", if ex < 0 { '-' } else { '+' }, ex.abs());
            if digits.len() == 1 { format!("{sign}{digits}{es}") } else { format!("{sign}{}.{}{es}", &digits[..1], &digits[1..]) }
        }
    }

    /// `ToString`.
    pub fn to_str(&mut self, v: Value) -> R<Str> {
        if let Some(s) = Realm::str_of(v) {
            return Ok(s);
        }
        let t = if let Some(i) = v.as_int() {
            i.to_string()
        } else if let Some(f) = v.as_f64() {
            Realm::num_to_string(f)
        } else if v.is_undefined() {
            "undefined".into()
        } else if v.is_null() {
            "null".into()
        } else if let Some(b) = v.as_bool() {
            b.to_string()
        } else if Realm::obj(v).is_some() {
            let p = self.to_primitive(v, Hint::String)?;
            return self.to_str(p);
        } else {
            let what = if Realm::sym_of(v).is_some() { "a Symbol value" } else { "a value" };
            return self.type_error(&format!("cannot convert {what} to a string"));
        };
        Ok(Str::from_str(&mut self.heap, &t).expect("memory"))
    }

    /// The property key of a value (`ToPropertyKey`).
    pub fn key_of(&mut self, v: Value) -> R<Key> {
        if let Some(i) = v.as_int().filter(|i| *i >= 0) {
            return Ok(Key::Idx(i as u32));
        }
        if let Some(f) = v.as_f64()
            && f >= 0.0
            && f < u32::MAX as f64
            && f.fract() == 0.0
        {
            return Ok(Key::Idx(f as u32));
        }
        let v = if Realm::obj(v).is_some() { self.to_primitive(v, Hint::String)? } else { v };
        if let Some(sym) = Realm::sym_of(v) {
            return Ok(Key::Name(sym));
        }
        let s = self.to_str(v)?;
        let s = self.flat(s);
        if let Some(Chars::One(b)) = s.chars()
            && !b.is_empty()
            && b.len() <= 10
            && b.iter().all(u8::is_ascii_digit)
            && (b[0] != b'0' || b.len() == 1)
            && let Ok(n) = std::str::from_utf8(b).unwrap_or("x").parse::<u32>()
            && n != u32::MAX
        {
            return Ok(Key::Idx(n));
        }
        Ok(Key::Name(self.atoms.atomize(&mut self.heap, s).expect("memory")))
    }

    /// The atom of the decimal text of an index (an integer key of a plain object).
    pub(crate) fn idx_atom(&mut self, i: u32) -> Str {
        self.intern(&i.to_string())
    }

    // ---- operators ----

    /// `a + b` when the fast path (two numbers) did not apply.
    pub fn add_slow(&mut self, mut x: Value, mut y: Value) -> R<Value> {
        if Realm::obj(x).is_some() {
            x = self.to_primitive(x, Hint::Default)?;
        }
        if Realm::obj(y).is_some() {
            y = self.to_primitive(y, Hint::Default)?;
        }
        if let (Some(a), Some(b)) = (Realm::str_of(x), Realm::str_of(y)) {
            return Ok(Str::concat(&mut self.heap, a, b).expect("memory").value());
        }
        if Realm::str_of(x).is_some() || Realm::str_of(y).is_some() {
            let (a, b) = (self.to_str(x)?, self.to_str(y)?);
            return Ok(Str::concat(&mut self.heap, a, b).expect("memory").value());
        }
        let (a, b) = (self.to_number(x)?, self.to_number(y)?);
        Ok(Value::number(a + b))
    }

    /// The abstract relational comparison: `Some(ordering)` or `None` for a NaN.
    pub fn compare(&mut self, mut x: Value, mut y: Value) -> R<Option<std::cmp::Ordering>> {
        if Realm::obj(x).is_some() {
            x = self.to_primitive(x, Hint::Number)?;
        }
        if Realm::obj(y).is_some() {
            y = self.to_primitive(y, Hint::Number)?;
        }
        if let (Some(a), Some(b)) = (Realm::str_of(x), Realm::str_of(y)) {
            let (a, b) = (self.flat(a), self.flat(b));
            let (ca, cb) = (a.chars().expect("flat"), b.chars().expect("flat"));
            return Ok(Some(ca.units().cmp(cb.units())));
        }
        let (a, b) = (self.to_number(x)?, self.to_number(y)?);
        Ok(a.partial_cmp(&b))
    }

    pub fn strict_eq(&mut self, x: Value, y: Value) -> bool {
        if x == y {
            return x.as_f64().is_none_or(|f| !f.is_nan());
        }
        if let (Some(a), Some(b)) = (x.as_f64(), y.as_f64()) {
            return a == b;
        }
        match (Realm::str_of(x), Realm::str_of(y)) {
            (Some(a), Some(b)) => Str::equals(&mut self.heap, a, b).expect("memory"),
            _ => false,
        }
    }

    /// `==`.
    pub fn loose_eq(&mut self, mut x: Value, mut y: Value) -> R<bool> {
        let nullish = |v: Value| v.is_undefined() || v.is_null();
        if nullish(x) || nullish(y) {
            return Ok(nullish(x) && nullish(y));
        }
        let (ox, oy) = (Realm::obj(x).is_some(), Realm::obj(y).is_some());
        if ox && oy {
            return Ok(x == y);
        }
        if ox {
            x = self.to_primitive(x, Hint::Default)?;
        } else if oy {
            y = self.to_primitive(y, Hint::Default)?;
        }
        if let (Some(_), Some(_)) = (Realm::str_of(x), Realm::str_of(y)) {
            return Ok(self.strict_eq(x, y));
        }
        // A symbol is equal to itself only; it never converts to a number.
        if x.as_pointer().is_some_and(|p| p.0 == Kind::Symbol) || y.as_pointer().is_some_and(|p| p.0 == Kind::Symbol) {
            return Ok(x == y);
        }
        let (a, b) = (self.to_number(x)?, self.to_number(y)?);
        Ok(a == b)
    }

    // ---- properties ----

    /// The prototype of an object as an object.
    pub(crate) fn proto_of(&self, o: Obj) -> Option<Obj> {
        o.proto(&self.shapes).and_then(|p| Realm::obj(Value::pointer(Kind::Object, p)))
    }

    /// What `o` itself holds for `key`.
    fn slot_at(&self, o: Obj, key: Str) -> Option<Slot> {
        let e = self.shapes.find(o.shape(&self.shapes), key)?;
        Some(if e.attrs() & A_ACC != 0 {
            Slot::Acc(o.slot(&self.shapes, e.slot()), o.slot(&self.shapes, e.slot() + 1))
        } else {
            Slot::Data(o.slot(&self.shapes, e.slot()))
        })
    }

    /// Look up `key` along the prototype chain: what the first object that has it holds. A name that a lazy table (`lazy.rs`) has and
    /// that is not made yet is made on the way.
    pub fn lookup_g(&mut self, mut o: Obj, key: Str) -> Option<Slot> {
        loop {
            self.lazy_prep(o, key);
            if let Some(s) = self.slot_at(o, key) {
                return Some(s);
            }
            o = self.proto_of(o)?;
        }
    }

    /// An index can be a named property, an array element, or a character of a String wrapper.
    #[cold]
    fn lookup_index_g(&mut self, mut o: Obj, index: u32) -> Option<Slot> {
        let key = self.idx_atom(index);
        loop {
            if let Some(s) = self.slot_at(o, key) {
                return Some(s);
            }
            o = self.proto_of(o)?;
            if let Some(v) = if o.is_indexed() {
                o.get_element(index)
            } else if let Some(s) = self.string_wrapper(o) {
                self.string_own(s, Key::Idx(index))
            } else {
                None
            } {
                return Some(Slot::Data(v));
            }
        }
    }

    /// An array or String wrapper can supply a length outside its shape.
    #[cold]
    fn lookup_length_g(&mut self, mut o: Obj) -> Option<Slot> {
        loop {
            match self.get_own(o, Key::Name(self.k_length)) {
                Some(crate::bi_object::Own::Data(v, _)) => return Some(Slot::Data(v)),
                Some(crate::bi_object::Own::Acc(g, s, _)) => return Some(Slot::Acc(g, s)),
                None => o = self.proto_of(o)?,
            }
        }
    }

    /// [`Realm::lookup_g`] for a name that no lazy table has (`name`, `message`), where the realm is not mutable.
    fn lookup_ro(&self, mut o: Obj, key: Str) -> Option<Slot> {
        loop {
            if let Some(s) = self.slot_at(o, key) {
                return Some(s);
            }
            o = self.proto_of(o)?;
        }
    }

    /// The value of an own or inherited data property. An accessor throws: use [`Realm::get_prop_g`] where the interpreter can call.
    pub(crate) fn lookup(&mut self, o: Obj, key: Str) -> R<Option<Value>> {
        self.fn_props(o);
        match self.lookup_g(o, key) {
            Some(Slot::Data(v)) => Ok(Some(v)),
            Some(Slot::Acc(..)) => self.type_error("accessors are not supported here (the host needs a call)"),
            None if key == self.k_prototype && self.needs_prototype(o) => self.make_prototype(o).map(Some),
            None => Ok(None),
        }
    }

    pub(crate) fn got(s: Option<Slot>) -> Got {
        match s {
            Some(Slot::Data(v)) => Got::Val(v),
            Some(Slot::Acc(g, _)) if !g.is_undefined() => Got::Get(g),
            _ => Got::Val(Value::UNDEFINED),
        }
    }

    pub(crate) fn setter(s: Value) -> Put {
        if s.is_undefined() { Put::Fail } else { Put::Set(s) }
    }

    fn undefined_read<T>(&mut self, recv: Value, name: &str) -> R<T> {
        let what = if recv.is_null() { "null" } else { "undefined" };
        self.type_error(&format!("cannot read properties of {what} (reading '{name}')"))
    }

    /// `recv[key]`; an accessor with a getter gives [`Got::Get`].
    pub fn get_prop_g(&mut self, recv: Value, key: Key) -> R<Got> {
        if let Some(o) = Realm::obj(recv) {
            if let Some(s) = self.string_wrapper(o)
                && let Some(v) = self.string_own(s, key)
            {
                return Ok(Got::Val(v));
            }
            if self.api.hs.active {
                match self.hs_walk(o, key, true)? {
                    crate::api_host::Walk::Hit(g) => return Ok(g),
                    crate::api_host::Walk::Miss => return Ok(Got::Val(Value::UNDEFINED)),
                    crate::api_host::Walk::NoHost => {}
                }
            }
            self.fn_props(o);
            return match key {
                Key::Idx(i) => {
                    if o.is_indexed()
                        && let Some(v) = o.get_element(i)
                    {
                        return Ok(Got::Val(v));
                    }
                    Ok(Self::got(self.lookup_index_g(o, i)))
                }
                Key::Name(k) => {
                    if k == self.k_length {
                        if self.is_array(o) {
                            return Ok(Got::Val(Realm::num(o.len() as f64)));
                        }
                        return Ok(Self::got(self.lookup_length_g(o)));
                    }
                    self.fn_prep(o, k);
                    match self.lookup_g(o, k) {
                        None if k == self.k_prototype && self.needs_prototype(o) => self.make_prototype(o).map(Got::Val),
                        s => Ok(Self::got(s)),
                    }
                }
            };
        }
        if let Some(s) = Realm::str_of(recv) {
            let s = self.flat(s);
            return Ok(match key {
                Key::Name(k) if k == self.k_length => Got::Val(Realm::num(s.len() as f64)),
                Key::Idx(i) => Got::Val(match s.unit_at(i as usize) {
                    Some(u) => {
                        let t = String::from_utf16_lossy(&[u]);
                        self.string(&t)
                    }
                    None => Value::UNDEFINED,
                }),
                Key::Name(k) => Self::got(self.lookup_g(self.string_proto, k)),
            });
        }
        if recv.is_number() || recv.as_bool().is_some() {
            let proto = if recv.is_number() { self.number_proto } else { self.boolean_proto };
            return Ok(match key {
                Key::Name(k) => Self::got(self.lookup_g(proto, k)),
                Key::Idx(_) => Got::Val(Value::UNDEFINED),
            });
        }
        if recv.as_pointer().is_some_and(|p| p.0 == Kind::Symbol) {
            self.fn_props(self.symbol_proto);
            return Ok(match key {
                Key::Name(k) => Self::got(self.lookup_g(self.symbol_proto, k)),
                Key::Idx(_) => Got::Val(Value::UNDEFINED),
            });
        }
        if recv.is_undefined() || recv.is_null() {
            let name = match key {
                Key::Idx(i) => i.to_string(),
                Key::Name(k) => self.text(k),
            };
            return self.undefined_read(recv, &name);
        }
        Ok(Got::Val(Value::UNDEFINED))
    }

    /// `recv[key]` for Rust code: a getter runs through [`Realm::call_function`].
    pub fn get_prop(&mut self, recv: Value, key: Key) -> R<Value> {
        match self.get_prop_g(recv, key)? {
            Got::Val(v) => Ok(v),
            Got::Get(g) => self.call_function(g, recv, &[]),
        }
    }

    /// `recv[key] = v`. A write to a primitive does nothing. A setter gives [`Put::Set`]. A write that the object refuses (not writable,
    /// not extensible) gives [`Put::Fail`]: sloppy code goes on, strict code throws. [`Realm::set_checked`] throws always.
    pub fn set_prop_g(&mut self, recv: Value, key: Key, v: Value) -> R<Put> {
        let Some(o) = Realm::obj(recv) else {
            if recv.is_undefined() || recv.is_null() {
                return self.type_error("cannot set properties of undefined or null");
            }
            return Ok(Put::Fail);
        };
        if self.api.hs.active && o.has_flag(F_HOST) && let Some(ok) = self.hs_write(o, key, crate::api_host::HsWrite::Set(v))? {
            return Ok(if ok { Put::Done } else { Put::Fail });
        }
        if self.has_string_own(o, key) {
            return Ok(Put::Fail);
        }
        self.fn_props(o);
        match key {
            Key::Idx(i) if self.shapes.get(o.shape(&self.shapes)).layout == Layout::Indexed && !o.has_flag(F_SLOWIDX) => {
                if o.get_element(i).is_some() {
                    if o.has_flag(F_ELEMS_RO) {
                        return Ok(Put::Fail);
                    }
                } else {
                    if let Some(put) = self.index_proto_setter(o, i) {
                        return Ok(put);
                    }
                    if o.has_flag(F_NOEXT) || (o.has_flag(F_LEN_RO) && i >= o.len()) {
                        return Ok(Put::Fail);
                    }
                }
                if o.set_element(&mut self.heap, i, v).is_none() {
                    return Err(self.error(ErrKind::Range, "invalid array length"));
                }
                Ok(Put::Done)
            }
            Key::Idx(i) => {
                let k = self.idx_atom(i);
                if self.store_entry(o, k)?.is_some() {
                    return self.set_named(o, k, v);
                }
                if let Some(put) = self.index_proto_setter(o, i) {
                    return Ok(put);
                }
                if !o.extensible() || (o.is_indexed() && o.has_flag(F_LEN_RO) && i >= o.len()) {
                    return Ok(Put::Fail);
                }
                self.add_named(o, k, v)?;
                if o.is_indexed() && i >= o.len() {
                    o.set_len(&mut self.heap, i + 1).expect("memory");
                }
                Ok(Put::Done)
            }
            Key::Name(k) => {
                if k == self.k_length && self.is_array(o) {
                    if o.has_flag(F_LEN_RO) {
                        return Ok(Put::Fail);
                    }
                    let d = crate::bi_object::Desc { value: Some(v), ..Default::default() };
                    return Ok(if self.define_length(o, &d)? { Put::Done } else { Put::Fail });
                }
                self.set_named(o, k, v)
            }
        }
    }

    /// `recv[key] = v` for Rust code: a setter runs through [`Realm::call_function`].
    pub fn set_prop(&mut self, recv: Value, key: Key, v: Value) -> R<()> {
        match self.set_prop_g(recv, key, v)? {
            Put::Done | Put::Fail => Ok(()),
            Put::Set(f) => self.call_function(f, recv, &[v]).map(|_| ()),
        }
    }

    /// An inherited index can supply a setter or prevent creation of an own element.
    #[cold]
    fn index_proto_setter(&mut self, o: Obj, i: u32) -> Option<Put> {
        let mut proto = self.proto_of(o);
        while let Some(p) = proto {
            match self.get_own(p, Key::Idx(i)) {
                Some(crate::bi_object::Own::Acc(_, set, _)) => return Some(Self::setter(set)),
                Some(crate::bi_object::Own::Data(_, attrs)) => return (attrs & A_W == 0).then_some(Put::Fail),
                None => proto = self.proto_of(p),
            }
        }
        None
    }

    /// Find the descriptor for a store. Ask the host for a lazy member before treating the key as absent.
    fn store_entry(&mut self, o: Obj, k: Str) -> R<Option<Entry>> {
        let mut entry = self.shapes.find(o.shape(&self.shapes), k);
        if entry.is_none() && o.has_flag(F_LAZYM) && self.hs_lazy(o, Some(Key::Name(k)))? {
            entry = self.shapes.find(o.shape(&self.shapes), k);
        }
        Ok(entry)
    }

    /// A setter or a rejected write from the first descriptor on the prototype chain.
    fn proto_setter(&mut self, o: Obj, k: Str) -> R<Option<Put>> {
        let mut proto = self.proto_of(o);
        while let Some(p) = proto {
            if k == self.k_length {
                if self.is_array(p) {
                    return Ok(p.has_flag(F_LEN_RO).then_some(Put::Fail));
                }
                if self.has_string_own(p, Key::Name(k)) {
                    return Ok(Some(Put::Fail));
                }
            }
            self.lazy_prep(p, k);
            if let Some(e) = self.store_entry(p, k)? {
                // An accessor: its setter. A data property that is not writable: nothing is written (`Put::Fail`).
                return Ok(if e.attrs() & A_ACC != 0 {
                    Some(Self::setter(p.slot(&self.shapes, e.slot() + 1)))
                } else if e.attrs() & A_W == 0 {
                    Some(Put::Fail)
                } else {
                    None
                });
            }
            proto = self.proto_of(p);
        }
        Ok(None)
    }

    /// Set an own named property: write it if it is there and writable, else call the setter that a prototype gives, else add it (when
    /// the object is extensible).
    fn set_named(&mut self, o: Obj, k: Str, v: Value) -> R<Put> {
        self.fn_prep(o, k);
        match self.store_entry(o, k)? {
            Some(e) if e.attrs() & A_ACC != 0 => Ok(Self::setter(o.slot(&self.shapes, e.slot() + 1))),
            Some(e) => {
                if e.attrs() & A_W == 0 {
                    return Ok(Put::Fail);
                }
                o.set_slot(&self.shapes, e.slot(), v);
                Ok(Put::Done)
            }
            None => {
                if let Some(p) = self.proto_setter(o, k)? {
                    return Ok(p);
                }
                if !o.extensible() {
                    return Ok(Put::Fail);
                }
                self.add_named(o, k, v).map(|_| Put::Done)
            }
        }
    }

    pub(crate) fn add_named(&mut self, o: Obj, k: Str, v: Value) -> R<u32> {
        match o.add(&mut self.heap, &mut self.shapes, k, A_W | A_E | A_C, &[v]) {
            Some(slot) => Ok(slot),
            None => Err(self.error(ErrKind::Range, "out of memory")),
        }
    }

    /// `recv.name` at a site, with the cache work of the module comment. The loop tried the hit already.
    pub fn get_field_slow(&mut self, fb: &mut Feedback, site: usize, name: Str, recv: Value) -> R<Got> {
        let Some(o) = Realm::obj(recv) else {
            // The `length` of a string is no property of the prototype chain.
            return if name == self.k_length { self.get_prop_g(recv, Key::Name(name)) } else { self.get_prim_field(fb, site, name, recv) };
        };
        self.get_obj_field(fb, site, name, o, recv)
    }

    /// `get_field_slow` for an object.
    #[inline(never)]
    fn get_obj_field(&mut self, fb: &mut Feedback, site: usize, name: Str, o: Obj, recv: Value) -> R<Got> {
        if o.has_flag(F_HOST) {
            return self.get_prop_g(recv, Key::Name(name));
        }
        self.lazy_prep(o, name);
        let id = o.shape(&self.shapes);
        if fb.caches[site].state == POLY
            && let Some((loc, 0)) = fb.poly_find(site, id.raw())
        {
            // SAFETY: the pool entry was made by `Obj::loc` for this shape id, and a cache holds tree shapes only.
            return Ok(Got::Val(unsafe { o.read_loc(loc) }));
        }
        if let Some(v) = self.proto_probe(fb, site, id.raw(), name, self.proto_of(o)) {
            return Ok(Got::Val(v));
        }
        if fb.caches[site].state != MEGA
            && self.shapes.cacheable(id)
            && let Some(e) = self.shapes.find(id, name)
            && e.attrs() & A_ACC == 0
        {
            let loc = o.loc(&self.shapes, e.slot());
            fb.fill(site, id.raw(), loc, 0);
            return Ok(Got::Val(o.slot(&self.shapes, e.slot())));
        }
        let was = fb.caches[site].state;
        if was == FRESH {
            fb.caches[site].state = SEEN;
        }
        let got = self.get_prop_g(recv, Key::Name(name))?;
        if was != FRESH && was != MEGA && matches!(got, Got::Val(_)) {
            self.fill_proto(fb, site, o, name);
        }
        Ok(got)
    }

    /// `recv.name` for a string, a number or a boolean: the prototype entry of the site, else the plain read.
    fn get_prim_field(&mut self, fb: &mut Feedback, site: usize, name: Str, recv: Value) -> R<Got> {
        let (key, proto) = if Realm::str_of(recv).is_some() {
            (K_STR, self.string_proto)
        } else if recv.is_number() {
            (K_NUM, self.number_proto)
        } else if recv.as_bool().is_some() {
            (K_BOOL, self.boolean_proto)
        } else {
            return self.get_prop_g(recv, Key::Name(name));
        };
        if let Some(v) = self.proto_probe(fb, site, key, name, Some(proto)) {
            return Ok(Got::Val(v));
        }
        let was = fb.caches[site].state;
        if was == FRESH {
            fb.caches[site].state = SEEN;
        }
        if was != FRESH && was != MEGA && let Some((holder, loc)) = self.proto_target(Some(proto), name) {
            let hit = ProtoHit { holder, loc, epoch: self.shapes.proto_epoch(), refills: 0 };
            fb.add_proto(site, key, hit);
            // SAFETY: `loc` was made by `Obj::loc_now` just now, and nothing ran in between.
            return Ok(Got::Val(unsafe { holder.read_loc(loc) }));
        }
        self.get_prop_g(recv, Key::Name(name))
    }

    /// The data property `name` that the chain from `start` gives: the holder and the place of the slot. It marks the objects that the
    /// walk passed with `F_PROTO` (so a change of one of them bumps `Shapes::proto_epoch`). `None` when no object has the key, or the
    /// first one that has it holds an accessor.
    fn proto_target(&mut self, start: Option<Obj>, name: Str) -> Option<(Obj, u32)> {
        let start = start?;
        let mut o = start;
        let (holder, slot) = loop {
            if name == self.k_length && (o.is_indexed() || o.is_kind(K_WRAP)) {
                return None;
            }
            self.lazy_prep(o, name);
            if let Some(e) = self.shapes.find(o.shape(&self.shapes), name) {
                if e.attrs() & A_ACC != 0 {
                    return None;
                }
                break (o, e.slot());
            }
            o = self.proto_of(o)?;
        };
        let mut p = start;
        loop {
            if !p.has_flag(F_PROTO) {
                p.set_flag(F_PROTO);
            }
            if p == holder {
                break;
            }
            p = self.proto_of(p)?;
        }
        Some((holder, holder.loc_now(&self.shapes, slot)))
    }

    /// The value of the prototype entry of the site for the receiver key `key`, made again if its epoch is stale. `None`: no entry, or
    /// the entry could not be made again (the site goes to `MEGA`).
    #[inline]
    fn proto_probe(&mut self, fb: &mut Feedback, site: usize, key: u32, name: Str, start: Option<Obj>) -> Option<Value> {
        let i = fb.proto_idx(site, key)?;
        let h = fb.protos[i];
        let now = self.shapes.proto_epoch();
        if h.epoch == now {
            // SAFETY: the epoch is current, so no object changed and no collection ran since `loc` was made for `holder`.
            return Some(unsafe { h.holder.read_loc(h.loc) });
        }
        if h.refills < PROTO_REFILLS
            && let Some((holder, loc)) = self.proto_target(start, name)
        {
            fb.protos[i] = ProtoHit { holder, loc, epoch: now, refills: h.refills + 1 };
            // SAFETY: as above, for the entry just made.
            return Some(unsafe { holder.read_loc(loc) });
        }
        fb.caches[site] = Cache { state: MEGA, ..Cache::default() };
        None
    }

    /// After a read of `o.name` that the own-property entries did not answer: make a prototype entry when the property comes from the
    /// prototype chain of the receiver.
    fn fill_proto(&mut self, fb: &mut Feedback, site: usize, o: Obj, name: Str) {
        // `prototype` of a function, `length` of an array and of a wrapper, and the indexes of a wrapper are not in the shape or the chain only.
        if name == self.k_prototype || o.is_kind(K_WRAP) {
            return;
        }
        // The shape after the read: the read may have given a function its own properties.
        let id = o.shape(&self.shapes);
        if !self.shapes.cacheable(id) || self.shapes.find(id, name).is_some() || (name == self.k_length && self.shapes.get(id).layout == Layout::Indexed) {
            return;
        }
        if let Some((holder, loc)) = self.proto_target(self.proto_of(o), name) {
            fb.add_proto(site, id.raw(), ProtoHit { holder, loc, epoch: self.shapes.proto_epoch(), refills: 0 });
        }
    }

    /// `GetGlobal`: the property of the global object, or a `ReferenceError` when it is not there.

    /// Make the entry of the global site `site` for the own data property in slot `slot` of the global object. The first run only marks
    /// the site (as for a property site), the second makes the entry.
    fn fill_global(&mut self, fb: &mut Feedback, site: usize, slot: u32) {
        let c = &mut fb.caches[site];
        if c.state == FRESH {
            c.state = SEEN;
        } else {
            *c = Cache::global_entry(self.shapes.watch_tag(), self.global.slot_addr(&self.shapes, slot));
        }
    }

    /// `GetGlobal`: the property of the global object, or a `ReferenceError` when it is not there. A global lexical binding comes first and
    /// has no entry (`declare_lex` changes the tag, so an entry for a property that a binding now hides does not match). An own data
    /// property of a tree shape gets a cache entry (the address of its slot, valid while the tag of the global object stays); an accessor
    /// and the rest are looked up each time.
    pub fn get_global_slow(&mut self, fb: &mut Feedback, site: usize, name: Str) -> R<Got> {
        if let Some(v) = self.lex_get(name) {
            return Ok(Got::Val(v?));
        }
        let o = self.global;
        let id = o.shape(&self.shapes);
        if self.shapes.cacheable(id)
            && let Some(e) = self.shapes.find(id, name)
            && e.attrs() & A_ACC == 0
        {
            self.fill_global(fb, site, e.slot());
            return Ok(Got::Val(o.slot(&self.shapes, e.slot())));
        }
        if self.is_lazy_regexp(name) {
            self.rx();
            return self.get_global_slow(fb, site, name);
        }
        let g = o.value();
        let got = self.get_prop_g(g, Key::Name(name))?;
        if let Got::Val(v) = got
            && v.is_undefined()
            && !self.has_prop(g, Key::Name(name))?
        {
            let n = self.text(name);
            return Err(self.error(ErrKind::Reference, &format!("{n} is not defined")));
        }
        Ok(got)
    }

    /// `SetGlobal` (`strict` false) and `SetGlobalS`: a global lexical binding first; then a writable own data property of a tree shape
    /// gets a cache entry as in `get_global_slow`. In strict code a name that is not there is a `ReferenceError`. An add, a setter and
    /// the rest are `set_prop_g`.
    pub fn set_global_slow(&mut self, fb: &mut Feedback, site: usize, name: Str, v: Value, strict: bool) -> R<Put> {
        if let Some(res) = self.lex_put(name, v) {
            res?;
            return Ok(Put::Done);
        }
        let o = self.global;
        let id = o.shape(&self.shapes);
        if self.shapes.cacheable(id)
            && let Some(e) = self.shapes.find(id, name)
            && e.attrs() & (A_ACC | A_W) == A_W
        {
            o.set_slot(&self.shapes, e.slot(), v);
            self.fill_global(fb, site, e.slot());
            return Ok(Put::Done);
        }
        if strict && !self.has_prop(o.value(), Key::Name(name))? {
            let t = self.text(name);
            return Err(self.error(ErrKind::Reference, &format!("{t} is not defined")));
        }
        self.set_prop_g(o.value(), Key::Name(name), v)
    }

    /// `recv.name = v` at a site.
    pub fn set_field_slow(&mut self, fb: &mut Feedback, site: usize, name: Str, recv: Value, v: Value) -> R<Put> {
        let Some(o) = Realm::obj(recv) else { return self.set_prop_g(recv, Key::Name(name), v) };
        if o.has_flag(F_HOST) || (name == self.k_length && o.is_kind(K_WRAP)) {
            return self.set_prop_g(recv, Key::Name(name), v);
        }
        self.fn_prep(o, name);
        self.fn_props(o);
        let id = o.shape(&self.shapes);
        if fb.caches[site].state == POLY
            && let Some((loc, to)) = fb.poly_find(site, id.raw())
        {
            // SAFETY: as in `get_field_slow`; an add entry was made for an object with room in its inline words. `set_shape_id` refuses
            // an object that a prototype entry depends on; the write of the slot is harmless then (the add below writes it again).
            unsafe {
                o.write_loc(loc, v);
                if to == 0 || o.set_shape_id(to) {
                    return Ok(Put::Done);
                }
            }
        }
        let layout_ok = self.shapes.cacheable(id) && fb.caches[site].state != MEGA && self.shapes.get(id).layout == Layout::Object;
        match self.shapes.find(id, name) {
            Some(e) if e.attrs() & A_ACC != 0 => Ok(Self::setter(o.slot(&self.shapes, e.slot() + 1))),
            Some(e) => {
                if e.attrs() & A_W == 0 {
                    return Ok(Put::Fail);
                }
                if layout_ok {
                    let loc = o.loc(&self.shapes, e.slot());
                    fb.fill(site, id.raw(), loc, 0);
                } else if fb.caches[site].state == FRESH {
                    fb.caches[site].state = SEEN;
                }
                o.set_slot(&self.shapes, e.slot(), v);
                Ok(Put::Done)
            }
            None => {
                if o.has_flag(F_LAZYM) || (name == self.k_length && self.shapes.get(id).layout == Layout::Indexed) {
                    return self.set_prop_g(recv, Key::Name(name), v);
                }
                if let Some(p) = self.proto_setter(o, name)? {
                    return Ok(p);
                }
                if !o.extensible() {
                    return Ok(Put::Fail);
                }
                let cache_add = layout_ok && o.shape(&self.shapes) == id;
                let slot = self.add_named(o, name, v)?;
                let new = o.shape(&self.shapes);
                // The global object gets no add entry: a hit would change its shape with no change of its tag (`Shapes::watch_tag`).
                if cache_add && self.shapes.cacheable(new) && o != self.global {
                    let loc = o.loc(&self.shapes, slot);
                    // Only an inline slot: the hit then needs no allocation.
                    if loc >> 9 == 0 {
                        fb.fill(site, id.raw(), loc, new.raw());
                    }
                } else if fb.caches[site].state == FRESH {
                    fb.caches[site].state = SEEN;
                }
                Ok(Put::Done)
            }
        }
    }

    /// Define the own data property `key` of `o` (an object literal or a computed key): add it, or overwrite it.
    pub fn define(&mut self, o: Obj, key: Key, v: Value) -> R<()> {
        self.fn_props(o);
        if o.has_flag(F_NOEXT | F_SLOWIDX | F_ELEMS_RO | F_ELEMS_FIXED | F_LEN_RO) {
            let d = crate::bi_object::Desc { value: Some(v), w: Some(true), e: Some(true), c: Some(true), ..Default::default() };
            return if self.define_own(o, key, &d)? { Ok(()) } else { self.type_error("cannot define the property") };
        }
        match key {
            Key::Idx(i) if self.shapes.get(o.shape(&self.shapes)).layout == Layout::Indexed => {
                if o.set_element(&mut self.heap, i, v).is_none() {
                    return Err(self.error(ErrKind::Range, "invalid array length"));
                }
                Ok(())
            }
            Key::Idx(i) => {
                let k = self.idx_atom(i);
                self.define(o, Key::Name(k), v)
            }
            Key::Name(k) => {
                let id = o.shape(&self.shapes);
                match self.shapes.find(id, k) {
                    Some(e) if e.attrs() & A_C == 0 => self.type_error("cannot redefine a property that is not configurable"),
                    Some(e) if e.attrs() & A_ACC == 0 => {
                        o.set_slot(&self.shapes, e.slot(), v);
                        Ok(())
                    }
                    Some(_) => {
                        self.reconfigure(o, k, A_W | A_E | A_C, &[v])?;
                        Ok(())
                    }
                    None => self.add_named(o, k, v).map(|_| ()),
                }
            }
        }
    }

    pub fn has_prop(&mut self, recv: Value, key: Key) -> R<bool> {
        let Some(mut o) = Realm::obj(recv) else { return self.type_error("cannot use 'in' on a primitive") };
        if self.api.hs.active {
            match self.hs_walk(o, key, false)? {
                crate::api_host::Walk::Hit(_) => return Ok(true),
                crate::api_host::Walk::Miss => return Ok(false),
                crate::api_host::Walk::NoHost => {}
            }
        }
        loop {
            self.fn_props(o);
            let found = match key {
                Key::Idx(i) => (o.is_indexed() && o.get_element(i).is_some()) || {
                    let k = self.idx_atom(i);
                    self.shapes.find(o.shape(&self.shapes), k).is_some()
                },
                Key::Name(k) => {
                    self.fn_prep(o, k);
                    self.shapes.find(o.shape(&self.shapes), k).is_some() || (k == self.k_length && self.is_array(o))
                }
            };
            if found || self.has_string_own(o, key) {
                return Ok(true);
            }
            match self.proto_of(o) {
                Some(p) => o = p,
                None => return Ok(false),
            }
        }
    }

    pub fn delete_prop(&mut self, recv: Value, key: Key) -> R<bool> {
        let Some(o) = Realm::obj(recv) else { return Ok(true) };
        if self.api.hs.active && o.has_flag(F_HOST) && let Some(ok) = self.hs_write(o, key, crate::api_host::HsWrite::Delete)? {
            return Ok(ok);
        }
        if self.has_string_own(o, key) {
            return Ok(false);
        }
        self.fn_props(o);
        match key {
            Key::Idx(i) if self.shapes.get(o.shape(&self.shapes)).layout == Layout::Indexed && !o.has_flag(F_SLOWIDX) => {
                if o.has_flag(F_ELEMS_FIXED) && o.get_element(i).is_some() {
                    return Ok(false);
                }
                o.delete_element(i);
                Ok(true)
            }
            Key::Idx(i) => {
                let k = self.idx_atom(i);
                self.delete_named(o, k)
            }
            Key::Name(k) => {
                if k == self.k_length && self.is_array(o) {
                    return Ok(false);
                }
                self.fn_prep(o, k);
                self.delete_named(o, k)
            }
        }
    }

    fn delete_named(&mut self, o: Obj, k: Str) -> R<bool> {
        if let Some(e) = self.shapes.find(o.shape(&self.shapes), k)
            && e.attrs() & A_C == 0
        {
            return Ok(false);
        }
        o.remove(&mut self.shapes, k);
        Ok(true)
    }

    /// `x instanceof f`: `f[Symbol.hasInstance](x)` if `f` has it, else the prototype chain of `x` holds the `prototype` property of `f`.
    /// `Function.prototype[Symbol.hasInstance]` is the ordinary test, so the common case skips the call.
    pub fn instance_of(&mut self, x: Value, f: Value) -> R<bool> {
        let Some(fo) = Realm::obj(f) else { return self.type_error("the right side of instanceof is not an object") };
        self.fn_props(fo);
        match self.lookup_g(fo, self.wk[crate::bi_symbol::HAS_INSTANCE]) {
            None => {}
            Some(Slot::Data(h)) if self.native_id(h).is_some_and(|i| self.natives[i].name == crate::bi_symbol::HAS_INSTANCE_NAME) => {}
            Some(s) => {
                let h = match Self::got(Some(s)) {
                    Got::Val(h) => h,
                    Got::Get(g) => self.call_function(g, f, &[])?,
                };
                if h.is_undefined() || h.is_null() {
                    if !fo.is_callable() {
                        return self.type_error("the right side of instanceof is not callable");
                    }
                    return self.ordinary_has_instance(fo, x);
                }
                if !Realm::obj(h).is_some_and(|h| h.is_callable()) {
                    return self.type_error("Symbol.hasInstance is not a function");
                }
                let r = self.call_function(h, f, &[x])?;
                return Ok(Realm::truthy(r));
            }
        }
        if !fo.is_callable() {
            return self.type_error("the right side of instanceof is not callable");
        }
        self.ordinary_has_instance(fo, x)
    }

    /// `OrdinaryHasInstance(fo, x)`.
    pub(crate) fn ordinary_has_instance(&mut self, fo: Obj, x: Value) -> R<bool> {
        if !fo.is_callable() {
            return Ok(false);
        }
        // A bound function asks its target (`BoundTargetFunction`).
        if self.native_id(fo.value()) == Some(self.bound_id as usize) {
            let target = fo.slot(&self.shapes, 1);
            return self.instance_of(x, target);
        }
        let Some(mut o) = Realm::obj(x) else { return Ok(false) };
        let p = self.get_prop(fo.value(), Key::Name(self.k_prototype))?;
        let Some(po) = Realm::obj(p) else { return self.type_error("function has non-object prototype in instanceof check") };
        while let Some(n) = self.proto_of(o) {
            if n == po {
                return Ok(true);
            }
            o = n;
        }
        Ok(false)
    }

    /// The own enumerable string keys of an object, as an array of strings (the keys of `for-in` and `Object.keys`), in the order of the
    /// specification: array indexes ascending, then the names as they were added.
    pub fn own_keys(&mut self, recv: Value) -> R<Value> {
        let keys: Vec<Value> = if let Some(o) = Realm::obj(recv) {
            self.own_key_list_h(o, true)?
        } else if let Some(s) = Realm::str_of(recv) {
            (0..s.len()).map(|i| self.string(&i.to_string())).collect()
        } else {
            // `for (k in undefined)` has no keys.
            Vec::new()
        };
        Ok(Obj::array(&mut self.heap, &self.shapes, self.s_array, &keys).expect("memory").value())
    }

    // ---- function cells ----

    /// A function cell for code `code` in context `ctx` (`undefined` for none).
    pub fn new_function(&mut self, code: usize, ctx: Value) -> Value {
        let f = Obj::new(&mut self.heap, &self.shapes, self.s_func).expect("memory");
        f.set_slot(&self.shapes, 0, Value::int(code as i32));
        f.set_slot(&self.shapes, 1, ctx);
        f.set_callable();
        f.set_flag(F_LAZY);
        f.value()
    }

    /// Give a function cell its own `length` and `name` now (not writable, not enumerable, configurable). It runs before any look at the
    /// own properties of the cell; a cell that nothing inspects never pays for them.
    #[inline]
    pub(crate) fn fn_props(&mut self, o: Obj) {
        if o.has_flag(F_LAZY) {
            self.fn_props_slow(o);
        }
    }

    #[cold]
    fn fn_props_slow(&mut self, o: Obj) {
        // The flag also marks `Symbol` and `Symbol.prototype`, which make their members at the first read (`bi_symbol.rs`).
        if o == self.symbol_proto || Some(o) == self.symbol_ctor {
            return self.symbol_members();
        }
        o.clear_flag(F_LAZY);
        let (name, len) = o.slot(&self.shapes, 0).as_int().and_then(|c| self.fn_info.get(c as usize).copied()).unwrap_or((None, 0));
        let name = match name {
            Some(s) => self.atoms.adopt(s),
            None => self.intern(""),
        };
        self.define_attrs(o, Key::Name(self.k_length), A_C, Value::int(len as i32)).expect("memory");
        self.define_attrs(o, Key::Name(self.k_name), A_C, name.value()).expect("memory");
        // A generator function has its own `prototype` (a plain function makes it at the first read of it).
        if o.slot(&self.shapes, 0).as_int().is_some_and(|c| matches!(self.prog[c as usize].gen_kind, crate::bytecode::GK_GEN | crate::bytecode::GK_ASYNCGEN)) {
            self.make_prototype(o).expect("memory");
        }
    }

    /// `SetFunctionName(f, key, prefix)` of a computed key: `prefix` 1 is `get `, 2 is `set `.
    pub fn name_function(&mut self, f: Value, key: Key, prefix: u32) -> R<()> {
        let Some(o) = Realm::obj(f).filter(|o| o.is_callable()) else { return Ok(()) };
        self.fn_props(o);
        let mut t = match key {
            Key::Idx(i) => i.to_string(),
            // The name of a function with a symbol key is the description in brackets, or empty for a symbol with none.
            Key::Name(k) if k.is_symbol() => if k.symbol_has_no_desc() { String::new() } else { format!("[{}]", self.text(k)) },
            Key::Name(k) => self.text(k),
        };
        if prefix != 0 {
            t.insert_str(0, if prefix == 1 { "get " } else { "set " });
        }
        let n = self.intern(&t);
        self.define_attrs(o, Key::Name(self.k_name), A_C, n.value())
    }

    /// The code index and the context of a function cell; `None` when the value is not callable.
    #[inline]
    pub fn function(&self, v: Value) -> Option<(usize, Value)> {
        // SAFETY: by the invariant of `Realm::obj`.
        if let Some((o, shape)) = unsafe { Obj::from_value_shape(v) }
            && shape == self.func_shape
        {
            // SAFETY: an object of the shape `s_func` has its two slots at these places (found in `new`).
            let (code, ctx) = unsafe { (o.read_inline(self.loc_code), o.read_inline(self.loc_ctx)) };
            return Some((code.as_int()? as usize, ctx));
        }
        let f = Realm::obj(v).filter(|o| o.is_callable() && !o.has_flag(F_CLASS | F_NATIVE))?;
        Some((f.slot(&self.shapes, 0).as_int()? as usize, f.slot(&self.shapes, 1)))
    }

    /// As [`Realm::function`], for a construct call: a class constructor is allowed.
    pub fn function_new(&self, v: Value) -> Option<(usize, Value)> {
        let f = Realm::obj(v).filter(|o| o.is_callable() && !o.has_flag(F_NATIVE))?;
        Some((f.slot(&self.shapes, 0).as_int()? as usize, f.slot(&self.shapes, 1)))
    }

    /// The fields function of the class constructor `f`: its code index, its context and the function itself. `None` when it has none.
    pub fn fields_of(&self, f: Value) -> Option<(usize, Value, Value)> {
        let o = Realm::obj(f).filter(|o| o.has_flag(F_CLASS))?;
        let ff = o.slot(&self.shapes, 3);
        let (ci, ctx) = self.function_new(ff)?;
        Some((ci, ctx, ff))
    }

    /// The object that `New` makes for constructor `f`: its prototype is `f.prototype` if that is an object.
    pub fn new_this(&mut self, f: Value) -> R<Value> {
        let proto = Realm::obj(f).and_then(|fo| self.lookup(fo, self.k_prototype).ok().flatten()).and_then(Realm::obj);
        let shape = match proto {
            Some(p) => self.shapes.root(Layout::Object, Some(p.cell()), 4),
            None => self.s_object,
        };
        Ok(Obj::new(&mut self.heap, &self.shapes, shape).expect("memory").value())
    }

    /// Add the own property `name` to `o` from Rust (tests and the host).
    pub fn set_own(&mut self, o: Obj, name: &str, v: Value) {
        let k = self.intern(name);
        self.define(o, Key::Name(k), v).expect("define");
    }
}
