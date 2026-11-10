//! Checked font API behavior, including private brands and live Set iteration.
use crate::browser_fetch_tests::{big, js, mk};
use crate::browser_fetch::FetchLimits;

#[test]
fn fonts_load_and_identities() {
    big(|| {
        let mut b=mk(None,FetchLimits::default());
        assert_eq!(js(&mut b,"var done='pending'; (async()=>{var s=document.fonts; var r=await s.load('normal 1px \"Material Symbols Outlined\"');done=String(r.length)+','+String(await s.ready===s)})();0"),"0");
        b.drain().unwrap();
        assert_eq!(js(&mut b,"done"),"0,true");
        assert_eq!(js(&mut b,"document.fonts===document.fonts && document.fonts.ready===document.fonts.ready"),"true");
        assert_eq!(js(&mut b,"document.fonts.status+','+document.fonts.check('12px Missing')"),"loaded,true");
    });
}
#[test]
fn faces_and_membership() {
    big(|| {
        let mut b=mk(None,FetchLimits::default());
        assert_eq!(js(&mut b,"var s=document.fonts,f=new FontFace('One','local(One)',{weight:700}),g=new FontFace('Two','url(no-fetch)'); f.status+','+f.family+','+f.weight"),"unloaded,One,700");
        assert_eq!(js(&mut b,"s.add(f)===s && s.add(f)===s && s.size===1 && s.has(f) && !s.has(g)"),"true");
        assert_eq!(js(&mut b,"s.delete(g)+','+s.delete(f)+','+s.delete(f)+','+s.size"),"false,true,false,0");
        assert_eq!(js(&mut b,"s.add(f);var a=[];var ctx={};s.forEach(function(v,k,t){a.push(v===k && t===s && this===ctx);if(v===f)s.add(g)},ctx);a.join()"),"true,true");
        assert_eq!(js(&mut b,"var names=[];s.forEach(function(v){names.push(v.family);if(v===f){s.clear();s.add(g)}});names.join()"),"One,Two");
        assert_eq!(js(&mut b,"s.clear()===undefined && s.size===0"),"true");
        assert_eq!(js(&mut b,"var p=f.loaded,loadedFace;p.then(function(v){loadedFace=v});f.load()===p && f.loaded===p && f.status==='loaded'"),"true");
        assert_eq!(js(&mut b,"s.add(f);var result='';s.load('12px One').then(function(a){result=a.length+','+(a[0]===f)});0"),"0");
        b.drain().unwrap();
        assert_eq!(js(&mut b,"result"),"1,true");
        assert_eq!(js(&mut b,"loadedFace===f && f.load()===p && f.source==='local(One)'"),"true");
    });
}
#[test]
fn fonts_validate_members_receivers_and_conversions() {
    big(|| {
        let mut b=mk(None,FetchLimits::default());
        assert_eq!(js(&mut b,"var s=document.fonts,n=0;for(var value of [null,undefined,1,'x',{},Object.create(FontFace.prototype)]){for(var name of ['add','delete','has']){try{s[name](value)}catch(e){if(e instanceof TypeError)n++}}}n"),"18");
        assert_eq!(js(&mut b,"var n=0;for(var name of ['add','delete','has','clear','forEach','load','check']){try{s[name].call({})}catch(e){if(e instanceof TypeError)n++}}n"),"7");
        assert_eq!(js(&mut b,"var n=0;for(var args of [[],['One']]){try{Reflect.construct(FontFace,args)}catch(e){if(e instanceof TypeError)n++}}try{s.forEach(1)}catch(e){if(e instanceof TypeError)n++}n"),"3");
        assert_eq!(js(&mut b,"var calls=[];s.check({toString(){calls.push('font');return '12px One'}},{toString(){calls.push('text');return 'x'}});calls.join()"),"font,text");
        assert_eq!(js(&mut b,"var caught='';s.load('bad').catch(function(e){caught=e.name});0"),"0");
        b.drain().unwrap();
        assert_eq!(js(&mut b,"caught"),"SyntaxError");
        assert_eq!(js(&mut b,"var n=0;try{FontFace.prototype.load.call({})}catch(e){if(e instanceof TypeError)n++}try{s.check(Symbol())}catch(e){if(e instanceof TypeError)n++}n"),"2");
        assert_eq!(js(&mut b,"var n=0;for(var args of [[Symbol(),'local(x)'],['x',Symbol()],['x','local(x)',{weight:Symbol()}]]){try{Reflect.construct(FontFace,args)}catch(e){if(e instanceof TypeError)n++}}n"),"3");
        assert_eq!(js(&mut b,"var n=0;try{Object.getOwnPropertyDescriptor(Document.prototype,'fonts').get.call({})}catch(e){if(e instanceof TypeError)n++}try{Object.getOwnPropertyDescriptor(FontFace.prototype,'loaded').get.call({})}catch(e){if(e instanceof TypeError)n++}n"),"2");
        assert_eq!(js(&mut b,"Object.getOwnPropertyNames(globalThis).includes('FontFace') && Object.getOwnPropertyNames(Document.prototype).includes('fonts')"),"true");
        assert_eq!(js(&mut b,"var otherDoc=document.implementation.createHTMLDocument('Other');otherDoc.fonts!==s && otherDoc.fonts===otherDoc.fonts && otherDoc.fonts.ready===otherDoc.fonts.ready && otherDoc.fonts.size===0"),"true");
        assert_eq!(js(&mut b,"var localFace=new FontFace('Local','local(Local)');otherDoc.fonts.add(localFace);otherDoc.fonts.size+','+s.size"),"1,0");
        let mut other=mk(None,FetchLimits::default());
        assert_eq!(js(&mut other,"document.fonts.size"),"0");
    });
}
#[test]
fn font_state_is_absent_until_used_and_obeys_collection_limits() {
    big(|| {
        let mut b=mk(None,FetchLimits::default());
        let d=b.dom.clone();
        let fonts=d.key(&mut b.it,"fonts").unwrap();
        let face=d.key(&mut b.it,"FontFace").unwrap();
        assert!(b.it.heap.peek_own_property(d.p().document,fonts).unwrap().is_none());
        assert!(b.it.heap.peek_own_property(b.it.global_object(),face).unwrap().is_none());
        js(&mut b,"document.body;document.title");
        assert!(b.it.heap.peek_own_property(d.p().document,fonts).unwrap().is_none());
        assert!(b.it.heap.peek_own_property(b.it.global_object(),face).unwrap().is_none());
        assert_eq!(js(&mut b,"var s=document.fonts,f=new FontFace('One','local(One)'),g=new FontFace('Two','local(Two)');s.add(f);s.size"),"1");
        b.it.set_sc_limits(bare_js::eval::ScLimits {max_entries:1,..Default::default()});
        assert!(js(&mut b,"s.add(g)").starts_with("limit:"));
        assert_eq!(js(&mut b,"s.has(g)+','+s.size"),"false,1");
    });
}
#[test]
fn fonts_keep_private_brands_when_collection_prototypes_change() {
    big(|| {
        let mut b=mk(None,FetchLimits::default());
        assert_eq!(js(&mut b,"var s=document.fonts,f=new FontFace('One','local(One)');WeakMap.prototype.has=function(){return true};WeakMap.prototype.get=function(){return {members:new Set()}};Set.prototype.add=function(){throw new Error('patched')};var n=0;try{s.add({})}catch(e){if(e instanceof TypeError)n++}n+','+(s.add(f)===s)+','+s.has(f)"),"1,true,true");
    });
}
