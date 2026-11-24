//! Structured clone: the record format, the cost model and the host trait (`docs/internal/design-host-clone.md`, plan step 2).
//!
//! An engine walks plain data itself and writes a [`Cloned`] with a [`RecordWriter`]. It reads one back with a [`RecordReader`] and
//! builds the objects. The host sees only host objects, the transfer list and the error ([`CloneHost`]). Both engines share this
//! module, so they share one format, one cost model and one set of limits. Each engine has only its own walk.
//!
//! # Format
//!
//! The record lives in memory only and is never stored, so it has no version. The browser never reads the bytes. Every integer is a
//! LEB128 varint. A signed `Int` is zigzag. A number that is not an `i32` or is `-0` is 8 bytes, little endian. A value is a tag byte
//! and a payload (see [`Tag`]).
//!
//! An object takes the next id when its tag is read, before its children, so a cycle works ([`Tag::Ref`] names an id). A view takes its
//! id after its buffer ([`RecordWriter::view_done`]). A property list ends with [`Tag::End`]. A key is [`Tag::KNew8`] or
//! [`Tag::KNew16`] (the next number of the key table), [`Tag::KRef`] (an earlier number) or [`Tag::KIdx`] (an array index). A repeated
//! key is one or two bytes, and a dense array has no keys.
//!
//! # Cost model
//!
//! [`Meter`] counts what the browser counted before the walk moved into the engine: the byte cost, the nodes, the depth and the steps.
//! The totals are the same. An engine can add the charges of an object at once.

use crate::{Abort, Engine, Error, Local, Scope};
use std::any::Any;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// Limits of one walk, and the names of their [`Abort::Limit`] errors. The engine counts nodes, depth and bytes with [`Meter`].
#[derive(Clone, Copy, Debug)]
pub struct CloneLimits {
    pub bytes: usize,
    pub depth: usize,
    pub nodes: usize,
    /// The name of the limit for bytes, depth and nodes.
    pub names: [&'static str; 3],
}

pub struct CloneOpts<'a, 's> {
    pub limits: CloneLimits,
    /// The transfer list. The host checked it: objects, no duplicates, none detached.
    pub transfer: &'a [Local<'s>],
}

/// What the engine refuses. The host makes the `DataCloneError` text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    Function,
    Symbol,
    Object,
    DetachedBuffer,
    DetachedTransfer,
}

/// A moved item of a record, in transfer-list order.
pub enum Moved {
    /// The bytes of an `ArrayBuffer`. The engine detached the source.
    Buffer(Vec<u8>),
    /// What [`CloneHost::transfer_out`] gave. The engine never looks inside.
    Host(Box<dyn Any>),
}

/// A structured clone as data. It holds no engine value, so it can wait in a history entry or cross to another heap.
pub struct Cloned {
    bytes: Box<[u8]>,
    moved: Vec<Moved>,
    cost: usize,
}

impl Cloned {
    /// For engines: the record of a finished walk. `cost` is [`Meter::bytes`].
    pub fn new(bytes: Box<[u8]>, moved: Vec<Moved>, cost: usize) -> Self {
        Cloned { bytes, moved, cost }
    }

    /// The byte cost of the walk. A message queue cap uses it.
    pub fn cost(&self) -> usize {
        self.cost
    }

    /// For engines: the bytes of the record. Read them with [`RecordReader`].
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn moved(&self) -> &[Moved] {
        &self.moved
    }

    /// The moved items. A host rewrites its own items here (for a message to another page).
    pub fn moved_mut(&mut self) -> &mut [Moved] {
        &mut self.moved
    }

    /// For engines: take the moved items out, to build the objects of [`CloneIn`]. A second call gives none.
    pub fn take_moved(&mut self) -> Vec<Moved> {
        std::mem::take(&mut self.moved)
    }

    /// For engines: take the record apart.
    pub fn into_parts(self) -> (Box<[u8]>, Vec<Moved>, usize) {
        (self.bytes, self.moved, self.cost)
    }
}

