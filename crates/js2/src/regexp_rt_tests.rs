// `RegExp` in the realm (`regexp_rt.rs`): the literal, the constructor, `exec`, `test`, `lastIndex` and the getters, each run
// normally and under constant collection (`check` of `script_tests.rs`).
use super::tests::check;

#[test]
fn literals_and_test() {
    check(&[
        ("var r = /a+b/.test('xaab') + ',' + /a+b/.test('xb');", "r", "true,false"),
        ("var r = /^\\d{3}$/.test('123') + ',' + /^\\d{3}$/.test('1234');", "r", "true,false"),
        ("var r = /ab/i.test('xAB');", "r", "true"),
        ("var r = new RegExp('a.c', 's').test('a\\nc');", "r", "true"),
        ("var r = RegExp('b+').exec('abbc')[0];", "r", "bb"),
        ("var r = typeof RegExp;", "r", "function"),
        ("var r = /a/ instanceof RegExp;", "r", "true"),
        ("var r = /é+/.test('xéé') + ',' + /é/u.test('é');", "r", "true,true"),
    ]);
}

#[test]
fn exec_result() {
    check(&[
        ("var m = /(\\d+)-(\\d+)?/.exec('ab 12-'); var r = [m.length, m[0], m[1], m[2], m.index, m.input].join('|');", "r", "3|12-|12||3|ab 12-"),
        ("var r = /x/.exec('abc');", "r", "null"),
        ("var m = /b/.exec('abc'); var r = Array.isArray(m) + ',' + (m.groups === undefined);", "r", "true,true"),
        ("var m = /\\u{1F600}/u.exec('a\\u{1F600}'); var r = m.index + ',' + m[0].length;", "r", "1,2"),
    ]);
}

#[test]
fn last_index() {
    check(&[
        ("var g = /a/g, r = []; while (g.test('aXa')) r.push(g.lastIndex); r.push(g.lastIndex); r = r.join(',');", "r", "1,3,0"),
        ("var y = /a/y; y.lastIndex = 1; var r = y.test('ba') + ',' + y.lastIndex + ',' + y.test('ba') + ',' + y.lastIndex;", "r", "true,2,false,0"),
        ("var p = /a/; p.lastIndex = 5; var r = p.test('a') + ',' + p.lastIndex;", "r", "true,5"),
        ("var g = /a/g; g.lastIndex = 9; var r = g.exec('a') + ',' + g.lastIndex;", "r", "null,0"),
        ("var g = /a/g; Object.freeze(g); try { g.test('a'); var r = 'no'; } catch (e) { r = e.name; }", "r", "TypeError"),
    ]);
}

#[test]
fn getters_and_errors() {
    check(&[
        ("var x = /a\\/b/gi; var r = [x.source, x.flags, x.global, x.ignoreCase, x.sticky, String(x)].join('|');", "r", "a\\/b|gi|true|true|false|/a\\/b/gi"),
        ("var r = new RegExp('').source + ',' + new RegExp('/').source;", "r", "(?:),\\/"),
        ("try { new RegExp('('); var r = 'no'; } catch (e) { r = e.name; }", "r", "SyntaxError"),
        ("try { new RegExp('a', 'gg'); var r = 'no'; } catch (e) { r = e.name; }", "r", "SyntaxError"),
        ("var a = /x/gi, b = new RegExp(a, 'm'); var r = b.flags + ',' + new RegExp(a).flags;", "r", "m,gi"),
        ("class R extends RegExp {} var q = new R('a'); var r = (q instanceof R) + ',' + q.test('a');", "r", "true,true"),
    ]);
}

#[test]
fn identity_and_names() {
    check(&[
        ("var a = /x/; var r = (RegExp(a) === a) + ',' + (RegExp(a, 'g') === a) + ',' + (new RegExp(a) === a);", "r", "true,false,false"),
        ("var r = Object.prototype.toString.call(/a/);", "r", "[object RegExp]"),
        ("var r = Object.getOwnPropertyDescriptor(RegExp.prototype, 'global').get.name + ',' + Object.getOwnPropertyDescriptor(RegExp.prototype, 'source').get.name;", "r", "get global,get source"),
        ("var m = /./u.exec('\\ud834\\udf06'); var r = m[0].length + ',' + m[0].charCodeAt(0); ", "r", "2,55348"),
    ]);
}

#[test]
fn fuel_stops_the_script() {
    let mut vm = crate::script::load("var r = /(a+)+b/.test('aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa');").expect("load");
    vm.realm.fuel = 10_000;
    assert!(matches!(vm.run_main(0), Err(crate::interp::Exit::Fuel)));
}
