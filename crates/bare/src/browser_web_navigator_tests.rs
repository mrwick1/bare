//! Tests of the `Navigator` interface object (`browser_web.rs`). WhatsApp reads `"setAppBadge" in Navigator.prototype`, and
//! a missing global threw a `ReferenceError`.

#[test]
fn navigator_is_an_interface_with_an_illegal_constructor() {
    let got = crate::browser_wasm_tests::go("var e = ''; try { new Navigator(); } catch (x) { e = x.name; }
        out.push(typeof Navigator, Object.getPrototypeOf(navigator) === Navigator.prototype, navigator instanceof Navigator, 'userAgent' in Navigator.prototype, e,
            Object.getOwnPropertyDescriptor(globalThis, 'Navigator').enumerable);");
    assert_eq!(got, "function|true|true|true|TypeError|false");
}
