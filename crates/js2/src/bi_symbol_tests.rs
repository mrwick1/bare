// Tests of `Symbol` (`bi_symbol.rs`): each case is a program, the global to read and the expected text. Each runs normally and under
// constant collection (the helpers of `script_tests.rs`).
use crate::script::{load, run};
use crate::heap::Trigger;

fn ev(src: &str, name: &str) -> String {
    match run(src) {
        Ok(mut vm) => vm.global_text(name),
        Err(e) => panic!("{e}\n--- source:\n{src}"),
    }
}

fn ev_gc(src: &str, name: &str) -> String {
    let mut vm = load(src).unwrap_or_else(|e| panic!("{e}\n--- source:\n{src}"));
    let k = if cfg!(miri) { 4 << 10 } else { 1 << 10 };
    vm.realm.heap.set_trigger(Trigger { min: k, slack: k });
    if let Err(e) = vm.run_main(0) {
        panic!("uncaught {e:?}\n--- source:\n{src}");
    }
    vm.global_text(name)
}

fn check(cases: &[(&str, &str, &str)]) {
    for (src, name, want) in cases {
        assert_eq!(ev(src, name), *want, "{src}");
        if !cfg!(miri) {
            assert_eq!(ev_gc(src, name), *want, "under collection: {src}");
        }
    }
}

#[test]
fn a_symbol_is_a_primitive_that_is_equal_to_itself_only() {
    check(&[
        ("var s = Symbol('a'); var r = typeof s + (s === s) + (s == s) + (s === Symbol('a')) + (s == Symbol('a')) + (s == 'a') + !!s;", "r", "symboltruetruefalsefalsefalsetrue"),
        ("var r = typeof Symbol() + typeof Object(Symbol()) + (Object(Symbol.iterator) == Symbol.iterator) + typeof Symbol;", "r", "symbolobjecttruefunction"),
        ("var s = Symbol('x'); var r = s.toString() + '|' + String(s) + '|' + s.description + '|' + Symbol().description + '|' + Symbol('').description + '|' + Symbol(undefined).description;", "r", "Symbol(x)|Symbol(x)|x|undefined||undefined"),
        ("var r = Symbol(1).description + Symbol(null).description + Symbol({toString() { return 'o'; }}).description;", "r", "1nullo"),
        ("var s = Symbol('z'); var r = (s.valueOf() === s) + ',' + (Object(s).valueOf() === s) + ',' + (s[Symbol.toPrimitive]() === s) + ',' + Object(s).description + ',' + Object(s).toString();", "r", "true,true,true,z,Symbol(z)"),
        ("var r = Object.getPrototypeOf(Symbol()) === Symbol.prototype; r = r + ',' + (Symbol.prototype.constructor === Symbol) + ',' + Symbol.prototype[Symbol.toStringTag] + ',' + Symbol.length + ',' + Symbol.name;", "r", "true,true,Symbol,0,Symbol"),
    ]);
}

#[test]
fn a_symbol_does_not_convert_to_a_string_or_a_number_by_itself() {
    check(&[
        ("var s = Symbol('q'); var r = ''; try { '' + s; } catch (e) { r += e.name; } try { `${s}`; } catch (e) { r += e.name; } try { +s; } catch (e) { r += e.name; } try { s * 2; } catch (e) { r += e.name; } try { s < 1; } catch (e) { r += e.name; } try { Number(s); } catch (e) { r += e.name; } try { Symbol(s); } catch (e) { r += e.name; }", "r", "TypeErrorTypeErrorTypeErrorTypeErrorTypeErrorTypeErrorTypeError"),
        ("var r = ''; try { new Symbol(); } catch (e) { r += e.name; } try { Symbol.prototype.toString.call('x'); } catch (e) { r += e.name; } try { Symbol.keyFor('x'); } catch (e) { r += e.name; } try { Symbol.prototype.valueOf.call({}); } catch (e) { r += e.name; }", "r", "TypeErrorTypeErrorTypeErrorTypeError"),
        ("var s = Symbol('d'); s.x = 1; var r = s.x + ',' + (s.foo === undefined) + ',' + (s instanceof Symbol);", "r", "undefined,true,false"),
    ]);
}

