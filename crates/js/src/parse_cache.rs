//! Parse cache: the syntax trees of scripts, shared by every realm of one thread.
//!
//! A browser session runs its documents on one thread, and each document makes a new realm. The scripts of a site
//! (`/_next/static` chunks) come back in each document. Without the cache each realm parses them again.
//!
//! - Key: a hash of the source bytes and the [`Kind`]. A hit also compares the bytes, so a hash collision is a miss.
//! - Value: the parse result. An `Ok` holds the owned [`Program`]. It has no heap values: names and strings are byte
//!   ranges of its source. Each realm interns them in its own heap when it walks the tree (`walk_program`), so nothing
//!   here depends on a heap. An `Err` holds the first error with its span.
//! - Node limit: a realm parses with `max_nodes` set to its room left. The entry keeps `used`, the count of nodes and
//!   list entries made before the end or the error. Since `used` only grows and is checked before each step, a parse
//!   with `max_nodes >= used` gives the stored result, and a parse with less room stops with `Limit(Nodes)`. A stored
//!   `Limit(Nodes)` is only known for a room up to the one it had; more room parses again.
//! - Memory: at most [`MAX_BYTES`] of source and tree per thread. The oldest entries go first. Sources under
//!   [`MIN_SOURCE`] bytes are not kept (a short `eval` or `Function` text parses faster than an entry costs).
//! - Age: each new realm (one document) calls [`new_realm`]. It drops the entries that no realm used in the last
//!   [`MAX_IDLE`] realms. A realm that still runs such a script keeps its own `Rc` of the tree.

use crate::ast::Program;
use crate::parse::{parse_module_used, parse_script_used, Error, ErrorKind, LimitKind, Limits};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

/// Most bytes of source and tree that one thread keeps.
pub const MAX_BYTES: usize = 32 << 20;
/// Shorter sources are not kept.
pub const MIN_SOURCE: usize = 512;
/// An entry that the last this many realms did not use goes at the next [`new_realm`].
pub const MAX_IDLE: u64 = 2;

/// The grammar of the source. The same bytes as a module and as a classic script are two entries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Classic,
    /// The module goal ([`crate::parse::parse_module`]).
    Module,
}

/// Counts of this thread, for tests and reports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub hits: u64,
    pub parses: u64,
    pub entries: usize,
    pub bytes: usize,
}

