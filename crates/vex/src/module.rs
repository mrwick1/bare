//! Modules of the realm (`docs/design-js2-host-api.md` section 12): the table of module records, link, evaluate, namespace objects and
//! the native behind `import()`. The compiler is `compile_module.rs`; the methods of the host interface are `api_module.rs`.
//!
//! Every value that a record holds is reachable from `func` (the function cell, which holds the context, which holds the environment
//! object, the `import.meta` object and the views), and `func`, the namespace, the error and the promise are rooted with
//! [`Realm::keep`]: a module lives as long as the realm.
use crate::builtins::{Native, arg};
use crate::compile::{ExpSrc, ModInfo};
use crate::interp::STOP;
use crate::interp_rt::{ErrKind, Key, R, Realm};
use crate::object::{Ctx, Obj};
use crate::cell::Layout;
use crate::value::Value;
use bare_js_api::ImportRequest;
use std::rc::Rc;

/// A request that the host has not answered.
pub(crate) const NO_MOD: u32 = u32::MAX;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum St {
    New,
    Linked,
    Evaluating,
    /// Done with the sync part, and waiting for a top-level `await` of itself or of a module that it needs (16.2.1.5.3).
    EvaluatingAsync,
    Evaluated,
}

struct ModRec {
    url: String,
    code: usize,
    info: Rc<ModInfo>,
    requested: Vec<u32>,
    state: St,
    /// The function cell with the context, and the environment object (`undefined` until the module is linked).
    func: Value,
    env: Value,
    ns: Value,
    /// The error of a failed evaluation, and the promise of `module_evaluate`.
    error: Option<Value>,
    done: Value,
    /// Cyclic module record fields of the async evaluation: the order number of a module that waits (0: it does not), the number of
    /// modules that it waits for, the modules that wait for it, the root of its strongly connected set, and the depth-first numbers.
    async_eval: u32,
    pending: u32,
    parents: Vec<u32>,
    cycle_root: u32,
    dfs: u32,
    anc: u32,
}

#[derive(Default)]
pub(crate) struct Modules {
    mods: Vec<ModRec>,
    pending: Vec<ImportRequest>,
    /// The indexes in `Realm::natives` of the getter of a view, the getter of a namespace and `%import`.
    id_view: usize,
    id_ns: usize,
    /// The natives that settle a module (`async_ok`, `async_err`: slot 1 is the module number) and a dynamic import (`import_ok`,
    /// `import_err`: slot 1 is the promise, slot 2 the module number).
    id_async: [usize; 4],
    /// The last order number of an async evaluation.
    async_count: u32,
}

/// What `ResolveExport` finds.
#[derive(Clone, PartialEq, Debug)]
enum Rx {
    Null,
    Ambiguous,
    /// The property `key` of the environment object of module `m`.
    Bind(u32, Box<str>),
    Namespace(u32),
}

impl Realm {
    // ---- the table ----

    /// Add a module that the compiler made. Gives its number.
    pub(crate) fn md_add(&mut self, url: &str, code: usize, info: ModInfo) -> u32 {
        let n = info.requests.len();
        let id = self.md.mods.len() as u32;
        self.md.mods.push(ModRec {
            url: url.to_owned(),
            code,
            info: Rc::new(info),
            requested: vec![NO_MOD; n],
            state: St::New,
            func: Value::UNDEFINED,
            env: Value::UNDEFINED,
            ns: Value::UNDEFINED,
            error: None,
            done: Value::UNDEFINED,
            async_eval: 0,
            pending: 0,
            parents: Vec::new(),
            cycle_root: id,
            dfs: 0,
            anc: 0,
        });
        id
    }

    pub(crate) fn md_requests(&self, m: u32) -> Vec<String> {
        self.md.mods[m as usize].info.requests.iter().map(|r| r.to_string()).collect()
    }