/// The result of [`Engine::clone_in`]: the copy, and the new objects of the moved host items, in list order.
pub struct CloneIn<'s> {
    pub value: Local<'s>,
    pub hosts: Vec<Local<'s>>,
}

/// What the engine asks of the page during a clone. The engine calls it only for a host object, a transfer item or an error, never for
/// plain data.
pub trait CloneHost<E: Engine> {
    /// Is `o` an object that the host owns (a wrapper, an event, a `URL`, a platform object)? The engine refuses it, unless the
    /// transfer list holds it.
    fn is_host_object(&self, o: &E::Raw) -> bool;
    /// May the host move `o`, an item of the transfer list that is not an `ArrayBuffer`? A port that no transfer moved.
    fn can_transfer(&self, o: &E::Raw) -> bool;
    /// Move `o` out of this page. The engine calls it after the walk succeeded and the checks passed, once for each such item, in list
    /// order.
    fn transfer_out(&mut self, o: &E::Raw) -> Box<dyn Any>;
    /// New objects for the moved host items, in list order.
    fn transfer_in<'s>(&mut self, s: &mut Scope<'s, E>, items: Vec<Box<dyn Any>>) -> Result<Vec<Local<'s>>, Error<'s>>;
    /// The error to throw for a refused value (`DataCloneError`).
    fn data_clone_error<'s>(&mut self, s: &mut Scope<'s, E>, what: Refused) -> Error<'s>;
}

// ---------- cost model ----------

/// The running cost of one walk. The numbers are the model of the browser's old walk (`browser_navigation_state.rs`): 48 for a node,
/// `2 * len + 16` for a string, 16 for a property, 16 for an entry, 8 for a limb, and one step for each value, key and entry.
///
/// A charge that passes a limit gives `Abort::Limit` with the name of the limit. A cost is `usize` and saturates.
#[derive(Clone, Debug)]
pub struct Meter {
    limits: CloneLimits,
    bytes: usize,
    nodes: usize,
    steps: u64,
}

impl Meter {
    pub fn new(limits: CloneLimits) -> Self {
        Meter { limits, bytes: 0, nodes: 0, steps: 0 }
    }

    /// The byte cost so far: [`Cloned::cost`].
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn nodes(&self) -> usize {
        self.nodes
    }

    /// Add `n` steps. The engine passes them to [`Engine::charge`].
    pub fn step(&mut self, n: u64) {
        self.steps = self.steps.saturating_add(n);
    }

    /// The steps since the last call. The engine charges them at its own pace; the total is the same.
    pub fn take_steps(&mut self) -> u64 {
        std::mem::take(&mut self.steps)
    }

    fn add(&mut self, n: usize) -> Result<(), Abort> {
        self.bytes = self.bytes.saturating_add(n);
        if self.bytes > self.limits.bytes {
            return Err(Abort::Limit(self.limits.names[0]));
        }
        Ok(())
    }

    fn node_cap(&self) -> Result<(), Abort> {
        if self.nodes >= self.limits.nodes {
            return Err(Abort::Limit(self.limits.names[2]));
        }
        Ok(())
    }

    /// One value: one step.
    pub fn value(&mut self) {
        self.step(1);
    }

    /// A string of `len` UTF-16 units (a value, a RegExp part or an error name): `2 * len + 16`, and `len / 32` steps.
    pub fn string(&mut self, len: usize) -> Result<(), Abort> {
        self.add(len.saturating_mul(2).saturating_add(16))?;
        self.step((len / 32) as u64);
        Ok(())
    }

    /// A BigInt of `limbs` 64-bit limbs: `8 * limbs + 16`.
    pub fn bigint(&mut self, limbs: usize) -> Result<(), Abort> {
        self.add(limbs.saturating_mul(8).saturating_add(16))
    }

    /// A string key of `len` units: `16 + 2 * len`. Charge the key step with [`Meter::step`] first: the old walk charged it for each
    /// own key, also for a symbol, a hidden or a deleted one.
    pub fn key_name(&mut self, len: usize) -> Result<(), Abort> {
        self.add(len.saturating_mul(2).saturating_add(16))
    }

