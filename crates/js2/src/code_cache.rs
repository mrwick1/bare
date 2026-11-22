//! Portable compile results for classic scripts and modules on this thread.
//! The cache owns source bytes, atom units and compiler metadata. Each realm binds its own
//! atoms and code indexes before it installs the code.
use crate::atoms::SharedAtoms;
use crate::bytecode::Code;
use crate::compile::{Child, Exp, ExpSrc, Imp, Level, ModInfo, ScriptMode};
use crate::facts::Facts;
use crate::parse::Limits;
use crate::parse_ahead::Identity;
use crate::string::{Chars, Str};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

pub const MAX_BYTES: usize = 32 << 20;
const MIN_SOURCE: usize = 512;
const MAX_IDLE: u64 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Mode {
    Script(ScriptMode),
    Module,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Counts {
    pub hits: u64,
    pub misses: u64,
    pub entries: usize,
    pub bytes: usize,
}

pub(crate) struct UnitCode<A = Str> {
    pub codes: Vec<Code<A>>,
    pub facts: Facts,
    pub pending: Vec<(usize, Child)>,
    pub info: Option<ModInfo>,
}

#[derive(Debug)]
pub(crate) enum AtomUnits {
    One(Box<[u8]>),
    Two(Box<[u16]>),
}

impl AtomUnits {
    fn chars(&self) -> Chars<'_> {
        match self {
            Self::One(s) => Chars::One(s),
            Self::Two(s) => Chars::Two(s),
        }
    }
}

/// All indexes in `code` are local to this unit. There are no pointers into a realm.
pub(crate) struct PortableUnit {
    pub source: Rc<str>,
    pub atoms: Vec<AtomUnits>,
    pub code: UnitCode<u32>,
}

impl PortableUnit {
    fn capture(source: Rc<str>, unit: &UnitCode) -> Self {
        let mut atoms = Vec::new();
        let mut table = HashMap::new();
        let codes = unit
            .codes
            .iter()
            .map(|c| {
                c.map_atoms(|s| {
                    *table.entry(s.cell()).or_insert_with(|| {
                        let id = atoms.len() as u32;
                        atoms.push(match s.chars().expect("a source atom is flat") {
                            Chars::One(b) => AtomUnits::One(b.into()),
                            Chars::Two(u) => AtomUnits::Two(u.into()),
                        });
                        id
                    })
                })
            })
            .collect();
        Self {
            source,
            atoms,
            code: UnitCode {
                codes,
                facts: unit.facts.clone(),
                pending: unit.pending.clone(),
                info: unit.info.clone(),
            },
        }
    }

    pub(crate) fn bind(&self, atoms: &SharedAtoms) -> Result<UnitCode, String> {
        let names = self
            .atoms
            .iter()
            .map(|s| atoms.intern(s.chars()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "out of memory")?;
        let codes = self.code.codes.iter().map(|c| c.map_atoms(|i| names[i as usize])).collect();
        Ok(UnitCode {
            codes,
            facts: self.code.facts.clone(),
            pending: self.code.pending.clone(),
            info: self.code.info.clone(),
        })
    }

    fn bytes(&self) -> usize {
        let UnitCode { codes, facts, pending, info } = &self.code;
        let mut levels = HashSet::new();
        let chains = pending
            .iter()
            .map(|(_, child)| {
                let mut bytes = child.name.as_ref().map_or(0, |s| s.len());
                let mut level = child.chain.as_ref();
                while let Some(current) = level {
                    if !levels.insert(Rc::<Level>::as_ptr(current)) {
                        break;
                    }
                    bytes += current.cache_bytes();
                    level = current.parent.as_ref();
                }
                bytes
            })
            .sum::<usize>();
        let module = info.as_ref().map_or(0, ModInfo::cache_bytes);
        size_of::<Self>()
            + self.source.len()
            + self.atoms.capacity() * size_of::<AtomUnits>()
            + self
                .atoms
                .iter()
                .map(|s| match s {
                    AtomUnits::One(b) => size_of_val(&**b),
                    AtomUnits::Two(u) => size_of_val(&**u),
                })
                .sum::<usize>()
            + codes.capacity() * size_of::<Code<u32>>()
            + codes.iter().map(Code::cache_bytes).sum::<usize>()
            + facts.fns.capacity() * size_of::<crate::facts::FnFacts>()
            + facts.caps.capacity() * size_of::<crate::ast::Span>()
            + facts.by_start.capacity() * size_of::<crate::facts::FnId>()
            + pending.capacity() * size_of::<(usize, Child)>()
            + chains
            + module
            + size_of::<(Key, Entry)>()
            + size_of::<(Age, Key)>()
    }
}

impl ModInfo {
    fn cache_bytes(&self) -> usize {
        self.requests.capacity() * size_of::<Box<str>>() + self.requests.iter().map(|s| s.len()).sum::<usize>()
            + self.imports.capacity() * size_of::<Imp>()
            + self.imports.iter().map(|i| i.name.as_ref().map_or(0, |s| s.len())).sum::<usize>()
            + self.exports.capacity() * size_of::<Exp>()
            + self.exports.iter().map(|e| {
                e.name.len() + match &e.src {
                    ExpSrc::Local(s) => s.len(),
                    ExpSrc::Indirect(_, s) => s.as_ref().map_or(0, |s| s.len()),
                }
            }).sum::<usize>()
            + self.stars.capacity() * size_of::<u32>()
            + self.env_props.capacity() * size_of::<(Box<str>, bool)>()
            + self.env_props.iter().map(|(s, _)| s.len()).sum::<usize>()
            + self.undef.capacity() * size_of::<u32>()
    }
}

type Key = (Identity, Mode);
type Age = (u64, u64);

struct Entry {
    unit: Rc<PortableUnit>,
    age: Age,
    bytes: usize,
}

#[derive(Default)]
struct Cache {
    entries: HashMap<Key, Entry>,
    order: BTreeMap<Age, Key>,
    counts: Counts,
    realm: u64,
    serial: u64,
}

impl Cache {
    fn age(&mut self) -> Age {
        self.serial += 1;
        (self.realm, self.serial)
    }

