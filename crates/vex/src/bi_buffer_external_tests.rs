use crate::interp_rt::Key;
use crate::script;
use std::ptr::NonNull;

/// Run `src` with the global `ext`: an external buffer over `bytes`.
fn run(bytes: &mut [u8], src: &str) -> crate::interp::Vm {
    let mut vm = script::load(src).unwrap();
    // SAFETY: `bytes` outlives the script run, and the tests detach the buffer before they drop the bytes.
    let b = unsafe { vm.realm.new_external_buffer(NonNull::new(bytes.as_mut_ptr()).unwrap(), bytes.len()) }.unwrap();
    let (g, k) = (vm.realm.global, vm.realm.intern("ext"));
    vm.realm.define_attrs(g, Key::Name(k), 0, b.value()).unwrap();
    vm.run_main(0).map_err(|_| "the script threw").unwrap();
    vm
}

#[test]
fn views_read_and_write_the_host_bytes() {
    let mut bytes = vec![1u8, 2, 3, 4, 0, 0, 0, 0];
    let mut vm = run(&mut bytes, r#"
        var u = new Uint8Array(ext);
        u[4] = 9;
        new DataView(ext).setUint16(6, 0x0102, true);
        var result = [ext.byteLength, u[0] + u[3], ext.resizable, ext.detached, ext.maxByteLength, ext.slice(1, 3).byteLength,
            new Uint8Array(ext.slice(0, 2))[1], new TextDecoder().decode(new Uint8Array(ext, 0, 1)).charCodeAt(0)].join();
    "#);
    assert_eq!(vm.global_text("result"), "8,5,false,false,8,2,2,1");
    assert_eq!(bytes, [1, 2, 3, 4, 9, 0, 2, 1]);
}

#[test]
fn an_external_buffer_cannot_be_transferred() {
    let mut bytes = vec![0u8; 4];
    let mut vm = run(&mut bytes, r#"
        var a = 0, b = 0;
        try { ext.transfer(); } catch (e) { a = e instanceof TypeError; }
        try { ext.transferToFixedLength(); } catch (e) { b = e instanceof TypeError; }
        var result = [a, b, ext.byteLength, ext.detached].join();
    "#);
    assert_eq!(vm.global_text("result"), "true,true,4,false");
}

#[test]
fn a_detached_external_buffer_has_no_bytes() {
    let mut bytes = vec![7u8; 4];
    let mut vm = run(&mut bytes, "var u = new Uint8Array(ext); function check() { return [ext.byteLength, ext.detached, u.length, u[0]].join(); }");
    let b = vm.global("ext");
    vm.realm.detach_buffer(crate::interp_rt::Realm::obj(b).unwrap());
    let check = vm.global("check");
    let v = vm.call(check, crate::value::Value::UNDEFINED, &[]).map_err(|_| "check threw").unwrap();
    assert_eq!(vm.display(v, false), "0,true,0,");
}