    /// An index key: 20.
    pub fn key_index(&mut self) -> Result<(), Abort> {
        self.add(20)
    }

    /// An entry of a Map or a Set: one step and 16.
    pub fn entry(&mut self) -> Result<(), Abort> {
        self.step(1);
        self.add(16)
    }

    /// The bytes of an `ArrayBuffer`: `n`, and `n / 64` steps.
    pub fn buffer(&mut self, n: usize) -> Result<(), Abort> {
        self.add(n)?;
        self.step((n / 64) as u64);
        Ok(())
    }

    /// A new object node at `depth` (0 for the root): the depth cap, then the node cap, then 48. The node takes a number.
    pub fn node(&mut self, depth: usize) -> Result<(), Abort> {
        if depth >= self.limits.depth {
            return Err(Abort::Limit(self.limits.names[1]));
        }
        self.node_cap()?;
        self.add(48)?;
        self.nodes += 1;
        Ok(())
    }

    /// A listed item that the graph reaches: a node with `64 + n` and `n / 64` steps (`n` is 0 for a host item). The node cap comes
    /// first, and there is no depth cap.
    pub fn moved(&mut self, n: usize) -> Result<(), Abort> {
        self.node_cap()?;
        self.unused(n)?;
        self.nodes += 1;
        Ok(())
    }

    /// A listed item that no node used: `64 + n` and `n / 64` steps, and no node.
    pub fn unused(&mut self, n: usize) -> Result<(), Abort> {
        self.add(n.saturating_add(64))?;
        self.step((n / 64) as u64);
        Ok(())
    }
}

// ---------- format ----------

/// The tag of a value in a record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Tag {
    Undef = 0,
    Null = 1,
    True = 2,
    False = 3,
    /// A zigzag varint, for an integer in `i32` range that is not `-0`.
    Int = 4,
    /// 8 bytes, little endian.
    Num = 5,
    /// A length, then that many Latin-1 bytes.
    Str8 = 6,
    /// A length, then that many UTF-16 units (little endian). A lone surrogate needs this tag.
    Str16 = 7,
    /// The id of an earlier object: a cycle or a shared object.
    Ref = 8,
    /// Properties, then `End`.
    Obj = 9,
    /// A dense array: the length, then that many values (`Hole` for a hole), then the other properties, then `End`.
    ArrD = 10,
    End = 11,
    /// A key: a length, then Latin-1 bytes. It takes the next number of the key table.
    KNew8 = 12,
    /// A key: a length, then UTF-16 units. It takes the next number of the key table.
    KNew16 = 13,
    /// A key: a number of the key table.
    KRef = 14,
    Hole = 15,
    /// A key: an array index.
    KIdx = 16,
    /// A sparse array: the length, then the properties (key and value), then `End`.
    Arr = 17,
    /// The time (8 bytes).
    Date = 18,
    /// Two string values: the source and the flags.
    RegExp = 19,
    /// Key and value pairs, then `End`.
    Map = 20,
    /// Values, then `End`.
    Set = 21,
    /// A string value (the name), then error fields, then `End`.
    Error = 22,
    /// The primitive value.
    Wrapper = 23,
    /// A length, then the bytes.
    Buf = 24,
    /// The element type, the byte offset and the length, then the buffer (`Ref` or `Buf`).
    View = 25,
    /// The number of the item in the moved list.
    Moved = 26,
    /// A sign byte, the limb count, then 8 bytes for each limb.
    Big = 27,
    /// Reserved for a host object that is cloned as data. No writer or reader uses it yet.
    Host = 28,
    ErrMessage = 29,
    ErrStack = 30,
    ErrCause = 31,
}

