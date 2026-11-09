// ES modules of the evaluator. Included by `eval.rs` (`include!`), so it shares that module's imports and private items.
//
// A module is a script (`Code`) whose program has `ModuleInfo` (see `parse::parse_module`). Its code is one synthetic
// function. The host owns loading: it parses each source with `module_parse`, reads `module_requests`, loads those
// modules, hands the answers back with `module_set_requested`, then calls `module_link` and `module_evaluate`.
//
// Environments. `module_link` makes the environment of every module of the graph before any code runs: the parent is
// the global environment, the `var`s are `undefined`, `let`, `const` and `class` are in their TDZ, and the function
// declarations are closures. So a cycle can call a function of a module that did not run yet. The environment is marked
// captured, so it is never freed, and `module_values` roots it for the collector.
//
// Live bindings. An import is a binding with the flag `IMPORT` and without `INIT`. A read finds no `INIT`, which is the TDZ
// path, so the hot path is the one of a script. The cold path (`get_name_uninit`) sees `IMPORT` and reads the binding that the
// import names, in the environment of the module that declares it: `ModState::imports[value]` is (module, name). `import * as ns`
// and `export * as ns` are plain constant bindings that hold the namespace object.
//
// Namespace objects. Null prototype, not extensible, `Symbol.toStringTag` "Module", one enumerable accessor property for
// each export name that resolves (a getter that reads the live binding). Deviation: the spec has data properties
// ({writable: true, enumerable: true, configurable: false}); here `getOwnPropertyDescriptor` shows an accessor. A write
// fails as in the spec (no setter: a TypeError in strict code).
//
// Evaluation follows ECMA-262 16.2.1.5.3 (Evaluate, InnerModuleEvaluation, ExecuteAsyncModule, AsyncModuleExecutionFulfilled and
// Rejected, GatherAvailableAncestors) with top-level await. A module with `await` runs on the async machine (`eval_async.rs`).
//
// `import()` queues a request (`take_dynamic_imports`). The host loads the graph, then calls `finish_dynamic_import`.
//
// Not done: import attributes are parsed and ignored; `import.meta` has only `url`; a script (not a module) that calls `import()`
// has the document as its referrer.

/// A binding that reads another module's binding. It has no `INIT` flag.
const IMPORT: u8 = 8;
const NO_MOD: u32 = u32::MAX;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MStatus {
    New,
    Linked,
    Evaluating,
    EvaluatingAsync,
    Evaluated,
}

struct ModRec {
    script: usize,
    url: String,
    prog: Rc<Program<'static>>,
    /// The module of each request (`NO_MOD` until the host answers).
    requested: Vec<u32>,
    env: u32,
    status: MStatus,
    dfs: u32,
    dfs_anc: u32,
    pending: u32,
    parents: Vec<u32>,
    /// The order of `[[AsyncEvaluation]]` (0: none). `done`: the async part finished.
    order: u64,
    done: bool,
    root: u32,
    cap: Option<ObjId>,
    error: Option<Value>,
    ns: Option<ObjId>,
    meta: Option<ObjId>,
}

struct DynImport {
    promise: ObjId,
    specifier: String,
    referrer: Option<u32>,
}

#[derive(Default)]
struct ModState {
    mods: Vec<ModRec>,
    order: u64,
    /// (module, binding name) for each import binding.
    imports: Vec<(u32, u32)>,
    dyn_imports: Vec<DynImport>,
    n_default: u32,
}

/// The functions of module reactions and namespace getters.
#[derive(Clone, Copy)]
enum ModFn {
    AsyncOk(u32),
    AsyncErr(u32),
    ImportOk { promise: ObjId, m: u32 },
    ImportErr { promise: ObjId },
    /// The getter of a namespace export: the binding `name` of module `m`.
    NsGet { m: u32, name: u32 },
    /// The getter of `export * as x from`: the namespace of module `m`.
    NsOf(u32),
}

enum Resolved {
    Binding(u32, u32),
    Namespace(u32),
    Null,
    Ambiguous,
}

/// A pending `import()`: the promise, the specifier, and the module that asked (`None`: a classic script).
pub struct ImportRequest {
    pub promise: ObjId,
    pub specifier: String,
    pub referrer: Option<u32>,
}