#[test]
fn a_symbol_is_a_property_key_that_no_string_equals() {
    check(&[
        ("var s = Symbol('a'), o = {}; o[s] = 1; o.a = 2; o['Symbol(a)'] = 3; var r = o[s] + ',' + o.a + ',' + o['Symbol(a)'] + ',' + (s in o) + ',' + o.hasOwnProperty(s) + ',' + delete o[s] + ',' + (s in o) + ',' + o.a;", "r", "1,2,3,true,true,true,false,2"),
        ("var s = Symbol('0'), o = {}; o[s] = 'x'; o[0] = 'y'; o['0'] = 'z'; var r = o[s] + o[0] + Object.keys(o).join();", "r", "xz0"),
        ("var s = Symbol('k'); var o = {[s]: 1, a: 2, get [Symbol.toStringTag]() { return 'T'; }}; var r = o[s] + ',' + Object.prototype.toString.call(o) + ',' + Object.keys(o).join();", "r", "1,[object T],a"),
        ("var s = Symbol('m'); class C { [s]() { return 7; } static [s] = 8; get [Symbol.toStringTag]() { return 'CC'; } } var r = new C()[s]() + ',' + C[s] + ',' + String(new C());", "r", "7,8,[object CC]"),
        ("var s = Symbol('f'); var o = {[s]() {}, get [Symbol.iterator]() { return 1; }, set [Symbol.species](v) {}}; var r = o[s].name + '|' + Object.getOwnPropertyDescriptor(o, Symbol.iterator).get.name;", "r", "[f]|get [Symbol.iterator]"),
    ]);
}

#[test]
fn the_keys_of_the_string_operations_skip_symbols() {
    check(&[
        ("var s = Symbol('s'), o = {b: 1, [s]: 2, a: 3}; var ks = []; for (var k in o) ks.push(k); var r = ks.join() + '|' + Object.entries(o).join(';') + '|' + Object.getOwnPropertySymbols(o).length + '|' + Object.getOwnPropertyNames(o) + '|' + Object.values(o);", "r", "b,a|b,1;a,3|1|b,a|1,3"),
        ("var s = Symbol('s'), t = Symbol('t'), o = {}; Object.defineProperty(o, t, {value: 1}); o[s] = 2; var a = Object.getOwnPropertySymbols(o); var r = a.length + ',' + (a[0] === t) + ',' + (a[1] === s) + ',' + o.propertyIsEnumerable(s) + ',' + o.propertyIsEnumerable(t);", "r", "2,true,true,true,false"),
        ("var r = Object.getOwnPropertySymbols('ab').length + ',' + Object.getOwnPropertySymbols([1]).length + ',' + Object.getOwnPropertySymbols(Symbol.prototype).length;", "r", "0,0,2"),
        ("var s = Symbol('s'), o = {[s]: 1, a: 2}; var c = Object.assign({}, o); var d = {...o}; var {a, ...rest} = o; var r = c[s] + ',' + d[s] + ',' + rest[s] + ',' + Object.keys(rest);", "r", "1,1,1,"),
        ("var s = Symbol('s'), o = {get a() { return 1; }, [s]: 2}; var c = Object.assign({}, o); var d = {...o}; var r = c[s] + ',' + d[s];", "r", "2,2"),
        ("var s = Symbol('s'), o = {[s]: 1}; var d = Object.getOwnPropertyDescriptors(o); var r = d[s].value + ',' + Object.getOwnPropertyDescriptor(o, s).enumerable; var p = Object.create(null, {[s]: {value: 5, enumerable: true}}); r += ',' + p[s];", "r", "1,true,5"),
        ("var s = Symbol('s'), o = {a: 1, b: s, [s]: 2}; var r = JSON.stringify(o) + '|' + JSON.stringify([s]) + '|' + JSON.stringify(s);", "r", "{\"a\":1}|[null]|undefined"),
        ("var s = Symbol('s'), o = {}; Object.defineProperty(o, s, {get() { return 9; }}); var r = o[s]; o = Object.freeze({[s]: 1}); r += ',' + Object.isFrozen(o);", "r", "9,true"),
    ]);
}

#[test]
fn the_registry_gives_one_symbol_for_a_key() {
    check(&[
        ("var a = Symbol.for('k'), b = Symbol.for('k'); var r = (a === b) + ',' + Symbol.keyFor(a) + ',' + a.description + ',' + (Symbol.for('j') === a) + ',' + Symbol.keyFor(Symbol('k')) + ',' + Symbol.keyFor(Symbol.iterator) + ',' + (Symbol.for() === Symbol.for('undefined')) + ',' + (Symbol.for('0') === Symbol.for(0));", "r", "true,k,k,false,undefined,undefined,true,true"),
        ("var a = Symbol.for('keep'); for (var i = 0; i < 40; i++) Symbol.for('x' + i); var big = []; for (i = 0; i < 300; i++) big.push({v: i}); var r = (Symbol.for('keep') === a) + ',' + Symbol.keyFor(a) + ',' + (Symbol.for('x39') === Symbol.for('x39'));", "r", "true,keep,true"),
    ]);
}

