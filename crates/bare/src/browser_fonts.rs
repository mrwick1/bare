//! Lazy document font sets. Native Set storage supplies bounds and live iteration.
use crate::browser::*;
use bare_js::eval::Interp;
use bare_js::value::Value;

#[cold]
pub(crate) fn define(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    let key = d.key(it, "fonts")?;
    if it.heap.peek_own_property(d.p().document, key).map_err(|e| it.heap_error(e))?.is_some() {
        return Ok(());
    }
    let source = format!("{}{}", crate::browser_fontface::SOURCE, SOURCE);
    let s = it.heap.intern_str(&source).map_err(|e| it.heap_error(e))?;
    it.eval_global(Value::String(s))?;
    Ok(())
}

const SOURCE: &str = r#"
const sets = new WeakMap();
const documentOwner = document;
let documentSet;
function set(value) {
    if (!sets.has(value)) throw new TypeError('Expected a FontFaceSet');
    return sets.get(value);
}
function query(font, text) {
    font = '' + font;
    if (text !== undefined) text = '' + text;
    const match = /(?:^|\s)(?:\d+(?:\.\d+)?)(?:px|pt|em|rem|%)(?:\s*\/\s*[^\s]+)?\s+(.+)$/.exec(font);
    if (!match) throw new SyntaxError('Invalid font shorthand');
    return match[1].split(',').map(function(name) {return name.trim().replace(/^["']|["']$/g,'').toLowerCase();});
}
class FontFaceSet {
    constructor() {
        const members = new Set();
        sets.set(this, {members:members,ready:resolve(this)});
    }
    get size() { return set(this).members.size; }
    get status() { set(this); return 'loaded'; }
    get ready() { return set(this).ready; }
    add(value) { const s=set(this);face(value);s.members.add(value);return this; }
    delete(value) { const s=set(this);face(value);return s.members.delete(value); }
    has(value) { const s=set(this);face(value);return s.members.has(value); }
    clear() { set(this).members.clear(); }
    forEach(callback, thisArg) {
        const s=set(this), owner=this;
        if (typeof callback !== 'function') throw new TypeError('Expected callback');
        s.members.forEach(function(value) { callback.call(thisArg,value,value,owner); });
    }
    load(font, text) {
        const s=set(this);
        try {
            if (!arguments.length) throw new TypeError('load requires font');
            const names=query(font,text), result=[];
            s.members.forEach(function(value) {
                if (names.includes(('' + value.family).replace(/^["']|["']$/g,'').toLowerCase())) { const state=face(value);state.status='loaded';state.settle(value);result.push(value); }
            });
            return resolve(result);
        } catch (error) { return Promise.reject(error); }
    }
    check(font, text) {
        set(this);
        if (!arguments.length) throw new TypeError('check requires font');
        query(font,text);
        return true;
    }
}
Object.defineProperty(Document.prototype, 'fonts', {configurable:true,enumerable:true,get:function() {
    if (this !== documentOwner) throw new TypeError('Expected document');
    if (documentSet === undefined) documentSet = new FontFaceSet();
    return documentSet;
}});
})();
"#;