    pub(crate) fn md_set_requested(&mut self, m: u32, deps: &[u32]) {
        let r = &mut self.md.mods[m as usize].requested;
        for (slot, &d) in r.iter_mut().zip(deps) {
            *slot = d;
        }
    }

    pub(crate) fn md_url(&self, m: u32) -> &str {
        &self.md.mods[m as usize].url
    }

    pub(crate) fn md_known(&self, m: u32) -> bool {
        (m as usize) < self.md.mods.len()
    }

    pub(crate) fn md_take_requests(&mut self) -> Vec<ImportRequest> {
        std::mem::take(&mut self.md.pending)
    }

    // ---- ResolveExport ----

    fn resolve_export(&self, m: u32, name: &str, seen: &mut Vec<(u32, Box<str>)>) -> Rx {
        if seen.iter().any(|(sm, sn)| *sm == m && &**sn == name) {
            return Rx::Null;
        }
        seen.push((m, name.into()));
        let rec = &self.md.mods[m as usize];
        for e in rec.info.exports.iter().filter(|e| &*e.name == name) {
            return match &e.src {
                ExpSrc::Local(key) => Rx::Bind(m, key.clone()),
                ExpSrc::Indirect(req, Some(n)) => self.resolve_export(rec.requested[*req as usize], n, seen),
                ExpSrc::Indirect(req, None) => Rx::Namespace(rec.requested[*req as usize]),
            };
        }
        if name == "default" {
            return Rx::Null;
        }
        let mut star = Rx::Null;
        for &req in &rec.info.stars {
            match self.resolve_export(rec.requested[req as usize], name, seen) {
                Rx::Ambiguous => return Rx::Ambiguous,
                Rx::Null => {}
                r if star == Rx::Null => star = r,
                r if r != star => return Rx::Ambiguous,
                _ => {}
            }
        }
        star
    }

    /// `GetExportedNames`.
    fn export_names(&self, m: u32, seen: &mut Vec<u32>, out: &mut Vec<Box<str>>) {
        if seen.contains(&m) {
            return;
        }
        seen.push(m);
        let rec = &self.md.mods[m as usize];
        out.extend(rec.info.exports.iter().map(|e| e.name.clone()));
        for &req in &rec.info.stars {
            let mut names = Vec::new();
            self.export_names(rec.requested[req as usize], seen, &mut names);
            for n in names {
                if &*n != "default" && !out.contains(&n) {
                    out.push(n);
                }
            }
        }
    }

    // ---- link ----

    /// `module_link`: check the imports of the graph of `root`, then make the contexts and the environments, then run phase 1.
    pub(crate) fn md_link(&mut self, root: u32) -> R<()> {
        let mut graph: Vec<u32> = Vec::new();
        let mut todo = vec![root];
        while let Some(m) = todo.pop() {
            if graph.contains(&m) || self.md.mods[m as usize].state != St::New {
                continue;
            }
            graph.push(m);
            for (i, &d) in self.md.mods[m as usize].requested.iter().enumerate() {
                if d == NO_MOD {
                    let msg = format!("the module '{}' of '{}' was not loaded", self.md.mods[m as usize].info.requests[i], self.md.mods[m as usize].url);
                    return Err(self.error(ErrKind::Type, &msg));
                }
                if !self.md_known(d) {
                    return Err(self.error(ErrKind::Type, "a request names a module that does not exist"));
                }
                todo.push(d);
            }
        }
        for &m in &graph {
            self.md_check(m)?;
        }
        for &m in &graph {
            self.md_instantiate(m);
        }
        for &m in &graph {
            self.md_bind(m)?;
        }
        for &m in &graph {
            let f = self.md.mods[m as usize].func;
            self.md_call(f, 1)?;
            self.md.mods[m as usize].state = St::Linked;
        }
        Ok(())
    }

    fn md_call(&mut self, f: Value, phase: i32) -> R<()> {
        self.api_call(f, Value::UNDEFINED, &[Value::int(phase)]).map(|_| ())
    }

