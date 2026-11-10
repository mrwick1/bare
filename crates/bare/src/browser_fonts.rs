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
    let factory = it.eval_global(Value::String(s))?;
    let validate = d.add_fn(it, is_document, "isDocument", 1, false)?;
    it.call(factory, Value::Undefined, &[Value::Object(validate)])?;
    Ok(())
}

fn is_document(d: &Dom, _: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    Ok(Value::Bool(d.node_of(a.first().copied().unwrap_or(Value::Undefined)).is_some_and(|n| matches!(d.doc.borrow().data(n), bare_html::dom::NodeData::Document))))
}

const SOURCE: &str = r#"
const sets = new WeakMap();
const add = Function.prototype.call.bind(Set.prototype.add);
const remove = Function.prototype.call.bind(Set.prototype.delete);
const has = Function.prototype.call.bind(Set.prototype.has);
const clear = Function.prototype.call.bind(Set.prototype.clear);
const each = Function.prototype.call.bind(Set.prototype.forEach);
const size = Function.prototype.call.bind(Object.getOwnPropertyDescriptor(Set.prototype,'size').get);
const documents = new WeakMap();
function set(value) {
    if (!weakHas(sets,value)) throw new TypeError('Expected a FontFaceSet');
    return weakGet(sets,value);
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
        weakSet(sets,this, {members:members,ready:resolve(this)});
    }
    get size() { return size(set(this).members); }
    get status() { set(this); return 'loaded'; }
    get ready() { return set(this).ready; }
    add(value) { const s=set(this);face(value);add(s.members,value);return this; }
    delete(value) { const s=set(this);face(value);return remove(s.members,value); }
    has(value) { const s=set(this);face(value);return has(s.members,value); }
    clear() { clear(set(this).members); }
    forEach(callback, thisArg) {
        const s=set(this), owner=this;
        if (typeof callback !== 'function') throw new TypeError('Expected callback');
        each(s.members,function(value) { invoke(callback,thisArg,[value,value,owner]); });
    }
    load(font, text) {
        const s=set(this);
        try {
            if (!arguments.length) throw new TypeError('load requires font');
            const names=query(font,text), result=[];
            each(s.members,function(value) {
                if (names.includes(('' + value.family).replace(/^["']|["']$/g,'').toLowerCase())) { const state=face(value);state.status='loaded';state.settle(value);result.push(value); }
            });
            return resolve(result);
        } catch (error) { return reject(error); }
    }
    check(font, text) {
        set(this);
        if (!arguments.length) throw new TypeError('check requires font');
        query(font,text);
        return true;
    }
}
Object.defineProperty(Document.prototype, 'fonts', {configurable:true,enumerable:true,get:function() {
    if (!isDocument(this)) throw new TypeError('Expected document');
    if (!weakHas(documents,this)) weakSet(documents,this,new FontFaceSet());
    return weakGet(documents,this);
}});
})
"#;
