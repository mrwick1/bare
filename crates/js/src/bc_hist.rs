//! Counters of the bytecode loop (feature `bc-hist`, for `tools/js/bc_hist.sh`): operations, pairs, triples, functions.
use super::*;
use crate::bc::Op;

pub(super) struct Hist {
    ops: Vec<u64>,
    pairs: Vec<u64>,
    triples: Vec<u32>,
    prev: [u8; 2],
    /// Misses of the inline cache: `GetField`, `SetField`.
    pub miss: [u64; 2],
    /// `GetEnv` runs by hop count (8 and more in the last), and the hop-0 runs in functions whose locals stay in the environment
    /// only because `rfc_analyze` met a lazy inner function.
    hops: [u64; 9],
    hops0_lazy: u64,
    /// Script, function, name, code.
    pub funcs: Vec<(u32, u32, String, Rc<Compiled>)>,
}

impl Default for Hist {
    fn default() -> Self {
        Hist { ops: vec![0; 256], pairs: vec![0; 1 << 16], triples: vec![0; 1 << 24], prev: [0; 2], miss: [0; 2], hops: [0; 9], hops0_lazy: 0, funcs: Vec::new() }
    }
}

fn name(o: u16) -> &'static str {
    Op::from_u8(o as u8).map_or("?", Op::name)
}

impl Hist {
    #[inline(always)]
    pub fn step(&mut self, w: u32, at: usize, c: &Compiled) {
        let o = w as u8;
        self.ops[o as usize] += 1;
        let p = (self.prev[0] as usize) << 8 | o as usize;
        self.pairs[p] += 1;
        self.triples[(self.prev[1] as usize) << 16 | p] += 1;
        self.prev = [o, self.prev[0]];
        c.hist.0.set(c.hist.0.get() + 1);
        if at == 0 {
            c.hist.1.set(c.hist.1.get() + 1);
        }
        if o == Op::GetEnv as u8 {
            let h = (w >> 16 & 255) as usize;
            self.hops[h.min(8)] += 1;
            self.hops0_lazy += (h == 0 && c.hist.2) as u64;
        }
    }

    pub fn label(&mut self, script: u32, fid: u32, n: String) {
        if let Some(f) = self.funcs.last_mut() {
            (f.0, f.1, f.2) = (script, fid, n);
        }
    }

    pub fn report(&self) {
        let total: u64 = self.ops.iter().sum();
        if total == 0 {
            return;
        }
        let pc = |n: u64| n as f64 * 100.0 / total as f64;
        eprintln!("bchist total_ops={total}");
        let mut v: Vec<_> = self.ops.iter().enumerate().filter(|x| *x.1 > 0).collect();
        v.sort_by(|a, b| b.1.cmp(a.1));
        let mut cum = 0;
        for (o, n) in v {
            cum += n;
            eprintln!("bchist op {:<14} {:>12} {:6.2}% cum {:6.2}%", name(o as u16), n, pc(*n), pc(cum));
        }
        let mut v: Vec<_> = self.pairs.iter().enumerate().filter(|x| *x.1 > 0).collect();
        v.sort_by(|a, b| b.1.cmp(a.1));
        for (p, n) in v.into_iter().take(40) {
            eprintln!("bchist pair {:<14} {:<14} {:>12} {:6.2}%", name(p as u16 >> 8), name(p as u16 & 255), n, pc(*n));
        }
        // Fused pairs with no operation that is a head of one pair and a tail of another: their runs cannot overlap, so the sum is exact.
        let (mut heads, mut tails, mut saved) = ([false; 256], [false; 256], 0);
        let mut v: Vec<_> = self.pairs.iter().enumerate().filter(|x| *x.1 > 0).collect();
        v.sort_by(|a, b| b.1.cmp(a.1));
        let mut k = 0;
        for (p, n) in v {
            let (h, t) = (p >> 8, p & 255);
            if tails[h] || heads[t] {
                continue;
            }
            (heads[h], tails[t]) = (true, true);
            saved += n;
            k += 1;
            if matches!(k, 4 | 8 | 16 | 32) {
                eprintln!("bchist fuse pairs={k} dispatches_removed={saved} {:.2}%", pc(saved));
            }
            if k == 32 {
                break;
            }
        }
        let mut v: Vec<_> = self.triples.iter().enumerate().filter(|x| *x.1 > 0).collect();
        v.sort_by(|a, b| b.1.cmp(a.1));
        for (t, n) in v.into_iter().take(40) {
            eprintln!("bchist triple {:<14} {:<14} {:<14} {:>12} {:6.2}%", name((t >> 16) as u16), name((t >> 8) as u16 & 255), name(t as u16 & 255), n, pc(*n as u64));
        }
        let mut f: Vec<_> = self.funcs.iter().map(|f| (f.3.hist.0.get(), f.3.hist.1.get(), f)).collect();
        f.sort_by_key(|b| std::cmp::Reverse(b.0));
        let mut cum = 0;
        for (i, (n, calls, f)) in f.iter().enumerate() {
            cum += n;
            if i < 25 {
                eprintln!("bchist func script={} fn={} name={} ops={} {:.2}% cum {:.2}% calls={} ops_per_call={:.1}", f.0, f.1, f.2, n, pc(*n), pc(cum), calls, *n as f64 / (*calls).max(1) as f64);
            }
        }
        let n = |k: &[Op]| k.iter().map(|o| self.ops[*o as usize]).sum::<u64>();
        let envs: u64 = self.hops.iter().sum();
        for (h, n) in self.hops.iter().enumerate().filter(|x| *x.1 > 0) {
            eprintln!("bchist getenv hops={}{} {:>12} {:6.2}% of GetEnv", h, if h == 8 { "+" } else { "" }, n, *n as f64 * 100.0 / envs as f64);
        }
        eprintln!("bchist getenv hops=0 in functions kept in the environment by a lazy inner function: {}", self.hops0_lazy);
        eprintln!("bchist funcs={} ic GetField execs={} misses={} SetField execs={} misses={}", f.len(), n(&[Op::GetField]), self.miss[0], n(&[Op::SetField]), self.miss[1]);
    }
}
