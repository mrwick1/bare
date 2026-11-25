// TMP probe: never merge.
#[test]
fn tmp_split_elision() {
    let mut vm = crate::script::run(r#"var e="aa.bbb.cc"; let[,t]=e.split("."); var x=(function(e){let[,t]=e.split(".");return t})(e); var r=[t,x,e.split(".").length, typeof TextDecoder].join("|");"#).unwrap();
    eprintln!("R={}", vm.global_text("r"));
}
