use super::*;
use bare_js_api::{Args, Intrinsic, task};
use std::rc::Rc;

struct HostFixture;

impl Host<Realm> for HostFixture {
    fn call<'s>(&self, s: &mut Scope<'s, Realm>, _: u32, _: Local<'s>, _: Args<'s>, _: bool) -> Result<Local<'s>, Error<'s>> {
        Ok(s.undefined())
    }
}

#[test]
fn typed_indices_stop_at_the_view_in_a_host_chain() {
    let mut r = crate::script::load("").unwrap().realm;
    r.set_host(Rc::new(HostFixture));
    task(&mut r, |s| {
        let p = s.intrinsic(Intrinsic::ObjectPrototype);
        s.set_lazy(p, false).unwrap();
        let v = s.run_script(r#"
            Object.prototype[9] = 91;
            Object.prototype["-0"] = 92;
            let view = new Uint16Array([65535]), child = Object.create(view);
            [view[0], child[0], 0 in view, 0 in child,
             view[9], child[9], 9 in view, 9 in child,
             view["-0"], child["-0"], "-0" in view, "-0" in child].join()
        "#).unwrap();
        assert_eq!(s.as_str(v).unwrap().to_string_lossy(), "65535,65535,true,true,,,false,false,,,false,false");
    }).unwrap();
}
