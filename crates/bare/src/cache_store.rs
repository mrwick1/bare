//! The Cache API store of a session (`caches`, step 6 of `docs/internal/design-service-workers.md`). It holds data only, no heap
//! object: the caches of each origin, in creation order, and the entries of each cache, in insertion order. Pages, dedicated workers
//! on a thread and service workers of the session share it (`WebStorage`). A profile keeps it in `caches.bin` (`profile.rs`).
//! `shims/caches.js` is the realm side; `browser_caches.rs` is the native op.
//!
//! - An entry is a GET request (its URL with no fragment, and the request headers that its response's `Vary` names) and a response
//!   (status, status text, headers, body, URL). A body is an `Arc`, so a match copies no bytes.
//! - The query follows "Query Cache" of the standard: a request that is not GET matches nothing unless `ignoreMethod`; `ignoreSearch`
//!   drops the query of both URLs; with no `ignoreVary` each header that `Vary` names must be equal, and `Vary: *` never matches.
//! - `put` removes the entries that the new request matches, then adds the new entry at the end.

use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq)]
pub struct CacheEntry {
    /// The request URL with no fragment.
    pub url: String,
    /// The request headers that the `Vary` header of the response names (names in lower case).
    pub vary: Vec<(String, String)>,
    pub status: u16,
    pub status_text: String,
    pub headers: Vec<(String, String)>,
    pub body: Arc<[u8]>,
    /// The URL of the response.
    pub resp_url: String,
}

/// A cache as the profile keeps it: origin, name, entries.
pub type SavedCache = (String, String, Vec<CacheEntry>);

/// A query: the request, and the options of `match`, `matchAll`, `delete` and `keys`.
pub(crate) struct Query<'a> {
    pub url: &'a str,
    pub method: &'a str,
    /// The request headers (names in lower case).
    pub headers: &'a [(String, String)],
    pub ignore_search: bool,
    pub ignore_method: bool,
    pub ignore_vary: bool,
}

#[derive(Default)]
pub(crate) struct Caches {
    by_origin: HashMap<String, Vec<(String, Vec<CacheEntry>)>>,
    /// Moves when the data changes (the profile writes it again).
    pub rev: u64,
}

fn no_fragment(u: &str) -> &str {
    u.split('#').next().unwrap_or("")
}

fn no_search(u: &str) -> &str {
    no_fragment(u).split('?').next().unwrap_or("")
}

/// The header names of the `Vary` values in `headers`, in lower case.
pub(crate) fn vary_names(headers: &[(String, String)]) -> Vec<String> {
    headers.iter().filter(|(k, _)| k.eq_ignore_ascii_case("vary")).flat_map(|(_, v)| v.split(',')).map(|n| n.trim().to_ascii_lowercase()).filter(|n| !n.is_empty()).collect()
}

fn header<'h>(list: &'h [(String, String)], name: &str) -> Option<&'h str> {
    list.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
}

impl Query<'_> {
    fn matches(&self, e: &CacheEntry) -> bool {
        let same = if self.ignore_search { no_search(self.url) == no_search(&e.url) } else { no_fragment(self.url) == e.url };
        if !same {
            return false;
        }
        if self.ignore_vary {
            return true;
        }
        vary_names(&e.headers).iter().all(|n| n != "*" && header(self.headers, n) == header(&e.vary, n))
    }

    /// A query that is not GET finds nothing unless `ignoreMethod`.
    fn usable(&self) -> bool {
        self.ignore_method || self.method.eq_ignore_ascii_case("GET")
    }
}

impl Caches {
    fn list(&self, origin: &str) -> &[(String, Vec<CacheEntry>)] {
        self.by_origin.get(origin).map_or(&[], Vec::as_slice)
    }

    fn cache_mut(&mut self, origin: &str, name: &str) -> Option<&mut Vec<CacheEntry>> {
        self.by_origin.get_mut(origin)?.iter_mut().find(|c| c.0 == name).map(|c| &mut c.1)
    }

    /// `caches.open(name)`: make the cache when it is not there.
    pub(crate) fn open(&mut self, origin: &str, name: &str) {
        let list = self.by_origin.entry(origin.to_string()).or_default();
        if !list.iter().any(|c| c.0 == name) {
            list.push((name.to_string(), Vec::new()));
            self.rev += 1;
        }
    }

    pub(crate) fn has(&self, origin: &str, name: &str) -> bool {
        self.list(origin).iter().any(|c| c.0 == name)
    }

    /// `caches.delete(name)`.
    pub(crate) fn delete(&mut self, origin: &str, name: &str) -> bool {
        let Some(list) = self.by_origin.get_mut(origin) else { return false };
        let n = list.len();
        list.retain(|c| c.0 != name);
        let gone = list.len() != n;
        if gone {
            self.rev += 1;
        }
        gone
    }

    /// `caches.keys()`: the names in creation order.
    pub(crate) fn names(&self, origin: &str) -> Vec<String> {
        self.list(origin).iter().map(|c| c.0.clone()).collect()
    }

    /// `cache.put`: the entries that the request matches go, the new one goes at the end. False when the cache is gone.
    pub(crate) fn put(&mut self, origin: &str, name: &str, headers: &[(String, String)], e: CacheEntry) -> bool {
        let Some(c) = self.cache_mut(origin, name) else { return false };
        let q = Query { url: &e.url, method: "GET", headers, ignore_search: false, ignore_method: false, ignore_vary: false };
        c.retain(|old| !q.matches(old));
        c.push(e);
        self.rev += 1;
        true
    }

    /// The entries that `q` matches, in order: in the cache `name`, or in all the caches of the origin (`None`). `first`: stop at
    /// the first one. A query with no URL (`keys()`, `matchAll()`) takes all the entries.
    pub(crate) fn find(&self, origin: &str, name: Option<&str>, q: Option<&Query>, first: bool) -> Vec<CacheEntry> {
        let mut out = Vec::new();
        if q.is_some_and(|q| !q.usable()) {
            return out;
        }
        for (n, entries) in self.list(origin) {
            if name.is_some_and(|x| x != n) {
                continue;
            }
            for e in entries {
                if q.is_none_or(|q| q.matches(e)) {
                    out.push(e.clone());
                    if first {
                        return out;
                    }
                }
            }
        }
        out
    }

    /// `cache.delete(request)`: false when nothing matched.
    pub(crate) fn delete_entries(&mut self, origin: &str, name: &str, q: &Query) -> bool {
        if !q.usable() {
            return false;
        }
        let Some(c) = self.cache_mut(origin, name) else { return false };
        let n = c.len();
        c.retain(|e| !q.matches(e));
        let gone = c.len() != n;
        if gone {
            self.rev += 1;
        }
        gone
    }

    /// The caches of all origins, for the profile. An opaque origin ("null") is not kept.
    pub(crate) fn export(&self) -> Vec<SavedCache> {
        let mut origins: Vec<&String> = self.by_origin.keys().filter(|o| o.as_str() != "null").collect();
        origins.sort();
        origins.into_iter().flat_map(|o| self.by_origin[o].iter().map(move |(n, e)| (o.clone(), n.clone(), e.clone()))).collect()
    }

    /// Caches from the profile. A cache that exists already stays as it is.
    pub(crate) fn import(&mut self, list: &[SavedCache]) {
        for (origin, name, entries) in list {
            let caches = self.by_origin.entry(origin.clone()).or_default();
            if !caches.iter().any(|c| &c.0 == name) {
                caches.push((name.clone(), entries.clone()));
            }
        }
    }
}
