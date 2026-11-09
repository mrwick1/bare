//! Module scripts for a page session. Child of `browser_loader`: it is part of `Run`.
//!
//! What runs: `<script type=module>` (inline or `src`, from the parsed page or added by a script) and `import()` (in classic scripts and
//! modules). The engine links and evaluates (`bare_js` `Interp::module_*`); this file fetches, resolves and caches.
//!
//! Timing (`resources::discover` decides): a module script is deferred (it runs after the page is parsed, in document order with
//! the `defer` scripts, before `DOMContentLoaded`), or `async` (it runs in the async stage). `nomodule` does nothing on a module.
//! An inline module is not importable and has `import.meta.url` of the document.
//!
//! Graph: the module of a script and its static imports are fetched at the turn of the script, once for each URL in a document (the
//! result is kept in `Modules::ids`; a failure is not tried again). The dependencies of one module start in parallel when there are
//! two or more. A fetch is `Expect::Script` (2xx, JavaScript MIME type, size caps) and counts in the fetch and byte budgets. A text
//! that `<link rel=modulepreload>` fetched is used first. Fetching blocks the page thread, as it does for a classic script, so no
//! module fetch is ever pending outside a task: the page cannot settle while one runs.
//!
//! Specifiers: `/x`, `./x`, `../x` and absolute URLs resolve against the URL of the importing module (the document base for an
//! inline module or `import()` in a classic script). Anything else is a bare specifier: a `TypeError` unless an import map maps it.
//! `<script type=importmap>` (inline only) is read once, at the first resolution: `imports` and `scopes`, with the longest-key rule
//! and `/` prefix keys. A later map is ignored (HTML does the same after the first module load). Not done: `integrity`, `crossorigin`,
//! `src` of an import map, `import(x, { with })` (the engine ignores the options), JSON and CSS modules, worker modules.
//!
//! Outcomes of a module script element:
//! - the module cannot be fetched (the file or a dependency: network, status, MIME, size): an `error` event at the element and a
//!   diagnostic; nothing runs.
//! - a syntax error, a bad specifier or a link error: the exception is reported (`window.onerror`) and the script has no effect, and
//!   an external script still gets `load`.
//! - the module throws, or its top-level `await` rejects (maybe in a later task): the exception is reported. The report comes when
//!   the promise of the evaluation is seen rejected, at the next checkpoint.
//! - the module runs: an external script gets `load` when its evaluation has started.
//!
//! `import()`: the engine queues the call; `after_js` (every microtask checkpoint) answers it: load the graph, link, evaluate, and settle
//! the promise with the namespace. A fetch or resolution failure rejects with a `TypeError`, a syntax error with a `SyntaxError`.

use super::{EvTarget, Flow, Run, Stop, WS, download, label};
use crate::resources::{self, Expect, Target};
use bare_html::dom::NodeId;
use bare_js::eval::{Error, PromiseStatus};
use bare_js::value::{ObjId, Value};
use bare_url::Url;
use std::collections::{HashMap, HashSet};

/// The depth of a chain of imports that the loader follows (it recurses on the page thread).
const MAX_DEPTH: u32 = 128;

/// Module state of one document.
#[derive(Default)]
pub(crate) struct Modules {
    /// Resolved URL to the module id of the engine, for the modules whose graph loaded.
    ids: HashMap<String, u32>,
    /// Resolved URL to why its fetch failed.
    failed: HashMap<String, String>,
    /// `None` until the first resolution.
    map: Option<ImportMap>,
    /// Promises of module evaluations that are not seen settled yet, with the label of the script.
    evals: Vec<(ObjId, String)>,
    /// Script elements that a script added with `type=module`.
    pub(super) nodes: HashSet<NodeId>,
}

/// Why a graph did not load.
pub(super) enum Load {
    /// A download failed: the element gets `error`.
    Net(String),
    /// A syntax error or a bad specifier: reported as an exception.
    Throw(Value),
    Fatal(Error),
}

// ------------------------------------------------------------------------------------------- import maps

#[derive(Default)]
struct ImportMap {
    imports: Vec<(String, Option<String>)>,
    /// (resolved scope prefix, entries), the longest prefix first.
    scopes: Vec<(String, Vec<(String, Option<String>)>)>,
}

enum Json {
    Obj(Vec<(String, Json)>),
    Str(String),
    Other,
}

/// A small JSON reader: objects and strings are kept, any other value is `Other`. `None`: not JSON.
fn json(s: &str) -> Option<Json> {
    let b = s.as_bytes();
    let mut i = 0;
    let v = json_value(s, b, &mut i, 0)?;
    skip_ws(b, &mut i);
    (i == b.len()).then_some(v)
}

