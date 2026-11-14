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

#[test]
fn for_in_matches_node_when_a_prototype_tail_changes() {
    big(|| {
        let mut bad = Vec::new();
        for &(name, src, want) in CASES {
            let mut it = Interp::realm(Heap::default(), EvalLimits::default()).unwrap();
            let got = run(&mut it, &format!("{KS}{src}"));
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
