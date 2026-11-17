//! `for-in` and the prototype tail that has no enumerable key (`Heap::enum_tail_clean`). The expected texts come from Node 22, one fresh
//! context for each program. Each program runs `ks` (a `for-in` over a value) more than once between two changes, so the first run
//! certifies the tail and the later runs skip it. A wrong certificate shows as a missing or an extra key.

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

const KS: &str = "function ks(o){var r=[];for(var k in o)r.push(k);return r.join('+');} ";

const CASES: &[(&str, &str, &str)] = &[
    ("plain", r#"var r=[]; for (var i=0;i<4;i++) r.push(ks([1,2])+'/'+ks({a:1,b:2})+'/'+ks('xy')); r.join(' ')"#, "0+1/a+b/0+1 0+1/a+b/0+1 0+1/a+b/0+1 0+1/a+b/0+1"),
    ("array_proto_add_between", r#"var r=[ks([1]),ks([1]),ks([1])]; Array.prototype.e1=1; r.push(ks([1]),ks({})); delete Array.prototype.e1; r.push(ks([1]),ks([1])); r.join(' ')"#, "0 0 0 0+e1  0 0"),
    ("object_proto_add_between", r#"var r=[ks([1]),ks({a:1}),ks([1]),ks({a:1})]; Object.defineProperty(Object.prototype,'x',{enumerable:true,configurable:true,value:1}); r.push(ks([1]),ks({a:1}),ks(function(){})); delete Object.prototype.x; r.push(ks([1]),ks({a:1})); r.join(' ')"#, "0 a 0 a 0+x a+x x 0 a"),
    ("add_during_loop", r#"var r=[],done=false; ks([1]);ks([1]); for (var k in [7,8]) { if(!done){Object.prototype.late=1;done=true} r.push(k) } r.push(ks([1])); delete Object.prototype.late; r.push(ks([1])); r.join(' ')"#, "0 1 0+late 0"),
    ("attr_flip", r#"var r=[ks([1]),ks([1])]; Object.defineProperty(Array.prototype,'at',{enumerable:true}); r.push(ks([1])); Object.defineProperty(Array.prototype,'at',{enumerable:false}); r.push(ks([1]),ks([1])); r.join(' ')"#, "0 0 0+at 0 0"),
    ("assign_and_defineProperties", r#"var r=[ks([1]),ks([1])]; Object.assign(Array.prototype,{ea:1}); r.push(ks([1])); delete Array.prototype.ea; Object.defineProperties(Object.prototype,{eb:{value:1,enumerable:true,configurable:true}}); r.push(ks([1]),ks({})); delete Object.prototype.eb; r.push(ks([1]),ks({})); r.join(' ')"#, "0 0 0+ea 0+eb eb 0 "),
    ("index_key_on_proto", r#"var r=[ks([1]),ks([1])]; Array.prototype[3]='x'; r.push(ks([1])); delete Array.prototype[3]; r.push(ks([1])); Object.prototype[0]='y'; r.push(ks([5]),ks({})); delete Object.prototype[0]; r.push(ks([5]),ks({})); r.join(' ')"#, "0 0 0+3 0 0 0 0 "),
    ("index_key_on_class_proto", r#"class A{}; var a=new A; var r=[ks(a),ks(a)]; A.prototype[2]=1; r.push(ks(a)); delete A.prototype[2]; r.push(ks(a)); r.join(' ')"#, "  2 "),
    ("shadow_nonenumerable", r#"var p={x:1,y:2}, o=Object.create(p); Object.defineProperty(o,'x',{value:0,enumerable:false}); var r=[ks(o),ks(o)]; p.z=3; r.push(ks(o)); r.join(' ')"#, "y y y+z"),
    ("shadow_by_own_key_in_tail", r#"var p={x:1}, o=Object.create(p); o.x=2; var r=[ks(o),ks(o)]; delete o.x; r.push(ks(o)); r.join(' ')"#, "x x x"),
    ("set_prototype_of_receiver", r#"function A(){} var a=new A; var r=[ks(a),ks(a),ks(a)]; Object.setPrototypeOf(a,{q:1}); r.push(ks(a)); Object.setPrototypeOf(a,Array.prototype); r.push(ks(a),ks(a)); r.join(' ')"#, "   q  "),
    ("set_prototype_of_class_proto", r#"function A(){} var a=new A; var r=[ks(a),ks(a),ks(a)]; Object.setPrototypeOf(A.prototype,{q:1}); r.push(ks(a)); Object.setPrototypeOf(A.prototype,Object.prototype); r.push(ks(a),ks(a)); r.join(' ')"#, "   q  "),
    ("dunder_proto", r#"var a=[1],r=[ks(a),ks(a),ks(a)]; a.__proto__={w:1}; r.push(ks(a)); a.__proto__=Array.prototype; r.push(ks(a)); var o={}; r.push(ks(o),ks(o)); o.__proto__=Object.create({v:1}); r.push(ks(o)); r.join(' ')"#, "0 0 0 0+w 0   v"),
    ("array_proto_proto_dirty", r#"var r=[ks([1]),ks([1])]; var saved=Object.getPrototypeOf(Array.prototype); r.push(Object.setPrototypeOf(Array.prototype,{ap:1})===Array.prototype); r.push(ks([1])); Object.setPrototypeOf(Array.prototype,saved); r.push(ks([1])); r.join(' ')"#, "0 0 true 0+ap 0"),
    ("object_proto_immutable", r#"var r=[ks({}),ks({})]; try{Object.setPrototypeOf(Object.prototype,{z:1}); r.push('no throw')}catch(e){r.push(e.name)} r.push(ks({}),ks([])); r.join(' ')"#, "  TypeError  "),
    ("proxy_receiver", r#"var p=new Proxy({a:1,b:2},{}); var r=[ks(p),ks(p),ks(p)]; r.join(' ')"#, "a+b a+b a+b"),
    ("proxy_proto", r#"var log=[]; var px=new Proxy({},{ownKeys(){log.push('ok');return ['pa','pb']},getOwnPropertyDescriptor(t,k){return {value:1,enumerable:k=='pa',configurable:true}}}); var o=Object.create(px); o.own=1; var r=[ks(o),ks(o),ks(o)]; r.push(log.length>=3); r.join(' ')"#, "own+pa own+pa own+pa true"),
    ("typed_array_proto", r#"var t=new Uint8Array(2); var o=Object.create(t); o.own=1; var r=[ks(o),ks(o),ks(t),ks(t)]; r.join(' ')"#, "own+0+1 own+0+1 0+1 0+1"),
    ("typed_array_receiver_and_array", r#"var t=new Uint8Array(3); var r=[ks(t),ks(t),ks([1,2,3]),ks(t)]; r.join(' ')"#, "0+1+2 0+1+2 0+1+2 0+1+2"),
    ("string_object", r#"var s=new String('ab'); s.z=1; var r=[ks(s),ks(s),ks('ab')]; Object.prototype.sx=1; r.push(ks(s)); delete Object.prototype.sx; r.push(ks(s)); r.join(' ')"#, "0+1+z 0+1+z 0+1 0+1+z+sx 0+1+z"),
    ("delete_during_loop", r#"var o={a:1,b:2,c:3},r=[]; ks(o);ks(o); for (var k in o){ delete o.b; r.push(k)} r.push(ks([1,2])); r.join(' ')"#, "a c 0+1"),
    ("add_own_during_loop", r#"var o={a:1},r=[]; for (var k in o){ o.n=1; r.push(k)} r.push(ks(o)); r.join(' ')"#, "a a+n"),
    ("class_proto_enumerable_method", r#"function A(){} var a=new A; var r=[ks(a),ks(a)]; A.prototype.m=function(){}; r.push(ks(a),ks(a)); delete A.prototype.m; r.push(ks(a)); r.join(' ')"#, "  m m "),
    ("class_syntax_methods", r#"class A{ m(){} static s(){} get g(){return 1} } class B extends A{ n(){} } var b=new B; b.own=1; var r=[ks(b),ks(b),ks(b)]; A.prototype.late=1; r.push(ks(b)); r.join(' ')"#, "own own own own+late"),
    ("inheritance_dirty_middle", r#"function A(){} A.prototype.shared=1; function B(){} B.prototype=Object.create(A.prototype); var b=new B; b.own=1; var r=[ks(b),ks(b)]; Object.defineProperty(A.prototype,'shared',{enumerable:false}); r.push(ks(b),ks(b)); Object.prototype.top=1; r.push(ks(b)); delete Object.prototype.top; r.push(ks(b)); r.join(' ')"#, "own+shared own+shared own own own+top own"),
    ("object_create_null", r#"var o=Object.create(null); o.a=1; var r=[ks(o),ks(o)]; r.join(' ')"#, "a a"),
    ("object_create_array_proto", r#"var o=Object.create(Array.prototype); o.a=1; var r=[ks(o),ks(o)]; Array.prototype.q=1; r.push(ks(o)); delete Array.prototype.q; r.push(ks(o)); r.join(' ')"#, "a a a+q a"),
    ("accessor_enumerable_on_proto", r#"var r=[ks([1]),ks([1])]; Object.defineProperty(Array.prototype,'acc',{get(){return 1},enumerable:true,configurable:true}); r.push(ks([1])); delete Array.prototype.acc; r.push(ks([1])); r.join(' ')"#, "0 0 0+acc 0"),
    ("freeze_prototype", r#"var r=[ks([1]),ks([1])]; Object.freeze(Array.prototype); r.push(ks([1]),ks([1])); r.join(' ')"#, "0 0 0 0"),
    ("symbol_keys_ignored", r#"var s=Symbol('s'); var r=[ks([1]),ks([1])]; Array.prototype[s]=1; r.push(ks([1])); delete Array.prototype[s]; Object.defineProperty(Object.prototype,s,{value:1,enumerable:true,configurable:true}); r.push(ks([1])); delete Object.prototype[s]; r.join(' ')"#, "0 0 0 0"),
    ("new_realm_function_prototype", r#"function f(){} f.p=1; var r=[ks(f),ks(f),ks(function(){}),ks(()=>1)]; Function.prototype.fe=1; r.push(ks(f),ks(()=>1)); delete Function.prototype.fe; r.push(ks(f)); r.join(' ')"#, "p p   p+fe fe p"),
    ("define_on_receiver_not_tail", r#"var r=[ks([1]),ks([1])]; var a=[1]; a.own=1; r.push(ks(a)); Object.defineProperty(a,'hid',{value:1,enumerable:false}); r.push(ks(a)); r.join(' ')"#, "0 0 0+own 0+own"),
    ("setter_on_proto_adds_key", r#"var r=[ks([1]),ks([1])]; Object.defineProperty(Object.prototype,'setme',{set(v){ Object.defineProperty(Array.prototype,'viaSetter',{value:v,enumerable:true,configurable:true}) },configurable:true}); var a=[1]; a.setme=5; r.push(ks([1])); delete Array.prototype.viaSetter; delete Object.prototype.setme; r.push(ks([1])); r.join(' ')"#, "0 0 0+viaSetter 0"),
];

/// Loops inside a function body, so `Auto` and `Byte` run them on the lane. The body ends with `return`. Node 22 gave the texts.
const LOOP_CASES: &[(&str, &str, &str)] = &[
    ("order_mixed", r#"var r=[ks({b:1,2:1,a:1,1:1}),ks({z:1,y:1,x:1})]; var d={}; for(var i=0;i<200;i++) d['k'+i]=i; d[5]=1; var s=ks(d).split('+'); r.push(s.length, s[0], s[1], s[2], s[200]); return r.join(' ')"#, "1+2+b+a z+y+x 201 5 k0 k1 k199"),
    ("own_symbol_skipped", r#"var o={a:1}; o[Symbol('s')]=1; o.b=2; return ks(o)+' '+ks(o)"#, "a+b a+b"),
    ("own_nonenumerable_skipped", r#"var o={a:1}; Object.defineProperty(o,'h',{value:1}); o.b=1; return ks(o)+' '+ks(o)"#, "a+b a+b"),
    ("delete_last_key", r#"var o={a:1,b:2,c:3},r=[]; for(var k in o){r.push(k); if(k=='a') delete o.c} return r.join('+')+' '+ks(o)"#, "a+b a+b"),
    ("delete_then_add_again", r#"var o={a:1,b:2,c:3},r=[]; for(var k in o){r.push(k); if(k=='a'){delete o.b; o.b=5}} return r.join('+')+' '+ks(o)"#, "a+b+c a+c+b"),
    ("delete_from_getter", r#"var o={a:1,b:2,c:3},r=[]; var g={get x(){delete o.b; return 1}}; for(var k in o){r.push(k); g.x} return r.join('+')"#, "a+c"),
    ("nested_delete_same_object", r#"var o={a:1,b:2,c:3},r=[]; for(var k in o){ for(var j in o){ r.push(k+j); delete o.b } } return r.join('+')"#, "aa+ac+ca+cc"),
    ("add_then_delete_during_loop", r#"var o={a:1,b:2},r=[]; for(var k in o){r.push(k); o.n=1; delete o.n} return r.join('+')+' '+ks(o)"#, "a+b a+b"),
    ("nested_same_object", r#"var o={a:1,b:2,c:3},r=[]; for(var x in o) for(var y in o) r.push(x+y); return r.join('+')"#, "aa+ab+ac+ba+bb+bc+ca+cb+cc"),
    ("own_and_proto_same_key", r#"var p={x:1,y:1}; var o=Object.create(p); o.x=2; o.z=1; return ks(o)+' '+ks(o)"#, "x+z+y x+z+y"),
    ("set_prototype_of_during_loop", r#"var o={a:1,b:2},r=[]; for(var k in o){r.push(k); Object.setPrototypeOf(o,{q:1})} return r.join('+')+' '+ks(o)"#, "a+b a+b+q"),
    ("null_undefined", r#"var n=0; for(var k in null) n++; for(var k in undefined) n++; return n+':'+ks(null)+':'+ks(undefined)"#, "0::"),
    ("array_hole_and_prop", r#"var a=[1,,3]; a.x=1; return ks(a)+' '+ks([])+' '+ks(new Array(3))"#, "0+2+x  "),
    ("function_receiver", r#"function f(){} f.a=1; var g=()=>1; g.b=2; return ks(f)+' '+ks(g)"#, "a b"),
    ("frozen_object", r#"return ks(Object.freeze({a:1,b:2}))+' '+ks(Object.seal({c:1}))"#, "a+b c"),
    ("exits", r#"var o={a:1,b:2,c:3},r=[]; for(var k in o){ if(k=='b') break; r.push(k) } for(var k in o){ for(var j in o){ if(j=='b') break; r.push(k+j) } } function f(){ for(var k in o){ for(var j in o){ if(j=='b') return k+j } } } r.push(f()); try{ for(var k in o){ for(var j in o){ if(k=='b') throw k+j } } }catch(e){r.push('t'+e)} for(var k in o) r.push(k); return r.join('+')"#, "a+aa+ba+ca+ab+tba+a+b+c"),
    ("attr_change_mid_loop", r#"var o={a:1,b:2,c:3},r=[]; for(var k in o){r.push(k); Object.defineProperty(o,'c',{enumerable:false})} var p={a:1,b:2},q=[]; for(var k in p){q.push(k); Object.freeze(p)} return r.join('+')+' '+q.join('+')+' '+ks(o)"#, "a+b+c a+b a+b"),
    ("to_dictionary_mid_loop", r#"var o={a:1,b:2,c:3},r=[]; for(var k in o){r.push(k); if(k=='a'){ for(var i=0;i<200;i++) o['n'+i]=i; delete o.b }} return r.join('+')+' '+ks(o).split('+').length"#, "a+c 202"),
    ("add_and_delete_many", r#"var o={a:1,b:2,c:3,d:4},r=[]; for(var k in o){r.push(k); o['x'+k]=1; delete o.d; delete o.a} return r.join('+')+' '+ks(o)"#, "a+b+c b+c+xa+xb+xc"),
    ("proxy_string_typed", r#"return ks(new Proxy({a:1,b:2},{}))+' '+ks('xy')+' '+ks(new String('ab'))+' '+ks(new Uint8Array(2))"#, "a+b 0+1 0+1 0+1"),
    ("empty_and_create_null", r#"var o=Object.create(null); o.a=1; o.b=2; return ks({})+'|'+ks(o)"#, "|a+b"),
    ("dict_receiver_shadows_proto", r#"var p={x:1,y:2}; var d=Object.create(p); for(var i=0;i<200;i++) d['k'+i]=i; Object.defineProperty(d,'x',{value:1,enumerable:false}); var s=ks(d).split('+'); return s.length+' '+s[0]+' '+s[199]+' '+s[200]"#, "201 k0 k199 y"),
    ("sparse_array_list_path", r#"var a=[]; a[5]=1; a[1000000]=2; a[2]=3; Object.defineProperty(a,3,{value:1,enumerable:false}); a.z=1; return ks(a)+' '+ks(a)"#, "2+5+1000000+z 2+5+1000000+z"),
    ("array_proto_with_elements", r#"var p=[9,9,9]; var a=Object.create(p); a[1]=1; a.q=1; return ks(a)+' '+ks(a)"#, "1+q+0+2 1+q+0+2"),
    ("dict_delete_during_loop", r#"var d={}; for(var i=0;i<200;i++) d['k'+i]=i; var r=[]; for(var k in d){ r.push(k); delete d.k1; delete d.k199 } return r.length+' '+r[1]+' '+r[197]"#, "198 k2 k198"),
];

/// Each case in a fresh realm in `Tree`, `Auto` and `Byte`, against the Node text. `wrap` puts the source in a function.
fn check_modes(cases: &[(&str, &str, &str)], wrap: bool) {
    let mut bad = Vec::new();
    for mode in [Exec::Tree, Exec::Auto, Exec::Byte] {
        for &(name, src, want) in cases {
            let mut it = Interp::realm(Heap::default(), EvalLimits::default()).unwrap();
            it.set_exec(mode);
            let got = if wrap { run(&mut it, &format!("{KS}(function(){{ {src} }})()")) } else { run(&mut it, &format!("{KS}{src}")) };
            if got != want {
                bad.push(format!("{mode:?} {name}: node {want:?}, bare {got:?}"));
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn for_in_matches_node_when_a_prototype_tail_changes() {
    big(|| check_modes(CASES, false));
}

#[test]
fn for_in_matches_node_in_every_mode() {
    big(|| check_modes(LOOP_CASES, true));
}

/// The failing-first test of the shape path (`docs/design-for-in-keys.md`, design B): a loop over a plain object in shape mode reads
/// the keys of its shape and builds no list. A dictionary, an array, a proxy and a String wrapper take the list path.
#[test]
fn a_for_in_over_a_plain_object_reads_its_shape() {
    big(|| {
        // The first loop over `{}` walks `Object.prototype` and certifies it (`enum_tail_certify`). The later loops skip it.
        let src = "function n(o){var c=0;for(var k in o)c++;return c} n({}); var d={}; for(var i=0;i<200;i++) d['k'+i]=i; \
                   var w=[n({a:1,b:2}), n(Object.create(null)), n({})]; \
                   var z=[n(d), n([1,2]), n(new Proxy({a:1},{})), n(new String('ab'))]; w.concat(z).join()";
        for mode in [Exec::Tree, Exec::Auto, Exec::Byte] {
            let mut it = Interp::realm(Heap::default(), EvalLimits::default()).unwrap();
            it.set_exec(mode);
            let c0 = it.ic_counts().heap.for_in_shape;
            assert_eq!(run(&mut it, src), "2,0,0,200,2,1,2", "{mode:?}");
            assert_eq!(it.ic_counts().heap.for_in_shape - c0, 3, "{mode:?}: one for each plain object, none for the others");
        }
    });
}

/// A collect between two scripts sweeps the shapes and gives the live ones new ids. A loop after the sweep reads the new id.
#[test]
fn a_shape_sweep_between_two_loops_keeps_the_keys() {
    big(|| {
        for mode in [Exec::Tree, Exec::Auto, Exec::Byte] {
            let mut it = Interp::realm(Heap::default(), EvalLimits::default()).unwrap();
            it.set_exec(mode);
            let src = "var o={a:1,b:2}; o.c=3; delete o.a; o.d=4; function f(){var r=[];for(var k in o)r.push(k);return r.join('+')} f()";
            assert_eq!(run(&mut it, &format!("{KS}{src}")), "b+c+d");
            // Garbage shapes past `SWEEP_MIN`, so that the collect sweeps.
            run(&mut it, "(function(){ for (var i = 0; i < 10000; i++) { var t = {}; t['g' + i] = i; } })()");
            let sweeps = it.heap.shape_sweeps();
            it.collect(&[]).unwrap();
            assert_eq!(it.heap.shape_sweeps(), sweeps + 1, "{mode:?}: the collect sweeps the shapes");
            assert_eq!(run(&mut it, "f() + ' ' + f() + ' ' + ks(o)"), "b+c+d b+c+d b+c+d", "{mode:?}");
        }
    });
}

/// The step limit still bounds a loop over a huge dictionary object: the list path spends a step for each key at entry.
#[test]
fn a_for_in_over_a_huge_object_hits_the_step_limit() {
    big(|| {
        for mode in [Exec::Tree, Exec::Auto, Exec::Byte] {
            let mut it = Interp::realm(Heap::default(), EvalLimits::default()).unwrap();
            it.set_exec(mode);
            run(&mut it, "var o={}; for (var i = 0; i < 100000; i++) o['k' + i] = i;");
            it.limits.max_steps = 50_000;
            let r = it.run_script("(function(){ for (var k in o) {} })()");
            assert!(matches!(r, Err(Error::Limit(LimitKind::Steps))), "{mode:?}: {r:?}");
        }
    });
}

/// Steps of one script run.
fn steps_of(it: &mut Interp, src: &str) -> u64 {
    let before = it.steps;
    run(it, src);
    it.steps - before
}

#[test]
fn a_clean_tail_is_not_walked_again() {
    big(|| {
        let mut it = Interp::realm(Heap::default(), EvalLimits::default()).unwrap();
        run(&mut it, KS);
        let src = "ks([1, 2, 3]); ks({ a: 1 });";
        steps_of(&mut it, src); // certifies Array.prototype and Object.prototype
        let (c0, s) = (it.ic_counts().heap.enum_tail_skips, steps_of(&mut it, src));
        assert_eq!(it.ic_counts().heap.enum_tail_skips - c0, 2, "one skip for each loop");
        // The same two loops with a dirty tail walk about 50 more keys each, and each key takes at least one step.
        run(&mut it, "Object.defineProperty(Object.prototype, 'x', { enumerable: true, configurable: true, value: 1 })");
        let dirty = steps_of(&mut it, src);
        assert!(dirty > s + 60, "clean {s} steps, dirty {dirty} steps");
        // An enumerable key makes the tail dirty at once, and removing it makes the next loop certify again.
        run(&mut it, "delete Object.prototype.x");
        steps_of(&mut it, src);
        let c1 = it.ic_counts().heap.enum_tail_skips;
        steps_of(&mut it, src);
        assert_eq!(it.ic_counts().heap.enum_tail_skips - c1, 2);
    });
}

/// The list path (design C): a receiver's own keys are unique, so the loop makes a seen set only when a prototype gives a key.
#[test]
fn the_list_path_makes_a_seen_set_only_for_prototype_keys() {
    big(|| {
        // The first loops walk the prototypes (their keys are not enumerable) and certify them.
        let warm = "function n(o){var c=0;for(var k in o)c++;return c} n({}); n([]); var d={}; for(var i=0;i<200;i++) d['k'+i]=i;";
        let src = "var p={x:1}, q=Object.create(p); q[0]=1; [n(d), n([1,2]), n(d), n(q)].join()";
        for mode in [Exec::Tree, Exec::Auto, Exec::Byte] {
            let mut it = Interp::realm(Heap::default(), EvalLimits::default()).unwrap();
            it.set_exec(mode);
            run(&mut it, warm);
            let c0 = it.ic_counts().heap.for_in_sets;
            assert_eq!(run(&mut it, src), "200,2,200,2", "{mode:?}");
            assert_eq!(it.ic_counts().heap.for_in_sets - c0, 1, "{mode:?}: only the loop over q, whose prototype gives x");
        }
    });
}