impl Tag {
    fn from_byte(b: u8) -> Result<Tag, ReadError> {
        const ALL: [Tag; 32] = [
            Tag::Undef,
            Tag::Null,
            Tag::True,
            Tag::False,
            Tag::Int,
            Tag::Num,
            Tag::Str8,
            Tag::Str16,
            Tag::Ref,
            Tag::Obj,
            Tag::ArrD,
            Tag::End,
            Tag::KNew8,
            Tag::KNew16,
            Tag::KRef,
            Tag::Hole,
            Tag::KIdx,
            Tag::Arr,
            Tag::Date,
            Tag::RegExp,
            Tag::Map,
            Tag::Set,
            Tag::Error,
            Tag::Wrapper,
            Tag::Buf,
            Tag::View,
            Tag::Moved,
            Tag::Big,
            Tag::Host,
            Tag::ErrMessage,
            Tag::ErrStack,
            Tag::ErrCause,
        ];
        match ALL.get(b as usize) {
            Some(Tag::Host) => Err(ReadError::Reserved),
            Some(&t) => Ok(t),
            None => Err(ReadError::BadTag(b)),
        }
    }

    /// Does a value with this tag take an id when the tag is read?
    fn takes_id(self) -> bool {
        matches!(self, Tag::Obj | Tag::ArrD | Tag::Arr | Tag::Date | Tag::RegExp | Tag::Map | Tag::Set | Tag::Error | Tag::Wrapper | Tag::Buf | Tag::Moved)
    }
}

/// A field of an error record: the own data properties that a clone keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorField {
    Message,
    Stack,
    Cause,
}

/// Why a record could not be read. A record that an engine wrote never gives one. A damaged or cut record does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadError {
    /// The record ends inside a value, or a length is more than the bytes that are left.
    Truncated,
    BadTag(u8),
    /// A varint of more than 64 bits, or an `Int` out of `i32` range.
    BadVarint,
    /// A `Ref` to an id that does not exist yet.
    BadRef,
    /// A `KRef` to a number that is not in the key table yet.
    BadKey,
    /// The reserved `Host` tag.
    Reserved,
    /// Bytes after the last value.
    Trailing,
}

/// A small map from a 64-bit identity to a number: a linear scan of up to 8 pairs, then a hash table with a cheap hash. An engine uses
/// it for the identity table of a walk (an object to its id) and the writer uses it for the key table.
#[derive(Default)]
pub struct IdMap {
    small: [(u64, u32); 8],
    len: usize,
    big: Option<HashMap<u64, u32, BuildHasherDefault<IdHash>>>,
}

#[derive(Default)]
struct IdHash(u64);

impl Hasher for IdHash {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, b: &[u8]) {
        for &x in b {
            self.write_u64(x as u64);
        }
    }
    fn write_u64(&mut self, i: u64) {
        self.0 = (self.0.rotate_left(5) ^ i).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}

impl IdMap {
    pub fn get(&self, k: u64) -> Option<u32> {
        match &self.big {
            Some(m) => m.get(&k).copied(),
            None => self.small[..self.len].iter().find(|e| e.0 == k).map(|e| e.1),
        }
    }

    /// Add a key that is not in the map.
    pub fn insert(&mut self, k: u64, v: u32) {
        if let Some(m) = &mut self.big {
            m.insert(k, v);
        } else if self.len < 8 {
            self.small[self.len] = (k, v);
            self.len += 1;
        } else {
            let mut m: HashMap<u64, u32, _> = self.small.iter().copied().collect();
            m.insert(k, v);
            self.big = Some(m);
        }
    }
}

/// Writes a record. Each object method gives the id of the object. The caller keeps its own identity table (see [`IdMap`]).
pub struct RecordWriter {
    out: Vec<u8>,
    ids: u32,
    keys: IdMap,
    nkeys: u32,
}

impl Default for RecordWriter {
    fn default() -> Self {
        RecordWriter { out: Vec::with_capacity(128), ids: 0, keys: IdMap::default(), nkeys: 0 }
    }
}

impl RecordWriter {
    pub fn new() -> Self {
        Self::default()
    }

    /// The number of bytes written.
    pub fn len(&self) -> usize {
        self.out.len()
    }

    pub fn is_empty(&self) -> bool {
        self.out.is_empty()
    }

    /// The number of ids given so far.
    pub fn ids(&self) -> u32 {
        self.ids
    }

