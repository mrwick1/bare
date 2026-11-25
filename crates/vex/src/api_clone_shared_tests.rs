use super::*;
use bare_js_api::clone::{AgentCluster, CloneHost, CloneLimits, CloneOpts, Refused};
use bare_js_api::{Scope, task};
use std::any::Any;

struct Host(Option<AgentCluster>);

impl CloneHost<Realm> for Host {
    fn shared_buffer_cluster(&self) -> Option<&AgentCluster> { self.0.as_ref() }
    fn is_host_object(&self, _: &Value) -> bool { false }
    fn can_transfer(&self, _: &Value) -> bool { false }
    fn transfer_out(&mut self, _: &Value) -> Box<dyn Any + Send> { unreachable!() }
    fn transfer_in<'s>(&mut self, _: &mut Scope<'s, Realm>, _: Vec<Box<dyn Any + Send>>) -> Result<Vec<Local<'s>>, Error<'s>> { unreachable!() }
    fn data_clone_error<'s>(&mut self, s: &mut Scope<'s, Realm>, _: Refused) -> Error<'s> {
        s.type_error("DataCloneError")
    }
}

#[test]
fn clone_shares_backing_only_with_the_same_cluster() {
    let cluster = AgentCluster::default();
    let mut src = crate::script::load("").unwrap().realm;
    src.install_test262_shared_memory(false);
    let (mut good, mut foreign) = task(&mut src, |s| {
        let value = s.run_script("var b = new SharedArrayBuffer(8, {maxByteLength: 16}); ({b, alias:b, view:new Uint32Array(b, 4), data:new DataView(b, 4), fixed:new Uint32Array(b, 4, 1)})").unwrap();
        let limits = CloneLimits { bytes: 4096, depth: 16, nodes: 64, names: ["bytes", "depth", "nodes"] };
        let opts = CloneOpts { limits, transfer: &[] };
        assert!(matches!(s.engine().clone_out(value, &opts, &mut Host(None)), Err(Error::Throw(_))));
        let buffer = s.get_str(value, "b").unwrap();
        let transfer = CloneOpts { limits, transfer: &[buffer] };
        let mut host = Host(Some(cluster.clone()));
        let probe = s.run_script("var touched = false; ({get item() { touched = true; return b; }})").unwrap();
        assert!(matches!(s.engine().clone_out(probe, &transfer, &mut host), Err(Error::Throw(_))));
        let touched = s.run_script("touched").unwrap();
        assert!(!s.to_boolean(touched));
        let good = s.engine().clone_out(value, &opts, &mut host).unwrap();
        let foreign = s.engine().clone_out(value, &opts, &mut host).unwrap();
        (good, foreign)
    }).unwrap();

    std::thread::Builder::new().stack_size(8 << 20).spawn(move || {
        let mut dst = crate::script::load("").unwrap().realm;
        dst.install_test262_shared_memory(false);
        task(&mut dst, |s| {
            assert!(matches!(s.engine().clone_in(&mut foreign, &mut Host(Some(AgentCluster::default()))), Err(Error::Throw(_))));
            let cloned = s.engine().clone_in(&mut good, &mut Host(Some(cluster))).unwrap().value;
            let global = s.global();
            s.set_str(global, "clone", cloned).unwrap();
            let ok = s.run_script(r#"
                clone.b.grow(16);
                clone.view[2] = 42;
                clone.b === clone.alias && clone.view.buffer === clone.b &&
                clone.data.buffer === clone.b && clone.view.byteOffset === 4 &&
                clone.view.length === 3 && clone.data.byteLength === 12 && clone.fixed.length === 1
            "#).unwrap();
            assert!(s.to_boolean(ok));
        }).unwrap();
    }).unwrap().join().unwrap();
    task(&mut src, |s| {
        let ok = s.run_script("b.byteLength === 16 && new Uint32Array(b)[3] === 42").unwrap();
        assert!(s.to_boolean(ok));
    }).unwrap();
}
