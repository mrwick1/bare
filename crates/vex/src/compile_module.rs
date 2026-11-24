//! Modules in the compiler (`docs/internal/design-js2-host-api.md` section 12). [`compile_module`] turns the code of a module into one function
//! with one parameter, `phase`: 0 runs the body, 1 makes the function declarations (so the host can do it at link time, before any
//! module runs). Every binding of the top level has a slot of the module context, which the host makes and gives to the function as
//! its closure context: the compiler builds the [`Level`] of that context and compiles the function as a child of it. A binding that is
//! exported, and every import, is a property of an environment object ([`LevelVar::env`]); the slot holds the object.
use super::*;
use super::stmt::Hoist;

/// What an import takes: `None` is the namespace (`import * as ns`).
#[derive(Clone, Debug)]
pub struct Imp {
    pub req: u32,
    pub name: Option<Box<str>>,
    /// The context slot of the binding: an environment object, a view, or the namespace.
    pub slot: u32,
}

#[derive(Clone, Debug)]
pub enum ExpSrc {
    /// A binding of this module: the property of the environment object.
    Local(Box<str>),
    /// A re-export of `name` (`None`: the namespace) of a request.
    Indirect(u32, Option<Box<str>>),
}

#[derive(Clone, Debug)]
pub struct Exp {
    pub name: Box<str>,
    pub src: ExpSrc,
}

/// The facts of a module that the host needs after the tree is gone.
#[derive(Clone, Debug, Default)]
pub struct ModInfo {
    pub requests: Vec<Box<str>>,
    pub imports: Vec<Imp>,
    pub exports: Vec<Exp>,
    pub stars: Vec<u32>,
    /// The properties of the environment object, and whether each one starts as the hole (else `undefined`).
    pub env_props: Vec<(Box<str>, bool)>,
    /// The size of the context, and the slots that start as `undefined` (the rest are the hole).
    pub slots: u32,
    pub undef: Vec<u32>,
    /// The slots of `this`, of the module number (`%module`) and of the `import.meta` object (`%meta`).
    pub this_slot: u32,
    pub id_slot: u32,
    pub meta_slot: u32,
    /// The code has a top-level `await`: it is an async function, and both phases give a promise (`compile_async.rs`).
    pub has_tla: bool,
}

/// Compile the top level of a module (`parse_module_lazy`).
pub fn compile_module(prog: &Program, facts: &Facts, atoms: &SharedAtoms) -> CR<(Compiled, ModInfo)> {
    let mi = prog.module.as_ref().expect("a module");
    let mut c = Fc::new(prog, facts, atoms, &Input::default());
    c.strict = true;
    if mi.has_tla {
        (c.gen_kind, c.noctor) = (crate::bytecode::GK_ASYNC, true);
    }
    let mut h = Hoist::default();
    c.hoist_list(prog.body, true, &mut h);
    let mut info = ModInfo { requests: mi.requests.to_vec(), stars: mi.stars.clone(), has_tla: mi.has_tla, ..Default::default() };
    // The export names of each local binding. The first one names the property of the environment object.
    let mut keys: HashMap<Box<str>, Box<str>> = HashMap::new();
    let mut has_default = false;
    for e in &mi.exports {
        let src = match &e.source {
            ExportSource::Local(sp) => {
                let local: Box<str> = prog.name(*sp).as_ref().into();
                let key = keys.entry(local).or_insert_with(|| e.name.clone()).clone();
                ExpSrc::Local(key)
            }
            ExportSource::Default => {
                has_default = true;
                let key = keys.entry("*default*".into()).or_insert_with(|| e.name.clone()).clone();
                ExpSrc::Local(key)
            }
            ExportSource::Indirect { request, name } => ExpSrc::Indirect(*request, import_name(name)),
        };
        info.exports.push(Exp { name: e.name.clone(), src });
    }
    // Slot 0 is the environment object of the module itself.
    let mut vars: Vec<LevelVar> = Vec::new();
    let mut next = 1u32;
    let add = |vars: &mut Vec<LevelVar>, name: &str, tdz: bool, konst: bool, env: Option<Box<str>>, slot: u32| {
        vars.push(LevelVar { name: name.into(), slot, tdz, konst, callee: false, env });
    };
    for i in &mi.imports {
        let local = prog.name(i.local);
        let slot = next;
        next += 1;
        match &i.name {
            ImportName::Name(n) => add(&mut vars, &local, true, true, Some(n.clone()), slot),
            ImportName::Namespace => add(&mut vars, &local, true, true, None, slot),
        }
        info.imports.push(Imp { req: i.request, name: import_name(&i.name), slot });
    }
    let mut seen: HashMap<Cow<str>, ()> = HashMap::new();
    let local = |vars: &mut Vec<LevelVar>, info: &mut ModInfo, name: &str, kind: VK, next: &mut u32| {
        let (tdz, konst) = (matches!(kind, VK::Let | VK::Const | VK::Class), kind == VK::Const);
        if let Some(key) = keys.get(name) {
            vars.push(LevelVar { name: name.into(), slot: 0, tdz, konst, callee: false, env: Some(key.clone()) });
            if !info.env_props.iter().any(|(k, _)| k == key) {
                info.env_props.push((key.clone(), tdz));
            }
            return;
        }
        vars.push(LevelVar { name: name.into(), slot: *next, tdz, konst, callee: false, env: None });
        if !tdz {
            info.undef.push(*next);
        }
        *next += 1;
    };
    for (n, k) in h.vars.iter().map(|n| (n, VK::Var)).chain(h.funcs.iter().map(|(_, n)| (n, VK::Func))).chain(h.lex.iter().map(|(n, k, _)| (n, *k))) {
        if seen.insert(n.clone(), ()).is_none() {
            local(&mut vars, &mut info, n, k, &mut next);
        }
    }
    if has_default {
        local(&mut vars, &mut info, "*default*", VK::Let, &mut next);
    }
    for (name, undef) in [("this", true), ("%module", false), ("%meta", false)] {
        add(&mut vars, name, false, true, None, next);
        match name {
            "this" => info.this_slot = next,
            "%module" => info.id_slot = next,
            _ => info.meta_slot = next,
        }
        if undef {
            info.undef.push(next);
        }
        next += 1;
    }
    info.slots = next;
    let level = Rc::new(Level::new(None, vars));
    c.outer = Some(level.clone());
    c.cur_ctx = Some(level);
    let kids: Vec<_> = facts.children(None).collect();
    c.note_children(&kids);
    // The phase is the one parameter.
    let phase = c.alloc();
    c.set_live(phase);
    c.async_init();
    let init = c.label();
    c.jmp_r(Op::JmpIf, phase, init);
    for s in prog.list(prog.body) {
        c.stmt(s)?;
    }
    c.ret_undef();
    c.bind(init);
    for (f, name) in &h.funcs {
        c.declare_function(*f, name)?;
    }
    for s in prog.list(prog.body) {
        if let StmtKind::ExportDefault(e, true) = prog.stmt(s).kind {
            c.export_default(e, true, true)?;
        }
    }
    Ok((c.finish(1)?, info))
}

