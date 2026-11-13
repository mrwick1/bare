//! JavaScript strings of all kinds: short atoms, long flat strings and ropes. Every case is checked against the spec by hand (they hold in Node).

use super::*;
use crate::heap::Heap;
use crate::parse::{parse_script, Limits};

fn run(src: &str) -> String {
    let src = src.to_string();
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || {
            let prog = parse_script(&src, Limits::default()).unwrap();
            let mut it = Interp::new(&prog, Heap::default(), EvalLimits { max_steps: u64::MAX / 2, ..Default::default() }).expect("setup");
            match it.run() {
                Ok(v) => it.display(v),
                Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw {}: {}", it.get_str(v, "name"), it.get_str(v, "message")),
                Err(e) => format!("error: {e:?}"),
            }
        })
        .unwrap()
        .join()
        .unwrap()
}

#[test]
fn long_strings_are_equal_by_content() {
    let src = r#"
        var a = 'x'.repeat(100) + 'y', b = 'x'.repeat(100) + 'y', c = 'x'.repeat(100) + 'z';
        var r = [];
        r.push(a === b, a !== b, a == b, a === c, a < c, a > c);
        r.push([a].includes(b), [a].indexOf(b), [c, a].lastIndexOf(b), Object.is(a, b));
        switch (b) { case c: r.push('c'); break; case a: r.push('a'); break; default: r.push('d'); }
        var s = new Set([a]); r.push(s.has(b), s.has(c), s.size, s.add(b).size);
        var m = new Map([[a, 1]]); r.push(m.get(b), m.has(c), m.set(b, 2).size, m.get(a), [...m.keys()][0] === b);
        var o = {}; o[a] = 1; r.push(o[b], b in o, Object.keys(o)[0] === a, o.hasOwnProperty(b), delete o[b], a in o);
        r.push(Symbol.for(a) === Symbol.for(b), Symbol.keyFor(Symbol.for(a)) === b, Symbol.for(a) === Symbol.for(c));
        r.join()
    "#;
    assert_eq!(run(src), "true,false,true,false,true,false,true,0,1,true,a,true,false,1,1,1,false,1,2,true,1,true,true,true,true,false,true,true,false");
}

#[test]
fn a_concat_loop_builds_the_same_string() {
    let src = r#"
        var s = '', t = [];
        for (var i = 0; i < 3000; i++) { s += 'ab' + (i % 10); t.push('ab' + (i % 10)); }
        var j = t.join('');
        var r = [s.length, s === j, s == j, s.charCodeAt(0), s.charCodeAt(s.length - 1), s.indexOf('ab9'), s.slice(7, 12), s.split('ab9').length];
        r.push(/^(ab\d)+$/.test(s), JSON.stringify(s).length, s.substring(5, 9), s.at(-1), s[3], s.toUpperCase().length);
        r.push((s + s).length, (s + 'tail').endsWith('ltail') || (s + 'tail').endsWith('9tail'), s < s + 'a', s + 'a' > s, [s].includes(j));
        var u = ''; for (var k = 0; k < 500; k++) u = 'cd' + u + 'x';
        r.push(u.length, u.slice(0, 4), u.slice(-2), u.indexOf('x'));
        r.join()
    "#;
    assert_eq!(run(src), "9000,true,true,97,57,27,b2ab3,301,true,9002,1ab2,9,a,9000,18000,true,true,true,true,1500,cdcd,xx,1000");
}

#[test]
fn a_million_parts_in_a_loop() {
    let src = "var s = ''; for (var i = 0; i < 1000000; i++) s += 'ab'; var p = ''; for (var i = 0; i < 300000; i++) p = 'cd' + p; [s.length, s.charCodeAt(1999999), p.length, p.charCodeAt(0), s.length + p.length].join()";
    assert_eq!(run(src), "2000000,98,600000,99,2600000");
}