fn skip_ws(b: &[u8], i: &mut usize) {
    while *i < b.len() && matches!(b[*i], b' ' | b'\t' | b'\n' | b'\r') {
        *i += 1;
    }
}

fn json_value(s: &str, b: &[u8], i: &mut usize, depth: u32) -> Option<Json> {
    if depth > 32 {
        return None;
    }
    skip_ws(b, i);
    match *b.get(*i)? {
        b'{' => {
            *i += 1;
            let mut out = Vec::new();
            skip_ws(b, i);
            if b.get(*i) == Some(&b'}') {
                *i += 1;
                return Some(Json::Obj(out));
            }
            loop {
                skip_ws(b, i);
                let Json::Str(k) = json_value(s, b, i, depth + 1)? else { return None };
                skip_ws(b, i);
                if b.get(*i) != Some(&b':') {
                    return None;
                }
                *i += 1;
                out.push((k, json_value(s, b, i, depth + 1)?));
                skip_ws(b, i);
                match b.get(*i)? {
                    b',' => *i += 1,
                    b'}' => {
                        *i += 1;
                        return Some(Json::Obj(out));
                    }
                    _ => return None,
                }
            }
        }
        b'[' => {
            *i += 1;
            skip_ws(b, i);
            if b.get(*i) == Some(&b']') {
                *i += 1;
                return Some(Json::Other);
            }
            loop {
                json_value(s, b, i, depth + 1)?;
                skip_ws(b, i);
                match b.get(*i)? {
                    b',' => *i += 1,
                    b']' => {
                        *i += 1;
                        return Some(Json::Other);
                    }
                    _ => return None,
                }
            }
        }
        b'"' => {
            *i += 1;
            let mut out = String::new();
            loop {
                let c = s[*i..].chars().next()?;
                *i += c.len_utf8();
                match c {
                    '"' => return Some(Json::Str(out)),
                    '\\' => {
                        let e = s[*i..].chars().next()?;
                        *i += e.len_utf8();
                        match e {
                            '"' | '\\' | '/' => out.push(e),
                            'b' => out.push('\u{8}'),
                            'f' => out.push('\u{c}'),
                            'n' => out.push('\n'),
                            'r' => out.push('\r'),
                            't' => out.push('\t'),
                            'u' => {
                                let hex = s.get(*i..*i + 4)?;
                                let n = u32::from_str_radix(hex, 16).ok()?;
                                *i += 4;
                                out.push(char::from_u32(n).unwrap_or(char::REPLACEMENT_CHARACTER));
                            }
                            _ => return None,
                        }
                    }
                    c if (c as u32) < 0x20 => return None,
                    c => out.push(c),
                }
            }
        }
        _ => {
            // A number, `true`, `false` or `null`.
            let start = *i;
            while *i < b.len() && !matches!(b[*i], b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r') {
                *i += 1;
            }
            let t = &s[start..*i];
            (matches!(t, "true" | "false" | "null") || t.parse::<f64>().is_ok()).then_some(Json::Other)
        }
    }
}

/// `spec` as a URL when it starts with `/`, `./` or `../`, or is absolute.
fn url_like(spec: &str, base: &Url) -> Option<Url> {
    if spec.starts_with('/') || spec.starts_with("./") || spec.starts_with("../") {
        return base.join(spec).ok();
    }
    Url::parse(spec).ok()
}

/// The entries of an `imports` object: keys normalized as URLs when they are URL-like, values resolved against `base`. A value that is
/// not a good URL (or a prefix value that lacks the final `/`) is `None`: the key is blocked.
fn map_entries(obj: &[(String, Json)], base: &Url) -> Vec<(String, Option<String>)> {
    let mut out: Vec<(String, Option<String>)> = Vec::new();
    for (k, v) in obj {
        if k.is_empty() {
            continue;
        }
        let key = url_like(k, base).map_or_else(|| k.clone(), |u| u.to_string());
        let val = match v {
            Json::Str(s) => url_like(s, base).map(|u| u.to_string()).filter(|u| !key.ends_with('/') || u.ends_with('/')),
            _ => None,
        };
        if !out.iter().any(|(k2, _)| *k2 == key) {
            out.push((key, val));
        }
    }
    // Longest first: a longer prefix key wins over a shorter one.
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out
}

