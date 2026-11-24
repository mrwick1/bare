//! Structured clone: every class that the walk knows, the errors it throws, the transfer list and the caps (`docs/design-host-clone.md`). Each case prints the clone as text (`dump`). The expected text is the output of the old browser walk, frozen at `55594ea6`.

use crate::browser::*;
use bare_html::tree;
use crate::eng_old::OldFault as Error;
use crate::js_limits::EvalLimits;
use crate::js_limits::HeapLimits;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn browser() -> Browser {
    let page = tree::parse("<!doctype html><html><head></head><body><div id=d></div></body></html>");
    let mut b = Browser::new(page, BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.set_document_url("https://a.test/").expect("url");
    b
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) => format!("throw:{}", b.it.display(v)),
        Err(e) => format!("other:{e:?}"),
    }
}

/// `dump(v)`: the class, the prototype, the data and the own properties of `v` (cycles as `#n`). `run(name, f)` adds a line: the dump of
/// `f()`, or `ERR:name:isDOMException`.
const DUMP: &str = r#"
function dump(v, seen) {
  seen = seen || [];
  var t = typeof v;
  if (t === 'number') return Object.is(v, -0) ? '-0' : String(v);
  if (t === 'bigint') return v + 'n';
  if (t === 'string') return JSON.stringify(v);
  if (t !== 'object' || v === null) return String(v);
  var i = seen.indexOf(v);
  if (i >= 0) return '#' + i;
  seen.push(v);
  var tag = Object.prototype.toString.call(v).slice(8, -1);
  var p = Object.getPrototypeOf(v);
  var out = tag + '/' + (p === Object.prototype ? 'O' : p === Array.prototype ? 'A' : p === null ? 'null' : p.constructor.name);
  if (v instanceof Date) out += '<' + v.getTime() + '>';
  else if (v instanceof RegExp) out += '<' + v.source + '/' + v.flags + '/' + v.lastIndex + '>';
  else if (v instanceof Map) out += '<' + Array.from(v).map(function (e) { return dump(e[0], seen) + '=>' + dump(e[1], seen); }).join(',') + '>';
  else if (v instanceof Set) out += '<' + Array.from(v).map(function (e) { return dump(e, seen); }).join(',') + '>';
  else if (v instanceof ArrayBuffer) out += '<' + Array.from(new Uint8Array(v)).join(',') + '>';
  else if (ArrayBuffer.isView(v)) out += '<' + v.constructor.name + ',' + v.byteOffset + ',' + v.byteLength + ',' + dump(v.buffer, seen) + '>';
  else if (tag === 'Number' || tag === 'String' || tag === 'Boolean' || tag === 'BigInt') out += '<' + dump(v.valueOf(), seen) + '>';
  var names = Object.getOwnPropertyNames(v);
  if (ArrayBuffer.isView(v)) names = names.filter(function (k) { return isNaN(k); });
  out += '{' + names.map(function (k) {
    var d = Object.getOwnPropertyDescriptor(v, k);
    return k + (d.enumerable ? '' : '~') + ':' + ('value' in d ? (k === 'stack' ? 'stack:' + typeof d.value : dump(d.value, seen)) : 'accessor');
  }).join(',') + '}';
  return out;
}
var lines = [];
function run(name, f) {
  var r;
  try { r = dump(f()); } catch (e) { r = 'ERR:' + e.name + ':' + (e instanceof DOMException); }
  lines.push(name + ' = ' + r);
}
function sc(v, o) { return structuredClone(v, o); }
"#;

