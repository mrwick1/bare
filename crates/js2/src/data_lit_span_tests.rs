use crate::facts::Facts;
use crate::parse::{Limits, parse_script_lazy};

fn bulk_source(src: &str) -> String {
    " ".repeat(crate::parse::data_scan::BULK_SOURCE_BYTES) + src
}

fn padding() -> String {
    vec!["0"; 550].join(",")
}

#[test]
fn a_large_constant_literal_uses_one_syntax_node() {
    let src = format!(r#"var a = {{"items":[{}], "child":{{"s":"\ud800","n":-0,"f":-123.4e+2}},"key":1,"key":2}};"#, padding());
    let src = bulk_source(&src);
    let program = parse_script_lazy(&src, Limits::default(), &mut Facts::new()).unwrap();
    assert_eq!(program.stats().exprs, 1, "a checked span must replace the literal tree");

    let src = format!("{src} var r = a.items.length === 550 && a.child.s.charCodeAt(0) === 55296 && 1 / a.child.n === -Infinity && a.child.f === -12340 && a.key === 2 && Object.keys(a).join(',') === 'items,child,key';");
    let mut vm = crate::script::run(&src).unwrap();
    assert_eq!(vm.global_text("r"), "true");
}

#[test]
fn large_literals_keep_limits_and_cover_grammar() {
    let parse = |s: &str, limits| parse_script_lazy(&bulk_source(s), limits, &mut Facts::new()).map(|_| ());
    let data = format!("[0,{}]", padding());
    for (limits, kind) in [
        (Limits { max_nodes: 64, ..Limits::default() }, crate::parse::LimitKind::Nodes),
        (Limits { max_list: 64, ..Limits::default() }, crate::parse::LimitKind::List),
        (Limits { max_depth: 5, ..Limits::default() }, crate::parse::LimitKind::Depth),
    ] {
        assert_eq!(parse(&format!("var a = [{data}];"), limits).unwrap_err().kind, crate::parse::ErrorKind::Limit(kind));
    }
    assert!(parse(&format!("{data} = [];"), Limits::default()).is_err());
    assert!(parse(&format!("({data}) => 0;"), Limits::default()).is_err());
    let empty = vec!["[]"; 550].join(",");
    assert!(parse(&format!("[{empty}] = [];"), Limits::default()).is_ok());
}

#[test]
fn large_non_json_literals_keep_ordinary_js_semantics() {
    let src = format!(
        r#"var a = {{"padding":[{}], "\u005f_proto__":{{"p":7}}}};
        var b = [{}, , 0x10, 1_000, 'x'];
        var r = a.p === 7 && !Object.hasOwn(a, "__proto__") &&
            !(550 in b) && b[551] === 16 && b[552] === 1000 && b[553] === "x";"#,
        padding(), padding()
    );
    let mut vm = crate::script::run(&bulk_source(&src)).unwrap();
    assert_eq!(vm.global_text("r"), "true");
}