impl<'p> Interp<'p> {
    // ------------------------------------------------------------ host API

    /// Parse and keep a module. A syntax error is a thrown `SyntaxError`. The module has no requested modules yet.
    pub fn module_parse(&mut self, source: &str, url: &str) -> Result<u32, Error> {
        if self.depth != 0 {
            return Err(unsupported("module_parse during a call", Span::default()));
        }
        let id = self.add_script_kind(source, crate::parse_cache::Kind::Module)?;
        let prog = self.scripts[id].prog.clone();
        let n = prog.module.as_ref().map_or(0, |m| m.requests.len());
        let rec = ModRec {
            script: id, url: url.to_string(), prog, requested: vec![NO_MOD; n], env: NONE, status: MStatus::New, dfs: 0, dfs_anc: 0, pending: 0,
            parents: Vec::new(), order: 0, done: false, root: NO_MOD, cap: None, error: None, ns: None, meta: None,
        };
        self.modules.mods.push(rec);
        Ok(self.modules.mods.len() as u32 - 1)
    }

    /// The specifiers that module `m` asks for, in order.
    pub fn module_requests(&self, m: u32) -> Vec<String> {
        self.modules.mods[m as usize].prog.module.as_ref().map_or_else(Vec::new, |i| i.requests.iter().map(|r| r.to_string()).collect())
    }

    /// Set the module for each request of `m` (same order as `module_requests`).
    pub fn module_set_requested(&mut self, m: u32, deps: &[u32]) {
        let r = &mut self.modules.mods[m as usize].requested;
        for (slot, &d) in r.iter_mut().zip(deps) {
            *slot = d;
        }
    }

    pub fn module_url(&self, m: u32) -> &str {
        &self.modules.mods[m as usize].url
    }

    /// True once `m` is linked or later.
    pub fn module_is_linked(&self, m: u32) -> bool {
        self.modules.mods[m as usize].status != MStatus::New
    }

    /// Link the graph of `root`: check every import, make the environments, bind the imports. A bad import is a thrown `SyntaxError`
    /// and nothing changes.
    pub fn module_link(&mut self, root: u32) -> Result<(), Error> {
        let mut order: Vec<u32> = Vec::new();
        let mut stack = vec![root];
        while let Some(m) = stack.pop() {
            if self.modules.mods[m as usize].status != MStatus::New || order.contains(&m) {
                continue;
            }
            order.push(m);
            for &r in &self.modules.mods[m as usize].requested {
                if r == NO_MOD {
                    let url = self.modules.mods[m as usize].url.clone();
                    return Err(self.mk_err(SYNTAX_ERROR, &format!("a module that '{url}' imports was not loaded")));
                }
                stack.push(r);
            }
        }
        for &m in &order {
            self.module_check_links(m)?;
        }
        for &m in &order {
            self.module_instantiate(m)?;
        }
        for &m in &order {
            self.module_bind_imports(m)?;
        }
        for &m in &order {
            self.modules.mods[m as usize].status = MStatus::Linked;
        }
        Ok(())
    }

    /// Evaluate a linked module and its dependencies. Gives the promise of the whole graph (`Evaluate()`).
    pub fn module_evaluate(&mut self, m: u32) -> Result<ObjId, Error> {
        if self.depth == 0 {
            self.begin();
        }
        let mut m = m;
        if matches!(self.modules.mods[m as usize].status, MStatus::EvaluatingAsync | MStatus::Evaluated) && self.modules.mods[m as usize].root != NO_MOD {
            m = self.modules.mods[m as usize].root;
        }
        if let Some(c) = self.modules.mods[m as usize].cap {
            return Ok(c);
        }
        if self.modules.mods[m as usize].status == MStatus::New {
            return Err(unsupported("module_evaluate before module_link", Span::default()));
        }
        let cap = self.new_promise()?;
        self.modules.mods[m as usize].cap = Some(cap);
        let mut stack = Vec::new();
        match self.mod_inner_eval(m, &mut stack, 0) {
            Ok(_) => {
                if self.modules.mods[m as usize].order == 0 {
                    self.resolve_promise(cap, Value::Undefined)?;
                }
            }
            Err(Error::Throw(v)) => {
                for s in stack {
                    let r = &mut self.modules.mods[s as usize];
                    r.status = MStatus::Evaluated;
                    r.error = Some(v);
                }
                self.reject_promise(cap, v)?;
            }
            Err(e) => return Err(e),
        }
        Ok(cap)
    }