    /// Call the code of a module with a top-level `await`: it gives its promise.
    fn md_call_async(&mut self, f: Value, phase: i32) -> R<Value> {
        self.api_call(f, Value::UNDEFINED, &[Value::int(phase)])
    }

    /// A bad import or re-export is a `SyntaxError`.
    fn md_check(&mut self, m: u32) -> R<()> {
        let rec = &self.md.mods[m as usize];
        let mut bad: Option<String> = None;
        let wanted = rec.info.imports.iter().filter_map(|i| i.name.as_ref().map(|n| (i.req, n))).chain(rec.info.exports.iter().filter_map(|e| match &e.src {
            ExpSrc::Indirect(req, Some(n)) => Some((*req, n)),
            _ => None,
        }));
        for (req, n) in wanted {
            let dep = rec.requested[req as usize];
            let spec = &rec.info.requests[req as usize];
            match self.resolve_export(dep, n, &mut Vec::new()) {
                Rx::Null => bad = Some(format!("The requested module '{spec}' does not provide an export named '{n}'")),
                Rx::Ambiguous => bad = Some(format!("The requested module '{spec}' contains conflicting star exports for the name '{n}'")),
                _ => {}
            }
            if bad.is_some() {
                break;
            }
        }
        match bad {
            Some(msg) => Err(self.error(ErrKind::Syntax, &msg)),
            None => Ok(()),
        }
    }

    /// The context, the environment object and the function cell of module `m`.
    fn md_instantiate(&mut self, m: u32) {
        let info = self.md.mods[m as usize].info.clone();
        let code = self.md.mods[m as usize].code;
        let cx = Ctx::new(&mut self.heap, info.slots as usize + 1, Value::HOLE).expect("memory");
        cx.set(0, Value::UNDEFINED);
        let env = Obj::new(&mut self.heap, &self.shapes, self.s_object).expect("memory");
        for (key, hole) in &info.env_props {
            let k = self.intern(key);
            self.define(env, Key::Name(k), if *hole { Value::HOLE } else { Value::UNDEFINED }).expect("define");
        }
        cx.set(1, env.value());
        for &s in &info.undef {
            cx.set(1 + s as usize, Value::UNDEFINED);
        }
        cx.set(1 + info.id_slot as usize, Value::int(m as i32));
        let meta = Obj::new(&mut self.heap, &self.shapes, self.s_object).expect("memory");
        let (k, url) = (self.intern("url"), self.string(&self.md.mods[m as usize].url.clone()));
        self.define(meta, Key::Name(k), url).expect("define");
        cx.set(1 + info.meta_slot as usize, meta.value());
        let f = self.new_function(code, cx.value());
        self.keep(f);
        let rec = &mut self.md.mods[m as usize];
        rec.func = f;
        rec.env = env.value();
    }

    /// Fill the slots of the imports of module `m`: an environment object, a view, or a namespace.
    fn md_bind(&mut self, m: u32) -> R<()> {
        let info = self.md.mods[m as usize].info.clone();
        let cx = self.md_ctx(m);
        for imp in &info.imports {
            let dep = self.md.mods[m as usize].requested[imp.req as usize];
            let v = match &imp.name {
                None => self.md_namespace(dep),
                Some(n) => match self.resolve_export(dep, n, &mut Vec::new()) {
                    Rx::Bind(mm, key) if key == *n => self.md.mods[mm as usize].env,
                    Rx::Bind(mm, key) => {
                        let env = self.md.mods[mm as usize].env;
                        self.md_view(n, ViewOf::Binding(env, &key))
                    }
                    Rx::Namespace(mm) => {
                        let ns = self.md_namespace(mm);
                        self.md_view(n, ViewOf::Namespace(ns))
                    }
                    Rx::Null | Rx::Ambiguous => unreachable!("md_check found every import"),
                },
            };
            cx.set(1 + imp.slot as usize, v);
        }
        Ok(())
    }

