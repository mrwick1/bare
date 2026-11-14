//! The `RegExp.prototype.test` result table (`Ext::re_tab`). The expected texts come from Node 22, one fresh context for each program.
//! Each program repeats the same (regex, input) pairs, so the first run fills the table and the later runs hit it.

use super::*;
use crate::heap::Heap;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn run(it: &mut Interp, src: &str) -> String {
    match it.run_script(src) {
        Ok(v) => it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", it.get_str(v, "name"), it.get_str(v, "message")),
        Err(e) => format!("error:{e:?}"),
    }
}

const CASES: &[(&str, &str, &str)] = &[
    ("repeat", r#"var re=/^(.+?)(ss|i)es$/, r=[]; for (var i=0;i<4;i++) r.push(re.test('caresses'), re.test('ponies'), re.test('cat')); r.join()"#, "true,true,false,true,true,false,true,true,false,true,true,false"),
    ("global_progress", r#"var g=/a/g, r=[]; for (var i=0;i<8;i++) r.push(g.test('aaa')+':'+g.lastIndex); r.join(' ')"#, "true:1 true:2 true:3 false:0 true:1 true:2 true:3 false:0"),
    ("global_set_last_index", r#"var g=/b/g, r=[]; for (var i=0;i<3;i++){ g.lastIndex=1; r.push(g.test('abab')+':'+g.lastIndex); g.lastIndex=9; r.push(g.test('abab')+':'+g.lastIndex); g.lastIndex=0; r.push(g.test('abab')+':'+g.lastIndex);} r.join(' ')"#, "true:2 false:0 true:2 true:2 false:0 true:2 true:2 false:0 true:2"),
    ("sticky", r#"var y=/a/y, r=[]; for (var i=0;i<3;i++){ y.lastIndex=0; r.push(y.test('ba')+':'+y.lastIndex); y.lastIndex=1; r.push(y.test('ba')+':'+y.lastIndex); r.push(y.test('ba')+':'+y.lastIndex);} r.join(' ')"#, "false:0 true:2 false:0 false:0 true:2 false:0 false:0 true:2 false:0"),
    ("same_content_other_object", r#"var s1='x'.repeat(40)+'y', s2='x'.repeat(30)+'x'.repeat(10)+'y', re=/x+y$/, r=[]; for (var i=0;i<3;i++) r.push(re.test(s1), re.test(s2), re.test(s1+'z'), re.test(s2.slice(1))); r.join()"#, "true,true,false,true,true,true,false,true,true,true,false,true"),
    ("short_atoms_and_concat", r#"var re=/^ab+c$/, r=[]; for (var i=0;i<3;i++){ var s='a'+'bb'+'c'; r.push(re.test(s), re.test('abbc'), re.test('abbd'), re.test(s+'')); } r.join()"#, "true,true,false,true,true,true,false,true,true,true,false,true"),
    ("user_exec_instance", r#"var re=/a/, r=[]; for(var i=0;i<3;i++) r.push(re.test('a')); re.exec=function(){ return null }; r.push(re.test('a')); re.exec=function(){ return [] }; r.push(re.test('b')); delete re.exec; r.push(re.test('a'), re.test('b')); r.join()"#, "true,true,true,false,true,true,false"),
    ("user_exec_prototype", r#"var re=/a/, r=[], ex=RegExp.prototype.exec; for(var i=0;i<3;i++) r.push(re.test('a')); RegExp.prototype.exec=function(s){ r.push('called'); return ex.call(this,'zzz') }; r.push(re.test('a')); RegExp.prototype.exec=ex; r.push(re.test('a')); r.join()"#, "true,true,true,called,false,true"),
    ("flags_differ", r#"var a=/a/, b=/a/i, c=/a/m, r=[]; for(var i=0;i<3;i++) r.push(a.test('A'), b.test('A'), c.test('A'), a.test('a'), b.test('a')); r.join()"#, "false,true,false,true,true,false,true,false,true,true,false,true,false,true,true"),
    ("new_regexp_same_source", r#"var r=[]; for(var i=0;i<3;i++){ var re=new RegExp('^b+$'), re2=new RegExp('^b+$','g'); r.push(re.test('bbb'), re2.test('bbb'), re2.lastIndex, re.test('bab')); } r.join()"#, "true,true,3,false,true,true,3,false,true,true,3,false"),
    ("last_index_valueof", r#"var g=/a/g, log=[], r=[]; for(var i=0;i<3;i++){ g.lastIndex={valueOf(){ log.push('v'); return 1 }}; r.push(g.test('aa'), g.lastIndex); } r.push(log.length); r.join()"#, "true,2,true,2,true,2,3"),
    ("captures_then_exec", r#"var re=/(a)(b)?/, r=[]; for(var i=0;i<3;i++){ r.push(re.test('ac')); var m=re.exec('ac'); r.push(m[0],m[1],m[2]===undefined); r.push(re.test('ab')); r.push(re.exec('ab')[2]) } r.join()"#, "true,a,a,true,true,b,true,a,a,true,true,b,true,a,a,true,true,b"),
    ("empty_match_global", r#"var g=/x*/g, r=[]; for(var i=0;i<6;i++) r.push(g.test('ab')+':'+g.lastIndex); r.join(' ')"#, "true:0 true:0 true:0 true:0 true:0 true:0"),
    ("unicode_and_case", r#"var u=/^\u{1F600}$/u, ci=/straße/i, r=[]; for(var i=0;i<3;i++) r.push(u.test('\u{1F600}'), u.test('\uD83D'), ci.test('STRASSE'), ci.test('STRAßE')); r.join()"#, "true,false,false,true,true,false,false,true,true,false,false,true"),
    ("match_replace_split_untouched", r#"var re=/a(b)/g, r=[]; for(var i=0;i<3;i++){ r.push(re.test('abab'), re.lastIndex); re.lastIndex=0; r.push('abab'.replace(re,'[$1]'), 'abab'.match(re).length, 'xaby'.split(re).join('|'), 'ab'.search(re)); } r.join(' ')"#, "true 2 [b][b] 2 x|b|y 0 true 2 [b][b] 2 x|b|y 0 true 2 [b][b] 2 x|b|y 0"),
    ("many_distinct_pairs", r#"var re=/^(.+?)(ss|i)es$/, n=0, r=[]; for(var round=0;round<3;round++) for(var i=0;i<3000;i++){ if(re.test('w'+i+'ies')) n++; } r.push(n); r.join()"#, "9000"),
    ("lookbehind_backreference", r#"var re=/(?<=a)(b)\1/, r=[]; for(var i=0;i<3;i++) r.push(re.test('abb'), re.test('bb'), re.test('abc')); r.join()"#, "true,false,false,true,false,false,true,false,false"),
    ("input_coerced", r#"var re=/1/, r=[]; for(var i=0;i<3;i++) r.push(re.test(1), re.test(null), re.test({toString(){return '1'}}), re.test(undefined)); r.join()"#, "true,false,true,false,true,false,true,false,true,false,true,false"),
    ("delete_and_gc_strings", r#"var re=/^k\d+$/, n=0; for(var i=0;i<5000;i++){ if(re.test('k'+(i%50))) n++; var junk=('j'+i).repeat(3); } n"#, "5000"),
];

#[test]
fn test_with_the_result_table_matches_node() {
    big(|| {
        let mut bad = Vec::new();
        for &(name, src, want) in CASES {
            let mut it = Interp::realm(Heap::default(), EvalLimits::default()).unwrap();
            let got = run(&mut it, src);
            if got != want {
                bad.push(format!("{name}: node {want:?}, bare {got:?}"));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    });
}

/// Steps of one script run.
fn steps_of(it: &mut Interp, src: &str) -> u64 {
    let before = it.steps;
    run(it, src);
    it.steps - before
}

#[test]
fn a_repeated_pair_is_not_matched_again_and_costs_the_same_steps() {
    big(|| {
        let mut it = Interp::realm(Heap::default(), EvalLimits::default()).unwrap();
        run(&mut it, "var re = /^(.+?)(ss|i)es$/, s = 'caresses'; function f() { return re.test(s); }");
        let miss = steps_of(&mut it, "f()");
        assert_eq!(it.ext.re_hits, 0);
        let hit = steps_of(&mut it, "f()");
        assert_eq!(it.ext.re_hits, 1, "the second call is a hit");
        assert_eq!(hit, miss, "a hit charges the steps the engine charged");
        assert!(miss > 20, "the engine did work: {miss}");
        // `exec`, `match` and `replace` do not use the table.
        let c = it.ext.re_hits;
        run(&mut it, "re.exec(s); re.exec(s); s.match(re); s.replace(re, 'x');");
        assert_eq!(it.ext.re_hits, c);
    });
}

#[test]
fn a_hit_that_does_not_fit_the_step_budget_runs_the_engine_and_fails_as_before() {
    big(|| {
        let mut it = Interp::realm(Heap::default(), EvalLimits::default()).unwrap();
        run(&mut it, "var re = /^(.+?)(ss|i)es$/, s = 'caresses'; function f() { return re.test(s); } f(); f();");
        assert_eq!(it.ext.re_hits, 1);
        // 12 steps cover the call itself (`g` shows it) but not the 30 or more steps that the engine charged.
        run(&mut it, "function g() { return 1; }");
        it.limits.max_steps = 12;
        assert_eq!(run(&mut it, "g()"), "1");
        assert!(matches!(it.run_script("f()"), Err(Error::Limit(LimitKind::Steps))));
        assert_eq!(it.ext.re_hits, 1, "no hit when the stored steps are over the budget");
    });
}

#[test]
fn a_loop_of_hits_still_ends_at_the_step_limit() {
    big(|| {
        let mut it = Interp::realm(Heap::default(), EvalLimits { max_steps: 300_000, ..Default::default() }).unwrap();
        assert!(matches!(it.run_script("var re = /^(.+?)(ss|i)es$/; while (true) re.test('caresses');"), Err(Error::Limit(LimitKind::Steps))));
        assert!(it.ext.re_hits > 100);
    });
}

#[test]
fn a_failed_match_is_not_stored_and_fails_again() {
    big(|| {
        let bl = BuiltinLimits { regexp_steps: 100_000, ..Default::default() };
        let mut it = Interp::realm(Heap::default(), EvalLimits::default()).unwrap();
        it.set_builtin_limits(bl);
        run(&mut it, "var S = 'a'; for (var i = 0; i < 6; i++) S += S; S += 'c'; function f() { return /(a+)+b/.test(S); }");
        for _ in 0..3 {
            assert!(matches!(it.run_script("f()"), Err(Error::Limit(LimitKind::Regexp(RegexpLimit::Steps)))));
        }
        assert_eq!(it.ext.re_hits, 0);
    });
}

#[test]
fn new_limits_empty_the_table() {
    big(|| {
        let mut it = Interp::realm(Heap::default(), EvalLimits::default()).unwrap();
        run(&mut it, "var re = /a+b/, s = 'xaab'; function f() { return re.test(s); } f(); f();");
        assert_eq!(it.ext.re_hits, 1);
        it.set_builtin_limits(BuiltinLimits { regexp_input_units: 3, ..Default::default() });
        assert!(matches!(it.run_script("f()"), Err(Error::Limit(LimitKind::Regexp(RegexpLimit::Input)))));
    });
}

#[test]
fn the_table_stays_within_its_memory_bound() {
    big(|| {
        let mut it = Interp::realm(Heap::default(), EvalLimits::default()).unwrap();
        assert_eq!(it.ext.re_tab_bytes(), 0, "nothing is allocated before the first test()");
        run(&mut it, "var re = /^w\\d+$/, n = 0; for (var i = 0; i < 200000; i++) if (re.test('w' + i)) n++; n");
        let b = it.ext.re_tab_bytes();
        assert!(b > 0 && b <= 1 << 20, "{b} bytes");
    });
}