    /// The namespace object of module `m`.
    pub fn module_namespace(&mut self, m: u32) -> Result<ObjId, Error> {
        if let Some(o) = self.modules.mods[m as usize].ns {
            return Ok(o);
        }
        let mut visited = Vec::new();
        let mut names = self.mod_exported_names(m, &mut visited);
        names.sort();
        names.dedup();
        let o = h!(self, self.heap.new_object(None));
        for name in names {
            let getter = match self.mod_resolve_export(m, &name, &mut Vec::new()) {
                Resolved::Binding(tm, tn) => PFn::Mod(ModFn::NsGet { m: tm, name: tn }),
                Resolved::Namespace(t) => PFn::Mod(ModFn::NsOf(t)),
                _ => continue,
            };
            let g = self.pfn(getter, "", 0)?;
            let key = h!(self, self.heap.key_from_str(&name));
            let d = PropertyDescriptor { get: Some(Some(g)), set: Some(None), enumerable: Some(true), configurable: Some(false), ..Default::default() };
            hxw!(self, self.heap.define_own_property(o, key, &d));
        }
        let tag = h!(self, self.heap.intern_str("Module"));
        self.def(o, self.sc.k_to_string_tag, Value::String(tag), false, false, false)?;
        h!(self, self.heap.prevent_extensions(o));
        self.modules.mods[m as usize].ns = Some(o);
        Ok(o)
    }

    /// The `import()` calls that ran since the last call, oldest first.
    pub fn take_dynamic_imports(&mut self) -> Vec<ImportRequest> {
        std::mem::take(&mut self.modules.dyn_imports).into_iter().map(|d| ImportRequest { promise: d.promise, specifier: d.specifier, referrer: d.referrer }).collect()
    }

    /// The host loaded the graph of a dynamic import: link and evaluate it, and settle `promise` with the namespace.
    pub fn finish_dynamic_import(&mut self, promise: ObjId, m: u32) -> Result<(), Error> {
        match self.module_link(m) {
            Ok(()) => {}
            Err(Error::Throw(v)) => return self.reject_promise(promise, v),
            Err(e) => return Err(e),
        }
        let ep = match self.module_evaluate(m) {
            Ok(p) => p,
            Err(Error::Throw(v)) => return self.reject_promise(promise, v),
            Err(e) => return Err(e),
        };
        let ok = self.pfn(PFn::Mod(ModFn::ImportOk { promise, m }), "", 1)?;
        let err = self.pfn(PFn::Mod(ModFn::ImportErr { promise }), "", 1)?;
        let derived = self.new_promise()?;
        self.perform_then(ep, Value::Object(ok), Value::Object(err), derived)
    }

    /// The host got an error while it parsed a module of a dynamic import (a thrown `SyntaxError`): reject `promise` with it.
    pub fn reject_dynamic_import(&mut self, promise: ObjId, e: Error) -> Result<(), Error> {
        match e {
            Error::Throw(v) => self.reject_promise(promise, v),
            e => Err(e),
        }
    }

    /// The host could not load a dynamic import: reject `promise` with a `TypeError`.
    pub fn fail_dynamic_import(&mut self, promise: ObjId, message: &str) -> Result<(), Error> {
        let e = self.type_err(message);
        let v = thrown(e)?;
        self.reject_promise(promise, v)
    }

    // ------------------------------------------------------------ roots

    /// The values and environments that the module records keep alive.
    fn module_values(&self, out: &mut Vec<Value>, envs: &mut Vec<u32>) {
        for r in &self.modules.mods {
            out.extend(r.ns.map(Value::Object));
            out.extend(r.meta.map(Value::Object));
            out.extend(r.cap.map(Value::Object));
            out.extend(r.error);
            if r.env != NONE {
                envs.push(r.env);
            }
        }
        out.extend(self.modules.dyn_imports.iter().map(|d| Value::Object(d.promise)));
    }

