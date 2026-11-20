//! Layout census (plan step M3 of `docs/plan-engine-rewrite.md`): the bytes of the engine by layout. `BARE_JS_STATS=1` prints the
//! sample with the most bytes when an `Interp` ends, next to the `gcstats` line. A sample walks the whole realm, so it costs time
//! only when the variable is set: the sample points are the start of `Interp::collect` (the heap is at its largest there, garbage
//! included, as in a tool that watches the allocator) and the end of the realm.
//!
//! The bytes are capacity bytes: what the engine asked of the allocator, not what the values use. Malloc headers are not counted.

use std::sync::OnceLock;

/// Is `BARE_JS_STATS` set? Read once per process.
pub(crate) fn stats_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("BARE_JS_STATS").is_some())
}

macro_rules! buckets {
    ($($name:ident: $doc:literal,)*) => {
        /// Bytes by layout. Each field is one bucket.
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
        pub struct LayoutCensus {
            $(#[doc = $doc] pub $name: usize,)*
        }

        impl LayoutCensus {
            /// The buckets in print order: name and bytes.
            pub fn buckets(&self) -> Vec<(&'static str, usize)> {
                vec![$((stringify!($name), self.$name),)*]
            }
            pub fn total(&self) -> usize {
                0 $(+ self.$name)*
            }
        }
    };
}

buckets! {
    objects: "Object records of the heap slab (header, flags, the elements and dictionary pointers), with the slab's own tables (generations, positions, marks).",
    slots: "Value slots of objects in shape mode: the three inline slots of each record, and the vectors of objects with more.",
    dict: "Dictionary-mode properties: the entry vectors, their hash indexes and the map records.",
    elements: "Dense element vectors of arrays and array-like objects.",
    shapes: "Shapes, their key tables and the transition maps.",
    strings: "String records, UTF-16 units, the interner table, ropes, flat rope units and the shared host texts.",
    binary: "Exotic payloads: buffers and the exotic table.",
    heap_other: "Symbols, BigInts and their tables.",
    bytecode: "Bytecode words, constants, handlers, line and call tables, and the per-function tables of the bytecode lane.",
    feedback: "Inline caches: the bytecode lane's per function, the tree walker's, and the heap's stub and keyed caches.",
    source: "Script source text.",
    ast: "Syntax trees.",
    tables: "Per-script tables of the tree walker (names, slots, scope records, flags).",
    scopes: "Scope records and frame templates of the whole realm.",
    envs: "Environments, their bindings, and the function table.",
    names: "The interned names of the evaluator.",
    collector: "Scratch of a collect at its peak (before the sweep): the mark bits, the gray stack, the work lists and the root vector. Only a sample taken inside `collect` has it.",
    stacks: "Register stacks, frame buffers and argument pools kept for reuse.",
}

impl LayoutCensus {
    /// The sample line: `layout total=... bucket=bytes ...`.
    pub fn line(&self) -> String {
        let mut s = format!("layout total={}", self.total());
        for (n, b) in self.buckets() {
            s.push_str(&format!(" {n}={b}"));
        }
        s
    }
}
