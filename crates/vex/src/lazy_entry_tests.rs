// Tests of the compact pending entry (`docs/internal/design-code-memory.md`, C4): a waiting function keeps no copy of its name; the
// lazy compile takes the name from the stub's record.
use super::*;

#[test]
fn a_pending_entry_is_24_bytes() {
    assert!(size_of::<Wait>() <= 24, "{} bytes", size_of::<Wait>());
}

#[test]
fn a_lazy_compile_gets_the_name_of_its_stub() {
    let src = "var f = function ünï(n) { return n ? ünï(n - 1) : ünï.name; };\n\
               var g = function () { return g.name; };\n\
               var o = { 日本(x) { return x; } };\n\
               var h = (0, function () {});\n\
               var r = [f(2), g(), o.日本.name, o.日本(5), h.name === '', f.name].join('|');";
    let mut vm = run(src).unwrap();
    assert_eq!(vm.global_text("r"), "ünï|g|日本|5|true|ünï");
}
