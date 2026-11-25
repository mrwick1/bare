#[test]
fn structured_clone_preserves_immutable_buffers_and_rejects_transfer() {
    super::run(&[(r#"
        var a = new Uint8Array([5, 6]);
        var buffer = a.buffer.transferToImmutable();
        var b = structuredClone({buffer, view: new Uint8Array(buffer)});
        var error;
        try { structuredClone(buffer, {transfer: [buffer]}); } catch (e) { error = e.name; }
        [b.buffer.immutable, b.view.buffer === b.buffer, b.view.join(),
         Reflect.set(b.view, 0, 7), buffer.detached, error].join('/')
    "#, "true/true/5,6/false/false/DataCloneError")]);
}
