//! Micro row for the owners of the collector (`api_host.rs`, design section 12): a heap of H objects (each with a small array), then K
//! owners that hold nothing, then R forced collections. The cost of the owners is the difference with K = 0, and it must not grow with H.
//! Count with `tools/js/instr.sh`.
//!   owner_load H K R
use bare_vex::script;
use bare_js_api::{Args, Engine, Error, Host, HostFn, Local, Scope, Attrs, enter};
use bare_vex::interp_rt::Realm;
use std::rc::Rc;

struct Own;

impl Host<Realm> for Own {
    fn call<'s>(&self, s: &mut Scope<'s, Realm>, _id: u32, _this: Local<'s>, args: Args<'s>, _new: bool) -> Result<Local<'s>, Error<'s>> {
        let (o, k) = (args.get(0).unwrap(), args.get(1).unwrap());
        let key = s.as_number(k).unwrap() as u32;
        s.set_host_owner(o, key)?;
        Ok(s.undefined())
    }
}

fn main() {
    let a: Vec<usize> = std::env::args().skip(1).filter_map(|s| s.parse().ok()).collect();
    let [h, k, r] = a[..] else { panic!("usage: owner_load H K R") };
    let mut vm = script::load("").unwrap();
    let rm = &mut vm.realm;
    rm.set_host(Rc::new(Own));
    enter(rm, |s| {
        let f = s.host_function("own", 0, 2, HostFn::Method).unwrap();
        let g = s.global();
        s.define_str(g, "own", f, Attrs::METHOD).unwrap();
    });
    rm.run_text(&format!("var keep = []; for (var i = 0; i < {h}; i++) keep.push({{ a: i, b: [i] }});")).unwrap();
    rm.run_text(&format!("var owners = []; for (var i = 0; i < {k}; i++) {{ var o = {{}}; own(o, i); owners.push(o); }}")).unwrap();
    for _ in 0..r {
        rm.collect(&mut [], |_| {});
    }
}
