use super::*;
use crate::shapes::{A_C, A_W};

pub(super) struct Inactive {
    state: RealmState,
    pr: crate::promise::Promises,
    rx: Option<Box<crate::regexp_rt::Rx>>,
    rng: u64,
    coll: Option<Box<crate::bi_collections::Colls>>,
}

pub(super) struct Realms {
    active: usize,
    states: Vec<Option<Inactive>>,
    owners: HashMap<u64, (Value, usize)>,
    host: &'static str,
}

impl Realms {
    pub(super) fn mark(&self, m: &mut Marker) {
        for realm in self.states.iter().flatten() {
            realm.state.mark_roots(m);
            realm.pr.walk(m);
            if let Some(rx) = &realm.rx {
                rx.mark(m);
            }
        }
    }

    pub(super) fn prune(&mut self, live: &crate::gc::Live) {
        // SAFETY: entries contain objects allocated in the collected heap.
        self.owners.retain(|_, (value, _)| unsafe { live.value(*value) });
    }
}

impl Realm {
    pub(crate) fn note_home(&mut self, o: Obj) {
        if let Some(realms) = &mut self.realms
            && realms.active != 0
        {
            let v = o.value();
            realms.owners.insert(v.bits(), (v, realms.active));
        }
    }

    pub(crate) fn cross_realm_enabled(&self) -> bool {
        self.realms.is_some()
    }

    pub(crate) fn active_realm(&self) -> usize {
        self.realms.as_ref().map_or(0, |r| r.active)
    }

    pub(crate) fn home_realm(&self, value: Value) -> usize {
        self.realms.as_ref().and_then(|r| r.owners.get(&value.bits())).map_or(0, |&(_, id)| id)
    }

    pub(crate) fn activate_function(&mut self, f: Value) -> usize {
        let previous = self.active_realm();
        if self.realms.is_some() {
            self.activate_realm(self.home_realm(f));
        }
        previous
    }

    pub(crate) fn activate_realm(&mut self, id: usize) {
        let Some(realms) = &mut self.realms else { return };
        if realms.active == id {
            return;
        }
        let mut target = realms.states[id].take().expect("inactive realm");
        std::mem::swap(&mut target.state, &mut self.state);
        self.pr.swap_intrinsics(&mut target.pr);
        std::mem::swap(&mut target.rx, &mut self.rx);
        std::mem::swap(&mut target.rng, &mut self.rng);
        std::mem::swap(&mut target.coll, &mut self.coll);
        realms.states[realms.active] = Some(target);
        realms.active = id;
        self.state.ltab.array_iter_spot = None;
        self.shapes.watch(self.state.global.cell().as_ptr() as usize);
    }

    pub(crate) fn in_realm<T>(&mut self, id: usize, run: impl FnOnce(&mut Self) -> T) -> T {
        let previous = self.active_realm();
        self.activate_realm(id);
        let value = run(self);
        self.activate_realm(previous);
        value
    }

    /// Install the Test262 realm factory after the host script has initialized $262.
    pub fn install_test262_realms(&mut self, host: &'static str) -> R<()> {
        if self.realms.is_none() {
            if self.es.base != 0 {
                return self.type_error("initialize realm support before running the script");
            }
            self.realms = Some(Box::new(Realms { active: 0, states: vec![None], owners: HashMap::new(), host }));
        }
        self.install_realm_factory()
    }

    fn install_realm_factory(&mut self) -> R<()> {
        let host = self.get_named(self.global.value(), "$262")?;
        let Some(object) = Self::obj(host) else {
            return self.type_error("Test262 host object is missing");
        };
        let factory = self.new_native(create_realm, false, 0, "createRealm");
        let name = self.intern("createRealm");
        self.define_attrs(object, Key::Name(name), A_W | A_C, factory.value())?;
        let eval = self.new_native(eval_script, false, 1, "evalScript");
        let name = self.intern("evalScript");
        self.define_attrs(object, Key::Name(name), A_W | A_C, eval.value())
    }

    fn new_test262_realm(&mut self) -> R<Value> {
        let keys = RealmBootstrapKeys {
            k_code: self.intern("\0code"), k_ctx: self.intern("\0ctx"),
            k_home: self.intern("\0home"), k_fields: self.intern("\0fields"),
            k_link: self.intern("\0link"), k_prim: self.k_prim,
            k_then: self.intern("then"), k_pstate: self.intern("\0pstate"),
            k_presult: self.intern("\0presult"),
            hidden: ["\0native", "\0n1", "\0n2", "\0n3"].map(|n| self.intern(n)),
            k_value: self.intern("value"), k_done: self.intern("done"),
            gkeys: ["\0gstate", "\0gframe", "\0gsent", "\0gmode", "\0gpromise", "\0gentry"].map(|n| self.intern(n)),
            empty: self.intern(""),
        };
        let RealmBootstrap { state, pr, consts, pins } = RealmState::allocate(&mut self.heap, &mut self.shapes, keys);
        self.consts.extend(consts);
        self.pins.extend(pins);
        let realms = self.realms.as_mut().expect("realm factory installed");
        let (id, host) = (realms.states.len(), realms.host);
        realms.states.push(Some(Inactive { state, pr, rx: None, rng: 0, coll: None }));
        self.in_realm(id, |rt| {
            rt.note_home(rt.symbol_proto);
            rt.install_builtins();
            rt.install_shared_globals();
            rt.test262_eval(host)?;
            rt.install_realm_factory()?;
            rt.get_named(rt.global.value(), "$262")
        })
    }

    fn test262_eval(&mut self, source: &str) -> R<Value> {
        let ci = self.script_code(source, crate::compile::ScriptMode { value: true, ..Default::default() })?;
        self.run_code(ci, true)
    }

    pub(crate) fn function_realm(&mut self, mut f: Value) -> R<usize> {
        loop {
            if let Some(bound) = self.bound_function(f) {
                f = bound.slot(&self.shapes, 1);
            } else if let Some(proxy) = Self::obj(f).filter(|o| o.is_kind(K_PROXY)) {
                f = self.proxy_target(proxy)?.value();
            } else {
                return Ok(self.home_realm(f));
            }
        }
    }
}

fn create_realm(rt: &mut Realm, _: Value, _: &[Value]) -> R<Value> {
    rt.new_test262_realm()
}

fn eval_script(rt: &mut Realm, _: Value, args: &[Value]) -> R<Value> {
    let source = rt.to_str(crate::builtins::arg(args, 0))?;
    rt.test262_eval(&rt.text(source))
}

#[cfg(test)]
#[path = "realms_tests.rs"]
mod tests;
