use super::*;
use bare_js_api::clone::{CloneHost, CloneLimits, CloneOpts, Cloned, RecordWriter, Refused};
use bare_js_api::{Scope, task};
use std::any::Any;

const LIMITS: CloneLimits = CloneLimits { bytes: 4096, depth: 16, nodes: 64, names: ["bytes", "depth", "nodes"] };

struct CloneFixture;

impl CloneHost<Realm> for CloneFixture {
    fn is_host_object(&self, _: &Value) -> bool { false }
    fn can_transfer(&self, _: &Value) -> bool { false }
    fn transfer_out(&mut self, _: &Value) -> Box<dyn Any> { unreachable!() }
    fn transfer_in<'s>(&mut self, _: &mut Scope<'s, Realm>, _: Vec<Box<dyn Any>>) -> Result<Vec<Local<'s>>, Error<'s>> { unreachable!() }
    fn data_clone_error<'s>(&mut self, s: &mut Scope<'s, Realm>, why: Refused) -> Error<'s> {
        s.type_error(&format!("DataCloneError: {why:?}"))
    }
}

#[test]
fn clone_preserves_graph_and_internal_slots() {
    let mut realm = crate::script::load("").unwrap().realm;
    task(&mut realm, |s| {
        let v = s.run_script(r#"
            let buf = new ArrayBuffer(8), view = new Uint16Array(buf, 2, 2);
            view[0] = 42;
            let a = [, NaN, -0, "\ud800", 1n << 130n], x = {
                a, view, buf, data: new DataView(buf, 1, 4), d: new Date(123),
                r: /a+/gi, m: new Map, set: new Set, w: Object(10n),
                e: new TypeError("boom")
            };
            a.extra = x; x.m.set(x, a); x.set.add(x.m); x.e.cause = x;
            x.r.lastIndex = 5;
            Object.defineProperty(x, "gone", {enumerable: true, configurable: true, value: 3});
            Object.defineProperty(x, "first", {enumerable: true, get() { delete x.last; return x.a }});
            x.last = 4; x[Symbol()] = 5; x
        "#).unwrap();
        let opts = CloneOpts { limits: LIMITS, transfer: &[] };
        let mut record = s.engine().clone_out(v, &opts, &mut CloneFixture).unwrap();
        let cp = s.engine().clone_in(&mut record, &mut CloneFixture).unwrap().value;
        let again = s.engine().clone_in(&mut record, &mut CloneFixture).unwrap().value;
        assert_ne!(s.engine().raw(cp), s.engine().raw(again));
        let g = s.global();
        s.set_str(g, "cp", cp).unwrap();
        let ok = s.run_script(r#"
            cp !== x && cp.a.extra === cp && cp.first === cp.a && !("last" in cp) &&
            cp.gone === 3 && Object.getOwnPropertySymbols(cp).length === 0 &&
            !(0 in cp.a) && Number.isNaN(cp.a[1]) && Object.is(cp.a[2],-0) &&
            cp.a[3].charCodeAt(0) === 0xd800 && cp.a[4] === 1n << 130n &&
            cp.m.get(cp) === cp.a && cp.set.has(cp.m) && cp.d.getTime() === 123 &&
            cp.view.buffer === cp.buf && cp.view.byteOffset === 2 && cp.view[0] === 42 &&
            cp.data.buffer === cp.buf && cp.data.byteOffset === 1 &&
            cp.r.source === "a+" && cp.r.flags === "gi" && cp.r.lastIndex === 0 &&
            cp.e instanceof TypeError && cp.e.message === "boom" && cp.e.cause === cp &&
            cp.w.valueOf() === 10n
        "#).unwrap();
        assert!(s.to_boolean(ok));
    }).unwrap();
}

#[test]
fn clone_limits_and_refusals() {
    let mut realm = crate::script::load("").unwrap().realm;
    task(&mut realm, |s| {
        for (src, why) in [("() => 1", "Function"), ("Symbol()", "Symbol"), ("new Proxy({}, {})", "Object")] {
            let v = s.run_script(src).unwrap();
            let Err(Error::Throw(e)) = s.engine().clone_copy(v, &LIMITS, &mut CloneFixture) else { panic!("{src} was not refused") };
            let reason = s.get_str(e, "message").unwrap();
            assert_eq!(s.as_str(reason).unwrap().to_string_lossy(), format!("DataCloneError: {why}"));
        }
        let v = s.run_script("({a: 'bc'})").unwrap();
        let (_, cost) = s.engine().clone_copy(v, &LIMITS, &mut CloneFixture).unwrap();
        assert_eq!(cost, 86);
        let v = s.run_script("({child: {}})").unwrap();
        for (limits, name) in [
            (CloneLimits { depth: 1, ..LIMITS }, "depth"),
            (CloneLimits { nodes: 1, ..LIMITS }, "nodes"),
            (CloneLimits { bytes: 48, ..LIMITS }, "bytes"),
        ] {
            assert_eq!(s.engine().clone_copy(v, &limits, &mut CloneFixture).err(), Some(Error::Abort(Abort::Limit(name))));
        }
    }).unwrap();
}

#[test]
fn clone_moves_a_buffer_and_checks_dense_records() {
    let mut realm = crate::script::load("").unwrap().realm;
    task(&mut realm, |s| {
        let v = s.run_script("new Uint8Array([17,19])").unwrap();
        let b = s.get_str(v, "buffer").unwrap();
        let mut c = s.engine().clone_out(v, &CloneOpts { limits: LIMITS, transfer: &[b] }, &mut CloneFixture).unwrap();
        assert!(s.engine().is_detached(b));
        let copy = s.engine().clone_in(&mut c, &mut CloneFixture).unwrap().value;
        assert_eq!(s.engine().bytes(copy), Some([17, 19].as_slice()));

        let mut writer = RecordWriter::new();
        writer.array_dense(3);
        writer.hole();
        writer.int(2);
        writer.reference(0);
        writer.end();
        let bytes = writer.finish();
        for n in 0..bytes.len() {
            let mut c = Cloned::new(bytes[..n].into(), vec![], 0);
            assert!(s.engine().clone_in(&mut c, &mut CloneFixture).is_err(), "cut at {n}");
        }
        let mut c = Cloned::new(bytes, vec![], 0);
        let copy = s.engine().clone_in(&mut c, &mut CloneFixture).unwrap().value;
        let self_ref = s.get_str(copy, "2").unwrap();
        assert_eq!(s.engine().raw(copy), s.engine().raw(self_ref));
    }).unwrap();
}
