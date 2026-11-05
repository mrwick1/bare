// `%TypedArray%.prototype.indexOf` for the byte views. Included by `eval.rs` (`include!`), so it shares that module's
// imports and private items. The dispatch is `BinFn::TaIndexOf` in `eval_binary.rs`.
//
// The search is strict equality. `searchElement` is never coerced. A number matches a byte of the same value, so `-0`
// matches `0`. `NaN`, a number that is not a byte value (`1.5`, `256`, `-1`) and every other value match nothing, and the
// search returns -1 without a scan.
// Order, as in the spec: the receiver is validated first. An empty view returns -1 before `fromIndex` is read.
// `fromIndex` is converted once with ToIntegerOrInfinity. Only then are the bytes borrowed, because a `valueOf` may write
// to the view.
// Limits: the scan runs over 64-byte chunks. Each chunk is charged one step before it is read (a started chunk costs one
// step), so a miss over a large view stops at the step limit, and an early match pays only for the chunks it reached. The
// scan copies no bytes, allocates nothing and runs no JavaScript.

/// Bytes per step of an `indexOf` scan.
const SEARCH_CHUNK: usize = 64;

impl<'p> Interp<'p> {
    /// `%TypedArray%.prototype.indexOf(searchElement, fromIndex)`.
    fn ta_index_of(&mut self, this: Value, search: Value, from: Value) -> Res<Value> {
        let (o, _, _, len) = self.this_view(this, "%TypedArray%.prototype.indexOf")?;
        if len == 0 {
            return Ok(Value::Number(-1.0));
        }
        let n = self.num_integer(from)?;
        // A negative start counts from the end and stops at 0. A start at or past the end has no match.
        let start = if n < 0.0 { (len as f64 + n).max(0.0) } else { n };
        // `x as u8` saturates, so the round trip is exact only for the byte values, and `-0` maps to `0`.
        let target = match search {
            Value::Number(x) => Some(x as u8).filter(|&b| b as f64 == x),
            _ => None,
        };
        let Some(target) = target.filter(|_| start < len as f64) else { return Ok(Value::Number(-1.0)) };
        // All JavaScript has run. Borrow the bytes now. Only `steps` is written in the loop, so the borrow stays valid.
        let bytes = self.heap.bytes(o).ok_or(Error::Heap(HeapError::Stale))?;
        let mut at = start as usize;
        for chunk in bytes.get(at..).unwrap_or(&[]).chunks(SEARCH_CHUNK) {
            self.steps = self.steps.saturating_add(1);
            if self.steps > self.step_limit {
                return Err(Error::Limit(LimitKind::Steps));
            }
            if let Some(p) = chunk.iter().position(|&b| b == target) {
                return Ok(Value::Number((at + p) as f64));
            }
            at += chunk.len();
        }
        Ok(Value::Number(-1.0))
    }
}
