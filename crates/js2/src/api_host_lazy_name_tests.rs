use super::*;
use bare_js_api::{Args, Error, Host, Local, Scope};
use std::cell::{Cell, RefCell};

thread_local! {
    pub(super) static CONVERSIONS: Cell<usize> = const { Cell::new(0) };
}

#[derive(Default)]
struct Names {
    seen: RefCell<Vec<String>>,
}

impl Host<Realm> for Names {
    fn call<'s>(&self, s: &mut Scope<'s, Realm>, _: u32, _: Local<'s>, _: Args<'s>, _: bool) -> Result<Local<'s>, Error<'s>> {
        Ok(s.undefined())
    }

    fn lazy_member<'s>(&self, _: &mut Scope<'s, Realm>, _: Local<'s>, what: Lazy<'_>) -> Result<(), Error<'s>> {
        self.seen.borrow_mut().push(match what {
            Lazy::All => "all".into(),
            Lazy::Symbols => "symbols".into(),
            Lazy::Name(n) => n.into(),
        });
        Ok(())
    }
}

#[test]
fn each_lazy_name_is_converted_only_on_the_first_ask() {
    let mut r = crate::script::load("").unwrap().realm;
    let host = std::rc::Rc::new(Names::default());
    r.set_host(host.clone());
    let o = Obj::new_plain(&mut r);
    let name = r.intern("雪");
    r.keep(o.value());
    r.keep(name.value());
    let keys = [Some(Key::Name(name)), Some(Key::Idx(70)), Some(Key::Name(r.wk[crate::bi_symbol::ITERATOR])), None];
    CONVERSIONS.with(|n| n.set(0));
    for round in 1..=2 {
        for key in keys {
            assert!(r.hs_lazy(o, key).unwrap());
            assert!(!r.hs_lazy(o, key).unwrap());
        }
        assert_eq!(CONVERSIONS.with(Cell::get), round * 2);
        r.collect(&mut [], |_| {});
    }
    assert_eq!(*host.seen.borrow(), ["雪", "70", "symbols", "all", "雪", "70", "symbols", "all"]);
}
