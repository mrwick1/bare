//! The keyed table of `GetElem` and `SetElem` (`Heap::get_keyed`, `Heap::set_keyed`, `docs/design-keyed-cache.md`). The expected
//! texts come from Node 22. Each program runs a keyed op many times, so the table fills and then hits, between two changes that a
//! stale entry would miss.

use super::*;
use crate::heap::Heap;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap()
}

fn run(it: &mut Interp, src: &str) -> String {
    match it.run_script(src) {
        Ok(v) => it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!(
            "throw:{}:{}",
            it.get_str(v, "name"),
            it.get_str(v, "message")
        ),
        Err(e) => format!("error:{e:?}"),
    }
}

fn realm(mode: Exec) -> Interp<'static> {
    let mut it = Interp::realm(Heap::default(), EvalLimits::default()).unwrap();
    it.set_exec(mode);
    it
}

/// Function bodies, so that `Auto` and `Byte` run them on the lane. Node 22 gave the texts.
const CASES: &[(&str, &str, &str)] = &[
    (
        "hit",
        r#"var os=[{a:1,b:2,c:3,d:4},{b:5,a:6,d:7,c:8},{x:0,a:9,b:10,c:11,d:12}], ks=['a','b','c','d'], t=0; for(var n=0;n<50;n++) for(var i=0;i<3;i++) for(var j=0;j<4;j++) t+=os[i][ks[j]]; var o={}; for(var n=0;n<5;n++) for(var j=0;j<4;j++){ var p={}; p[ks[j]]=n; o[ks[j]]=(o[ks[j]]|0)+p[ks[j]]; } return t+' '+JSON.stringify(o)"#,
        r#"3900 {"a":10,"b":10,"c":10,"d":10}"#,
    ),
    (
        "holes",
        r#"var k='1', r=[]; for(var n=0;n<4;n++) r.push([1,,3][k], [1,,3][1]); Array.prototype[1]='p'; for(var n=0;n<3;n++) r.push([1,,3][k], [1,,3][1]); Object.defineProperty(Array.prototype,'1',{get(){return 'g'},configurable:true}); for(var n=0;n<3;n++) r.push([1,,3][k]); delete Array.prototype[1]; r.push([1,,3][k]); return r.join()"#,
        r#",,,,,,,,p,p,p,p,p,p,g,g,g,"#,
    ),
    (
        "proto_index_getter",
        r#"var o={a:1}, r=[], k='1'; for(var n=0;n<4;n++) r.push(o[k], o[1]); Object.defineProperty(Object.prototype,'1',{get(){return 'G'},configurable:true}); for(var n=0;n<3;n++) r.push(o[k], o[1], {}[k]); delete Object.prototype[1]; r.push(o[k]); return r.join()"#,
        r#",,,,,,,,G,G,G,G,G,G,G,G,G,"#,
    ),
    (
        "accessor_own",
        r#"var c=0, s=[], o={get a(){return ++c}, set a(v){s.push(v)}}, k='a', r=[]; for(var n=0;n<5;n++){ r.push(o[k]); o[k]=n; } return r.join()+' '+s.join()"#,
        r#"1,2,3,4,5 0,1,2,3,4"#,
    ),
    (
        "setter_on_proto_after_fill",
        r#"var k='q', made=[], log=[]; for(var n=0;n<5;n++){ var o={}; o[k]=n; made.push(Object.keys(o).join()); } Object.defineProperty(Object.prototype,'q',{set(v){log.push(v)},configurable:true}); for(var n=0;n<3;n++){ var o={}; o[k]=10+n; made.push(Object.keys(o).join()||'-'); } delete Object.prototype.q; var o={}; o[k]=1; made.push(Object.keys(o).join()); return made.join('|')+' '+log.join()"#,
        r#"q|q|q|q|q|-|-|-|q 10,11,12"#,
    ),
    (
        "readonly_on_proto_after_fill",
        r#"var k='w', r=[]; for(var n=0;n<5;n++){ var o={}; o[k]=n; r.push(o[k]); } Object.defineProperty(Object.prototype,'w',{value:7,writable:false,configurable:true}); for(var n=0;n<3;n++){ var o={}; o[k]=n; r.push(o[k], Object.keys(o).length); } var e='none'; try { (function(){'use strict'; var o={}; o[k]=1;})() } catch(x){ e=x.name } delete Object.prototype.w; return r.join()+' '+e"#,
        r#"0,1,2,3,4,7,0,7,0,7,0 TypeError"#,
    ),
    (
        "setter_adds_key",
        r#"var k='z', r=[]; var p={set z(v){ Object.defineProperty(this,'z',{value:v*2,writable:true,enumerable:true,configurable:true}) }}; for(var n=0;n<5;n++){ var o=Object.create(p); o[k]=n; o[k]=n+100; r.push(o[k]); } return r.join()"#,
        r#"100,101,102,103,104"#,
    ),
    (
        "frozen_sealed_nonext",
        r#"var k='m', j='nw', r=[]; for(var n=0;n<4;n++){ var a={m:1}; a[k]=n; a[j]=n; r.push(a[k],a[j]); } 'use strict'; var f={m:1}; f[k]=2; Object.freeze(f); f[k]=3; f[j]=4; var s={m:1}; s[k]=2; Object.seal(s); s[k]=5; s[j]=6; var x={m:1}; x[k]=2; Object.preventExtensions(x); x[k]=7; x[j]=8; r.push(f[k],f[j],s[k],s[j],x[k],x[j]); var e=[]; (function(){'use strict'; try{ f[k]=9 }catch(t){e.push(t.name)} try{ s[j]=9 }catch(t){e.push(t.name)} try{ x[j]=9 }catch(t){e.push(t.name)} try{ x[k]=10 }catch(t){e.push('bad')} })(); return r.join()+' '+e.join()+' '+x.m"#,
        r#"0,0,1,1,2,2,3,3,2,,5,,7, TypeError,TypeError,TypeError 10"#,
    ),
    (
        "array_length",
        r#"var k='length', r=[]; for(var n=0;n<5;n++){ var a=[1,2,3]; r.push(a[k]); a[k]=1; r.push(a.length, a[k]); } var b=[1,2]; Object.defineProperty(b,'length',{writable:false}); b[k]=0; r.push(b.length); var o={length:3}; for(var n=0;n<3;n++){ o[k]=n; r.push(o[k]); } return r.join()"#,
        r#"3,1,1,3,1,1,3,1,1,3,1,1,3,1,1,2,0,1,2"#,
    ),
    (
        "symbol_keys",
        r#"var s=Symbol('s'), t=Symbol.iterator, r=[]; for(var n=0;n<5;n++){ var o={}; o[s]=n; r.push(o[s], typeof [][t], o[t]); } return r.join()"#,
        r#"0,function,,1,function,,2,function,,3,function,,4,function,"#,
    ),
    (
        "numeric_string_keys",
        r#"var ks=['1','01','-0','4294967295','4294967294','1.5','NaN'], r=[]; for(var n=0;n<3;n++){ var o={}, a=[]; for(var i=0;i<ks.length;i++){ o[ks[i]]=i; a[ks[i]]=i; } r.push(Object.keys(o).join('/'), a.length, o['1'], o[1], a[1]); } return r.join(' ')"#,
        r#"1/4294967294/01/-0/4294967295/1.5/NaN 4294967295 0 0 0 1/4294967294/01/-0/4294967295/1.5/NaN 4294967295 0 0 0 1/4294967294/01/-0/4294967295/1.5/NaN 4294967295 0 0 0"#,
    ),
    (
        "long_and_object_keys",
        r#"var lk='abcdefghijabcdefghijabcdefghijabcdefghij', c=0, ko={toString(){c++; return 'kk'}}, r=[]; for(var n=0;n<4;n++){ var o={}; o[lk]=n; o[ko]=n+1; r.push(o[lk], o[ko], o.kk); } return r.join()+' '+c"#,
        r#"0,1,1,1,2,2,2,3,3,3,4,4 8"#,
    ),
    (
        "dictionary_mode",
        r#"var r=[]; var d={}; for(var i=0;i<200;i++) d['k'+i]=i; for(var n=0;n<3;n++){ r.push(d['k5'], d['k199'], d['zz']); d['k'+n]=-n; d['new'+n]=n; } var e={a:1,b:2,c:3}; delete e.b; for(var n=0;n<3;n++){ var k=['a','b','c'][n]; e[k]=(e[k]|0)+10; r.push(e[k]); } return r.join()+' '+Object.keys(e).join()"#,
        r#"5,199,,5,199,,5,199,,11,10,13 a,c,b"#,
    ),
    (
        "delete_between_reads",
        r#"var o={a:1,b:2,c:3}, k='c', r=[]; for(var n=0;n<4;n++) r.push(o[k]); delete o.c; r.push(o[k]); o.c=9; r.push(o[k]); return r.join()"#,
        r#"3,3,3,3,,9"#,
    ),
    (
        "set_prototype_of",
        r#"var p1={x:1}, p2={y:2}, o=Object.create(p1), r=[]; for(var n=0;n<4;n++) r.push(o['x'], o['y']); Object.setPrototypeOf(o,p2); for(var n=0;n<3;n++) r.push(o['x'], o['y']); p2.x=5; r.push(o['x']); return r.join()"#,
        r#"1,,1,,1,,1,,,2,,2,,2,5"#,
    ),
    (
        "proto_epoch_change",
        r#"var p={}, r=[]; function C(){} C.prototype=p; for(var n=0;n<4;n++){ var o=new C; r.push(o['v']); o['u']=n; } p.v='pv'; Object.defineProperty(p,'u',{set(x){r.push('set'+x)},configurable:true}); for(var n=0;n<3;n++){ var o=new C; r.push(o['v']); o['u']=n; r.push(Object.keys(o).length); } return r.join()"#,
        r#",,,,pv,set0,0,pv,set1,0,pv,set2,0"#,
    ),
    (
        "for_in_copy",
        r#"var srcs=[{a:1,b:2},{b:3,a:4,c:5},{ref:0,key:1,x:2}], out=[]; for(var n=0;n<20;n++) for(var i=0;i<3;i++){ var c={}, l=srcs[i]; for(var u in l) u!=='ref' && (c[u]=l[u]); out.push(JSON.stringify(c)); } return out.slice(0,3).join()+' '+out.slice(-3).join()+' '+out.length"#,
        r#"{"a":1,"b":2},{"b":3,"a":4,"c":5},{"key":1,"x":2} {"a":1,"b":2},{"b":3,"a":4,"c":5},{"key":1,"x":2} 60"#,
    ),
    (
        "views",
        r#"var t=new Uint8Array(2), k='foo', r=[]; for(var n=0;n<3;n++){ t[k]={n:n}; r.push(typeof t[k], t[k].n); t['-0']=5; r.push(t['-0'], t['1.5']); t['1.5']=3; r.push(t['1.5']); } return r.join()"#,
        r#"object,0,,,,object,1,,,,object,2,,,"#,
    ),
    (
        "exceptions",
        r#"var o={get a(){ throw new Error('boom') }}, k='a', r=[]; for(var n=0;n<3;n++){ try{ o[k] }catch(e){ r.push(e.message) } } try{ null[k] }catch(e){ r.push(e.message) } try{ undefined[k]=1 }catch(e){ r.push(e.name) } return r.join('|')"#,
        r#"boom|boom|boom|Cannot read properties of null (reading 'a')|TypeError"#,
    ),
];

