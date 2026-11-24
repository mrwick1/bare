use super::*;
use bare_js_api::{Args, Attrs, Engine, Error, Host, Local, Scope};
use std::cell::Cell;

const MISSING_INDEX: u32 = 3_000_000_001;

#[derive(Default)]
struct IndexedLazy {
    misses: Cell<u32>,
}

impl Host<Realm> for IndexedLazy {
    fn call<'s>(&self, s: &mut Scope<'s, Realm>, _: u32, _: Local<'s>, _: Args<'s>, _: bool) -> Result<Local<'s>, Error<'s>> {
        Ok(s.undefined())
    }

    fn lazy_member<'s>(&self, s: &mut Scope<'s, Realm>, o: Local<'s>, what: Lazy<'_>) -> Result<(), Error<'s>> {
        match what {
            Lazy::Name("3000000001") => self.misses.set(self.misses.get() + 1),
            Lazy::Name("3000000002") => {
                let v = s.number(42.0);
                s.define_str(o, "3000000002", v, Attrs::ALL)?;
            }
            _ => {}
        }
        Ok(())
    }
}

#[test]
fn an_index_miss_does_not_intern_its_name() {
    let mut r = crate::script::load("").unwrap().realm;
    let h = std::rc::Rc::new(IndexedLazy::default());
    r.set_host(h.clone());
    let p = Obj::new_plain(&mut r);
    r.hs_set_lazy(p, false);
    let key = Key::Idx(MISSING_INDEX);
    assert!(r.find_idx_atom(MISSING_INDEX).is_none());
    assert!(r.get_prop(p.value(), key).unwrap().is_undefined());
    assert!(!r.has_prop(p.value(), key).unwrap());
    assert_eq!(h.misses.get(), 1);
    assert!(r.find_idx_atom(MISSING_INDEX).is_none(), "a lookup miss must not create an index atom");
    assert_eq!(r.get_prop(p.value(), Key::Idx(3_000_000_002)).unwrap().as_int(), Some(42));
}

#[test]
fn a_named_index_is_found_after_a_miss_and_in_a_dictionary() {
    let mut vm = crate::script::load("").unwrap();
    let p = vm.realm.object_proto;
    vm.realm.hs_set_lazy(p, false);
    vm.realm.run_text(r#"
        var p = {first: true, other: true}, a = Object.create(p), d = [7];
        var miss = a[3000000007] === undefined;
        p[3000000007] = 33;
        var shape = a[3000000007] === 33;
        delete p.first;
        Object.defineProperty(p, "3000000007", {get: function() {return this === a ? 11 : 12}});
        var ok = miss && shape && a[3000000007] === 11 && 3000000007 in a && d[0] === 7;
    "#).unwrap();
    let p = Realm::obj(vm.global("p")).unwrap();
    assert!(!vm.realm.shapes.cacheable(p.shape(&vm.realm.shapes)));
    assert_eq!(vm.global_text("ok"), "true");
}