    fn md_ctx(&self, m: u32) -> Ctx {
        let f = Realm::obj(self.md.mods[m as usize].func).expect("a module function");
        Realm::ctx(f.slot(&self.shapes, 1)).expect("a module context")
    }

    /// An object with one property `name`: an accessor that reads the binding, or the namespace.
    fn md_view(&mut self, name: &str, of: ViewOf) -> Value {
        let o = Obj::new(&mut self.heap, &self.shapes, self.s_object).expect("memory");
        let k = Key::Name(self.intern(name));
        match of {
            ViewOf::Namespace(ns) => self.define(o, k, ns).expect("define"),
            ViewOf::Binding(env, key) => {
                let g = self.md_getter(self.md.id_view, env, key);
                self.define_accessor(o, k, g, false, true).expect("define");
            }
        }
        o.value()
    }

    /// A native getter with slot 1 the environment object and slot 2 the name of the property.
    fn md_getter(&mut self, id: usize, env: Value, key: &str) -> Value {
        let g = self.native_cell(id);
        let k = self.intern(key);
        g.set_slot(&self.shapes, 1, env);
        g.set_slot(&self.shapes, 2, k.value());
        g.value()
    }

    // ---- namespace ----

    /// The namespace object of module `m`, made at the first request.
    pub(crate) fn md_namespace(&mut self, m: u32) -> Value {
        let ns = self.md.mods[m as usize].ns;
        if !ns.is_undefined() {
            return ns;
        }
        let shape = self.shapes.root(Layout::Object, None, 4);
        let o = Obj::new(&mut self.heap, &self.shapes, shape).expect("memory");
        self.md.mods[m as usize].ns = o.value();
        self.keep(o.value());
        let mut names = Vec::new();
        self.export_names(m, &mut Vec::new(), &mut names);
        names.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
        names.dedup();
        for n in names {
            let k = Key::Name(self.intern(&n));
            match self.resolve_export(m, &n, &mut Vec::new()) {
                Rx::Bind(mm, key) => {
                    let env = self.md.mods[mm as usize].env;
                    let g = self.md_getter(self.md.id_ns, env, &key);
                    let d = crate::bi_object::Desc { get: Some(g), e: Some(true), c: Some(false), ..Default::default() };
                    self.define_own(o, k, &d).expect("define");
                }
                Rx::Namespace(mm) => {
                    let v = self.md_namespace(mm);
                    let d = crate::bi_object::Desc { value: Some(v), w: Some(true), e: Some(true), c: Some(false), ..Default::default() };
                    self.define_own(o, k, &d).expect("define");
                }
                Rx::Null | Rx::Ambiguous => {}
            }
        }
        let tag = self.string("Module");
        let _ = self.define_attrs(o, Key::Name(self.wk[3]), 0, tag);
        assert!(self.prevent_extensions_internal(o).expect("ordinary module namespace"));
        o.value()
    }

    // ---- evaluate ----

    /// `module_evaluate`: run the graph of `m` and give the promise of the evaluation (`Evaluate` of 16.2.1.5.3). It is settled already
    /// when no module of the graph has a top-level `await`; else it settles when the jobs of the graph have run.
    pub(crate) fn md_evaluate(&mut self, m: u32) -> R<Value> {
        let m = match self.md.mods[m as usize].state {
            St::EvaluatingAsync | St::Evaluated => self.md.mods[m as usize].cycle_root,
            _ => m,
        };
        let done = self.md.mods[m as usize].done;
        if !done.is_undefined() {
            return Ok(done);
        }
        let p = self.new_promise();
        self.keep(p.value());
        self.md.mods[m as usize].done = p.value();
        let mut stack = Vec::new();
        match self.md_inner(m, &mut stack, 0) {
            Ok(_) => {
                if self.md.mods[m as usize].async_eval == 0 {
                    self.resolve_promise(p, Value::UNDEFINED)?;
                }
            }
            Err(e) => {
                let stopped = e == STOP;
                let e = if stopped { self.error(ErrKind::Type, "the evaluation of the module was stopped") } else { e };
                self.keep(e);
                for s in stack {
                    let rec = &mut self.md.mods[s as usize];
                    (rec.state, rec.error) = (St::Evaluated, Some(e));
                }
                if stopped {
                    return Err(STOP);
                }
                self.reject_promise(p, e);
            }
        }
        Ok(p.value())
    }