    /// Make room for `n` more bytes (a large buffer).
    pub fn reserve(&mut self, n: usize) {
        self.out.reserve(n);
    }

    /// The record, with no spare room.
    pub fn finish(self) -> Box<[u8]> {
        self.out.into_boxed_slice()
    }

    fn tag(&mut self, t: Tag) {
        self.out.push(t as u8);
    }

    fn id(&mut self, t: Tag) -> u32 {
        self.tag(t);
        self.ids += 1;
        self.ids - 1
    }

    /// An unsigned varint (LEB128). A host can use it for its own payloads.
    pub fn varint(&mut self, mut n: u64) {
        while n >= 0x80 {
            self.out.push(n as u8 | 0x80);
            n >>= 7;
        }
        self.out.push(n as u8);
    }

    pub fn undefined(&mut self) {
        self.tag(Tag::Undef);
    }

    pub fn null(&mut self) {
        self.tag(Tag::Null);
    }

    pub fn boolean(&mut self, b: bool) {
        self.tag(if b { Tag::True } else { Tag::False });
    }

    pub fn hole(&mut self) {
        self.tag(Tag::Hole);
    }

    pub fn end(&mut self) {
        self.tag(Tag::End);
    }

    pub fn int(&mut self, i: i32) {
        self.tag(Tag::Int);
        self.varint(((i << 1) ^ (i >> 31)) as u32 as u64);
    }

    /// A number as `Int` (an integer in `i32` range that is not `-0`) or as 8 bytes.
    pub fn number(&mut self, n: f64) {
        if n as i32 as f64 == n && !(n == 0.0 && n.is_sign_negative()) {
            self.int(n as i32);
        } else {
            self.tag(Tag::Num);
            self.out.extend_from_slice(&n.to_le_bytes());
        }
    }

    fn units(&mut self, t8: Tag, t16: Tag, u: &[u16]) {
        if u.iter().all(|&c| c < 256) {
            self.tag(t8);
            self.varint(u.len() as u64);
            self.out.extend(u.iter().map(|&c| c as u8));
        } else {
            self.tag(t16);
            self.varint(u.len() as u64);
            for &c in u {
                self.out.extend_from_slice(&c.to_le_bytes());
            }
        }
    }

    /// A string value. It is `Str8` when every unit is below 256.
    pub fn string(&mut self, u: &[u16]) {
        self.units(Tag::Str8, Tag::Str16, u);
    }

    /// A string value of Latin-1 bytes.
    pub fn string_latin1(&mut self, b: &[u8]) {
        self.tag(Tag::Str8);
        self.varint(b.len() as u64);
        self.out.extend_from_slice(b);
    }

    pub fn bigint(&mut self, negative: bool, limbs: &[u64]) {
        self.tag(Tag::Big);
        self.out.push(negative as u8);
        self.varint(limbs.len() as u64);
        for l in limbs {
            self.out.extend_from_slice(&l.to_le_bytes());
        }
    }

    /// A reference to the object `id`.
    pub fn reference(&mut self, id: u32) {
        self.tag(Tag::Ref);
        self.varint(id as u64);
    }

    /// A string key. `ident` names the key in the engine (an atom or an interned string): the same `ident` is the same text, so the
    /// second use is a `KRef`. Two idents with one text are valid and make two entries.
    pub fn key(&mut self, ident: u64, text: &[u16]) {
        if !self.key_ref(ident) {
            self.units(Tag::KNew8, Tag::KNew16, text);
        }
    }

    /// A string key of Latin-1 bytes. See [`RecordWriter::key`].
    pub fn key_latin1(&mut self, ident: u64, text: &[u8]) {
        if !self.key_ref(ident) {
            self.tag(Tag::KNew8);
            self.varint(text.len() as u64);
            self.out.extend_from_slice(text);
        }
    }

    /// Write `KRef` and say true, or register the key and say false (the caller writes `KNew`).
    fn key_ref(&mut self, ident: u64) -> bool {
        match self.keys.get(ident) {
            Some(n) => {
                self.tag(Tag::KRef);
                self.varint(n as u64);
                true
            }
            None => {
                self.keys.insert(ident, self.nkeys);
                self.nkeys += 1;
                false
            }
        }
    }