impl ImportMap {
    /// Add the map in `text` (the later map never replaces a key that is set). A map that is not valid is skipped.
    fn add(&mut self, text: &str, base: &Url) -> bool {
        let Some(Json::Obj(top)) = json(text) else { return false };
        for (k, v) in &top {
            match (k.as_str(), v) {
                ("imports", Json::Obj(o)) => {
                    for e in map_entries(o, base) {
                        if !self.imports.iter().any(|(k2, _)| *k2 == e.0) {
                            self.imports.push(e);
                        }
                    }
                    self.imports.sort_by(|a, b| b.0.cmp(&a.0));
                }
                ("scopes", Json::Obj(o)) => {
                    for (prefix, entries) in o {
                        let Json::Obj(entries) = entries else { continue };
                        let Some(p) = url_like(prefix, base).map(|u| u.to_string()) else { continue };
                        let mut new = map_entries(entries, base);
                        match self.scopes.iter_mut().find(|(p2, _)| *p2 == p) {
                            Some((_, old)) => {
                                new.retain(|(k, _)| !old.iter().any(|(k2, _)| k2 == k));
                                old.extend(new);
                                old.sort_by(|a, b| b.0.cmp(&a.0));
                            }
                            None => self.scopes.push((p, new)),
                        }
                    }
                    self.scopes.sort_by(|a, b| b.0.cmp(&a.0));
                }
                _ => {}
            }
        }
        true
    }
}

/// The URL that `entries` give for `key`: `Some(Ok(url))`, `Some(Err(()))` for a blocked key, `None` for no match.
fn lookup(entries: &[(String, Option<String>)], key: &str) -> Option<Result<String, ()>> {
    for (k, v) in entries {
        if k == key {
            return Some(v.clone().ok_or(()));
        }
        if k.ends_with('/') && key.starts_with(k.as_str()) {
            return Some(match v {
                Some(v) => Ok(format!("{v}{}", &key[k.len()..])),
                None => Err(()),
            });
        }
    }
    None
}

impl Modules {
    /// The URL for `spec` imported by the module at `referrer`, by the import map.
    fn resolve(&self, spec: &str, referrer: &Url) -> Result<Url, String> {
        let as_url = url_like(spec, referrer);
        let key = as_url.as_ref().map_or_else(|| spec.to_string(), |u| u.to_string());
        let from = referrer.to_string();
        let mut hit = None;
        if let Some(map) = &self.map {
            for (prefix, entries) in &map.scopes {
                if from == *prefix || (prefix.ends_with('/') && from.starts_with(prefix.as_str())) {
                    hit = lookup(entries, &key);
                    if hit.is_some() {
                        break;
                    }
                }
            }
            if hit.is_none() {
                hit = lookup(&map.imports, &key);
            }
        }
        match hit {
            Some(Ok(u)) => Url::parse(&u),
            Some(Err(())) => Err(format!("Failed to resolve module specifier \"{spec}\". The import map blocks it.")),
            None => as_url.ok_or_else(|| format!("Failed to resolve module specifier \"{spec}\". Relative references must start with either \"/\", \"./\", or \"../\".")),
        }
    }
}

// ------------------------------------------------------------------------------------------- the loader

fn copy(u: &Url) -> Url {
    Url { scheme: u.scheme, host: u.host.clone(), port: u.port, path: u.path.clone() }
}

