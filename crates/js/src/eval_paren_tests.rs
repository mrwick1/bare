//! What parentheses mean at run time. The expected value of each script is what V8 (Node) gives. A parenthesized
//! expression is the inner expression in the tree, so these checks guard the cases where `(a)` once differed from `a`:
//! the `this` of a call, an optional chain, `delete`, `typeof`, names of functions, targets, `toString`, directives.

use super::*;
use crate::heap::Heap;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn out(it: &mut Interp, src: &str) -> String {
    match it.run_script(src) {
        Ok(v) => it.display(v),
        Err(Error::Throw(v)) => format!("throw:{}", it.get_str(v, "message")),
        Err(e) => format!("other:{e:?}"),
    }
}

#[test]
fn parenthesized_expressions_give_the_values_of_v8() {
    big(|| {
        let cases: &[(&str, &str)] = &[
        ("var o={f(){return this===o}}; (o.f)()", "true"),
        ("var o={f(){return this===o}}; ((o.f))()", "true"),
        ("var o={f(){return this===o}}; (o['f'])()", "true"),
        ("var o={f(){return this===o}}; (o?.f)()", "true"),
        ("var o={f(){return this===o}}; (o.f)``", "true"),
        ("var o={f(){return this===o}}; (0,o.f)()", "false"),
        ("var o={f(){return this===o}}; var a=(o.f,o.f); a()", "false"),
        ("var o={f(){return this===o}}; o.g=o.f; (true?o.g:0)()", "false"),
        ("var o=null; try{(o?.b).c; 'no'}catch(e){e instanceof TypeError}", "true"),
        ("var o=null; String(o?.b.c)", "undefined"),
        ("var o=null; String((o?.b)?.c)", "undefined"),
        ("var o={}; try{(o?.b)(); 'no'}catch(e){e instanceof TypeError}", "true"),
        ("var o={p:1}; var r=delete (o.p); r+String('p' in o)", "truefalse"),
        ("var o={p:1}; var r=delete ((o.p)); r+String('p' in o)", "truefalse"),
        ("var r=delete (1+1); String(r)", "true"),
        ("globalThis.zz=1; var r=delete (zz); String(r)+typeof zz", "trueundefined"),
        ("typeof (nope)", "undefined"),
        ("typeof ((nope))", "undefined"),
        ("var x=(function(){}); x.name", "x"),
        ("var x=((function(){})); x.name", "x"),
        ("var x=(0,function(){}); x.name", ""),
        ("var x=(class{}); x.name", "x"),
        ("var x=(()=>1); x.name", "x"),
        ("var x=(function(){}), y=(x); y.name", "x"),
        ("var o={a:(function(){})}; o.a.name", "a"),
        ("var o={}; o.a=(function(){}); o.a.name", ""),
        ("(function  ( a ,b ) { /*c*/ return 1 }).toString()", "function  ( a ,b ) { /*c*/ return 1 }"),
        ("(  class  A { } ).toString()", "class  A { }"),
        ("var f=(  async  ( a ) => a  ); f.toString()", "async  ( a ) => a"),
        ("var a=1; (a)=2; (a)+=3; (a)++; ++(a); String(a)", "7"),
        ("var o={p:1}; (o.p)=5; (o.p)+=1; (o).p++; String(o.p)", "7"),
        ("var a,b; [(a),(b)]=[1,2]; a+b", "3"),
        ("var o={}; [(o.x)]=[4]; ({k:(o.y)}={k:5}); o.x+o.y", "9"),
        ("var a; for((a) of [1,2,3]); a", "3"),
        ("(-2)**2", "4"),
        ("var a=2; (-a)**2", "4"),
        ("(1,2)", "2"),
        ("var f=(1,2,3)+1; f", "4"),
        ("[(1,2),(3,4)].join()", "2,4"),
        ("function f(){return arguments.length}; f((1,2),3)", "2"),
        ("(null ?? 1) || 2", "1"),
        ("(0 || null) ?? 3", "3"),
        ("((x)=>x+1)(2)", "3"),
        ("(x=>x)?1:2", "1"),
        ("(x=>x)+''", "x=>x"),
        ("var g=(async function(){}); g.constructor.name", "AsyncFunction"),
        ("var s=''; (function*(){ s+=(yield 1) })().next(); 1", "1"),
        ("\"use strict\"; (function(){ return this })()===undefined", "true"),
        ("(\"use strict\"); (function(){ return this===globalThis })()", "true"),
        ("function f(){ (\"use strict\"); return this===globalThis } f()", "true"),
        ("function f(){ \"use strict\"; return this===undefined } f()", "true"),
        ("var C=class{static m(){return this===C}}; (C.m)()", "true"),
        ("class A{m(){return 1}} class B extends A{m(){return (super.m)()}} new B().m()", "1"),
        ("var o={C:function(){this.v=1}}; new (o.C)().v", "1"),
        ("var o={C:function(){this.v=1}}; new (o.C)().v", "1"),
        ("var f=function(){return new.target}; new (f)()===undefined", "false"),
        ("var a=[]; a[(0)]=1; a[((1))]=2; a.join()", "1,2"),
        ("var o={['k'+(1)]:2}; o.k1", "2"),
        ("var a=5; var b=(a)?(a):(0); b", "5"),
        ("var n=0; if((n))n=1; else n=2; n", "2"),
        ("`${(1)}${(2,3)}`", "13"),
        ("var o={get x(){return this===o}}; (o).x", "true"),
        ("let a=1; { let a=2; (a)=3 } a", "1"),
        ("var x=1; ((x))=function(){}; typeof x", "function"),
        ];
        let mut bad = Vec::new();
        for &(src, want) in cases {
            let mut it = Interp::realm(Heap::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }).expect("realm");
            let got = out(&mut it, src);
            if got != want {
                bad.push(format!("{src:?}: expected {want:?}, got {got:?}"));
            }
        }
        assert!(bad.is_empty(), "{} differ:\n{}", bad.len(), bad.join("\n"));
    });
}

