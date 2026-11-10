use super::*;

#[test]
fn sparse_objects_bound_element_capacity() {
    let mut h = Heap::default();
    for _ in 0..1000 {
        let o = h.new_object(None).unwrap();
        for n in 0..30 {
            h.set(o, PropertyKey::Index(n * 950 / 29), Value::Number(n as f64)).unwrap();
        }
    }
    let bytes = h.footprint().elems_cap;
    assert!(bytes <= 128_000, "element capacity {bytes} exceeds 128000 bytes");
}

#[test]
fn sparse_values_survive_collect() {
    let mut h = Heap::default();
    let o = h.new_object(None).unwrap();
    let v = h.new_object(None).unwrap();
    h.set(o, PropertyKey::Index(950), Value::Object(v)).unwrap();
    h.collect(&[Value::Object(o)]).unwrap();
    assert!(matches!(h.get(o, PropertyKey::Index(950)), Ok(Get::Value(Value::Object(x))) if x == v));
    assert!(h.get(v, PropertyKey::Index(0)).is_ok());
}

#[test]
fn sparse_script_semantics() {
    std::thread::Builder::new().stack_size(64 << 20).spawn(|| {
        let src = r#"
        function check(x) { if (!x) throw new Error('sparse check'); }
        for (var named of [false, true]) for (var far of [16, 17, 32, 950, 2000]) {
            var proto = {}; proto[far - 1] = 7;
            var o = Object.create(proto); if (named) o.name = 1; o[far] = 9; o[2] = 3; o[0] = 1;
            var want = '0,2,' + far + (named ? ',name' : '');
            check(Object.keys(o).join() === want);
            check(Reflect.ownKeys(o).join() === want);
            var keys = []; for (var k in o) keys.push(k);
            check(keys.join() === want + ',' + (far - 1));
            check(o[far - 1] === 7);
            delete o[far]; check(!(far in o)); o[far] = 9;
            Object.defineProperty(o, far, {value: 11, writable: false});
            check(o[far] === 11);
            Object.defineProperty(o, far - 2, {get: function() {return 13;}, configurable: true});
            check(o[far - 2] === 13);
            Object.seal(o); check(Object.isSealed(o));
            Object.freeze(o); check(Object.isFrozen(o));
            var a = []; a[far] = 3; a[2] = 1;
            check(a.length === far + 1); a.push(2); check(a.length === far + 2);
            check(a.indexOf(3) === far);
            a.splice(1, 1); check(a.indexOf(3) === far - 1);
            a.sort(); check(a[0] === 1 && a[1] === 2 && a[2] === 3);
            check(JSON.stringify({0: 1, 950: 2}) === '{"0":1,"950":2}');
        }
        true
        "#;
        let p = crate::parse::parse_script(src, crate::parse::Limits::default()).unwrap();
        let mut it = crate::eval::Interp::new(&p, Heap::default(), crate::eval::EvalLimits::default()).unwrap();
        assert!(matches!(it.run(), Ok(Value::Bool(true))));
    }).unwrap().join().unwrap();
}