#[test]
fn keyed_ops_match_node_in_every_mode() {
    big(|| {
        let mut bad = Vec::new();
        for mode in [Exec::Tree, Exec::Auto, Exec::Byte] {
            for &(name, src, want) in CASES {
                let got = run(&mut realm(mode), &format!("(function(){{ {src} }})()"));
                if got != want {
                    bad.push(format!("{mode:?} {name}: node {want:?}, bare {got:?}"));
                }
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    });
}

/// Reads over 3 shapes and 4 keys, and adds of 4 keys: the lane hits the table; the tree walker never uses it.
#[test]
fn keyed_ops_on_the_lane_hit_the_table() {
    big(|| {
        let src = "(function(){ var os=[{a:1,b:2,c:3,d:4},{b:5,a:6,d:7,c:8},{x:0,a:9,b:10,c:11,d:12}], ks=['a','b','c','d'], t=0; \
                   for (var n=0;n<50;n++) for (var i=0;i<3;i++) for (var j=0;j<4;j++) t+=os[i][ks[j]]; \
                   for (var n=0;n<50;n++) { var o={}; for (var j=0;j<4;j++) o[ks[j]]=j; t+=o.d; } return t })()";
        for mode in [Exec::Tree, Exec::Auto, Exec::Byte] {
            let mut it = realm(mode);
            let h0 = it.ic_counts().heap.keyed_hits;
            assert_eq!(run(&mut it, src), "4050", "{mode:?}");
            let hits = it.ic_counts().heap.keyed_hits - h0;
            if mode == Exec::Tree {
                assert_eq!(hits, 0, "the tree walker has no keyed path");
            } else {
                // 600 reads and 200 adds; only the first op of each pair misses.
                assert!(hits >= 750, "{mode:?}: {hits} hits");
            }
        }
    });
}

/// A collect that sweeps the shapes gives live objects new ids and frees the others, so a later shape can reuse the id of an entry.
/// The sweep empties the table, and keyed ops after it are right.
#[test]
fn a_shape_sweep_empties_the_keyed_table() {
    big(|| {
        let fill = "function rd(o,k){return o[k]} function wr(o,k,v){o[k]=v} \
                    function churn(p){ var bad=0; for (var i=0;i<10000;i++) { var t={}; wr(t,p+(i%50),i); wr(t,'g'+i,i); \
                    if (rd(t,'g'+i)!==i || rd(t,p+(i%50))!==i || Object.keys(t).join()!==p+(i%50)+',g'+i) bad++; } return bad } churn('h')";
        let again = "var r=[churn('h'), churn('j')]; var o={}; wr(o,'a',1); wr(o,'b',2); r.push(rd(o,'a'), rd(o,'b'), rd(o,'g5'), Object.keys(o).join()); r.join()";
        for mode in [Exec::Tree, Exec::Auto, Exec::Byte] {
            let mut it = realm(mode);
            assert_eq!(run(&mut it, fill), "0", "{mode:?}");
            if mode != Exec::Tree {
                assert!(
                    it.heap.keyed_entries() > 0,
                    "{mode:?}: the loop fills the table"
                );
            }
            let sweeps = it.heap.shape_sweeps();
            it.collect(&[]).unwrap();
            assert_eq!(
                it.heap.shape_sweeps(),
                sweeps + 1,
                "{mode:?}: the collect sweeps the shapes"
            );
            assert_eq!(
                it.heap.keyed_entries(),
                0,
                "{mode:?}: the sweep empties the table"
            );
            assert_eq!(run(&mut it, again), "0,0,1,2,,a,b", "{mode:?}");
        }
    });
}

/// Each heap has its own table. Two realms make objects of the same shape ids and keys, and only one of them has a setter on its
/// `Object.prototype`.
#[test]
fn two_realms_keep_their_own_keyed_entries() {
    big(|| {
        let src = "var log=[]; function wr(o,k,v){o[k]=v} function rd(o,k){return o[k]} \
                   function go(){ var r=[]; for (var n=0;n<4;n++) { var o={}; wr(o,'s',n); r.push(rd(o,'s'), rd(o,'p')); } return r.join() }";
        for mode in [Exec::Tree, Exec::Auto, Exec::Byte] {
            let (mut a, mut b) = (realm(mode), realm(mode));
            run(&mut a, src);
            run(&mut b, src);
            assert_eq!(run(&mut a, "go()"), "0,,1,,2,,3,", "{mode:?}");
            run(
                &mut b,
                "Object.defineProperty(Object.prototype,'s',{set(v){log.push(v)},get(){return 'S'},configurable:true}); Object.prototype.p='P'",
            );
            assert_eq!(
                run(&mut b, "go()+' '+log.join()"),
                "S,P,S,P,S,P,S,P 0,1,2,3",
                "{mode:?}"
            );
            assert_eq!(run(&mut a, "go()"), "0,,1,,2,,3,", "{mode:?}");
        }
    });
}
