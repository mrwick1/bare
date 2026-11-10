// Shared-realm multi-script execution. Included by `eval.rs` (`include!`), so it shares that module's imports.
//
// One realm is one heap, one global object and one global lexical environment (env 0). Scripts come in through
// `Interp::new` (the first one) and `Interp::run_script` (any later one, text in, owned by the realm).
//
// How code ownership works
// - A script is an owned `Program<'static>` (its source is copied) behind an `Rc`. No borrow, no leak, no unsafe.
// - Everything the evaluator caches per node (names, literal strings, function scope tables) belongs to one script.
//   The ids are indexes into that script's arenas, so two scripts reuse the same ids for different nodes.
//   The code of the CURRENT script lives in the `Interp` fields (`prog`, `expr_name`, ...), so the hot paths
//   are unchanged. The other scripts are parked in `scripts`. `select` swaps one in with 8 pointer-sized swaps.
// - Every closure records `script`, the slot of the script that made it. `call_script` selects that script
//   and puts the caller's script back on every exit, normal, throw or limit.
// - Names are interned once for the whole realm (`names`), so a binding name means the same in every script.
// - Retention: a script and its caches stay until the realm is dropped, even if no closure reaches them. A classic
//   script with the same text as an earlier one reuses that slot (`script_ids`, one bucket for each hash). The
//   bounds are `EvalLimits::max_scripts`, `max_script_source` and `max_script_nodes`, checked before the
//   source is parsed and while it is parsed. A script rejected during parse or preflight is not kept and
//   its interned names are removed again. Runtime failures keep code for already-created closures.
//
// GC: `script_roots` gives the parked scripts' cached strings to `roots`. The names are rooted once.

/// Bits of the hash that pick a bucket of `Interp::script_ids` (4,096 buckets, 16 KiB).
const SCRIPT_IDS_BITS: u32 = 12;

/// A multiply hash of the text for `Interp::script_ids`. The last bytes are read as an overlapping word, so it has no copy of a
/// remainder (a `memcpy` call cost more than the rest for a text of 10 bytes).
fn script_hash(b: &[u8]) -> u64 {
    const K: u64 = 0x517c_c1b7_2722_0a95;
    let n = b.len();
    let mut h = n as u64;
    if n >= 8 {
        for w in b.chunks_exact(8) {
            h = (h.rotate_left(5) ^ u64::from_le_bytes(w.try_into().unwrap())).wrapping_mul(K);
        }
        h = (h.rotate_left(5) ^ u64::from_le_bytes(b[n - 8..].try_into().unwrap())).wrapping_mul(K);
    } else if n >= 4 {
        h ^= (u32::from_le_bytes(b[..4].try_into().unwrap()) as u64) << 32 | u32::from_le_bytes(b[n - 4..].try_into().unwrap()) as u64;
    } else if n > 0 {
        h ^= (b[0] as u64) << 16 | (b[n / 2] as u64) << 8 | b[n - 1] as u64;
    }
    h.wrapping_mul(K)
}

/// The per-script code and caches (see the header). The slot of the current script holds an empty placeholder.
struct Code {
    prog: Rc<Program<'static>>,
    expr_name: Vec<u32>,
    pat_name: Vec<u32>,
    stmt_label: Vec<u32>,
    func_name: Vec<u32>,
    site_strs: Vec<StrId>,
    prop_str: Vec<Option<StrId>>,
    scopes: Vec<FScope>,
    slots: Slots,
    /// Class walk flags (see `eval_classes.rs`): per function and per class of this script.
    fflags: Vec<u8>,
    cflags: Vec<u8>,
}

impl Code {
    /// Capacity bytes of the caches of this script (not the syntax tree).
    fn cache_bytes(&self) -> usize {
        use std::mem::size_of;
        (self.expr_name.capacity() + self.pat_name.capacity() + self.stmt_label.capacity() + self.func_name.capacity() + self.slots.sites.len()) * 4
            + self.site_strs.capacity() * size_of::<StrId>() + self.prop_str.capacity() * size_of::<Option<StrId>>()
            + self.scopes.capacity() * size_of::<FScope>() + self.fflags.capacity() + self.cflags.capacity()
    }