#[test]
fn ropes_work_as_keys_arguments_and_results() {
    let src = r#"
        var a = 'k'.repeat(40), s = '';
        for (var i = 0; i < 10; i++) s += a;
        var o = {}; o[s] = 5; o[s + 'x'] = 6;
        var r = [o[a.repeat(10)], o[s + 'x'], Object.keys(o).length, Object.keys(o)[0].length];
        r.push(parseInt('1' + '0'.repeat(30)) === 1e30, Number('1' + '0'.repeat(30)) === 1e30, ('7' + '0'.repeat(30)).length);
        r.push(new RegExp(s.slice(0, 5) + '+').exec(s)[0].length, s.replace(/k+/, 'Z'), String(s).length, s.concat(s).length, [s, s].join('-').length);
        r.push(typeof s, s.localeCompare(s), encodeURIComponent(s).length);
        r.push(JSON.parse('"' + s + '"') === s, JSON.stringify({ [s]: s }).length);
        r.join()
    "#;
    let want = "5,6,2,400,true,true,31,400,Z,400,800,801,string,0,400,true,807";
    assert_eq!(run(src), want);
}

#[test]
fn long_names_are_keys() {
    let src = r#"
        var aVeryLongGlobalVariableNameOfMoreThanThirtyTwoCharacters = 1;
        aVeryLongGlobalVariableNameOfMoreThanThirtyTwoCharacters += 4;
        function aVeryLongFunctionNameOfMoreThanThirtyTwoCharacters() { return 'f'; }
        var o = { aVeryLongPropertyNameOfMoreThanThirtyTwoCharacters: 10, 'a quoted property name of more than thirty-two characters': 20, [('c' + 'd').repeat(30)]: 30 };
        o.anotherVeryLongPropertyNameOfMoreThanThirtyTwoCharacters = 40;
        class K { aVeryLongMethodNameOfMoreThanThirtyTwoCharacters() { return 'm'; } static aVeryLongStaticNameOfMoreThanThirtyTwoCharacters = 7; get aVeryLongGetterNameOfMoreThanThirtyTwoCharacters() { return 'g'; } }
        var { aVeryLongPropertyNameOfMoreThanThirtyTwoCharacters: d1, anotherVeryLongPropertyNameOfMoreThanThirtyTwoCharacters: d2 } = o;
        var j = JSON.parse('{"aVeryLongJsonKeyOfMoreThanThirtyTwoCharactersInTotal": 5}');
        var r = [aVeryLongGlobalVariableNameOfMoreThanThirtyTwoCharacters, aVeryLongFunctionNameOfMoreThanThirtyTwoCharacters(), globalThis.aVeryLongGlobalVariableNameOfMoreThanThirtyTwoCharacters];
        r.push(o.aVeryLongPropertyNameOfMoreThanThirtyTwoCharacters, o['a quoted property name of more than thirty-two characters'], o['cd'.repeat(30)], o.anotherVeryLongPropertyNameOfMoreThanThirtyTwoCharacters);
        r.push(new K().aVeryLongMethodNameOfMoreThanThirtyTwoCharacters(), K.aVeryLongStaticNameOfMoreThanThirtyTwoCharacters, new K().aVeryLongGetterNameOfMoreThanThirtyTwoCharacters, d1, d2);
        r.push(j.aVeryLongJsonKeyOfMoreThanThirtyTwoCharactersInTotal, Object.keys(j)[0].length, Object.keys(o).join().length, JSON.stringify(o).length);
        r.push('aVeryLongPropertyNameOfMoreThanThirtyTwoCharacters' in o, delete o.aVeryLongPropertyNameOfMoreThanThirtyTwoCharacters, o.aVeryLongPropertyNameOfMoreThanThirtyTwoCharacters);
        r.push(K.name, new K().aVeryLongMethodNameOfMoreThanThirtyTwoCharacters.name, Object.getOwnPropertyNames(K.prototype).join());
        r.join()
    "#;
    assert_eq!(run(src), "5,f,5,10,20,30,40,m,7,g,10,40,5,52,226,248,true,true,,K,aVeryLongMethodNameOfMoreThanThirtyTwoCharacters,constructor,aVeryLongMethodNameOfMoreThanThirtyTwoCharacters,aVeryLongGetterNameOfMoreThanThirtyTwoCharacters");
}
