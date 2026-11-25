//! Tests of `stack` on errors that the host makes (`Engine::new_error`, for example a DOMException of a DOM call).
use super::*;
use crate::script;
use bare_js_api::{Attrs, HostFn, Scope, enter};

struct MakeError;

impl Host<Realm> for MakeError {
    fn call<'s>(&self, s: &mut Scope<'s, Realm>, _id: u32, _this: Local<'s>, args: Args<'s>, _is_new: bool) -> Result<Local<'s>, Error<'s>> {
        let proto = args.get(0).unwrap();
        s.new_error(proto, "from the host")
    }
}

#[test]
fn an_error_that_the_host_makes_has_the_frames_of_the_script() {
    let mut r = script::load("").unwrap().realm;
    r.set_host(Rc::new(MakeError));
    enter(&mut r, |s| {
        let f = s.host_function("probe", 0, 1, HostFn::Method).unwrap();
        let g = s.global();
        s.define_str(g, "probe", f, Attrs::ALL).unwrap();
    });
    r.run_text("function inner() { return probe(Error.prototype) }\nfunction outer() { return inner() }\nvar text = outer().stack;").unwrap();
    let text = enter(&mut r, |s| {
        let g = s.global();
        let v = s.get_str(g, "text").unwrap();
        s.as_str(v).map(|t| t.to_string_lossy()).unwrap_or_default()
    });
    assert_eq!(text, "Error: from the host\n    at inner (<anonymous>:1:27)\n    at outer (<anonymous>:2:27)\n    at <anonymous>:3:12");
}
