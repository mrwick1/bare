//! The bounded font model stores descriptors but does not fetch or decode font data.
//! The source runs only after a font API name is read. Private weak maps provide brands.
pub(crate) const SOURCE: &str = r#"
(function () {
'use strict';
const weakHas = Function.prototype.call.bind(WeakMap.prototype.has);
const weakGet = Function.prototype.call.bind(WeakMap.prototype.get);
const weakSet = Function.prototype.call.bind(WeakMap.prototype.set);
const invoke = Reflect.apply;
const faces = new WeakMap();
const resolve = Promise.resolve.bind(Promise);
const reject = Promise.reject.bind(Promise);
const descriptors = {style:'normal',weight:'normal',stretch:'normal',width:'normal',unicodeRange:'U+0-10FFFF',variant:'normal',featureSettings:'normal',variationSettings:'normal',display:'auto',ascentOverride:'normal',descentOverride:'normal',lineGapOverride:'normal',sizeAdjust:'100%'};
function face(value) {
    if (!weakHas(faces,value)) throw new TypeError('Expected a FontFace');
    return weakGet(faces,value);
}
class FontFace {
    constructor(family, source, options) {
        if (arguments.length < 2) throw new TypeError('FontFace requires family and source');
        this.family = '' + family;
        this.source = '' + source;
        if (options != null && typeof options !== 'object' && typeof options !== 'function') throw new TypeError('Expected descriptors');
        for (const key of Object.keys(descriptors)) {
            const value = options == null ? undefined : options[key];
            this[key] = value === undefined ? descriptors[key] : '' + value;
        }
        let settle;
        const loaded = new Promise(function(resolve) { settle = resolve; });
        weakSet(faces,this, {status:'unloaded', loaded:loaded, settle:settle});
    }
    get status() { return face(this).status; }
    get loaded() { return face(this).loaded; }
    load() {
        const state = face(this);
        state.status = 'loaded';
        state.settle(this);
        return state.loaded;
    }
}
Object.defineProperty(globalThis, 'FontFace', {value:FontFace,writable:true,configurable:true});
"#;
