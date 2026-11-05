// `indexOf`, `lastIndexOf` and `includes` of `%TypedArray%.prototype`, for every element type. Included by `eval.rs`
// (`include!`), so it shares that module's imports and private items. The dispatch is `BinFn::TaIndexOf` and `TaM` in
// `eval_binary.rs`.
//
// The search never converts `searchElement`. Only a number can match an element: `indexOf` and `lastIndexOf` use strict
// equality (`NaN` never matches, `-0` matches `0`), `includes` uses SameValueZero (`NaN` matches a `NaN` element). An element
// is always a number, so any other value gives -1 (or `false`) at once, with no scan. A BigInt typed array holds BigInts, and
// a number is never equal to one, so it gives the same answer with no scan (and no gate).
// Order, as in the spec: the receiver is validated first. An empty array answers before `fromIndex` is read.
// `fromIndex` is converted once with ToIntegerOrInfinity. Only then are the bytes borrowed, because a `valueOf` may write
// to the array, or detach its buffer (then every element reads as `undefined`: only `includes(undefined)` can match).
// How: a value that the type stores exactly (the round trip through `store` and `load` is the same number) is encoded once
// and found as a byte pattern. A float `0` (it has two encodings) and a `NaN` (for `includes`) use the generic loop on
// decoded elements. A value that the type cannot hold (`1.5` in an `Int8Array`, `300` in a `Uint8Array`) matches nothing.
// Limits: the scan runs over 64-byte chunks. Each chunk is charged one step before it is read (a started chunk costs one
// step), so a miss over a large array stops at the step limit, and an early match pays only for the chunks it reached. The
// scan copies no bytes, allocates nothing and runs no JavaScript.

/// Bytes per step of a search.
const SEARCH_CHUNK: usize = 64;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Search {
    IndexOf,
    LastIndexOf,
    Includes,
}

