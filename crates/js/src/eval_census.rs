// The layout census of the realm (plan step M3, `layout_census.rs`). Included into `eval.rs`, so it reads the private tables.
// Every size is a capacity in bytes. A shared tree, source or compiled function counts once.

use crate::layout_census::{stats_on, LayoutCensus};

impl Caches {
    /// Capacity bytes of the tables of one script (not the syntax tree).
    fn table_bytes(&self) -> usize {
        use std::mem::size_of;
        let (c, k) = (self, &*self.cold);
        (c.expr_name.capacity() + k.pat_name.capacity() + k.stmt_label.capacity() + k.func_name.capacity() + c.slots.sites.len()) * 4
            + k.site_strs.capacity() * size_of::<StrId>() + k.prop_str.capacity() * size_of::<Option<StrId>>()
            + c.scopes.capacity() * size_of::<FScope>() + c.scopes.iter().map(|s| s.vars.capacity() * 4).sum::<usize>()
            + k.fflags.capacity() + k.cflags.capacity() + size_of::<Cold>()
    }
}

impl Compiled {
    /// (bytecode bytes, feedback bytes) of one compiled function.
    fn census_bytes(&self) -> (usize, usize) {
        use std::mem::size_of;
        let c = &self.code;
        let code = size_of::<Compiled>() + 16 // the `Rc` counters
            + c.ops.capacity() * 4
            + c.consts.capacity() * size_of::<crate::bc::Const>()
            + c.consts.iter().map(|k| if let crate::bc::Const::String(s) = k { s.len() * 2 } else { 0 }).sum::<usize>()
            + c.handlers.capacity() * size_of::<crate::bc::Handler>()
            + (c.lines.capacity() + c.calls.capacity()) * size_of::<(u32, u32)>()
            + std::mem::size_of_val(&*self.kv) + std::mem::size_of_val(&*self.cells);
        (code, std::mem::size_of_val(&*c.ics))
    }
}

impl<'p> Interp<'p> {
    /// The bytes of this realm by layout. It walks the heap, every script and every compiled function: for `BARE_JS_STATS`, not for a hot path.
    pub fn layout_census(&self) -> LayoutCensus {
        use std::mem::size_of;
        let mut c = LayoutCensus::default();
        self.heap.census(&mut c);
        // Scripts: the current one's tables are in `cc`, the others' in `scripts`. A tree is shared by the slots of its lazy functions.
        let (mut trees, mut texts): (std::collections::HashSet<*const Program<'static>>, std::collections::HashSet<(*const u8, usize)>) = Default::default();
        let mut tree = |p: &Rc<Program<'static>>, c: &mut LayoutCensus| {
            if !trees.insert(Rc::as_ptr(p)) {
                return;
            }
            c.ast += p.capacity_bytes() + size_of::<Program>() + 16;
            let t = (p.source.as_ptr(), p.source.len());
            if texts.insert(t) {
                c.source += t.1;
            }
        };
        tree(&self.prog, &mut c);
        c.tables += self.cc.table_bytes();
        for s in &self.scripts {
            tree(&s.prog, &mut c);
            c.tables += s.cc.table_bytes();
        }
        c.tables += self.scripts.capacity() * size_of::<Code>() + self.script_ids.capacity() * 4
            + self.lazy.capacity() * (size_of::<((u32, u32), LazySlot)>() + 1) + self.lazy_owner.capacity() * (size_of::<(u32, u32)>() + 1);
        // The bytecode lane.
        let mut seen: std::collections::HashSet<*const Compiled> = Default::default();
        for slots in &self.bc.codes {
            c.bytecode += std::mem::size_of_val(&**slots);
            for s in slots.iter() {
                if let BcSlot::Byte(rc) = s
                    && seen.insert(Rc::as_ptr(rc))
                {
                    let (code, ics) = rc.census_bytes();
                    c.bytecode += code;
                    c.feedback += ics;
                }
            }
        }
        c.bytecode += self.bc.codes.capacity() * size_of::<Box<[BcSlot]>>()
            + self.bc.fn_scopes.iter().map(|f| f.len() * 4 + size_of::<Box<[u32]>>()).sum::<usize>()
            + self.bc.tmpl.capacity() * (size_of::<(u32, Rc<[(u32, u8)]>)>() + 1) + self.bc.tmpl.values().map(|t| t.len() * 8 + 16).sum::<usize>();
        c.stacks += self.bc.pool.iter().map(|r| r.census_bytes()).sum::<usize>() + self.bc.pool.capacity() * size_of::<bc_vm::RegStack>();
        c.feedback += self.ics.ics.capacity() * size_of::<Ic>();
        c.scopes += self.sc_all.capacity() * size_of::<RScope>() + (self.sc_pool.capacity() + self.fr_names.capacity() + self.fr_ends.capacity()) * 4
            + self.fr_pool.capacity() * size_of::<Binding>();
        c.envs += self.envs.capacity() * size_of::<Env>() + self.free_envs.capacity() * 4 + self.funcs.bytes()
            + self.envs.iter().map(|e| e.bindings.capacity() * size_of::<Binding>() + e.index.as_ref().map_or(0, |m| m.capacity() * (size_of::<(u32, u32)>() + 1) + size_of::<FxMap<u32, u32>>())).sum::<usize>();
        c.names += self.name_map.capacity() * (size_of::<(String, u32)>() + 1) + self.names.capacity() * size_of::<String>()
            + self.name_map.keys().map(|k| k.capacity()).sum::<usize>() + self.names.iter().map(|k| k.capacity()).sum::<usize>()
            + self.name_strs.capacity() * size_of::<Option<StrId>>() + self.name_gpos.capacity() * 4;
        c.stacks += self.sf_pool.iter().map(|f| f.capacity() * size_of::<SFrame>()).sum::<usize>() + self.sf_pool.capacity() * size_of::<Vec<SFrame>>()
            + self.arg_pool.iter().map(|a| a.capacity() * size_of::<Value>()).sum::<usize>() + self.arg_pool.capacity() * size_of::<Vec<Value>>();
        c
    }

    /// Take a sample, and keep it when it has more bytes than the peak so far (`GcCounters::peak`). `scratch` is the collector's work lists, held
    /// by a collect that is running; the heap adds its own (`Heap::collector_bytes`).
    fn census_sample(&mut self, scratch: usize) {
        let mut c = self.layout_census();
        c.collector = scratch + if scratch > 0 { self.heap.collector_bytes() } else { 0 };
        if c.total() > self.gc_counters.peak.total() {
            self.gc_counters.peak = c;
        }
    }
}

impl Drop for Interp<'_> {
    fn drop(&mut self) {
        if stats_on() {
            self.census_sample(0);
        }
    }
}