fn import_name(n: &ImportName) -> Option<Box<str>> {
    match n {
        ImportName::Name(n) => Some(n.clone()),
        ImportName::Namespace => None,
    }
}

impl<'a> Fc<'a> {
    /// `export default expr`. A function that is not in parentheses is made in phase 1 (`init`), the others when the statement runs.
    pub(crate) fn export_default(&mut self, e: ExprId, hoisted: bool, init: bool) -> CR {
        if hoisted != init {
            return Ok(());
        }
        let m = self.mark();
        let t = self.alloc();
        self.name_hint(e, "default");
        self.to(e, t)?;
        self.store_var("*default*", u32::MAX, t, true)?;
        self.free(m);
        Ok(())
    }

    /// A read of a module binding: the property of the environment object in the slot. The hole is the dead zone.
    pub(crate) fn read_env(&mut self, name: &str, hops: u32, slot: u32, tdz: bool, dst: u32) -> CR {
        let key = self.env_key(name).expect("an environment variable");
        let s = self.site_of(&key)?;
        self.opx(Op::GetCtxField, dst, hops, slot, s);
        if tdz {
            self.set_live(dst);
            let a = self.atom(name)?;
            self.op(Op::CheckInit, dst, a, 0);
        }
        Ok(())
    }

    /// A write of a module binding. An import and a `const` throw; a `let` in its dead zone throws.
    pub(crate) fn store_env(&mut self, name: &str, hops: u32, slot: u32, tdz: bool, konst: bool, src: u32, init: bool) -> CR {
        let a = self.atom(name)?;
        if konst && !init {
            if tdz {
                let t = self.alloc();
                self.read_env(name, hops, slot, true, t)?;
                self.free(t);
            }
            self.op(Op::ThrowConst, 0, a, 0);
            return Ok(());
        }
        let m = self.mark();
        if tdz && !init {
            let t = self.alloc();
            self.read_env(name, hops, slot, true, t)?;
        }
        let key = self.env_key(name).expect("an environment variable");
        let s = self.site_of(&key)?;
        let o = self.alloc();
        self.op(Op::GetCtx, o, hops, slot);
        self.set_live(o);
        self.op(Op::SetField, o, src, s);
        self.free(m);
        Ok(())
    }

    /// `import(specifier, options)`: a call of the hidden global `%import` with the specifier and the module that asks (`undefined` in a
    /// classic script). The options are evaluated and dropped.
    pub(crate) fn import_call(&mut self, arg: ExprId, opts: Option<ExprId>, dst: u32) -> CR {
        let m = self.mark();
        let base = self.window(3);
        let s = self.site_of("%import")?;
        self.op(Op::GetGlobal, base, s, 0);
        self.set_live(base);
        self.to(arg, base + 1)?;
        if let Some(o) = opts {
            self.effect(o)?;
        }
        if matches!(self.resolve("%module", 0), Res::Global) {
            self.op(Op::LoadUndef, base + 2, 0, 0);
            self.set_live(base + 2);
        } else {
            self.read_var("%module", 0, base + 2)?;
            self.set_live(base + 2);
        }
        self.line(self.prog.expr(arg).span.start);
        self.op(Op::CallU, dst, base, 2);
        self.free(m);
        Ok(())
    }
}
