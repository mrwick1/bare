//! The gap census: with `BARE_CENSUS=FILE`, each name that a script reads (or tests with `in`) on a host object and that is not
//! there is written to FILE as `TAG<TAB>NAME<TAB>get|has<TAB>COUNT`. A line is written at the counts 1, 2, 4, 8, …, so the file stays
//! small; the last line of a name gives its count to a factor of 2. Off, the cost is one load on a host miss.

use crate::interp_rt::{Key, Realm};
use std::collections::HashMap;
use std::io::Write;
use std::sync::{Mutex, OnceLock};

type Log = Mutex<Census<std::fs::File>>;

/// The counts of the misses, and the file that gets a line at each power of 2.
pub(crate) struct Census<W> {
    pub(crate) out: W,
    seen: HashMap<(String, String, bool), u64>,
}

impl<W: Write> Census<W> {
    pub(crate) fn new(out: W) -> Self {
        Census { out, seen: HashMap::new() }
    }

    pub(crate) fn add(&mut self, tag: String, name: String, want: bool) {
        let line = format!("{tag}\t{name}\t{}", if want { "get" } else { "has" });
        let n = self.seen.entry((tag, name, want)).or_insert(0);
        *n += 1;
        if n.is_power_of_two() {
            let _ = writeln!(self.out, "{line}\t{n}");
        }
    }
}

fn log() -> &'static Option<Log> {
    static LOG: OnceLock<Option<Log>> = OnceLock::new();
    LOG.get_or_init(|| {
        let path = std::env::var_os("BARE_CENSUS")?;
        let f = std::fs::OpenOptions::new().create(true).append(true).open(path).ok()?;
        Some(Mutex::new(Census::new(f)))
    })
}

#[inline]
pub(crate) fn on() -> bool {
    log().is_some()
}

thread_local!(static BUSY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });

/// Notes a miss of `key` on the host object `o` (`want`: a read, else `in`).
pub(crate) fn note(rt: &mut Realm, o: crate::object::Obj, key: Key, want: bool) {
    let (Some(log), Key::Name(k)) = (log(), key) else { return };
    if k.is_symbol() || BUSY.with(|b| b.replace(true)) {
        return;
    }
    // The tag is the one `Object.prototype.toString` gives: the read can miss again, which `BUSY` stops.
    let tag = rt.get_prop(o.value(), Key::Name(rt.wk[crate::bi_symbol::TO_STRING_TAG])).ok().and_then(Realm::str_of);
    let tag = tag.map_or_else(|| "?".to_string(), |t| rt.text(t));
    let name = rt.text(k);
    BUSY.with(|b| b.set(false));
    log.lock().unwrap_or_else(|e| e.into_inner()).add(tag, name, want);
}

#[cfg(test)]
#[path = "census_count_tests.rs"]
mod tests;