    // ------------------------------------------------------------ export resolution

    fn mod_exported_names(&mut self, m: u32, visited: &mut Vec<u32>) -> Vec<String> {
        if visited.contains(&m) {
            return Vec::new();
        }
        visited.push(m);
        let prog = self.modules.mods[m as usize].prog.clone();
        let info = prog.module.as_ref().expect("module");
        let mut names: Vec<String> = info.exports.iter().map(|e| e.name.to_string()).collect();
        for &s in &info.stars {
            let t = self.modules.mods[m as usize].requested[s as usize];
            for n in self.mod_exported_names(t, visited) {
                if n != "default" && !names.contains(&n) {
                    names.push(n);
                }
            }
        }
        names
    }

    fn mod_resolve_export(&mut self, m: u32, name: &str, set: &mut Vec<(u32, String)>) -> Resolved {
        if set.iter().any(|(sm, sn)| *sm == m && sn == name) {
            return Resolved::Null;
        }
        set.push((m, name.to_string()));
        let prog = self.modules.mods[m as usize].prog.clone();
        let info = prog.module.as_ref().expect("module");
        for e in &info.exports {
            if &*e.name != name {
                continue;
            }
            return match &e.source {
                ExportSource::Local(sp) => Resolved::Binding(m, self.intern_name(&prog.name(*sp))),
                ExportSource::Default => Resolved::Binding(m, self.default_name()),
                ExportSource::Indirect { request, name: ImportName::Namespace } => Resolved::Namespace(self.modules.mods[m as usize].requested[*request as usize]),
                ExportSource::Indirect { request, name: ImportName::Name(n) } => {
                    let t = self.modules.mods[m as usize].requested[*request as usize];
                    self.mod_resolve_export(t, n, set)
                }
            };
        }
        if name == "default" {
            return Resolved::Null;
        }
        let mut star: Option<Resolved> = None;
        for &s in &info.stars {
            let t = self.modules.mods[m as usize].requested[s as usize];
            let r = self.mod_resolve_export(t, name, set);
            match (r, &star) {
                (Resolved::Ambiguous, _) => return Resolved::Ambiguous,
                (Resolved::Null, _) => {}
                (r, None) => star = Some(r),
                (Resolved::Binding(a, b), Some(Resolved::Binding(c, d))) => {
                    if a != *c || b != *d {
                        return Resolved::Ambiguous;
                    }
                }
                (Resolved::Namespace(a), Some(Resolved::Namespace(c))) => {
                    if a != *c {
                        return Resolved::Ambiguous;
                    }
                }
                _ => return Resolved::Ambiguous,
            }
        }
        star.unwrap_or(Resolved::Null)
    }

    fn default_name(&mut self) -> u32 {
        if self.modules.n_default == 0 {
            self.modules.n_default = self.intern_name("*default*") + 1;
        }
        self.modules.n_default - 1
    }

    // ------------------------------------------------------------ linking

    fn module_check_links(&mut self, m: u32) -> Res<()> {
        let prog = self.modules.mods[m as usize].prog.clone();
        let info = prog.module.as_ref().expect("module");
        let mut wants: Vec<(u32, &str)> = Vec::new();
        for i in &info.imports {
            if let ImportName::Name(n) = &i.name {
                wants.push((i.request, n));
            }
        }
        for e in &info.exports {
            if let ExportSource::Indirect { request, name: ImportName::Name(n) } = &e.source {
                wants.push((*request, n));
            }
        }
        for (req, n) in wants {
            let t = self.modules.mods[m as usize].requested[req as usize];
            let bad = match self.mod_resolve_export(t, n, &mut Vec::new()) {
                Resolved::Null => "does not provide an export named",
                Resolved::Ambiguous => "has an ambiguous export named",
                _ => continue,
            };
            let spec = &info.requests[req as usize];
            return Err(self.mk_err(SYNTAX_ERROR, &format!("The requested module '{spec}' {bad} '{n}'")));
        }
        Ok(())
    }