    fn placeholder(prog: &Rc<Program<'static>>) -> Code {
        Code { prog: prog.clone(), expr_name: Vec::new(), pat_name: Vec::new(), stmt_label: Vec::new(), func_name: Vec::new(), site_strs: Vec::new(), prop_str: Vec::new(), scopes: Vec::new(), slots: Slots::default(), fflags: Vec::new(), cflags: Vec::new() }
    }

    /// Empty caches sized for the tree.
    fn new(prog: Rc<Program<'static>>) -> Code {
        let st = prog.stats();
        Code {
            expr_name: vec![0; st.exprs],
            pat_name: vec![0; st.pats],
            stmt_label: vec![0; st.stmts],
            func_name: vec![0; st.funcs],
            site_strs: Vec::new(),
            prop_str: vec![None; st.props],
            scopes: (0..=st.funcs).map(|_| FScope::default()).collect(),
            slots: Slots::new(&st),
            fflags: vec![0; st.funcs],
            cflags: vec![0; st.classes],
            prog,
        }
    }
}

fn is_restricted(p: Option<Property>) -> bool {
    matches!(p, Some(Property::Data { configurable: false, .. } | Property::Accessor { configurable: false, .. }))
}

impl<'p> Interp<'p> {
    /// A realm with no script yet. Builtins are installed. Add code with [`Interp::run_script`].
    /// The first slot holds an empty script and counts toward `max_scripts`.
    pub fn realm(heap: Heap, limits: EvalLimits) -> Result<Interp<'p>, Error> {
        crate::parse_cache::new_realm();
        let empty = crate::parse::parse_script("", Default::default()).map_err(|_| unsupported("empty script", Span::default()))?;
        let mut it = Self::with_code(Rc::new(empty.into_owned()), heap, limits)?;
        it.heap.set_setup(true);
        it.install_builtins()?;
        it.install_sc()?;
        it.install_ext()?;
        it.install_array_object()?;
        it.install_promise()?;
        it.install_binary()?;
        it.install_reflect()?;
        it.install_date()?;
        it.install_intl()?;
        it.install_bigint()?;
        it.heap.set_setup(false);
        Ok(it)
    }

    /// Parse `source` as a script, check it (Unsupported is found before anything runs), keep it, and run it in
    /// the shared realm. It sees every earlier global (`var`, functions, `let`, `const`) and closures from earlier
    /// scripts keep running their own code. Redeclaring an existing lexical, or a lexical over an existing `var` or
    /// non-configurable global, throws a `SyntaxError` before the script changes anything.
    ///
    /// Errors: a syntax error is a thrown `SyntaxError`. [`Error::Unsupported`] has the span in THIS source.
    /// `Limit(Scripts | ScriptSource | ScriptNodes)` means the realm refused the script and nothing changed.
    /// The text is copied: the host may drop `source` at once. Not allowed while a call is active.
    pub fn run_script(&mut self, source: &str) -> Result<Value, Error> {
        if self.depth != 0 {
            return Err(unsupported("run_script during a call", Span::default()));
        }
        let id = self.add_script(source)?;
        self.exec_script(id)
    }

    /// Parse, check and keep a script. On any error nothing is kept.
    fn add_script(&mut self, source: &str) -> Res<usize> {
        self.add_script_kind(source, crate::parse_cache::Kind::Classic)
    }

    fn add_script_kind(&mut self, source: &str, kind: crate::parse_cache::Kind) -> Res<usize> {
        if self.scripts.len() >= self.limits.max_scripts {
            return Err(Error::Limit(LimitKind::Scripts));
        }
        // The same classic text keeps one slot: its code and caches do not depend on how often it runs, and a new run only
        // instantiates it again. Each run still makes its own functions and checks its own declarations. A harness that
        // polls with one text (`expect js`) would otherwise fill `max_scripts`. A full realm refuses first, so a refusal costs no lookup.
        let key = (kind == crate::parse_cache::Kind::Classic).then(|| (script_hash(source.as_bytes()) >> (64 - SCRIPT_IDS_BITS)) as usize);
        if let Some(k) = key {
            if let Some(&id) = self.script_ids.get(k).filter(|&&id| id != 0) {
                let id = id as usize - 1;
                let prog = if id == self.cur { &self.prog } else { &self.scripts[id].prog };
                if prog.source == source {
                    return Ok(id);
                }
            }
        }
        let src_room = self.limits.max_script_source.saturating_sub(self.script_source);
        let per_script_source = src_room.min(crate::lex::MAX_SOURCE_LEN);
        if source.len() > per_script_source {
            return Err(Error::Limit(LimitKind::ScriptSource));
        }
        let node_room = self.limits.max_script_nodes.saturating_sub(self.script_nodes);
        let pl = crate::parse::Limits { max_source: per_script_source, max_nodes: node_room.min(u32::MAX as usize) as u32, ..Default::default() };
        let parsed = match crate::parse_cache::parse(kind, source, pl) {
            Ok(p) => p,
            Err(e) => {
                use crate::parse::{ErrorKind as K, LimitKind as L};
                return Err(match e.kind {
                    K::Limit(L::Source) => Error::Limit(LimitKind::ScriptSource),
                    K::Limit(L::Nodes | L::List) => Error::Limit(LimitKind::ScriptNodes),
                    K::Limit(L::Depth) => Error::Limit(LimitKind::TraversalDepth),
                    K::Unsupported => unsupported(e.msg, Span { start: e.start, end: e.end }),
                    _ => self.mk_err(4, e.msg),
                });
            }
        };
        let st = parsed.stats();
        let nodes = st.nodes() + st.list_items;
        if nodes > node_room {
            return Err(Error::Limit(LimitKind::ScriptNodes));
        }
        let (id, old, names_mark, sc_mark) = (self.scripts.len(), self.cur, self.names.len(), self.sc_all.len());
        self.scripts.push(Code::new(parsed));
        self.select(id);
        let r = self.in_call(|it| it.walk_program());
        self.select(old);
        if let Err(e) = r {
            self.scripts.pop();
            self.ext.lits.retain(|k, _| k.0 as usize != id);
            for n in self.names.drain(names_mark..) {
                self.name_map.remove(&n);
            }
            self.name_strs.truncate(names_mark);
            self.name_gpos.truncate(names_mark);
            self.sc_all.truncate(sc_mark);
            self.async_forget(id);
            return Err(e);
        }
        self.script_source += source.len();
        self.script_nodes += nodes;
        if let Some(k) = key {
            if self.script_ids.is_empty() {
                self.script_ids = vec![0; 1 << SCRIPT_IDS_BITS];
            }
            self.script_ids[k] = id as u32 + 1;
        }
        Ok(id)
    }

    /// Run indirect eval in a fresh lexical environment outside the caller.
    #[cold]
    #[inline(never)]
    pub fn eval_global(&mut self, value: Value) -> Res<Value> {
        let Value::String(s)=value else { return Ok(value) };
        let source=h!(self,self.heap.to_string_lossy(s));
        let id=self.add_script(&source)?;
        let (old,env,strict,completion)=(self.cur,self.env,self.strict,self.completion);
        let scope=std::mem::take(&mut self.hs.scope);
        // Completion tracking uses depth zero. Keep the remaining call budget.
        let (depth,call_limit)=(self.depth,self.limits.max_call_depth);
        self.limits.max_call_depth=call_limit.saturating_sub(depth);
        self.depth=0;
        self.select(id);
        self.strict=self.prog.strict;
        let mut eval_env=None;
        let r=(|| {
            self.env=self.new_env(0,self.strict)?;
            eval_env=Some(self.env);
            self.completion=Value::Undefined;
            let prog=self.prog.clone();
            let top=self.scopes.len()-1;
            for i in 0..self.scopes[top].vars.len() {
                let n=self.scopes[top].vars[i];
                if self.strict { self.env_declare(self.env,n,INIT|MUT,Value::Undefined); }
                else {
                    if self.find_in(0,n).is_some() { return Err(self.mk_err(4,"Eval var conflicts with a global lexical binding")); }
                    let k=self.name_key(n)?;
                    if !hx!(self,self.heap.has_own_property(self.global,k)) { self.def(self.global,k,Value::Undefined,true,true,true)?; }
                }
            }
            for stmt in prog.list(prog.body) {
                match prog.stmt(stmt).kind {
                    StmtKind::Var {kind,decls} if kind != VarKind::Var => {
                        for decl in prog.list(decls) { self.declare_pat_lexical(prog.decl(decl).target,if kind==VarKind::Let {MUT} else {0}); }
                    }
                    StmtKind::Class(c)=>self.declare_class(c),
                    StmtKind::Function(fid)=>{
                        let f=self.make_closure(fid,None,false)?;
                        let n=self.func_name[fid.index()];
                        if self.strict {self.env_declare(self.env,n,INIT|MUT,f);}
                        else {
                            if self.find_in(0,n).is_some() {return Err(self.mk_err(4,"Eval function conflicts with a global lexical binding"));}
                            let k=self.name_key(n)?;
                            let own=hx!(self,self.heap.get_own_property(self.global,k));
                            self.def(self.global,k,f,true,true,own.is_none_or(|p|p.configurable()))?;
                        }
                    }
                    _=>{}
                }
            }
            match self.exec_list(prog.body) {
                Ok(())=>Ok(self.completion),
                Err(Ctl::Err(e))=>Err(e),
                Err(_)=>Err(unsupported("stray eval control flow",Span::default())),
            }
        })();
        if let Some(e)=eval_env { self.release(e); }
        self.select(old);
        self.env=env;
        self.strict=strict;
        self.completion=completion;
        self.hs.scope=scope;
        self.depth=depth;
        self.limits.max_call_depth=call_limit;
        r
    }

    /// CreateDynamicFunction: `Function(p1, p2, body)`, and the same for the generator and async kinds. `kind`: 0 normal, 1 generator, 2 async.
    /// The text is a script of one function declaration, which is parsed and walked but not run, so no global changes. The closure
    /// has the global environment. The parameters and the body must parse alone, so a comment or a brace cannot reach across.
    fn dynamic_function(&mut self, kind: u8, args: &[Value]) -> Res<Value> {
        let (head, last) = (["function", "function*", "async function", "async function*"][kind as usize], args.len().saturating_sub(1));
        let mut params = String::new();
        for (i, &a) in args[..last].iter().enumerate() {
            let s = self.coerce_string(a)?;
            params.push_str(&if i > 0 { format!(",{}", h!(self, self.heap.to_string_lossy(s))) } else { h!(self, self.heap.to_string_lossy(s)) });
        }
        let body = match args.last() {
            Some(&b) => {
                let s = self.coerce_string(b)?;
                h!(self, self.heap.to_string_lossy(s))
            }
            None => String::new(),
        };
        for part in [format!("{head} anonymous({params}\n) {{\n}}"), format!("{head} anonymous(\n) {{\n{body}\n}}")] {
            if let Err(e) = crate::parse::parse_script(&part, Default::default()) {
                return Err(if e.kind == crate::parse::ErrorKind::Unsupported { unsupported(e.msg, Span::default()) } else { self.mk_err(4, e.msg) });
            }
        }
        let id = self.add_script(&format!("{head} anonymous({params}\n) {{\n{body}\n}}"))?;
        let prog = self.scripts[id].prog.clone();
        let mut stmts = prog.list(prog.body);
        let (Some(st), None) = (stmts.next(), stmts.next()) else { return Err(self.mk_err(4, "Unexpected token '}'")) };
        let StmtKind::Function(fid) = prog.stmt(st).kind else { return Err(self.mk_err(4, "Unexpected token '}'")) };
        let (old, env) = (self.cur, self.env);
        self.select(id);
        self.env = 0;
        let f = self.make_closure(fid, None, false);
        self.env = env;
        self.select(old);
        f
    }

    /// Make script `id` the current one: its code and caches go into the `Interp` fields.
    fn select(&mut self, id: usize) {
        if id == self.cur {
            return;
        }
        let c = &mut self.scripts[id];
        std::mem::swap(&mut self.prog, &mut c.prog);
        std::mem::swap(&mut self.expr_name, &mut c.expr_name);
        std::mem::swap(&mut self.pat_name, &mut c.pat_name);
        std::mem::swap(&mut self.stmt_label, &mut c.stmt_label);
        std::mem::swap(&mut self.func_name, &mut c.func_name);
        std::mem::swap(&mut self.site_strs, &mut c.site_strs);
        std::mem::swap(&mut self.prop_str, &mut c.prop_str);
        std::mem::swap(&mut self.scopes, &mut c.scopes);
        std::mem::swap(&mut self.slots, &mut c.slots);
        std::mem::swap(&mut self.cls.fflags, &mut c.fflags);
        std::mem::swap(&mut self.cls.cflags, &mut c.cflags);
        // slot `cur` held the placeholder and slot `id` now holds the old current code: exchange them
        self.scripts.swap(self.cur, id);
        self.cur = id;
    }

    /// The function node `fid` of script `script` (a closure's own code, not the current script's).
    fn func_of(&self, script: u32, fid: FuncId) -> Func {
        if script as usize == self.cur { *self.prog.func(fid) } else { *self.scripts[script as usize].prog.func(fid) }
    }

    /// Cached strings of the parked scripts, for `roots`.
    fn script_roots(&self, v: &mut Vec<Value>) {
        for c in &self.scripts {
            v.extend(c.site_strs.iter().map(|&s| Value::String(s)));
            v.extend(c.prop_str.iter().flatten().map(|&s| Value::String(s)));
        }
    }

    /// Run script `id` at the top level and give back the caller's script.
    fn exec_script(&mut self, id: usize) -> Result<Value, Error> {
        let old = self.cur;
        self.begin();
        self.completion = Value::Undefined;
        self.select(id);
        let r = self.in_call(|it| it.in_frame(id as u32, Where::Top, false, |it| it.exec_current()));
        self.select(old);
        self.env = 0;
        r
    }

    /// GlobalDeclarationInstantiation checks (before anything is declared), for the current script.
    fn check_global_decls(&mut self) -> Res<()> {
        let prog = self.prog.clone();
        let top = self.scopes.len() - 1;
        let (mut lex, mut funcs) = (Vec::new(), Vec::new());
        for s in prog.list(prog.body) {
            match prog.stmt(s).kind {
                StmtKind::Var { kind, decls } if kind != VarKind::Var => {
                    for d in prog.list(decls) {
                        self.pat_names(prog.decl(d).target, &mut lex);
                    }
                }
                StmtKind::Function(fid) => funcs.push(self.func_name[fid.index()]),
                _ => {}
            }
        }
        let redeclared = |it: &mut Self, n: u32| it.mk_err(4, &format!("Identifier '{}' has already been declared", it.names[n as usize]));
        for &n in &lex {
            let key = self.name_key(n)?;
            if self.find_in(0, n).is_some() || is_restricted(hx!(self, self.heap.get_own_property(self.global, key))) {
                return Err(redeclared(self, n));
            }
        }
        for i in 0..self.scopes[top].vars.len() + funcs.len() {
            let n = if i < funcs.len() { funcs[i] } else { self.scopes[top].vars[i - funcs.len()] };
            if self.find_in(0, n).is_some() {
                return Err(redeclared(self, n));
            }
            if i < funcs.len() {
                // CanDeclareGlobalFunction: an existing non-configurable property must be a writable, enumerable data property
                let key = self.name_key(n)?;
                match hx!(self, self.heap.get_own_property(self.global, key)) {
                    Some(Property::Data { writable: true, enumerable: true, .. }) | None => {}
                    Some(Property::Data { configurable: true, .. } | Property::Accessor { configurable: true, .. }) => {}
                    Some(_) => return Err(self.type_err(&format!("Cannot redefine property: {}", self.names[n as usize]))),
                }
            }
        }
        Ok(())
    }

    /// Instantiate and run the current script's body in the global environment.
    fn exec_current(&mut self) -> Result<Value, Error> {
        self.env = 0;
        self.strict = self.prog.strict;
        let prog = self.prog.clone();
        self.check_global_decls()?;
        let n = self.scopes.len() - 1;
        for i in 0..self.scopes[n].vars.len() {
            let name = self.scopes[n].vars[i];
            let key = self.name_key(name)?;
            if !hx!(self, self.heap.has_own_property(self.global, key)) {
                let d = PropertyDescriptor { value: Some(Value::Undefined), writable: Some(true), enumerable: Some(true), configurable: Some(false), ..Default::default() };
                h!(self, self.heap.define_own_property(self.global, key, &d));
            }
        }
        let r = match self.declare_scope(prog.body, true) {
            Ok(()) => self.exec_list(prog.body),
            Err(e) => Err(Ctl::Err(e)),
        };
        self.env = 0;
        match r {
            Ok(()) => Ok(self.completion),
            Err(Ctl::Err(e)) => Err(e),
            Err(_) => Err(unsupported("stray break, continue or return", Span::default())),
        }
    }
}

