use bare_js_api::task;

fn check(src: &str, want: &str) {
    let mut r = crate::script::load("").unwrap().realm;
    task(&mut r, |s| {
        match s.run_script(src) {
            Ok(v) => assert_eq!(s.as_str(v).unwrap().to_string_lossy(), want),
            Err(bare_js_api::Error::Throw(e)) => {
                let v = s.engine().a_at(e);
                panic!("{}", s.engine().error_text(v));
            }
            Err(e) => panic!("{e:?}"),
        }
    }).unwrap();
}

#[test]
fn encodes_scalars_and_counts_utf16_units() {
    check(r#"
        let e = new TextEncoder(), dest = new Uint8Array(new ArrayBuffer(8), 1, 5);
        let r = e.encodeInto("A😀B", dest);
        let foo = e.encode("\ud800€"), u8 = e.encode();
        [e.encoding, foo instanceof Uint8Array, [...foo].join(","), u8.length,
         [...dest].join(","), r.read, r.written,
         e.encodeInto("😀", new Uint8Array(3)).written].join("|")
    "#, "utf-8|true|239,191,189,226,130,172|0|65,240,159,152,128|3|5|0");
}

#[test]
fn decoding_keeps_a_split_sequence_and_consumes_the_bom_once() {
    check(r#"
        let d = new TextDecoder(" UTF8 "), u8 = (...b) => new Uint8Array(b);
        let stream = {stream:true}, bom = [239,187,191], input = u8(4, 65, 6);
        let out = [d.encoding, d.fatal, d.ignoreBOM, d.decode(u8(239), stream),
            d.decode(u8(187,191,240,159), stream), d.decode(u8(152,128,...bom)),
            d.decode(new DataView(input.buffer, 1, 1)),
            new TextDecoder(undefined, {ignoreBOM:true}).decode(u8(...bom,65)).charCodeAt(0),
            d.decode(u8(226,65)), d.decode(u8(226), stream), d.decode()];
        out.join("|")
    "#, "utf-8|false|false|||😀\u{feff}|A|65279|�A||�");
}

#[test]
fn invalid_input_respects_fatal_and_the_receiver_brand() {
    check(r#"
        function error(f) { try { f(); return "-"; } catch(e) { return e.name; } }
        let d = new TextDecoder("utf-8", {fatal:true}), bytes = new Uint8Array([0xe2]);
        d.decode(bytes, {stream:true});
        [error(() => d.decode()), d.decode(new Uint8Array([65])),
         error(() => new TextDecoder("invalid")), error(() => d.decode("text")),
         error(() => TextEncoder.prototype.encode.call({})),
         error(() => new TextEncoder().encodeInto("a", new Uint8ClampedArray(3)))].join()
    "#, "TypeError,A,RangeError,TypeError,TypeError,TypeError");
}
