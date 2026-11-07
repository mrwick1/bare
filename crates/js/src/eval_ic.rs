// The inline caches of the tree walker (step A2 of `docs/engine-plan.md`). Included by `eval.rs`.
//
// A member read `o.x` keeps a number in `expr_name` of its node. That table names only identifiers, `this` and private names, so the
// entry of a member node is free, and the cache costs no table of 4 bytes for each node:
// - 0: the site has not run.
// - 1: it ran once. A site that runs once (most of the code at load) gets no entry.
// - n >= 2: its entry is `ics[n]`. `ics[0]` and `ics[1]` are `Ic::EMPTY`, so the inline test needs no branch on the number.
// The heap answers a hit (`Heap::ic_own` inline, `Heap::get_ic` out of line). The bytecode interpreter keeps its own entries.

/// The inline cache entries of the tree walker, for every script (an entry number is unique in the realm).
struct IcState {
    ics: Vec<Ic>,
    /// Inline own hits, and reads that took the out-of-line path (`member_ic`). For `Interp::ic_counts`.
    own_hits: u64,
    slow: u64,
    first_runs: u64,
}

impl Default for IcState {
    fn default() -> Self {
        IcState { ics: vec![Ic::EMPTY; 2], own_hits: 0, slow: 0, first_runs: 0 }
    }
}

/// Counters of the member read caches of the tree walker (`Interp::ic_counts`).
#[derive(Clone, Copy, Debug, Default)]
pub struct IcCounts {
    /// Member reads on an object: all of them, the inline own hits, the reads of a site's first run (no entry), and the entries.
    pub reads: u64,
    pub own_hits: u64,
    pub first_runs: u64,
    pub entries: usize,
    /// The counters of the heap: hits of the other kinds, fills, refusals and epoch bumps.
    pub heap: IcStats,
}

impl Interp<'_> {
    /// The counters of the member read caches.
    pub fn ic_counts(&self) -> IcCounts {
        let s = &self.ics;
        IcCounts { reads: s.own_hits + s.slow, own_hits: s.own_hits, first_runs: s.first_runs, entries: s.ics.len() - 2, heap: self.heap.ic_stats() }
    }

    /// `get_prop` at a site in native code with a few entries (`toJSON` in `JSON.stringify`: records, their arrays and their inner
    /// objects). A miss moves the entries down by one and fills the first.
    fn get_prop_ic(&mut self, v: Value, key: PropertyKey, ics: &mut [Ic; 4]) -> Res<Value> {
        let Value::Object(o) = v else { return self.get_prop(v, key) };
        let i = match ics.iter().position(|ic| self.heap.ic_shape_is(o, ic)) {
            Some(i) => i,
            None => {
                ics.copy_within(0..3, 1);
                0
            }
        };
        let got = match self.heap.get_ic(o, key, &mut ics[i]) {
            Ok(g) => g,
            Err(err) => self.get_miss(o, key, v, err)?,
        };
        match got {
            Get::Value(v) => Ok(v),
            Get::Accessor { getter: Some(g) } => self.call_fn(Value::Object(g), v, &[]),
            Get::Accessor { getter: None } => Ok(Value::Undefined),
        }
    }

    /// `o.name` at the member site `e` when the inline own hit failed: a hit of another kind, a fill, or the plain read of a first run.
    #[inline(never)]
    fn member_ic(&mut self, e: ExprId, name: Span, o: ObjId, base: Value) -> Res<Value> {
        self.ics.slow += 1;
        let key = self.expr_key(e, name)?;
        let mut n = self.expr_name[e.index()] as usize;
        let got = if n == 0 {
            self.expr_name[e.index()] = 1;
            self.ics.first_runs += 1;
            self.heap.get(o, key)
        } else if self.heap.ic_refuses(o) {
            // A receiver that is never cached (a proxy, a named host object, a String wrapper, a dictionary): the plain read.
            self.heap.get_refused(o, key)
        } else {
            if n == 1 {
                n = self.ics.ics.len();
                self.ics.ics.push(Ic::EMPTY);
                self.expr_name[e.index()] = n as u32;
            }
            let mut ic = self.ics.ics[n];
            let r = self.heap.get_ic(o, key, &mut ic);
            self.ics.ics[n] = ic;
            r
        };
        let got = match got {
            Ok(g) => g,
            Err(err) => self.get_miss(o, key, base, err)?,
        };
        match got {
            Get::Value(v) => Ok(v),
            Get::Accessor { getter: Some(g) } => self.call_fn(Value::Object(g), base, &[]),
            Get::Accessor { getter: None } => Ok(Value::Undefined),
        }
    }
}