    /// `InnerModuleEvaluation`: run the sync modules of the graph in order and start the async ones. Gives the next depth-first number.
    fn md_inner(&mut self, m: u32, stack: &mut Vec<u32>, mut index: u32) -> R<u32> {
        match self.md.mods[m as usize].state {
            St::EvaluatingAsync | St::Evaluated => return self.md.mods[m as usize].error.map_or(Ok(index), Err),
            St::Evaluating => return Ok(index),
            St::New => return self.type_error("the module is not linked"),
            St::Linked => {}
        }
        let rec = &mut self.md.mods[m as usize];
        (rec.state, rec.dfs, rec.anc, rec.pending) = (St::Evaluating, index, index, 0);
        index += 1;
        stack.push(m);
        for d in self.md.mods[m as usize].requested.clone() {
            index = self.md_inner(d, stack, index)?;
            let mut req = d;
            if self.md.mods[d as usize].state == St::Evaluating {
                let a = self.md.mods[d as usize].anc;
                let rec = &mut self.md.mods[m as usize];
                rec.anc = rec.anc.min(a);
            } else {
                req = self.md.mods[d as usize].cycle_root;
                if let Some(e) = self.md.mods[req as usize].error {
                    return Err(e);
                }
            }
            if self.md.mods[req as usize].async_eval != 0 {
                self.md.mods[m as usize].pending += 1;
                self.md.mods[req as usize].parents.push(m);
            }
        }
        let f = self.md.mods[m as usize].func;
        if self.md.mods[m as usize].pending > 0 || self.md.mods[m as usize].info.has_tla {
            self.md.async_count += 1;
            self.md.mods[m as usize].async_eval = self.md.async_count;
            if self.md.mods[m as usize].pending == 0 {
                self.md_exec_async(m)?;
            }
        } else {
            self.md_call(f, 0)?;
        }
        let rec = &self.md.mods[m as usize];
        if rec.anc == rec.dfs {
            while let Some(r) = stack.pop() {
                let rec = &mut self.md.mods[r as usize];
                rec.state = if rec.async_eval == 0 { St::Evaluated } else { St::EvaluatingAsync };
                rec.cycle_root = m;
                if r == m {
                    break;
                }
            }
        }
        Ok(index)
    }

    /// `ExecuteAsyncModule`: call the code of `m`. It gives a promise, and the two natives settle the module when the promise settles.
    fn md_exec_async(&mut self, m: u32) -> R<()> {
        let f = self.md.mods[m as usize].func;
        let v = self.md_call_async(f, 0)?;
        let Some(p) = Realm::promise_obj(v) else { return self.md_async_fulfilled(m) };
        let [ok, err, ..] = self.md.id_async;
        let (ok, err) = (self.native_cell(ok), self.native_cell(err));
        ok.set_slot(&self.shapes, 1, Value::int(m as i32));
        err.set_slot(&self.shapes, 1, Value::int(m as i32));
        if let Err(e) = self.perform_then(p, ok.value(), err.value(), Value::UNDEFINED) {
            self.md_async_rejected(m, e)?;
        }
        Ok(())
    }