const CASES: &str = r#"
run('primitives', function () { return [undefined, null, true, 1.5, -0, NaN, Infinity, 'héllo', 'lone\ud800x', '', 12345678901234567890n, -5n, 0n, 2n ** 200n].map(function (x) { return sc(x); }); });
run('sparse', function () { var a = [1, , 3]; return sc(a); });
run('array extra', function () { var a = []; a[5] = 1; a.x = 'p'; a.length = 9; return sc(a); });
run('keys', function () { var s = Symbol('s'), o = { b: 1, 2: 'two', a: 2, 1: 'one', '01': 'z', '4294967295': 'big', '4294967294': 'max' }; o[s] = 9; Object.defineProperty(o, 'hid', { value: 1, enumerable: false }); return sc(o); });
run('getter', function () { var n = 0; return sc({ get g() { return ++n; }, h: 1 }); });
run('getter deletes', function () { return sc({ get a() { delete this.b; return 1; }, b: 2, c: 3 }); });
run('getter throws', function () { return sc({ get a() { throw new RangeError('x'); } }); });
run('accessor key', function () { var o = {}; Object.defineProperty(o, 'k', { get: function () { return 1; }, enumerable: true }); return sc(o); });
run('class instance', function () { function A() { this.v = 1; } A.prototype.m = function () {}; return sc(new A()); });
run('null proto', function () { var o = Object.create(null); o.q = 1; return sc(o); });
run('cycle', function () { var o = { n: 1 }; o.self = o; o.a = [o, { o: o }]; return sc(o); });
run('shared', function () { var s = { z: 1 }; return sc([s, s, { s: s }]); });
run('date', function () { return sc([new Date(1e12), new Date(NaN), new Date(-1)]); });
run('regexp', function () { var r = /ab+c/gi; r.lastIndex = 3; return sc([r, /\u{1F600}/u, /x/ymsd]); });
run('map', function () { var k = {}; var m = new Map([[1, { a: 1 }], [k, k], ['s', new Set([1, 'a'])]]); m.set(m, m); return sc(m); });
run('set', function () { var s = new Set([NaN, -0, 'a', { x: 1 }]); return sc(s); });
run('error', function () { return sc(new RangeError('boom', { cause: { x: 1 } })); });
run('error types', function () { return [new Error('a'), new TypeError('b'), new RangeError('c'), new ReferenceError('d'), new SyntaxError('e'), new URIError('f'), new EvalError('g')].map(function (x) { return sc(x); }); });
run('error custom', function () { var e = new Error('m'); e.name = 'Custom'; e.extra = 5; return sc(e); });
run('error subclass', function () { function E2(m) { this.message = m; } E2.prototype = Object.create(TypeError.prototype); E2.prototype.name = 'E2'; return sc(new E2('hi')); });
run('error accessor', function () { var e = new Error('m'); Object.defineProperty(e, 'message', { get: function () { return 'g'; } }); return sc(e); });
run('error odd', function () { var e = new Error('m'); e.name = 7; Object.defineProperty(e, 'message', { value: 12, writable: true, configurable: true }); e.stack = 5; return sc(e); });
run('error no message', function () { var e = new Error(); return sc(e); });
run('wrappers', function () { return sc([Object(1n), new Number(3), new String('ab'), new Boolean(false), Object(-0)]); });
run('symbol wrapper', function () { return sc(Object(Symbol('s'))); });
run('buffer', function () { return sc(new Uint8Array([1, 2, 3]).buffer); });
run('views', function () { var b = new ArrayBuffer(16); var a = new Int16Array(b, 2, 4), c = new Uint8Array(b, 8, 4); a[0] = 258; c[1] = 9; var r = sc([a, c, new Float64Array([1.5, -2]), new BigInt64Array([-3n]), new DataView(b, 2, 6), new Uint8ClampedArray([300, -4]), new Float32Array([0.5])]); return [r, r[0].buffer === r[1].buffer, r[0].buffer === r[4].buffer]; });
run('view alone', function () { return sc(new Uint8Array(new ArrayBuffer(8), 3, 2)); });
run('detached buffer', function () { var b = new ArrayBuffer(4); b.transfer(); return sc(b); });
run('detached view', function () { var b = new ArrayBuffer(4), v = new Uint8Array(b); b.transfer(); return sc(v); });
run('function', function () { return sc({ f: function () {} }); });
run('symbol', function () { return sc(Symbol('s')); });
run('bound', function () { return sc((function () {}).bind(null)); });
run('weakmap', function () { return sc(new WeakMap()); });
run('weakref', function () { return sc(new WeakRef({})); });
run('promise', function () { return sc(Promise.resolve(1)); });
run('proxy', function () { return sc(new Proxy({}, {})); });
run('generator', function () { return sc((function* () {})()); });
run('iterator', function () { return sc([1][Symbol.iterator]()); });
run('intl', function () { return sc(new Intl.NumberFormat()); });
run('dom node', function () { return sc(document.body); });
run('dom in graph', function () { return sc({ a: [document.getElementById('d')] }); });
run('document', function () { return sc(document); });
run('window', function () { return sc(window); });
run('url', function () { return sc(new URL('http://a/')); });
run('params', function () { return sc(new URLSearchParams('a=1')); });
run('abort', function () { return sc(new AbortController()); });
run('signal', function () { return sc(new AbortController().signal); });
run('location', function () { return sc(location); });
run('storage', function () { return sc(localStorage); });
run('event', function () { return sc(new Event('x')); });
run('textencoder', function () { return sc(new TextEncoder()); });
run('transfer buffer', function () { var b = new Uint8Array([1, 2, 3]).buffer; var c = sc(b, { transfer: [b] }); return [c, b.byteLength, b.detached]; });
run('transfer in graph', function () { var b = new ArrayBuffer(4), v = new Uint8Array(b, 1, 2); v[0] = 7; var c = sc({ b: b, v: v }, { transfer: [b] }); return [c, c.b === c.v.buffer, b.byteLength]; });
run('transfer unused', function () { var b = new ArrayBuffer(4), u = new ArrayBuffer(8); var c = sc({ b: b }, { transfer: [b, u] }); return [c, u.byteLength]; });
run('transfer twice', function () { var b = new ArrayBuffer(4); return sc(b, { transfer: [b, b] }); });
run('transfer detached', function () { var b = new ArrayBuffer(4); b.transfer(); return sc(1, { transfer: [b] }); });
run('transfer getter detaches', function () { var b = new ArrayBuffer(4); return sc({ get a() { b.transfer(); return 1; } }, { transfer: [b] }); });
run('transfer bad', function () { return sc(1, { transfer: [{}] }); });
run('transfer port', function () { var c = new MessageChannel(); var r = sc({ p: c.port1 }, { transfer: [c.port1] }); return [Object.prototype.toString.call(r.p), r.p !== c.port1]; });
run('history state', function () { var s = { m: new Map([[1, [2]]]), d: new Date(5), self: null }; s.self = s; history.replaceState(s, ''); var a = history.state; return [a, a === history.state, a.self === a]; });
run('push state', function () { history.pushState(new Set([1, { a: /x/g }]), '', '/p'); var a = history.state; return [a, history.length > 1]; });
run('push function', function () { history.pushState({ f: function () {} }, ''); });
run('push dom', function () { history.pushState({ n: document.body }, ''); });
run('deep ok', function () { var a = []; for (var i = 0, p = a; i < 100; i++) { var q = []; p.push(q); p = q; } return JSON.stringify(sc(a)).length; });
lines.join('\n');
"#;