impl Run<'_> {
    /// The base URL of the document: where an inline module and `import()` in a classic script resolve from.
    pub(super) fn doc_base(&self) -> Option<Url> {
        self.dom.scripts.base.borrow().as_deref().and_then(|b| Url::parse(b).ok())
    }

    /// Read the import maps of the document, once, at the first resolution.
    fn ensure_map(&mut self, base: &Url) {
        if self.dom.scripts.modules.borrow().map.is_some() {
            return;
        }
        let mut map = ImportMap::default();
        let texts: Vec<String> = {
            let doc = self.dom.doc.borrow();
            let (script, typ) = (doc.find_atom("script"), doc.find_atom("type"));
            let mut out = Vec::new();
            let mut stack = vec![doc.root()];
            while let Some(n) = stack.pop() {
                if let bare_html::dom::NodeData::Element { name, .. } = *doc.data(n)
                    && Some(name) == script
                    && !doc.is_foreign(n)
                    && typ.and_then(|t| doc.attr(n, t)).is_some_and(|t| t.trim_matches(WS).eq_ignore_ascii_case("importmap"))
                    && doc.find_atom("src").and_then(|a| doc.attr(n, a)).is_none()
                {
                    out.push(resources::inline_source(&doc, n).into_owned());
                }
                let mut c = doc.last_child(n);
                while let Some(k) = c {
                    stack.push(k);
                    c = doc.prev_sibling(k);
                }
            }
            out
        };
        for t in texts {
            if !map.add(&t, base) {
                self.diag("import map is not valid JSON: ignored".into());
            }
        }
        self.dom.scripts.modules.borrow_mut().map = Some(map);
    }

    /// A `TypeError` object (the value to throw).
    fn type_error_value(&mut self, msg: &str) -> Value {
        match self.b.it.type_error(msg) {
            Error::Throw(v) => v,
            _ => Value::Undefined,
        }
    }

    fn resolve_spec(&mut self, spec: &str, referrer: &Url) -> Result<Url, Load> {
        self.ensure_map(referrer);
        let r = self.dom.scripts.modules.borrow().resolve(spec, referrer);
        r.map_err(|m| Load::Throw(self.type_error_value(&m)))
    }

    /// The text of the module at `url`: a `modulepreload` result, else a download. A failure is kept.
    fn module_text(&mut self, url: &Url) -> Result<String, String> {
        let key = url.to_string();
        if let Some(why) = self.dom.scripts.modules.borrow().failed.get(&key) {
            return Err(why.clone());
        }
        let preloaded = self.dom.scripts.preloads.borrow_mut().take_script(url);
        let r = match preloaded {
            Some(r) => r,
            None => {
                let kept = self.kept();
                let target = Target { expect: Expect::Script, url: copy(url) };
                download(&mut self.net, self.https_only, &self.lim, self.deadline, kept, &mut self.res, &target)
            }
        };
        if let Err(why) = &r {
            self.dom.scripts.modules.borrow_mut().failed.insert(key, why.clone());
        }
        r
    }

    /// Load the module at `url` and everything it imports. `text` is the source when the caller has downloaded it. On an error the
    /// modules that this call made are forgotten, so that no module with a missing import stays in the cache.
    pub(super) fn load_graph(&mut self, url: &Url, text: Option<String>) -> Result<u32, Load> {
        let mut made = Vec::new();
        let r = self.load_rec(url, text, 0, &mut made);
        if r.is_err() {
            let mut m = self.dom.scripts.modules.borrow_mut();
            for k in made {
                m.ids.remove(&k);
            }
        }
        r
    }

    /// An inline module: parsed with the document URL, not cached by URL.
    fn load_inline(&mut self, base: &Url, text: &str) -> Result<u32, Load> {
        let mut made = Vec::new();
        let id = match self.b.it.module_parse(text, &base.to_string()) {
            Ok(id) => id,
            Err(Error::Throw(v)) => return Err(Load::Throw(v)),
            Err(e) => return Err(Load::Fatal(e)),
        };
        let r = self.load_deps(id, base, 0, &mut made).map(|()| id);
        if r.is_err() {
            let mut m = self.dom.scripts.modules.borrow_mut();
            for k in made {
                m.ids.remove(&k);
            }
        }
        r
    }

    fn load_rec(&mut self, url: &Url, text: Option<String>, depth: u32, made: &mut Vec<String>) -> Result<u32, Load> {
        let key = url.to_string();
        if let Some(&id) = self.dom.scripts.modules.borrow().ids.get(&key) {
            return Ok(id);
        }
        if depth > MAX_DEPTH {
            return Err(Load::Net(format!("module graph deeper than {MAX_DEPTH}")));
        }
        let text = match text {
            Some(t) => t,
            None => self.module_text(url).map_err(Load::Net)?,
        };
        let id = match self.b.it.module_parse(&text, &key) {
            Ok(id) => id,
            Err(Error::Throw(v)) => return Err(Load::Throw(v)),
            Err(e) => return Err(Load::Fatal(e)),
        };
        self.dom.scripts.modules.borrow_mut().ids.insert(key.clone(), id);
        made.push(key);
        self.load_deps(id, url, depth, made)?;
        Ok(id)
    }

    /// Resolve the imports of module `id` (at `url`), fetch them (in parallel from two), and give them to the engine.
    fn load_deps(&mut self, id: u32, url: &Url, depth: u32, made: &mut Vec<String>) -> Result<(), Load> {
        let specs = self.b.it.module_requests(id);
        let mut urls = Vec::with_capacity(specs.len());
        for s in &specs {
            urls.push(self.resolve_spec(s, url)?);
        }
        {
            let m = self.dom.scripts.modules.borrow();
            let preloads = self.dom.scripts.preloads.borrow();
            let todo: Vec<&Url> = urls
                .iter()
                .filter(|u| !m.ids.contains_key(&u.to_string()) && !m.failed.contains_key(&u.to_string()) && !preloads.has_script(u))
                .filter(|u| !(self.https_only && u.scheme == bare_url::Scheme::Http))
                .collect();
            if todo.len() >= 2 || (!todo.is_empty() && self.net.client.parallel_pays()) {
                let kept = self.kept();
                for u in todo {
                    self.net.start(Expect::Script, u, &self.lim.resources, self.deadline, kept);
                }
            }
        }
        let mut deps = Vec::with_capacity(urls.len());
        for u in &urls {
            deps.push(self.load_rec(u, None, depth + 1, made)?);
        }
        self.b.it.module_set_requested(id, &deps);
        Ok(())
    }

    /// A module script element has its text (or why it failed): load the graph, run it, and send the events.
    pub(super) fn finish_module(&mut self, node: NodeId, what: &str, url: &Url, text: Result<String, String>, external: bool) -> Flow {
        self.budget()?;
        let text = match text {
            Ok(t) => t,
            Err(why) => return self.module_failed(node, what, &why),
        };
        self.res.scripts_run += 1;
        self.res.tasks += 1;
        let graph = if external { self.load_graph(url, Some(text)) } else { self.load_inline(url, &text) };
        match graph {
            Err(Load::Net(why)) => return self.module_failed(node, what, &why),
            Err(Load::Throw(v)) => self.script_threw(v, what),
            Err(Load::Fatal(e)) => return Err(self.fatal(e, what)),
            Ok(id) => self.run_graph(id, what)?,
        }
        if external {
            self.after_js()?;
            self.fire(EvTarget::Node(node), "load", false)?;
        }
        Ok(())
    }

    /// Link and evaluate. A link error is reported. The promise of the evaluation is watched (`settle_evals`).
    fn run_graph(&mut self, id: u32, what: &str) -> Flow {
        match self.b.it.module_link(id) {
            Ok(()) => {}
            Err(Error::Throw(v)) => {
                self.script_threw(v, what);
                return Ok(());
            }
            Err(e) => return Err(self.fatal(e, what)),
        }
        match self.b.it.module_evaluate(id) {
            Ok(p) => self.dom.scripts.modules.borrow_mut().evals.push((p, what.to_string())),
            Err(Error::Throw(v)) => self.script_threw(v, what),
            Err(e) => return Err(self.fatal(e, what)),
        }
        Ok(())
    }

    fn module_failed(&mut self, node: NodeId, what: &str, why: &str) -> Flow {
        self.res.tasks += 1;
        self.res.load_errors += 1;
        self.diag(format!("script failed: {what}: {why}"));
        self.fire(EvTarget::Node(node), "error", false)
    }

    /// Report the evaluations that were seen rejected since the last checkpoint.
    pub(super) fn settle_evals(&mut self) {
        let list = std::mem::take(&mut self.dom.scripts.modules.borrow_mut().evals);
        if list.is_empty() {
            return;
        }
        let mut keep = Vec::new();
        for (p, what) in list {
            match self.b.it.promise_status(Value::Object(p)) {
                Some(PromiseStatus::Pending) => keep.push((p, what)),
                Some(PromiseStatus::Rejected(v)) => self.script_threw(v, &what),
                _ => {}
            }
        }
        self.dom.scripts.modules.borrow_mut().evals.extend(keep);
    }

    /// Answer the `import()` calls that ran. True when there were any (the caller runs the microtasks again).
    pub(super) fn serve_imports(&mut self) -> Result<bool, Stop> {
        let reqs = self.b.it.take_dynamic_imports();
        if reqs.is_empty() {
            return Ok(false);
        }
        for r in reqs {
            self.budget()?;
            self.res.tasks += 1;
            let base = match r.referrer {
                Some(m) => Url::parse(self.b.it.module_url(m)).ok(),
                None => self.doc_base(),
            };
            let loaded = match base {
                None => Err(Load::Net("no base URL".into())),
                Some(base) => match self.resolve_spec(&r.specifier, &base) {
                    Ok(u) => self.load_graph(&u, None).map_err(|e| match e {
                        Load::Net(why) => Load::Net(format!("Failed to fetch dynamically imported module: {}: {why}", label(&u))),
                        e => e,
                    }),
                    Err(e) => Err(e),
                },
            };
            let done = match loaded {
                Ok(id) => self.b.it.finish_dynamic_import(r.promise, id),
                Err(Load::Net(msg)) => {
                    self.diag(msg.clone());
                    self.b.it.fail_dynamic_import(r.promise, &msg)
                }
                Err(Load::Throw(v)) => self.b.it.reject_dynamic_import(r.promise, Error::Throw(v)),
                Err(Load::Fatal(e)) => return Err(self.fatal(e, "import()")),
            };
            if let Err(e) = done {
                return Err(self.fatal(e, "import()"));
            }
        }
        Ok(true)
    }
}
