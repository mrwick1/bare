//! Tests of the `responseText` charset of `XMLHttpRequest` (`browser_xhr.rs`).

use crate::browser_xhr_tests::{browser, js, server, settle};

/// A page reads binary data as text with `charset=x-user-defined` (WhatsApp Web does). The Encoding Standard maps a byte below
/// 0x80 to itself and a byte from 0x80 to U+F780 + (byte - 0x80). Chrome gives the same codes.
#[test]
fn x_user_defined_gives_one_code_per_byte() {
    std::thread::Builder::new().stack_size(64 << 20).spawn(|| {
        let srv = server();
        let mut b = browser(srv.port);
        js(&mut b, "var out = '', finished = false; var x = new XMLHttpRequest(); x.open('GET', '/bin'); x.overrideMimeType('text/plain; charset=x-user-defined'); x.onloadend = () => { var t = x.responseText; out = [...t].map(c => c.charCodeAt(0)).join(',') + '|' + [...t].map(c => c.charCodeAt(0) & 0xff).join(','); finished = true; }; x.send();");
        settle(&mut b);
        assert_eq!(js(&mut b, "out"), "0,1,2,63485,63486,63487|0,1,2,253,254,255");
    }).unwrap().join().unwrap();
}
