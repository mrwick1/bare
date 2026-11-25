use super::*;

#[test]
fn bootstrap_allocates_distinct_intrinsics_on_the_same_heap() {
    let mut rt = Realm::new(Arc::new(SharedAtoms::default()));
    let keys = RealmBootstrapKeys {
        k_code: rt.intern("\0code"),
        k_ctx: rt.intern("\0ctx"),
        k_home: rt.intern("\0home"),
        k_fields: rt.intern("\0fields"),
        k_link: rt.intern("\0link"),
        k_prim: rt.k_prim,
        k_then: rt.intern("then"),
        k_pstate: rt.intern("\0pstate"),
        k_presult: rt.intern("\0presult"),
        hidden: ["\0native", "\0n1", "\0n2", "\0n3"].map(|name| rt.intern(name)),
        k_value: rt.intern("value"),
        k_done: rt.intern("done"),
        gkeys: ["\0gstate", "\0gframe", "\0gsent", "\0gmode", "\0gpromise", "\0gentry"].map(|name| rt.intern(name)),
        empty: rt.intern(""),
    };
    let watch = rt.shapes.watch_tag();
    let RealmBootstrap { state, consts, pins, .. } = RealmState::allocate(&mut rt.heap, &mut rt.shapes, keys);
    assert_eq!(rt.shapes.watch_tag(), watch);
    assert_ne!(rt.global, state.global);
    assert_ne!(rt.object_proto, state.object_proto);
    assert_ne!(rt.function_proto, state.function_proto);
    assert_ne!(rt.array_proto, state.array_proto);

    rt.consts.extend(consts);
    rt.pins.extend(pins);
    rt.collect(&mut [], |_| {});
    assert_eq!(rt.proto_of(state.global), Some(state.object_proto));
    assert_eq!(rt.proto_of(state.array_proto), Some(state.object_proto));
    assert_eq!(rt.proto_of(rt.global), Some(rt.object_proto));
}