    /// Make the environment of `m`: declarations hoisted, imports as placeholders.
    fn module_instantiate(&mut self, m: u32) -> Res<()> {
        let (script, prog) = {
            let r = &self.modules.mods[m as usize];
            (r.script, r.prog.clone())
        };
        let info = prog.module.as_ref().expect("module");
        let fid = info.func;
        let env = self.new_env(0, true)?;
        {
            let e = &mut self.envs[env as usize];
            e.captured = true;
            e.this = Some(Value::Undefined);
        }
        self.modules.mods[m as usize].env = env;
        let (saved_env, saved_cur, saved_strict) = (self.env, self.cur, self.strict);
        self.select(script);
        self.env = env;
        self.strict = true;
        let r = self.in_call(|it| it.in_frame(script as u32, Where::Func(fid), false, |it| it.module_declare(&prog, fid)));
        self.select(saved_cur);
        self.env = saved_env;
        self.strict = saved_strict;
        r
    }

    fn module_declare(&mut self, prog: &Rc<Program<'static>>, fid: FuncId) -> Res<()> {
        let env = self.env;
        let info = prog.module.as_ref().expect("module");
        for i in 0..self.scopes[fid.index()].vars.len() {
            let n = self.scopes[fid.index()].vars[i];
            self.declare_var(env, n);
        }
        for imp in &info.imports {
            let n = self.intern_name(&prog.name(imp.local));
            self.env_declare(env, n, IMPORT, Value::Undefined);
        }
        if info.exports.iter().any(|e| e.source == ExportSource::Default) {
            let n = self.default_name();
            self.env_declare(env, n, MUT, Value::Undefined);
        }
        if let Body::Block(l) = prog.func(fid).body {
            self.declare_scope(l, true)?;
        }
        Ok(())
    }

