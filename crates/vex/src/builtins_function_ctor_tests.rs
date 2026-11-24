use crate::script;

fn run(source: &str) -> String {
    let mut vm = script::load(source).unwrap();
    vm.run_main(0).unwrap();
    vm.global_text("result")
}

#[test]
fn a_concatenated_body_compiles_as_its_literal_text_does() {
    let source = r#"
        function t(c) { try { return typeof new Function('a', 'b', c); } catch (e) { return e.message; } }
        var lit = "return {'q':function(){return y }}";
        var cat = "return {'q':function(){return " + "y" + " }}";
        var result = [t(lit), t(cat), cat === lit].join();
    "#;
    assert_eq!(run(source), "function,function,true");
}

#[test]
fn eval_and_a_nested_rope_give_their_whole_text() {
    let source = r#"
        var parts = ['ret', 'urn ', '[', '"é"', ',', "'x'", ']'];
        var s = '';
        for (var i = 0; i < parts.length; i++) s = (i % 2 ? s + parts[i] : parts[i] === '[' ? s + (parts[i] + '') : s + parts[i]);
        var deep = ('(' + 'function(){' + s) + ('}' + ')');
        var result = [eval(deep)().join('|'), new Function('return ' + '{a:' + 'function(){return 7}}')().a()].join();
    "#;
    assert_eq!(run(source), "é|x,7");
}