    fn get(&mut self, key: Key, source: &str) -> Option<Rc<PortableUnit>> {
        if !self.entries.get(&key).is_some_and(|e| e.unit.source.as_ref() == source) {
            self.counts.misses += 1;
            return None;
        }
        let age = self.age();
        let entry = self.entries.get_mut(&key).expect("a cache hit");
        self.order.remove(&entry.age);
        self.order.insert(age, key);
        entry.age = age;
        self.counts.hits += 1;
        Some(entry.unit.clone())
    }

    fn remove(&mut self, key: Key) {
        if let Some(e) = self.entries.remove(&key) {
            self.order.remove(&e.age);
            self.counts.bytes -= e.bytes;
        }
    }

    fn insert(&mut self, key: Key, unit: Rc<PortableUnit>, bytes: usize, cap: usize) {
        if bytes > cap {
            return;
        }
        self.remove(key);
        while self.counts.bytes > cap - bytes {
            let (_, &oldest) = self.order.first_key_value().expect("the cache has bytes");
            self.remove(oldest);
        }
        let age = self.age();
        self.order.insert(age, key);
        self.entries.insert(key, Entry { unit, age, bytes });
        self.counts.bytes += bytes;
    }

    fn new_realm(&mut self) {
        self.realm += 1;
        while let Some((&(last, _), &key)) = self.order.first_key_value() {
            if self.realm - last <= MAX_IDLE {
                break;
            }
            self.remove(key);
        }
    }
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::default();
}

/// The counts of this thread.
pub fn counts() -> Counts {
    CACHE.with_borrow(|c| Counts {
        entries: c.entries.len(),
        ..c.counts
    })
}

pub(crate) fn new_realm() {
    CACHE.with_borrow_mut(Cache::new_realm);
}

/// Clear entries and counts on this thread.
pub fn clear() {
    CACHE.with_borrow_mut(|c| *c = Cache::default());
}

pub(crate) fn get_or_compile(
    source: &str,
    mode: Mode,
    limits: Limits,
    atoms: &SharedAtoms,
    compile: impl FnOnce() -> Result<UnitCode, String>,
) -> Result<(Rc<str>, UnitCode), String> {
    if source.len() < MIN_SOURCE || limits != Limits::default() {
        let code = compile()?;
        return Ok((Rc::from(source), code));
    }
    let key = (Identity::of(source), mode);
    if let Some(unit) = CACHE.with_borrow_mut(|c| c.get(key, source)) {
        return Ok((unit.source.clone(), unit.bind(atoms)?));
    }
    compile_on_miss(key, source, compile)
}

/// A disk cache can look here before compile, and store the same portable unit after compile.
fn compile_on_miss(key: Key, source: &str, compile: impl FnOnce() -> Result<UnitCode, String>) -> Result<(Rc<str>, UnitCode), String> {
    let code = compile()?;
    let source: Rc<str> = source.into();
    let unit = Rc::new(PortableUnit::capture(source.clone(), &code));
    let bytes = unit.bytes();
    CACHE.with_borrow_mut(|c| c.insert(key, unit, bytes, MAX_BYTES));
    Ok((source, code))
}

#[cfg(test)]
#[path = "code_cache_tests.rs"]
mod tests;