    fn module_bind_imports(&mut self, m: u32) -> Res<()> {
        let prog = self.modules.mods[m as usize].prog.clone();
        let env = self.modules.mods[m as usize].env;
        let info = prog.module.as_ref().expect("module");
        for imp in &info.imports {
            let t = self.modules.mods[m as usize].requested[imp.request as usize];
            let local = self.intern_name(&prog.name(imp.local));
            let (flags, value) = match &imp.name {
                ImportName::Namespace => (INIT, Value::Object(self.module_namespace(t)?)),
                ImportName::Name(n) => match self.mod_resolve_export(t, n, &mut Vec::new()) {
                    Resolved::Binding(tm, tn) => {
                        self.modules.imports.push((tm, tn));
                        (IMPORT, Value::Number((self.modules.imports.len() - 1) as f64))
                    }
                    Resolved::Namespace(tm) => (INIT, Value::Object(self.module_namespace(tm)?)),
                    _ => continue,
                },
            };
            if let Some(i) = self.find_in(env, local) {
                let b = &mut self.envs[env as usize].bindings[i];
                b.flags = flags;
                b.value = value;
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------ reading bindings

    /// The value of the binding `name` declared by module `m`.
    fn mod_binding(&mut self, m: u32, name: u32) -> Res<Value> {
        let env = self.modules.mods[m as usize].env;
        if env != NONE
            && let Some(i) = self.find_in(env, name)
        {
            let b = self.envs[env as usize].bindings[i];
            if b.flags & INIT != 0 {
                return Ok(b.value);
            }
        }
        Err(self.tdz(name))
    }

    /// An import binding in `get_name`: the value of the binding that it names.
    #[cold]
    #[inline(never)]
    fn get_name_uninit(&mut self, n: u32, site: u32) -> Res<Value> {
        if let Some((e, i)) = self.find_name(n, site) {
            let b = self.envs[e as usize].bindings[i];
            if b.flags & IMPORT != 0
                && let Value::Number(k) = b.value
            {
                let (tm, tn) = self.modules.imports[k as usize];
                return match self.mod_binding(tm, tn) {
                    // The error names the importing name, as the name the code wrote.
                    Err(Error::Throw(_)) => Err(self.tdz(n)),
                    r => r,
                };
            }
        }
        Err(self.tdz(n))
    }

    /// The statement `export default expr;`. A function after `export default` was made when the module was instantiated.
    #[cold]
    #[inline(never)]
    fn exec_export_default(&mut self, e: ExprId) -> Res<()> {
        if matches!(self.prog.expr(e).kind, ExprKind::Function(_)) {
            return Ok(());
        }
        let s = h!(self, self.heap.intern_str("default"));
        let v = self.eval_named(e, Some(s))?;
        self.set_default_export(v)
    }

    /// `export default expr` ran: store the value in the `*default*` binding of the running module.
    fn set_default_export(&mut self, v: Value) -> Res<()> {
        let n = self.default_name();
        match self.find_in(self.env, n) {
            Some(i) => {
                let b = &mut self.envs[self.env as usize].bindings[i];
                b.flags = INIT | MUT;
                b.value = v;
                Ok(())
            }
            None => Err(unsupported("export default outside a module", Span::default())),
        }
    }

    // ------------------------------------------------------------ evaluation

    fn mod_has_tla(&self, m: u32) -> bool {
        self.modules.mods[m as usize].prog.module.as_ref().is_some_and(|i| i.has_tla)
    }

    fn mod_pending_async(&self, m: u32) -> bool {
        let r = &self.modules.mods[m as usize];
        r.order != 0 && !r.done
    }

    fn mod_inner_eval(&mut self, m: u32, stack: &mut Vec<u32>, mut index: u32) -> Res<u32> {
        match self.modules.mods[m as usize].status {
            MStatus::EvaluatingAsync | MStatus::Evaluated => {
                return match self.modules.mods[m as usize].error {
                    None => Ok(index),
                    Some(e) => Err(Error::Throw(e)),
                };
            }
            MStatus::Evaluating => return Ok(index),
            MStatus::New => return Err(unsupported("module evaluated before it was linked", Span::default())),
            MStatus::Linked => {}
        }
        {
            let r = &mut self.modules.mods[m as usize];
            r.status = MStatus::Evaluating;
            r.dfs = index;
            r.dfs_anc = index;
            r.pending = 0;
        }
        index += 1;
        stack.push(m);
        let reqs = self.modules.mods[m as usize].requested.clone();
        for r in reqs {
            index = self.mod_inner_eval(r, stack, index)?;
            let mut rm = r;
            if self.modules.mods[rm as usize].status == MStatus::Evaluating {
                let a = self.modules.mods[rm as usize].dfs_anc;
                let me = &mut self.modules.mods[m as usize];
                me.dfs_anc = me.dfs_anc.min(a);
            } else {
                if self.modules.mods[rm as usize].root != NO_MOD {
                    rm = self.modules.mods[rm as usize].root;
                }
                if let Some(e) = self.modules.mods[rm as usize].error {
                    return Err(Error::Throw(e));
                }
            }
            if self.mod_pending_async(rm) {
                self.modules.mods[m as usize].pending += 1;
                self.modules.mods[rm as usize].parents.push(m);
            }
        }
        if self.modules.mods[m as usize].pending > 0 || self.mod_has_tla(m) {
            self.modules.order += 1;
            self.modules.mods[m as usize].order = self.modules.order;
            if self.modules.mods[m as usize].pending == 0 {
                self.mod_exec_async(m)?;
            }
        } else {
            self.mod_exec_sync(m)?;
        }
        if self.modules.mods[m as usize].dfs_anc == self.modules.mods[m as usize].dfs {
            loop {
                let Some(r) = stack.pop() else { break };
                let rec = &mut self.modules.mods[r as usize];
                rec.status = if rec.order == 0 { MStatus::Evaluated } else { MStatus::EvaluatingAsync };
                rec.root = m;
                if r == m {
                    break;
                }
            }
        }
        Ok(index)
    }

    /// Run the code of module `m` in its environment.
    fn mod_exec_sync(&mut self, m: u32) -> Res<()> {
        let (script, prog, env) = {
            let r = &self.modules.mods[m as usize];
            (r.script, r.prog.clone(), r.env)
        };
        let fid = prog.module.as_ref().expect("module").func;
        let Body::Block(l) = prog.func(fid).body else { return Ok(()) };
        let (saved_env, saved_cur, saved_strict) = (self.env, self.cur, self.strict);
        self.select(script);
        self.env = env;
        self.strict = true;
        let r = self.in_call(|it| {
            it.in_frame(script as u32, Where::Func(fid), false, |it| match it.exec_list(l) {
                Ok(()) => Ok(()),
                Err(Ctl::Err(e)) => Err(e),
                Err(_) => Err(unsupported("stray break, continue or return", Span::default())),
            })
        });
        self.select(saved_cur);
        self.env = saved_env;
        self.strict = saved_strict;
        r
    }

    /// `ExecuteAsyncModule`: run `m` on the async machine. Its result promise settles `AsyncModuleExecutionFulfilled` or `Rejected`.
    fn mod_exec_async(&mut self, m: u32) -> Res<()> {
        let cap = self.new_promise()?;
        let ok = self.pfn(PFn::Mod(ModFn::AsyncOk(m)), "", 1)?;
        let err = self.pfn(PFn::Mod(ModFn::AsyncErr(m)), "", 1)?;
        let derived = self.new_promise()?;
        self.perform_then(cap, Value::Object(ok), Value::Object(err), derived)?;
        let (script, prog, env) = {
            let r = &self.modules.mods[m as usize];
            (r.script, r.prog.clone(), r.env)
        };
        let fid = prog.module.as_ref().expect("module").func;
        let (saved_env, saved_cur, saved_strict) = (self.env, self.cur, self.strict);
        self.select(script);
        self.env = env;
        self.strict = true;
        let r = self.in_call(|it| it.in_frame(script as u32, Where::Func(fid), false, |it| it.async_start_prepared(script as u32, fid, env, cap)));
        self.select(saved_cur);
        self.env = saved_env;
        self.strict = saved_strict;
        r
    }

    fn mod_root_of(&self, m: u32) -> u32 {
        let r = self.modules.mods[m as usize].root;
        if r == NO_MOD { m } else { r }
    }

    fn mod_gather(&self, m: u32, list: &mut Vec<u32>, pending: &mut FxMap<u32, u32>) {
        for &p in &self.modules.mods[m as usize].parents {
            if list.contains(&p) || self.modules.mods[self.mod_root_of(p) as usize].error.is_some() {
                continue;
            }
            let n = pending.entry(p).or_insert(self.modules.mods[p as usize].pending);
            *n -= 1;
            if *n == 0 {
                list.push(p);
                if !self.mod_has_tla(p) {
                    self.mod_gather(p, list, pending);
                }
            }
        }
    }

    fn mod_async_fulfilled(&mut self, m: u32) -> Res<()> {
        if self.modules.mods[m as usize].status == MStatus::Evaluated {
            return Ok(());
        }
        {
            let r = &mut self.modules.mods[m as usize];
            r.done = true;
            r.status = MStatus::Evaluated;
        }
        if let Some(c) = self.modules.mods[m as usize].cap {
            self.resolve_promise(c, Value::Undefined)?;
        }
        let (mut list, mut pending) = (Vec::new(), FxMap::default());
        self.mod_gather(m, &mut list, &mut pending);
        for &p in &list {
            self.modules.mods[p as usize].pending = 0;
        }
        list.sort_by_key(|&p| self.modules.mods[p as usize].order);
        for p in list {
            if self.modules.mods[p as usize].status == MStatus::Evaluated {
                continue;
            }
            if self.mod_has_tla(p) {
                self.mod_exec_async(p)?;
                continue;
            }
            match self.mod_exec_sync(p) {
                Ok(()) => {
                    let r = &mut self.modules.mods[p as usize];
                    r.done = true;
                    r.status = MStatus::Evaluated;
                    if let Some(c) = r.cap {
                        self.resolve_promise(c, Value::Undefined)?;
                    }
                }
                Err(Error::Throw(v)) => self.mod_async_rejected(p, v)?,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    fn mod_async_rejected(&mut self, m: u32, v: Value) -> Res<()> {
        if self.modules.mods[m as usize].status == MStatus::Evaluated {
            return Ok(());
        }
        {
            let r = &mut self.modules.mods[m as usize];
            r.error = Some(v);
            r.status = MStatus::Evaluated;
        }
        if let Some(c) = self.modules.mods[m as usize].cap {
            self.reject_promise(c, v)?;
        }
        for p in self.modules.mods[m as usize].parents.clone() {
            self.mod_async_rejected(p, v)?;
        }
        Ok(())
    }

    /// The call of a module function (a reaction or a namespace getter).
    fn mod_fn_call(&mut self, f: ModFn, a0: Value) -> Res<Value> {
        match f {
            ModFn::AsyncOk(m) => self.mod_async_fulfilled(m).map(|()| Value::Undefined),
            ModFn::AsyncErr(m) => self.mod_async_rejected(m, a0).map(|()| Value::Undefined),
            ModFn::ImportOk { promise, m } => {
                let ns = self.module_namespace(m)?;
                self.resolve_promise(promise, Value::Object(ns))?;
                Ok(Value::Undefined)
            }
            ModFn::ImportErr { promise } => self.reject_promise(promise, a0).map(|()| Value::Undefined),
            ModFn::NsGet { m, name } => self.mod_binding(m, name),
            ModFn::NsOf(m) => Ok(Value::Object(self.module_namespace(m)?)),
        }
    }

    // ------------------------------------------------------------ import() and import.meta

    fn mod_of_current(&self) -> Option<u32> {
        self.modules.mods.iter().position(|m| m.script == self.cur).map(|i| i as u32)
    }

    #[cold]
    #[inline(never)]
    fn eval_import_call(&mut self, arg: ExprId, opts: Option<ExprId>) -> Res<Value> {
        let spec = self.eval_expr(arg)?;
        let opts = match opts {
            Some(o) => self.eval_expr(o)?,
            None => Value::Undefined,
        };
        let referrer = self.mod_of_current();
        let p = self.new_promise()?;
        if self.modules.dyn_imports.len() >= (1 << 16) {
            return Err(Error::Limit(LimitKind::Promises));
        }
        if !matches!(opts, Value::Undefined | Value::Object(_)) {
            let e = self.type_err("The second argument of import() must be an object");
            let v = thrown(e)?;
            self.reject_promise(p, v)?;
            return Ok(Value::Object(p));
        }
        match self.coerce_string(spec) {
            Ok(s) => {
                let specifier = h!(self, self.heap.to_string_lossy(s));
                self.modules.dyn_imports.push(DynImport { promise: p, specifier, referrer });
            }
            Err(Error::Throw(v)) => self.reject_promise(p, v)?,
            Err(e) => return Err(e),
        }
        Ok(Value::Object(p))
    }

    #[cold]
    #[inline(never)]
    fn import_meta(&mut self) -> Res<Value> {
        let Some(m) = self.mod_of_current() else { return Err(unsupported("import.meta outside a module", Span::default())) };
        if let Some(o) = self.modules.mods[m as usize].meta {
            return Ok(Value::Object(o));
        }
        let o = h!(self, self.heap.new_object(None));
        let url = self.modules.mods[m as usize].url.clone();
        let s = h!(self, self.heap.intern_str(&url));
        let k = h!(self, self.heap.key_from_str("url"));
        self.def(o, k, Value::String(s), true, true, true)?;
        self.modules.mods[m as usize].meta = Some(o);
        Ok(Value::Object(o))
    }

    /// The async machine for a module whose environment `module_link` made: bind nothing, declare nothing, run the body.
    fn async_start_prepared(&mut self, script: u32, fid: FuncId, env: u32, result: ObjId) -> Res<()> {
        let prog = self.prog.clone();
        let Body::Block(l) = prog.func(fid).body else { return self.resolve_promise(result, Value::Undefined) };
        let mut run = Run { script, fid, strict: true, agen: false, result, call_env: env, cur_env: env, awaiting: None, stack: Vec::new(), slots: Vec::new() };
        let first = self.fpush(&mut run, Fr::Body).and_then(|()| self.list_from(&mut run, l, 0)).map(|s| match s {
            Step::Next(n) => n,
            Step::Out(_) => Next::Val(Value::Undefined),
        });
        let r = match first {
            Ok(n) => self.drive(&mut run, n),
            Err(Error::Throw(v)) => self.drive(&mut run, Next::Abr(Abr::Throw(v))),
            Err(e) => Err(e),
        };
        self.async_finish(run, r)
    }
}