enum Res {
    Ok(Rc<Program<'static>>),
    /// The first error and the source it belongs to (for the byte compare).
    Err(Error, Box<str>),
}

struct Entry {
    res: Res,
    used: u32,
    /// The `max_nodes` of the parse that gave `Limit(Nodes)`.
    room: u32,
    bytes: usize,
    /// The realm count at the last parse or hit.
    last: u64,
}

#[derive(Default)]
struct Cache {
    map: HashMap<(u64, Kind), Entry>,
    order: VecDeque<(u64, Kind)>,
    counts: Counts,
    realms: u64,
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::default();
}

/// Multiply hash of 8 bytes at a time, and the length.
fn hash(b: &[u8]) -> u64 {
    const K: u64 = 0x517c_c1b7_2722_0a95;
    let mut h = b.len() as u64;
    let mut c = b.chunks_exact(8);
    for w in &mut c {
        h = (h.rotate_left(5) ^ u64::from_le_bytes(w.try_into().unwrap())).wrapping_mul(K);
    }
    let mut t = [0u8; 8];
    t[..c.remainder().len()].copy_from_slice(c.remainder());
    (h.rotate_left(5) ^ u64::from_le_bytes(t)).wrapping_mul(K)
}

fn nodes_err(at: u32) -> Error {
    Error { kind: ErrorKind::Limit(LimitKind::Nodes), msg: "too many syntax tree nodes", start: at, end: at }
}

/// The result of `parse_script(source, limits)` with an owned tree, from the cache when it has one.
/// `source.len() <= limits.max_source` must be checked first (the realm does it).
pub fn parse(kind: Kind, source: &str, limits: Limits) -> Result<Rc<Program<'static>>, Error> {
    let fresh = |limits| {
        let (r, used) = if kind == Kind::Module { parse_module_used(source, limits) } else { parse_script_used(source, limits) };
        (r.map(|p| Rc::new(p.into_owned())), used)
    };
    let plain = Limits { max_source: limits.max_source, max_nodes: limits.max_nodes, ..Limits::default() };
    if source.len() < MIN_SOURCE || limits != plain {
        let r = if kind == Kind::Module { crate::parse::parse_module(source, limits) } else { crate::parse::parse_script(source, limits) };
        return r.map(|p| Rc::new(p.into_owned()));
    }
    let key = (hash(source.as_bytes()), kind);
    let hit = CACHE.with_borrow_mut(|c| {
        let realms = c.realms;
        let e = c.map.get_mut(&key)?;
        let r = match &e.res {
            Res::Ok(p) if *p.source == *source => Ok(p.clone()),
            Res::Err(err, s) if **s == *source => {
                if err.kind == ErrorKind::Limit(LimitKind::Nodes) && limits.max_nodes > e.room {
                    return None;
                }
                Err(*err)
            }
            _ => return None,
        };
        e.last = realms;
        c.counts.hits += 1;
        Some(if limits.max_nodes < e.used { Err(nodes_err(0)) } else { r })
    });
    if let Some(r) = hit {
        return r;
    }
    let (r, used) = fresh(limits);
    let (res, bytes) = match &r {
        Ok(p) => (Res::Ok(p.clone()), p.source.len() + p.stats().bytes),
        Err(e) => (Res::Err(*e, source.into()), source.len()),
    };
    CACHE.with_borrow_mut(|c| {
        c.counts.parses += 1;
        if bytes > MAX_BYTES {
            return;
        }
        if let Some(old) = c.map.remove(&key) {
            c.counts.bytes -= old.bytes;
        }
        while c.counts.bytes + bytes > MAX_BYTES {
            let Some(k) = c.order.pop_front() else { break };
            if let Some(old) = c.map.remove(&k) {
                c.counts.bytes -= old.bytes;
            }
        }
        c.map.insert(key, Entry { res, used, room: limits.max_nodes, bytes, last: c.realms });
        c.order.push_back(key);
        c.counts.bytes += bytes;
        c.counts.entries = c.map.len();
    });
    r
}

/// The counts of this thread.
pub fn counts() -> Counts {
    CACHE.with_borrow(|c| Counts { entries: c.map.len(), ..c.counts })
}

/// A new realm starts on this thread: drop the entries that the last [`MAX_IDLE`] realms did not use.
pub fn new_realm() {
    CACHE.with_borrow_mut(|c| {
        c.realms += 1;
        let before = c.map.len();
        let (realms, mut freed) = (c.realms, 0);
        c.map.retain(|_, e| {
            let keep = realms - e.last <= MAX_IDLE;
            if !keep {
                freed += e.bytes;
            }
            keep
        });
        if c.map.len() != before {
            c.counts.bytes -= freed;
            c.order.retain(|k| c.map.contains_key(k));
        }
    });
}