    pub fn key_index(&mut self, i: u32) {
        self.tag(Tag::KIdx);
        self.varint(i as u64);
    }

    /// A plain object. Write `key`, value pairs, then `end`.
    pub fn object(&mut self) -> u32 {
        self.id(Tag::Obj)
    }

    /// A dense array of `len` values. Write the `len` values (`hole` for a hole), the other properties, then `end`.
    pub fn array_dense(&mut self, len: usize) -> u32 {
        let id = self.id(Tag::ArrD);
        self.varint(len as u64);
        id
    }

    /// A sparse array. Write `key`, value pairs (an index key for an element), then `end`.
    pub fn array_sparse(&mut self, len: u32) -> u32 {
        let id = self.id(Tag::Arr);
        self.varint(len as u64);
        id
    }

    pub fn date(&mut self, time: f64) -> u32 {
        let id = self.id(Tag::Date);
        self.out.extend_from_slice(&time.to_le_bytes());
        id
    }

    pub fn regexp(&mut self, source: &[u16], flags: &[u16]) -> u32 {
        let id = self.id(Tag::RegExp);
        self.string(source);
        self.string(flags);
        id
    }

    /// A Map. Write key and value pairs, then `end`.
    pub fn map(&mut self) -> u32 {
        self.id(Tag::Map)
    }

    /// A Set. Write the values, then `end`.
    pub fn set(&mut self) -> u32 {
        self.id(Tag::Set)
    }

    /// An error with this name. Write each field with `error_field` and its value, then `end`.
    pub fn error(&mut self, name: &[u16]) -> u32 {
        let id = self.id(Tag::Error);
        self.string(name);
        id
    }

    /// The tag of a field of an error. The value follows.
    pub fn error_field(&mut self, f: ErrorField) {
        self.tag(match f {
            ErrorField::Message => Tag::ErrMessage,
            ErrorField::Stack => Tag::ErrStack,
            ErrorField::Cause => Tag::ErrCause,
        });
    }

    /// A wrapper object. Write the primitive value next.
    pub fn wrapper(&mut self) -> u32 {
        self.id(Tag::Wrapper)
    }

    pub fn buffer(&mut self, bytes: &[u8]) -> u32 {
        let id = self.id(Tag::Buf);
        self.varint(bytes.len() as u64);
        self.out.extend_from_slice(bytes);
        id
    }

    /// The header of a view. Write its buffer next (`reference` or `buffer`), then call `view_done`.
    pub fn view(&mut self, ty: u8, byte_offset: usize, len: usize) {
        self.tag(Tag::View);
        self.out.push(ty);
        self.varint(byte_offset as u64);
        self.varint(len as u64);
    }

    /// The view takes its id now, after its buffer.
    pub fn view_done(&mut self) -> u32 {
        self.ids += 1;
        self.ids - 1
    }

    /// The item `index` of the moved list.
    pub fn moved(&mut self, index: u32) -> u32 {
        let id = self.id(Tag::Moved);
        self.varint(index as u64);
        id
    }
}