    /// `AsyncModuleExecutionFulfilled`: `m` is done. The modules that waited only for it run next, in the order that they started.
    fn md_async_fulfilled(&mut self, m: u32) -> R<()> {
        if self.md.mods[m as usize].state == St::Evaluated {
            return Ok(());
        }
        let rec = &mut self.md.mods[m as usize];
        (rec.async_eval, rec.state) = (0, St::Evaluated);
        self.md_settle_done(m, None)?;
        let mut list = Vec::new();
        self.md_gather(m, &mut list);
        list.sort_by_key(|&x| self.md.mods[x as usize].async_eval);
        for x in list {
            if self.md.mods[x as usize].state == St::Evaluated {
                continue;
            }
            if self.md.mods[x as usize].info.has_tla {
                if let Err(e) = self.md_exec_async(x) {
                    if e == STOP {
                        return Err(STOP);
                    }
                    self.md_async_rejected(x, e)?;
                }
            } else {
                let f = self.md.mods[x as usize].func;
                match self.md_call(f, 0) {
                    Err(STOP) => return Err(STOP),
                    Err(e) => self.md_async_rejected(x, e)?,
                    Ok(()) => {
                        let rec = &mut self.md.mods[x as usize];
                        (rec.async_eval, rec.state) = (0, St::Evaluated);
                        self.md_settle_done(x, None)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// `GatherAvailableAncestors`.
    fn md_gather(&mut self, m: u32, list: &mut Vec<u32>) {
        for p in self.md.mods[m as usize].parents.clone() {
            let root = self.md.mods[p as usize].cycle_root;
            if list.contains(&p) || self.md.mods[root as usize].error.is_some() {
                continue;
            }
            let rec = &mut self.md.mods[p as usize];
            rec.pending -= 1;
            if rec.pending == 0 {
                list.push(p);
                if !rec.info.has_tla {
                    self.md_gather(p, list);
                }
            }
        }
    }

    /// `AsyncModuleExecutionRejected`: `m` and the modules that wait for it fail with `e`.
    fn md_async_rejected(&mut self, m: u32, e: Value) -> R<()> {
        if self.md.mods[m as usize].state == St::Evaluated {
            return Ok(());
        }
        self.keep(e);
        let rec = &mut self.md.mods[m as usize];
        (rec.error, rec.state) = (Some(e), St::Evaluated);
        self.md_settle_done(m, Some(e))?;
        for p in self.md.mods[m as usize].parents.clone() {
            self.md_async_rejected(p, e)?;
        }
        Ok(())
    }

    /// Settle the promise of `module_evaluate` of `m`, if it was asked.
    fn md_settle_done(&mut self, m: u32, error: Option<Value>) -> R<()> {
        let Some(p) = Realm::promise_obj(self.md.mods[m as usize].done) else { return Ok(()) };
        match error {
            Some(e) => self.reject_promise(p, e),
            None => self.resolve_promise(p, Value::UNDEFINED)?,
        }
        Ok(())
    }

    // ---- import() ----

    /// `finish_dynamic_import`: link and evaluate the graph of `m`, then settle `p` with the namespace or the error. When the evaluation
    /// waits for a top-level `await`, `p` settles when it ends.
    pub(crate) fn md_finish(&mut self, p: Obj, m: u32) -> R<()> {
        let r = self.md_link(m).and_then(|()| self.md_evaluate(m));
        match r {
            Err(STOP) => Err(STOP),
            Err(e) => {
                self.reject_promise(p, e);
                Ok(())
            }
            Ok(done) => match Realm::promise_status(self, done) {
                Some((2, e)) => {
                    self.reject_promise(p, e);
                    Ok(())
                }
                Some((0, _)) => {
                    let [.., ok, err] = self.md.id_async;
                    let (ok, err) = (self.native_cell(ok), self.native_cell(err));
                    for f in [ok, err] {
                        f.set_slot(&self.shapes, 1, p.value());
                        f.set_slot(&self.shapes, 2, Value::int(m as i32));
                    }
                    let dp = Realm::promise_obj(done).expect("a promise");
                    if let Err(e) = self.perform_then(dp, ok.value(), err.value(), Value::UNDEFINED) {
                        self.reject_promise(p, e);
                    }
                    Ok(())
                }
                _ => {
                    let ns = self.md_namespace(m);
                    self.resolve_promise(p, ns)
                }
            },
        }
    }

    pub(crate) fn install_modules(&mut self) {
        for f in [view_get as crate::builtins::NativeFn, ns_get, async_ok, async_err, import_ok, import_err] {
            self.natives.push(Native { f, ctor: false, len: 0, name: "" });
        }
        let n = self.natives.len();
        (self.md.id_view, self.md.id_ns) = (n - 6, n - 5);
        self.md.id_async = [n - 4, n - 3, n - 2, n - 1];
        let g = self.global;
        self.method(g, "%import", 2, import_fn);
    }
}

enum ViewOf<'a> {
    Binding(Value, &'a str),
    Namespace(Value),
}

/// The getter of a view: the property of the environment object, the hole as it is (the importer checks it).
fn view_get(rt: &mut Realm, _this: Value, _args: &[Value]) -> R<Value> {
    let f = Realm::obj(rt.callee).expect("a getter");
    let (env, key) = (f.slot(&rt.shapes, 1), Realm::str_of(f.slot(&rt.shapes, 2)).expect("a name"));
    rt.get_prop(env, Key::Name(key))
}

/// The getter of a namespace property: the binding, or a `ReferenceError` in its dead zone.
fn ns_get(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let v = view_get(rt, this, args)?;
    if v.is_hole() {
        let f = Realm::obj(rt.callee).expect("a getter");
        let name = Realm::str_of(f.slot(&rt.shapes, 2)).map(|s| rt.text(s)).unwrap_or_default();
        return Err(rt.error(ErrKind::Reference, &format!("Cannot access '{name}' before initialization")));
    }
    Ok(v)
}

/// The module number in slot 1 of the running native (`async_ok`, `async_err`).
fn module_of(rt: &Realm) -> u32 {
    let f = Realm::obj(rt.callee).expect("a module native");
    f.slot(&rt.shapes, 1).as_int().expect("a module number") as u32
}

/// The promise of a module that has a top-level `await` was fulfilled.
fn async_ok(rt: &mut Realm, _this: Value, _args: &[Value]) -> R<Value> {
    let m = module_of(rt);
    rt.md_async_fulfilled(m)?;
    Ok(Value::UNDEFINED)
}

/// The promise of a module that has a top-level `await` was rejected with `args[0]`.
fn async_err(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let m = module_of(rt);
    rt.md_async_rejected(m, arg(args, 0))?;
    Ok(Value::UNDEFINED)
}

/// The evaluation of a dynamic import ended: `import()` gives the namespace. Slot 1 is its promise, slot 2 the module.
fn import_ok(rt: &mut Realm, _this: Value, _args: &[Value]) -> R<Value> {
    let f = Realm::obj(rt.callee).expect("a native");
    let (p, m) = (f.slot(&rt.shapes, 1), f.slot(&rt.shapes, 2).as_int().expect("a module number") as u32);
    let ns = rt.md_namespace(m);
    rt.resolve_promise(Realm::promise_obj(p).expect("a promise"), ns)?;
    Ok(Value::UNDEFINED)
}

/// The evaluation of a dynamic import failed with `args[0]`.
fn import_err(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let f = Realm::obj(rt.callee).expect("a native");
    let p = f.slot(&rt.shapes, 1);
    rt.reject_promise(Realm::promise_obj(p).expect("a promise"), arg(args, 0));
    Ok(Value::UNDEFINED)
}

/// `import(specifier)` (the compiler calls `%import(specifier, referrer)`): a promise that the host settles.
fn import_fn(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let p = rt.new_promise();
    let referrer = arg(args, 1).as_int().map(|i| i as u32);
    match rt.to_str(arg(args, 0)) {
        Ok(s) => {
            let specifier = rt.text(s);
            let promise = rt.api_persist(p.value());
            rt.md.pending.push(ImportRequest { promise, specifier, referrer });
        }
        Err(STOP) => return Err(STOP),
        Err(e) => rt.reject_promise(p, e),
    }
    Ok(p.value())
}

#[cfg(test)]
#[path = "module_tests.rs"]
mod tests;
