#[test]
fn float16_views_clone_and_are_rejected_by_random_values() {
    super::run(&[
        (r#"
            var buffer = new ArrayBuffer(8);
            var a = new Float16Array(buffer, 2, 2);
            a.set([-0, 1.5]);
            var copy = structuredClone({a, buffer});
            [copy.a instanceof Float16Array, copy.a.buffer === copy.buffer,
             copy.a.byteOffset, 1 / copy.a[0], copy.a[1]].join()
        "#, "true,true,2,-Infinity,1.5"),
        (r#"
            try { crypto.getRandomValues(new Float16Array(1)); }
            catch (e) { e.name + ':' + e.code }
        "#, "TypeMismatchError:17"),
    ]);
}
