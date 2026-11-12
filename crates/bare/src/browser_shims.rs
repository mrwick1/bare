//! Web APIs that are written in JavaScript and made at the first read of one of their names: `Element.prototype.animate` and `Animation`
//! (`shims/animation.js`), `Path2D` (`shims/path2d.js`), `indexedDB` and the `IDB*` interfaces (`shims/idb.js`), `EventSource` (`shims/eventsource.js`),
//! `WritableStream`, `TransformStream`, `TextDecoderStream`, `TextEncoderStream` and `ReadableStream.prototype.pipeTo` and `pipeThrough`
//! (`shims/streams.js`).
//!
//! A read of the name finds nothing, so the heap asks the host once (`Heap::browser_name`, `browser_handlers::lazy`). The host runs the
//! source as an indirect eval; the source defines the globals itself. A realm that never reads the names pays nothing: no object, no
//! function, no script. The source of a shim defines its own sentinel global, so it runs once.
//!
//! Gates: none of them has a timeline, a layout or a store on disk. `animate` ends the animation at once (one timer). `Path2D` records
//! nothing. `indexedDB` keeps its data in the realm only and runs the requests of a transaction one after another in
//! microtasks. Not compared with a real browser.

use crate::browser::*;
use bare_js::eval::Interp;
use bare_js::value::Value;

/// `(names that ask for it, the global that the source defines first, source)`.
const SHIMS: [(&[&str], &str, &str); 5] = [
    (&["animate", "Animation"], "Animation", include_str!("shims/animation.js")),
    (&["Path2D"], "Path2D", include_str!("shims/path2d.js")),
    (&["EventSource"], "EventSource", include_str!("shims/eventsource.js")),
    (&["indexedDB"], "indexedDB", include_str!("shims/idb.js")),
    (&STREAM_NAMES, "WritableStream", include_str!("shims/streams.js")),
];

/// The globals of `shims/streams.js`. `crates/js/src/heap.rs` (`browser_name`) lists the same names.
const STREAM_NAMES: [&str; 7] = ["WritableStream", "WritableStreamDefaultWriter", "WritableStreamDefaultController", "TransformStream", "TransformStreamDefaultController", "TextDecoderStream", "TextEncoderStream"];

/// Make the shim that `name` asks for. Gives whether `name` is one of them.
#[cold]
pub(crate) fn define(d: &Dom, it: &mut Interp<'_>, name: &str) -> R<bool> {
    let Some(&(_, sentinel, src)) = SHIMS.iter().find(|s| s.0.contains(&name) || (name.starts_with("IDB") && s.1 == "indexedDB")) else { return Ok(false) };
    let g = it.global_object();
    let k = d.key(it, sentinel)?;
    if it.heap.peek_own_property(g, k).map_err(|e| it.heap_error(e))?.is_none() {
        // Not `js_str`: the source is part of the host, not script data, and a small `max_string_bytes` must not stop the browser.
        let s = it.heap.intern_str(src).map_err(|e| it.heap_error(e))?;
        it.eval_global(Value::String(s))?;
    }
    Ok(true)
}