/// Drop every entry of this thread and reset the counts.
pub fn clear() {
    CACHE.with_borrow_mut(|c| *c = Cache::default());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(tail: &str) -> String {
        format!("var pad = '{}';\n{tail}", "x".repeat(MIN_SOURCE))
    }

    #[test]
    fn the_same_source_parses_once_and_a_changed_one_again() {
        clear();
        let (a, b) = (src("var a = 1;"), src("var a = 2;"));
        let p1 = parse(Kind::Classic, &a, Limits::default()).unwrap();
        let p2 = parse(Kind::Classic, &a, Limits::default()).unwrap();
        assert!(Rc::ptr_eq(&p1, &p2));
        assert_eq!((counts().parses, counts().hits), (1, 1));
        let p3 = parse(Kind::Classic, &b, Limits::default()).unwrap();
        assert!(!Rc::ptr_eq(&p1, &p3));
        assert_eq!((counts().parses, counts().hits, counts().entries), (2, 1, 2));
    }

    #[test]
    fn two_realms_run_one_parse_of_the_same_script() {
        use crate::eval::{EvalLimits, Interp};
        use crate::value::Value;
        clear();
        let (ok, bad) = (src("var a = 41; a + 1"), src("var = 1;"));
        for _ in 0..2 {
            let mut it = Interp::realm(crate::heap::Heap::default(), EvalLimits::default()).unwrap();
            assert!(matches!(it.run_script(&ok), Ok(Value::Number(n)) if n == 42.0));
            assert!(matches!(it.run_script(&bad), Err(crate::eval::Error::Throw(_))));
        }
        assert_eq!((counts().parses, counts().hits), (2, 2));
    }

    #[test]
    fn a_syntax_error_is_kept_with_its_span() {
        clear();
        let s = src("var = 1;");
        let want = crate::parse::parse_script(&s, Limits::default()).err().unwrap();
        for _ in 0..2 {
            assert_eq!(parse(Kind::Classic, &s, Limits::default()).err(), Some(want));
        }
        assert_eq!((counts().parses, counts().hits), (1, 1));
    }

    #[test]
    fn the_node_room_gives_the_result_of_a_fresh_parse() {
        clear();
        let (ok, bad) = (src("var a = [1, 2, 3];"), src("var a = [1, 2, 3]; var = 1;"));
        for s in [&ok, &bad] {
            let full = parse_script_used(s, Limits::default());
            for room in [0, 1, full.1 - 1, full.1, full.1 + 1, 16 << 20, 3, full.1] {
                let l = Limits { max_nodes: room, ..Limits::default() };
                let want = crate::parse::parse_script(s, l).map(|p| p.stats()).map_err(|e| e.kind);
                let got = parse(Kind::Classic, s, l).map(|p| p.stats()).map_err(|e| e.kind);
                assert_eq!(got, want, "room {room}");
            }
        }
    }

    #[test]
    fn a_module_and_a_classic_script_do_not_share_an_entry() {
        clear();
        let s = src("var a = 1;");
        parse(Kind::Classic, &s, Limits::default()).unwrap();
        parse(Kind::Module, &s, Limits::default()).unwrap();
        assert_eq!((counts().parses, counts().hits, counts().entries), (2, 0, 2));
    }

    #[test]
    fn short_sources_are_not_kept_and_the_cache_stays_under_its_cap() {
        clear();
        parse(Kind::Classic, "var a = 1;", Limits::default()).unwrap();
        assert_eq!(counts(), Counts::default());
        let big = format!("var s = '{}';", "y".repeat(MAX_BYTES / 3));
        for i in 0..4 {
            parse(Kind::Classic, &format!("{big}{i}"), Limits::default()).unwrap();
            assert!(counts().bytes <= MAX_BYTES);
        }
        assert_eq!(counts().entries, 2);
    }

    #[test]
    fn an_entry_that_no_recent_realm_used_goes() {
        clear();
        let (a, b) = (src("var a = 1;"), src("var b = 1;"));
        parse(Kind::Classic, &a, Limits::default()).unwrap();
        parse(Kind::Classic, &b, Limits::default()).unwrap();
        for _ in 0..MAX_IDLE {
            new_realm();
            parse(Kind::Classic, &a, Limits::default()).unwrap();
        }
        assert_eq!(counts().entries, 2);
        new_realm();
        assert_eq!(counts().entries, 1);
        assert_eq!(counts().bytes, a.len() + parse(Kind::Classic, &a, Limits::default()).unwrap().stats().bytes);
        parse(Kind::Classic, &b, Limits::default()).unwrap();
        assert_eq!((counts().parses, counts().entries), (3, 2));
    }
}