/// A string of a record: Latin-1 bytes or UTF-16 units. It borrows the record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Text<'r> {
    Latin1(&'r [u8]),
    /// Little-endian pairs of bytes.
    Utf16(&'r [u8]),
}

impl<'r> Text<'r> {
    /// The number of UTF-16 units.
    pub fn len(&self) -> usize {
        match self {
            Text::Latin1(b) => b.len(),
            Text::Utf16(b) => b.len() / 2,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn units(&self) -> impl Iterator<Item = u16> + 'r {
        let (l, u) = match *self {
            Text::Latin1(b) => (Some(b), None),
            Text::Utf16(b) => (None, Some(b)),
        };
        l.into_iter().flatten().map(|&c| c as u16).chain(u.into_iter().flat_map(|b| b.chunks_exact(2)).map(|c| u16::from_le_bytes([c[0], c[1]])))
    }

    pub fn to_vec(&self) -> Vec<u16> {
        self.units().collect()
    }
}

/// A key of an object, an array or a record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key<'r> {
    /// A new string. It takes the next number of the key table; the reader counts the numbers.
    New(Text<'r>),
    /// The number of an earlier `New`.
    Ref(u32),
    Index(u32),
}

/// Reads a record. Every method checks its bounds: a cut or damaged record gives a [`ReadError`], never a panic, and no length is
/// trusted before it is compared with the bytes that are left. The reader assigns ids as the writer did. The engine pushes the object
/// that it makes for a tag to its own id table at once, so that a `Ref` finds it.
pub struct RecordReader<'r> {
    b: &'r [u8],
    p: usize,
    ids: u32,
    keys: u32,
}

type R<T> = Result<T, ReadError>;

impl<'r> RecordReader<'r> {
    pub fn new(b: &'r [u8]) -> Self {
        RecordReader { b, p: 0, ids: 0, keys: 0 }
    }

    /// The number of ids given so far.
    pub fn ids(&self) -> u32 {
        self.ids
    }

    /// The bytes that are left.
    pub fn remaining(&self) -> usize {
        self.b.len() - self.p
    }

    /// No bytes may follow the last value.
    pub fn finish(&self) -> R<()> {
        if self.p == self.b.len() { Ok(()) } else { Err(ReadError::Trailing) }
    }

    fn u8(&mut self) -> R<u8> {
        let c = *self.b.get(self.p).ok_or(ReadError::Truncated)?;
        self.p += 1;
        Ok(c)
    }

    fn take(&mut self, n: usize) -> R<&'r [u8]> {
        let s = self.b.get(self.p..).and_then(|r| r.get(..n)).ok_or(ReadError::Truncated)?;
        self.p += n;
        Ok(s)
    }

    /// A length of `unit` bytes each that the record still holds. It is never more than the bytes that are left, so a damaged length
    /// cannot make a large allocation.
    fn len(&mut self, unit: usize) -> R<usize> {
        let n = usize::try_from(self.varint()?).map_err(|_| ReadError::Truncated)?;
        if n.checked_mul(unit).is_none_or(|t| t > self.remaining()) {
            return Err(ReadError::Truncated);
        }
        Ok(n)
    }

    /// An unsigned varint (LEB128).
    pub fn varint(&mut self) -> R<u64> {
        let mut n = 0u64;
        for i in 0..10 {
            let c = self.u8()?;
            if i == 9 && c > 1 {
                return Err(ReadError::BadVarint);
            }
            n |= ((c & 0x7f) as u64) << (7 * i);
            if c < 0x80 {
                return Ok(n);
            }
        }
        Err(ReadError::BadVarint)
    }

    fn varint32(&mut self) -> R<u32> {
        u32::try_from(self.varint()?).map_err(|_| ReadError::BadVarint)
    }

    /// The next tag. A tag that makes an object gives it the next id.
    pub fn tag(&mut self) -> R<Tag> {
        let t = Tag::from_byte(self.u8()?)?;
        if t.takes_id() {
            self.ids += 1;
        }
        Ok(t)
    }

    /// Say true and take the `End` tag when it is next. The record must not end here.
    pub fn end(&mut self) -> R<bool> {
        if Tag::from_byte(*self.b.get(self.p).ok_or(ReadError::Truncated)?)? == Tag::End {
            self.p += 1;
            return Ok(true);
        }
        Ok(false)
    }

    /// The payload of `Int`.
    pub fn int(&mut self) -> R<i32> {
        let u = self.varint32()?;
        Ok((u >> 1) as i32 ^ -((u & 1) as i32))
    }

    /// The payload of `Num` and `Date`.
    pub fn num(&mut self) -> R<f64> {
        let b = self.take(8)?;
        Ok(f64::from_le_bytes(b.try_into().expect("8 bytes")))
    }

    /// The payload of `Str8` or `Str16`, given the tag that was read.
    pub fn text(&mut self, t: Tag) -> R<Text<'r>> {
        match t {
            Tag::Str8 | Tag::KNew8 => {
                let n = self.len(1)?;
                Ok(Text::Latin1(self.take(n)?))
            }
            Tag::Str16 | Tag::KNew16 => {
                let n = self.len(2)?;
                Ok(Text::Utf16(self.take(n * 2)?))
            }
            t => Err(ReadError::BadTag(t as u8)),
        }
    }

    /// A string value: its tag and its payload.
    pub fn string(&mut self) -> R<Text<'r>> {
        let t = self.tag()?;
        if !matches!(t, Tag::Str8 | Tag::Str16) {
            return Err(ReadError::BadTag(t as u8));
        }
        self.text(t)
    }

    /// The payload of `Big`: the sign. The limbs go to `limbs`, which is cleared first.
    pub fn bigint(&mut self, limbs: &mut Vec<u64>) -> R<bool> {
        let negative = self.u8()? != 0;
        let n = self.len(8)?;
        limbs.clear();
        for c in self.take(n * 8)?.chunks_exact(8) {
            limbs.push(u64::from_le_bytes(c.try_into().expect("8 bytes")));
        }
        Ok(negative)
    }

    /// The payload of `Ref`: an id that exists.
    pub fn reference(&mut self) -> R<u32> {
        let id = self.varint32()?;
        if id < self.ids { Ok(id) } else { Err(ReadError::BadRef) }
    }

    /// The next key, or `None` at `End`.
    pub fn key(&mut self) -> R<Option<Key<'r>>> {
        let t = Tag::from_byte(self.u8()?)?;
        Ok(Some(match t {
            Tag::End => return Ok(None),
            Tag::KNew8 | Tag::KNew16 => {
                let s = self.text(t)?;
                self.keys += 1;
                Key::New(s)
            }
            Tag::KRef => {
                let n = self.varint32()?;
                if n >= self.keys {
                    return Err(ReadError::BadKey);
                }
                Key::Ref(n)
            }
            Tag::KIdx => Key::Index(self.varint32()?),
            t => return Err(ReadError::BadTag(t as u8)),
        }))
    }

    /// The payload of `ArrD`: the number of values that follow. Each value is at least one byte, so it is at most the bytes that are left.
    pub fn dense_len(&mut self) -> R<usize> {
        self.len(1)
    }

    /// The payload of `Arr`: the length of the array.
    pub fn sparse_len(&mut self) -> R<u32> {
        self.varint32()
    }

    /// The payload of `Buf`: the bytes.
    pub fn buffer(&mut self) -> R<&'r [u8]> {
        let n = self.len(1)?;
        self.take(n)
    }

    /// The payload of `RegExp`: the source and the flags.
    pub fn regexp(&mut self) -> R<(Text<'r>, Text<'r>)> {
        Ok((self.string()?, self.string()?))
    }

    /// The next field of an error, or `None` at `End`. The value follows a field.
    pub fn error_field(&mut self) -> R<Option<ErrorField>> {
        Ok(Some(match Tag::from_byte(self.u8()?)? {
            Tag::End => return Ok(None),
            Tag::ErrMessage => ErrorField::Message,
            Tag::ErrStack => ErrorField::Stack,
            Tag::ErrCause => ErrorField::Cause,
            t => return Err(ReadError::BadTag(t as u8)),
        }))
    }

    /// The payload of `View`: the element type, the byte offset and the length. The buffer follows, then call `view_done`. A length
    /// or an offset is not checked against the buffer: the engine does that.
    pub fn view(&mut self) -> R<(u8, usize, usize)> {
        let ty = self.u8()?;
        let off = usize::try_from(self.varint()?).map_err(|_| ReadError::Truncated)?;
        let len = usize::try_from(self.varint()?).map_err(|_| ReadError::Truncated)?;
        Ok((ty, off, len))
    }

    /// The view takes its id now, after its buffer.
    pub fn view_done(&mut self) -> u32 {
        self.ids += 1;
        self.ids - 1
    }

    /// The payload of `Moved`: the number of the item. The engine checks it against its moved list.
    pub fn moved(&mut self) -> R<u32> {
        self.varint32()
    }
}