impl<'p> Interp<'p> {
    /// `indexOf(searchElement, fromIndex)`, `lastIndexOf(searchElement, fromIndex)` and `includes(searchElement, fromIndex)`.
    fn ta_search(&mut self, kind: Search, this: Value, args: &[Value]) -> Res<Value> {
        let what = match kind {
            Search::IndexOf => "%TypedArray%.prototype.indexOf",
            Search::LastIndexOf => "%TypedArray%.prototype.lastIndexOf",
            Search::Includes => "%TypedArray%.prototype.includes",
        };
        let t = self.this_ta(this, what, true)?;
        let (search, from) = (arg(args, 0), arg(args, 1));
        let miss = if kind == Search::Includes { Value::Bool(false) } else { Value::Number(-1.0) };
        if t.len == 0 {
            return Ok(miss);
        }
        let len = t.len as f64;
        // The first index to read, and the direction.
        let start = if kind == Search::LastIndexOf {
            let n = if args.len() > 1 { self.num_integer(from)? } else { len - 1.0 };
            let k = if n >= 0.0 { n.min(len - 1.0) } else { len + n };
            if k < 0.0 {
                return Ok(miss);
            }
            k
        } else {
            let n = self.num_integer(from)?;
            if n >= len {
                return Ok(miss);
            }
            if n >= 0.0 { n } else { (len + n).max(0.0) }
        };
        // The conversion may have detached the buffer: each element is `undefined` now.
        if self.heap.view_detached(t.o) {
            return Ok(if kind == Search::Includes && matches!(search, Value::Undefined) { Value::Bool(true) } else { miss });
        }
        let Value::Number(x) = search else { return Ok(miss) };
        let nan_matches = kind == Search::Includes;
        if t.ty.is_big() || (x.is_nan() && !nan_matches) {
            return Ok(miss);
        }
        let size = t.ty.size();
        // The pattern: only for a value that the type stores exactly. A float zero has two encodings and a NaN has many.
        let pat = if x.is_nan() || (t.ty.is_float() && x == 0.0) {
            None
        } else {
            let mut p = [0u8; 8];
            store(t.ty, &mut p, x);
            if load(t.ty, &p) != x {
                return Ok(miss);
            }
            Some(p)
        };
        let bytes = self.heap.bytes(t.o).ok_or(Error::Heap(HeapError::Stale))?;
        // Whole elements of this type, first or last match in one chunk.
        let b0 = pat.map_or(0, |p| p[0]);
        let find = |c: &[u8], rev: bool| -> Option<usize> {
            match pat {
                Some(_) if size == 1 => find_byte(c, b0, rev),
                Some(p) => match size {
                    2 => find_fixed::<2>(c, p[..2].try_into().ok()?, rev),
                    4 => find_fixed::<4>(c, p[..4].try_into().ok()?, rev),
                    _ => find_fixed::<8>(c, &p, rev),
                },
                None => {
                    let hit = |e: &[u8]| {
                        let v = load(t.ty, e);
                        v == x || (nan_matches && x.is_nan() && v.is_nan())
                    };
                    if rev { c.chunks_exact(size).rposition(hit) } else { c.chunks_exact(size).position(hit) }
                }
            }
        };
        let chunk_bytes = (SEARCH_CHUNK / size).max(1) * size;
        let mut found = None;
        if kind == Search::LastIndexOf {
            // From `start` down to 0, one chunk at a time, the chunks counted from the end.
            let mut hi = start as usize + 1;
            for chunk in bytes[..hi * size].rchunks(chunk_bytes) {
                self.steps = self.steps.saturating_add(1);
                if self.steps > self.step_limit {
                    return Err(Error::Limit(LimitKind::Steps));
                }
                let n = chunk.len() / size;
                if let Some(p) = find(chunk, true) {
                    found = Some(hi - n + p);
                    break;
                }
                hi -= n;
            }
        } else {
            let mut at = start as usize;
            for chunk in bytes[at * size..].chunks(chunk_bytes) {
                self.steps = self.steps.saturating_add(1);
                if self.steps > self.step_limit {
                    return Err(Error::Limit(LimitKind::Steps));
                }
                if let Some(p) = find(chunk, false) {
                    found = Some(at + p);
                    break;
                }
                at += chunk.len() / size;
            }
        }
        Ok(match (kind, found) {
            (Search::Includes, f) => Value::Bool(f.is_some()),
            (_, Some(i)) => Value::Number(i as f64),
            (_, None) => Value::Number(-1.0),
        })
    }
}

/// The first (or, with `rev`, last) whole element of `c` that is the `N` bytes `p`.
fn find_fixed<const N: usize>(c: &[u8], p: &[u8; N], rev: bool) -> Option<usize> {
    let mut it = c.chunks_exact(N);
    if rev { it.rposition(|e| e == p) } else { it.position(|e| e == p) }
}

/// The first (or, with `rev`, last) byte `b` of `c`, 8 bytes at a time. The test is exact: bit 7 of a byte of `m` is set
/// when that byte of the word equals `b`, so the lowest or highest set bit is the match.
fn find_byte(c: &[u8], b: u8, rev: bool) -> Option<usize> {
    const M: u64 = 0x7f7f_7f7f_7f7f_7f7f;
    let p = u64::from_ne_bytes([b; 8]);
    let eq = |w: &[u8]| {
        let x = u64::from_le_bytes(w.try_into().unwrap_or([0; 8])) ^ p;
        !(((x & M) + M) | x | M)
    };
    if rev {
        let mut it = c.rchunks_exact(8);
        let mut end = c.len();
        for w in it.by_ref() {
            let m = eq(w);
            if m != 0 {
                return Some(end - 8 + ((63 - m.leading_zeros()) / 8) as usize);
            }
            end -= 8;
        }
        it.remainder().iter().rposition(|&x| x == b)
    } else {
        let mut at = 0;
        let mut it = c.chunks_exact(8);
        for w in it.by_ref() {
            let m = eq(w);
            if m != 0 {
                return Some(at + (m.trailing_zeros() / 8) as usize);
            }
            at += 8;
        }
        it.remainder().iter().position(|&x| x == b).map(|i| at + i)
    }
}
