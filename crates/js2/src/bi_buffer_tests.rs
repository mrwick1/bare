use crate::script::{load, run};

fn check(src: &str, expected: &str) {
    let mut vm = run(src).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(vm.global_text("r"), expected);
}

#[test]
fn storage_aliases_and_byte_order() {
    check(
        r#"
        var b = new ArrayBuffer(8);
        var a = new Uint8Array(b);
        var d = new DataView(b, 2, 4);
        d.setUint16(0, 0x1234);
        var copy = b.slice(2, 4);
        a[2] = 0xab;
        var r = [a.join(), d.getUint16(0, true), new Uint8Array(copy).join(),
                 ArrayBuffer.isView(a), ArrayBuffer.isView(d), ArrayBuffer.isView(b)].join('|');
    "#,
        "0,0,171,52,0,0,0,0|13483|18,52|true|true|false",
    );
}

#[test]
fn typed_methods_and_integer_indexed_properties() {
    check(
        r#"
        var a = new Int16Array([4, 1, 2, 3]);
        var saved = a.slice(1);
        a.set(a.subarray(0, 3), 1);
        a.fill(-1, 0, 1);
        a.sort();
        var calls = [];
        a.forEach((v, i, array) => calls.push(v + i + (array === a)));
        var entries = [];
        for (var e of a.entries()) entries.push(e.join(':'));
        var desc = Object.getOwnPropertyDescriptor(a, '0');
        var r = [a.join(), saved.join(), a.map(x => x * 2).join(),
                 a.reduce((x,y) => x+y, 0), a.at(-1), a.indexOf(1), a.includes(4),
                 calls.join(), entries.join(), Object.keys(a).join(),
                 desc.writable, desc.configurable, Reflect.deleteProperty(a, '0'),
                 Reflect.defineProperty(a, '99', {value: 1}), '-0' in a].join('|');
    "#,
        "-1,1,2,4|1,2,3|-2,2,4,8|6|4|1|true|0,3,5,8|0:-1,1:1,2:2,3:4|0,1,2,3|true|true|false|false|false",
    );
}

#[test]
fn views_keep_storage_live_across_collection() {
    let mut vm = run(r#"
        var a = new Uint8ClampedArray(new ArrayBuffer(4));
        a.set([1.5, 2.5, 256, -1]);
        var view = a.subarray(1, 3);
        a = null;
    "#)
    .unwrap();
    vm.realm.collect(&mut [], |_| {});
    bare_js_api::enter(&mut vm.realm, |s| {
        let v = s
            .run_script("[view.join(), new DataView(view.buffer).getUint8(2)].join('|')")
            .unwrap();
        assert_eq!(s.as_str(v).unwrap().to_string_lossy(), "2,255|255");
    });
}

#[test]
fn resize_rechecks_bounds_after_script_and_transfer_detaches_views() {
    check(
        r#"
        var b = new ArrayBuffer(6, {maxByteLength: 9});
        var a = new Uint8Array(b);
        var fixed = new Uint8Array(b, 2, 4);
        var d = new DataView(b, 2, 4);
        a.set([1, 2, 3, 4, 5, 6]);
        b.resize(3);
        var short = [a.join(), fixed.length, d.byteLength,
                     Reflect.preventExtensions(a)].join('/');
        b.resize(7);
        var grown = [a.join(), fixed.join(), d.byteOffset].join('/');
        var failure;
        try {
            d.setUint16(0, {valueOf() { b.transfer(); return 17; }});
        } catch (e) {
            failure = e.name;
        }
        var r = [short, grown, b.detached, a.length, d.byteLength, failure].join('|');
    "#,
        "1,2,3/0/0/false|1,2,3,0,0,0,0/3,0,0,0/2|true|0|0|TypeError",
    );
}

#[test]
fn typed_array_species_and_shared_iterator_prototype() {
    check(
        r#"
        var a = new Uint16Array([10, 20, 30, 40, 50, 60]);
        a.constructor = {[Symbol.species]: function() { return new Uint16Array(a.buffer, 4); }};
        var b = a.slice(1, 4);
        var iter = a.values();
        var next = Object.getPrototypeOf([].values()).next;
        var r = [b.join(), Object.getPrototypeOf(iter) === Object.getPrototypeOf([].values()),
                 next.call(iter).value, iter.next().value].join('|');
    "#,
        "20,20,20,60|true|10|20",
    );
}

#[test]
fn host_borrows_the_view_window_and_observes_detach() {
    use bare_js_api::{Elem, enter};

    let mut vm = load("").unwrap();
    enter(&mut vm.realm, |s| {
        let buf = s.new_array_buffer(vec![1, 2, 3, 4]).unwrap();
        let g = s.global();
        s.set_str(g, "hostBuffer", buf).unwrap();
        let a = s
            .run_script("var hostView = new Uint8Array(hostBuffer, 1, 2); hostView")
            .unwrap();
        let v = s.view(a).unwrap();
        assert_eq!((v.elem, v.offset, v.len), (Elem::U8, 1, 2));
        assert_eq!(s.bytes(a), Some(&[2, 3][..]));
        s.bytes_mut(a).unwrap()[0] = 9;
        assert_eq!(s.bytes(v.buffer), Some(&[1, 9, 3, 4][..]));
        assert_eq!(s.view_get(a, 0), Some(9.0));
        assert_eq!(s.view_get(a, 2), None);
        let d = s.run_script("new DataView(hostBuffer, 2, 1)").unwrap();
        assert_eq!(s.view(d).unwrap().elem, Elem::Data);
        assert_eq!(s.bytes(d), Some(&[3][..]));

        let host_f64 = s.new_typed_array(Elem::F64, 1.5f64.to_ne_bytes().to_vec()).unwrap();
        assert_eq!(s.view_get(host_f64, 0), Some(1.5));
        assert!(s.new_typed_array(Elem::I16, vec![0]).is_err());

        let target = s.run_script("hostBuffer.transfer()").unwrap();
        assert!(s.is_detached(buf) && s.is_detached(a) && s.is_detached(d));
        assert_eq!(s.bytes(a), Some(&[][..]));
        assert_eq!(s.view_get(a, 0), None);
        assert_eq!(s.bytes(target), Some(&[1, 9, 3, 4][..]));
    });
}