#[test]
fn the_well_known_symbols_are_values_with_their_descriptions() {
    check(&[
        ("var r = [Symbol.iterator, Symbol.asyncIterator, Symbol.species, Symbol.toStringTag, Symbol.hasInstance, Symbol.toPrimitive].map(function (s) { return s.description + ':' + typeof s; }).join();", "r", "Symbol.iterator:symbol,Symbol.asyncIterator:symbol,Symbol.species:symbol,Symbol.toStringTag:symbol,Symbol.hasInstance:symbol,Symbol.toPrimitive:symbol"),
        ("var d = Object.getOwnPropertyDescriptor(Symbol, 'iterator'); var r = d.writable + ',' + d.enumerable + ',' + d.configurable + ',' + (Symbol.iterator === Symbol.iterator);", "r", "false,false,false,true"),
        ("var r = Object.getOwnPropertyNames(Symbol).sort().join();", "r", "asyncIterator,for,hasInstance,iterator,keyFor,length,match,matchAll,name,prototype,replace,search,species,split,toPrimitive,toStringTag"),
        ("var d = Object.getOwnPropertyDescriptor(Symbol.prototype, 'description'); var r = typeof d.get + ',' + d.set + ',' + d.enumerable + ',' + d.configurable;", "r", "function,undefined,false,true"),
    ]);
}

#[test]
fn the_hooks_read_the_well_known_symbols() {
    check(&[
        ("var p = {[Symbol.toPrimitive](h) { return h; }}; var r = (+{[Symbol.toPrimitive](h) { return h === 'number' ? 5 : 0; }}) + ',' + `${p}` + ',' + (p + '') + ',' + (p == 'default') + ',' + ({})[p];", "r", "5,string,default,true,undefined"),
        ("var o = {[Symbol.toPrimitive]: 1}; var r = ''; try { +o; } catch (e) { r += e.name; } o = {[Symbol.toPrimitive]() { return {}; }}; try { +o; } catch (e) { r += e.name; } o = {[Symbol.toPrimitive]: undefined, valueOf() { return 3; }}; r += (+o);", "r", "TypeErrorTypeError3"),
        ("function F() {} var r = (new F() instanceof F) + ',' + (Function.prototype[Symbol.hasInstance].call(F, new F())) + ',' + Function.prototype[Symbol.hasInstance].name + ',' + (Function.prototype[Symbol.hasInstance].call({}, {}));", "r", "true,true,[Symbol.hasInstance],false"),
        ("var E = {[Symbol.hasInstance](v) { return v === 1; }}; var r = (1 instanceof E) + ',' + (2 instanceof E); try { 1 instanceof {}; } catch (e) { r += ',' + e.name; } try { 1 instanceof 1; } catch (e) { r += ',' + e.name; }", "r", "true,false,TypeError,TypeError"),
        ("class A { static [Symbol.hasInstance]() { return true; } } var r = ({} instanceof A) + ',' + (1 instanceof A) + ',' + (Object.create(null) instanceof A);", "r", "true,true,true"),
        ("var r = Object.prototype.toString.call(Symbol()) + Object.prototype.toString.call(Object(Symbol())) + Object.prototype.toString.call(1) + Object.prototype.toString.call(null) + Object.prototype.toString.call({[Symbol.toStringTag]: 5});", "r", "[object Symbol][object Symbol][object Number][object Null][object Object]"),
    ]);
}

#[test]
fn symbols_survive_collection_as_values_keys_and_registry_entries() {
    check(&[
        ("var syms = []; for (var i = 0; i < 60; i++) syms.push(Symbol('s' + i)); var o = {}; for (i = 0; i < 60; i += 2) o[syms[i]] = i; var junk = []; for (i = 0; i < 300; i++) junk.push([i]); var r = Object.getOwnPropertySymbols(o).length + ',' + o[syms[58]] + ',' + syms[59].description;", "r", "30,58,s59"),
    ]);
}

#[test]
fn the_prototype_of_a_symbol_has_its_members_however_it_is_reached() {
    check(&[
        // The members are made at the first read of `Symbol` or `Symbol.prototype`; no other way to the prototype may miss them.
        ("var o = Object.create(Object.getPrototypeOf(Symbol('p'))); var r = o.toString === Symbol.prototype.toString;", "r", "true"),
        ("var o = {}; Object.setPrototypeOf(o, Object.getPrototypeOf(Symbol('p'))); var r = typeof o.valueOf + typeof o.constructor;", "r", "functionfunction"),
        ("var r = Object(Symbol('w')).description + Object.getPrototypeOf(Object(Symbol())) .constructor.name;", "r", "wSymbol"),
        ("var r = Symbol('a').description + Object.keys(Symbol).length + Object.getOwnPropertyNames(Symbol.prototype).sort();", "r", "a0constructor,description,toString,valueOf"),
        ("var t = Object.assign(Symbol('t'), {x: 1}); var r = typeof t + t.x + (t.description);", "r", "object1t"),
    ]);
}

#[test]
fn instanceof_follows_a_bound_function_to_its_target() {
    check(&[
        ("function F() {} var B = F.bind(null); var r = (new F() instanceof B) + ',' + ({} instanceof B);", "r", "true,false"),
        ("var F = ({ m() {} }).m; Object.defineProperty(F, 'prototype', {get() { throw new RangeError('x'); }}); var r = ''; try { ({}) instanceof F; } catch (e) { r = e.name; } try { 1 instanceof F; r += ',ok'; } catch (e) { r += ',' + e.name; }", "r", "RangeError,ok"),
    ]);
}