/// The position of a frame is the start of the call it runs. A parenthesized callee does not move it.
#[test]
fn a_call_with_a_parenthesized_callee_keeps_its_stack_position() {
    big(|| {
        let g = "function g(){return new Error('x').stack.split('\\n').slice(1).join('|')}\nvar o={g:g};\n";
        let cases: &[(&str, &str)] = &[
            ("function f(){ return g() } f()", "    at g (script#1:1:21)|    at f (script#1:3:22)|    at script#1:3:28"),
            ("function f(){ return (g)() } f()", "    at g (script#1:1:21)|    at f (script#1:3:22)|    at script#1:3:30"),
            ("function f(){ return ((g))() } f()", "    at g (script#1:1:21)|    at f (script#1:3:22)|    at script#1:3:32"),
            ("function f(){ return (o.g)() } f()", "    at g (script#1:1:21)|    at f (script#1:3:22)|    at script#1:3:32"),
            ("function f(){ return (o?.g)() } f()", "    at g (script#1:1:21)|    at f (script#1:3:22)|    at script#1:3:33"),
            ("function f(){ return (0, g)() } f()", "    at g (script#1:1:21)|    at f (script#1:3:22)|    at script#1:3:33"),
            ("function f(){ return (g)`x` } f()", "    at g (script#1:1:21)|    at f (script#1:3:22)|    at script#1:3:31"),
            ("function f(){ return ( g )( ) } f()", "    at g (script#1:1:21)|    at f (script#1:3:22)|    at script#1:3:33"),
        ];
        let mut bad = Vec::new();
        for &(src, want) in cases {
            let mut it = Interp::realm(Heap::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }).expect("realm");
            let got = out(&mut it, &format!("{g}{src}"));
            if got != want {
                bad.push(format!("{src:?}: expected {want:?}, got {got:?}"));
            }
        }
        assert!(bad.is_empty(), "{} differ:\n{}", bad.len(), bad.join("\n"));
    });
}
