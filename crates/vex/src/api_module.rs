//! The module methods of `Engine` for `Realm` (`docs/internal/design-js2-host-api.md` section 12). A child of `api`: it reads the private state of
//! the interface. The work is in `module.rs`.
use super::*;
use bare_js_api::{ImportRequest, Persistent};

impl Realm {
    pub(super) fn api_module_parse<'s>(&mut self, source: &str, url: &str) -> Result<u32, Error<'s>> {
        if self.a_nested() {
            return Err(self.a_unsup("module_parse inside a call"));
        }
        self.api.abort = None;
        let Some(l) = self.lazy.as_mut() else { return Err(self.a_unsup("a realm with no compiler")) };
        match l.module(&self.prog, source) {
            Ok((ci, info)) => {
                l.set_name(ci, url);
                self.fn_info.extend((self.fn_info.len()..self.prog.len()).map(|i| crate::interp::code_info(&self.prog[i])));
                Ok(self.md_add(url, ci, info))
            }
            Err(m) => {
                let (kind, text) = match m.strip_prefix("SyntaxError: ") {
                    Some(t) => (ErrKind::Syntax, t),
                    None => (ErrKind::Plain, m.as_str()),
                };
                Err(self.a_throw(kind, text))
            }
        }
    }

    pub(super) fn api_module_link<'s>(&mut self, root: u32) -> Result<(), Error<'s>> {
        self.api.abort = None;
        self.md_link(root).map_err(|e| self.a_err(e))
    }

    pub(super) fn api_module_evaluate<'s>(&mut self, m: u32) -> Result<Local<'s>, Error<'s>> {
        self.api.abort = None;
        let r = self.md_evaluate(m);
        self.a_ret(r)
    }

    fn a_promise<'s>(&mut self, p: &Persistent) -> Result<Obj, Error<'s>> {
        let v = self.api.persistent[p.id() as usize];
        match Realm::obj(v) {
            Some(o) => Ok(o),
            None => Err(self.a_unsup("a dynamic import with no promise")),
        }
    }

    pub(super) fn api_finish_import<'s>(&mut self, p: &Persistent, m: u32) -> Result<(), Error<'s>> {
        self.api.abort = None;
        let p = self.a_promise(p)?;
        self.md_finish(p, m).map_err(|e| self.a_err(e))
    }

    pub(super) fn api_reject_import<'s>(&mut self, p: &Persistent, reason: Local<'_>) -> Result<(), Error<'s>> {
        let (p, e) = (self.a_promise(p)?, self.a_at(reason));
        self.reject_promise(p, e);
        Ok(())
    }

    pub(super) fn api_fail_import<'s>(&mut self, p: &Persistent, message: &str) -> Result<(), Error<'s>> {
        let p = self.a_promise(p)?;
        let e = self.error(ErrKind::Type, message);
        self.reject_promise(p, e);
        Ok(())
    }

    /// A handle that keeps `v` until it is dropped (for the promise of an `import()`).
    pub(crate) fn api_persist(&mut self, v: Value) -> Persistent {
        let a = &mut self.api;
        let id = a.slots.take();
        match a.persistent.get_mut(id as usize) {
            Some(slot) => *slot = v,
            None => a.persistent.push(v),
        }
        Persistent::from_id(id, a.slots.clone())
    }

    /// `a_call` for the module code (`module.rs`).
    pub(crate) fn api_call(&mut self, f: Value, this: Value, args: &[Value]) -> R<Value> {
        self.a_call(f, this, args)
    }

    pub(super) fn api_take_imports(&mut self) -> Vec<ImportRequest> {
        self.md_take_requests()
    }
}