/// The expected text, one line for each `run`.
const EXPECT: &str = r##"primitives = Array/A{0:undefined,1:null,2:true,3:1.5,4:-0,5:NaN,6:Infinity,7:"héllo",8:"lone\ud800x",9:"",10:12345678901234567890n,11:-5n,12:0n,13:1606938044258990275541962092341162602522202993782792835301376n,length~:14}
sparse = Array/A{0:1,2:3,length~:3}
array extra = Array/A{5:1,length~:9,x:"p"}
keys = Object/O{1:"one",2:"two",4294967294:"max",b:1,a:2,01:"z",4294967295:"big"}
getter = Object/O{g:1,h:1}
getter deletes = Object/O{a:1,c:3}
getter throws = ERR:RangeError:false
accessor key = Object/O{k:1}
class instance = Object/O{v:1}
null proto = Object/O{q:1}
cycle = Object/O{n:1,self:#0,a:Array/A{0:#0,1:Object/O{o:#0},length~:2}}
shared = Array/A{0:Object/O{z:1},1:#1,2:Object/O{s:#1},length~:3}
date = Array/A{0:Date/Date<1000000000000>{},1:Date/Date<NaN>{},2:Date/Date<-1>{},length~:3}
regexp = Array/A{0:RegExp/RegExp<ab+c/gi/0>{lastIndex~:0},1:RegExp/RegExp<\u{1F600}/u/0>{lastIndex~:0},2:RegExp/RegExp<x/dmsy/0>{lastIndex~:0},length~:3}
map = Map/Map<1=>Object/O{a:1},Object/O{}=>#2,"s"=>Set/Set<1,"a">{},#0=>#0>{}
set = Set/Set<NaN,0,"a",Object/O{x:1}>{}
error = Error/RangeError{message~:"boom",stack~:stack:string,cause~:Object/O{x:1}}
error types = Array/A{0:Error/Error{message~:"a",stack~:stack:string},1:Error/TypeError{message~:"b",stack~:stack:string},2:Error/RangeError{message~:"c",stack~:stack:string},3:Error/ReferenceError{message~:"d",stack~:stack:string},4:Error/SyntaxError{message~:"e",stack~:stack:string},5:Error/URIError{message~:"f",stack~:stack:string},6:Error/EvalError{message~:"g",stack~:stack:string},length~:7}
error custom = Error/Error{message~:"m",stack~:stack:string}
error subclass = Object/Error{message~:"hi"}
error accessor = Error/Error{stack~:stack:string}
error odd = Object/Error{message~:"12"}
error no message = Error/Error{stack~:stack:string}
wrappers = Array/A{0:BigInt/BigInt<1n>{},1:Number/Number<3>{},2:String/String<"ab">{0:"a",1:"b",length~:2},3:Boolean/Boolean<false>{},4:Number/Number<-0>{},length~:5}
symbol wrapper = ERR:DataCloneError:true
buffer = ArrayBuffer/ArrayBuffer<1,2,3>{}
views = Array/A{0:Array/A{0:Int16Array/Int16Array<Int16Array,2,8,ArrayBuffer/ArrayBuffer<0,0,2,1,0,0,0,0,0,9,0,0,0,0,0,0>{}>{},1:Uint8Array/Uint8Array<Uint8Array,8,4,#3>{},2:Float64Array/Float64Array<Float64Array,0,16,ArrayBuffer/ArrayBuffer<0,0,0,0,0,0,248,63,0,0,0,0,0,0,0,192>{}>{},3:BigInt64Array/BigInt64Array<BigInt64Array,0,8,ArrayBuffer/ArrayBuffer<253,255,255,255,255,255,255,255>{}>{},4:DataView/DataView<DataView,2,6,#3>{},5:Uint8ClampedArray/Uint8ClampedArray<Uint8ClampedArray,0,2,ArrayBuffer/ArrayBuffer<255,0>{}>{},6:Float32Array/Float32Array<Float32Array,0,4,ArrayBuffer/ArrayBuffer<0,0,0,63>{}>{},length~:7},1:true,2:true,length~:3}
view alone = Uint8Array/Uint8Array<Uint8Array,3,2,ArrayBuffer/ArrayBuffer<0,0,0,0,0,0,0,0>{}>{}
detached buffer = ERR:DataCloneError:true
detached view = ERR:DataCloneError:true
function = ERR:DataCloneError:true
symbol = ERR:DataCloneError:true
bound = ERR:DataCloneError:true
weakmap = ERR:DataCloneError:true
weakref = ERR:DataCloneError:true
promise = ERR:DataCloneError:true
proxy = ERR:DataCloneError:true
generator = ERR:DataCloneError:true
iterator = ERR:DataCloneError:true
intl = ERR:DataCloneError:true
dom node = ERR:DataCloneError:true
dom in graph = ERR:DataCloneError:true
document = ERR:DataCloneError:true
window = ERR:DataCloneError:true
url = ERR:DataCloneError:true
params = ERR:DataCloneError:true
abort = ERR:DataCloneError:true
signal = ERR:DataCloneError:true
location = ERR:DataCloneError:true
storage = ERR:DataCloneError:true
event = ERR:DataCloneError:true
textencoder = ERR:DataCloneError:true
transfer buffer = Array/A{0:ArrayBuffer/ArrayBuffer<1,2,3>{},1:0,2:true,length~:3}
transfer in graph = Array/A{0:Object/O{b:ArrayBuffer/ArrayBuffer<0,7,0,0>{},v:Uint8Array/Uint8Array<Uint8Array,1,2,#2>{}},1:true,2:0,length~:3}
transfer unused = Array/A{0:Object/O{b:ArrayBuffer/ArrayBuffer<0,0,0,0>{}},1:0,length~:2}
transfer twice = ERR:DataCloneError:true
transfer detached = ERR:DataCloneError:true
transfer getter detaches = ERR:DataCloneError:true
transfer bad = ERR:DataCloneError:true
transfer port = Array/A{0:"[object EventTarget]",1:true,length~:2}
history state = Array/A{0:Object/O{m:Map/Map<1=>Array/A{0:2,length~:1}>{},d:Date/Date<5>{},self:#1},1:true,2:true,length~:3}
push state = Array/A{0:Set/Set<1,Object/O{a:RegExp/RegExp<x/g/0>{lastIndex~:0}}>{},1:true,length~:2}
push function = ERR:DataCloneError:true
push dom = ERR:DataCloneError:true
deep ok = 202"##;

#[test]
fn every_class_and_error_of_the_walk() {
    big(|| {
        let mut b = browser();
        js(&mut b, DUMP);
        let out = js(&mut b, CASES);
        if std::env::var("CLONE_DUMP").is_ok() {
            println!("{out}");
        }
        assert_eq!(out, EXPECT);
    });
}

/// The caps are not catchable errors: a depth over 256 and a size over 64 MiB stop the script with a `Limit`.
#[test]
fn the_caps_of_structured_clone_stop_the_script() {
    big(|| {
        let mut b = browser();
        let r = js(&mut b, "var a = []; for (var i = 0, p = a; i < 300; i++) { var q = []; p.push(q); p = q; } try { structuredClone(a); 'caught' } catch (e) { 'catch:' + e.name }");
        assert_eq!(r, "other:Limit(Host(\"clone-depth\"))", "{r}");
        // 2^20 small objects are over the byte cap (64 MiB) before the node cap.
        let r = js(&mut b, "var big = new Array(1 << 20); for (var i = 0; i < big.length; i++) big[i] = {}; try { structuredClone(big); 'caught' } catch (e) { 'catch:' + e.name }");
        assert_eq!(r, "other:Limit(Host(\"clone-bytes\"))", "{r}");
    });
}
