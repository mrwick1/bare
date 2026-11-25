use crate::script;

#[test]
fn immutable_buffer_rejects_mutation_and_reports_read_only_indices() {
    let mut vm = script::run(r#"
        var src = new Uint8Array([4, 5]);
        var buffer = src.buffer.transferToImmutable(3);
        var a = new Uint8Array(buffer);
        var d = Object.getOwnPropertyDescriptor(a, '0');
        var child = Object.create(a);
        var touched = false, put = Reflect.set(a, '0', {valueOf() { touched = true; return 7; }});
        var inherited = Reflect.set(child, '0', 7);
        var same = Reflect.defineProperty(a, '0', {value: 4});
        var different = Reflect.defineProperty(a, '0', {value: '4'});
        var frozen = Object.isFrozen(Object.freeze(a));
        var result = [src.buffer.detached, buffer.immutable, buffer.resizable, a.join('/'),
                      d.writable, d.configurable, d.enumerable,
                      put, inherited, touched, same, different, frozen].join();
    "#).unwrap();
    assert_eq!(vm.global_text("result"), "true,true,false,4/5/0,false,false,true,false,false,false,true,false,true");
}
