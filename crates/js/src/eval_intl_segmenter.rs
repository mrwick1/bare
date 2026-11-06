// `Intl.Segmenter` (ECMA-402 section 18). Included by `eval_intl.rs`. The breaking rules are `icu_segmenter` (ICU4X, UAX #29),
// called in `icu.rs` (`segment_breaks`). Logic of the objects follows Boa's `core/engine/src/builtins/intl/segmenter`
// (Unlicense OR MIT) for the shape of the three classes, with the boundaries computed once for a string and shared.
//
// Observed need: the staging app calls `new Intl.Segmenter(undefined, { granularity: ... })` guarded by `"Segmenter" in Intl`,
// then `Array.from(segmenter.segment(text))`.
//
// Supported: `Intl.Segmenter` (needs `new`), `supportedLocalesOf`, `resolvedOptions` (`locale`, `granularity`), `segment(string)`;
// `granularity` grapheme (default), word and sentence. `segment` gives a `%Segments%` object: `containing(index)` and
// `[Symbol.iterator]` (a `%SegmentIterator%`, `next`, `@@toStringTag` "Segmenter String Iterator"). A segment data object has
// `segment`, `index`, `input` and, for the word granularity, `isWordLike`.
//
// Gates: words of Chinese, Japanese, Thai, Lao, Khmer and Burmese are cut by the rules only (no dictionary or LSTM data: it is
// 1 MB or more in the binary). The English locales have no tailoring, so the locale does not change the result.
//
// Storage: `intl.segs` (segmenters), `intl.segments` (`%Segments%`) and `intl.seg_iters` (iterators), pruned by `collect`. The
// boundaries of a string are made by the first `containing` or iteration, charged as steps, and shared by the iterators of
// that `%Segments%`. Tracked objects are capped by `max_iterators`.

#[derive(Clone, Copy)]
struct IntlSeg {
    gran: u8,
    loc: u8,
}

/// The boundaries of one string: segment `i` is `b[i]..b[i + 1]`; `wl[i]` is its `isWordLike`.
struct Brk {
    b: Vec<u32>,
    wl: Vec<bool>,
}

/// A `%Segments%` object.
struct SegData {
    gran: u8,
    s: StrId,
    brk: Option<Rc<Brk>>,
}

/// A `%SegmentIterator%` object.
struct SegIter {
    gran: u8,
    s: StrId,
    brk: Rc<Brk>,
    i: usize,
}

const GRANULARITIES: [&str; 3] = ["grapheme", "word", "sentence"];

impl<'p> Interp<'p> {
    fn intl_seg_tracked(&self) -> usize {
        self.intl.segments.len() + self.intl.seg_iters.len()
    }

    fn this_intl_seg(&mut self, this: Value, method: &str) -> Res<IntlSeg> {
        if let Value::Object(o) = this
            && let Some(s) = self.intl.segs.get(&o)
        {
            return Ok(*s);
        }
        let t = self.intl_show(this);
        Err(self.type_err(&format!("Method {method} called on incompatible receiver {t}")))
    }

    fn intl_seg_create(&mut self, locales: Value, options: Value) -> Res<IntlSeg> {
        const SEG: &str = "Intl.Segmenter";
        let requested = self.intl_locales(locales)?;
        let opts = self.intl_options(options, SEG)?;
        self.intl_opt_for(SEG, opts, "localeMatcher", &["lookup", "best fit"])?;
        let gran = self.intl_opt_for(SEG, opts, "granularity", &GRANULARITIES)?.unwrap_or(0) as u8;
        Ok(IntlSeg { gran, loc: self.intl_pick(&requested).unwrap_or(0) })
    }

    /// The boundaries of `s`, charged as steps.
    fn seg_breaks(&mut self, gran: u8, s: StrId) -> Res<Rc<Brk>> {
        let len = h!(self, self.heap.str_len(s));
        self.spend(len as u64 / 4 + 1)?;
        let (b, wl) = icu::segment_breaks(gran, h!(self, self.heap.str_units(s)));
        Ok(Rc::new(Brk { b, wl }))
    }

    /// The segment data object of segment `i`.
    fn seg_data_object(&mut self, gran: u8, s: StrId, brk: &Brk, i: usize) -> Res<Value> {
        let (a, b) = (brk.b[i] as usize, brk.b[i + 1] as usize);
        let seg = self.sub(s, a, b)?;
        let o = h!(self, self.heap.new_object(Some(self.object_proto)));
        let props = [("segment", Value::String(seg)), ("index", Value::Number(a as f64)), ("input", Value::String(s))];
        for (n, v) in props {
            let k = h!(self, self.heap.key_from_str(n));
            self.def(o, k, v, true, true, true)?;
        }
        if gran == 1 {
            let k = h!(self, self.heap.key_from_str("isWordLike"));
            self.def(o, k, Value::Bool(brk.wl[i]), true, true, true)?;
        }
        Ok(Value::Object(o))
    }

    fn seg_track(&self) -> Res<()> {
        if self.intl_seg_tracked() >= self.sc.limits.max_iterators {
            return Err(Error::Limit(LimitKind::Sc(ScLimit::Iterators)));
        }
        Ok(())
    }

    fn call_intl_seg(&mut self, f: IntlFn, this: Value, args: &[Value], is_new: bool) -> Res<Value> {
        let (a0, a1) = (arg(args, 0), arg(args, 1));
        match f {
            IntlFn::SegCtor => {
                if !is_new {
                    return Err(self.type_err("Constructor Intl.Segmenter requires 'new'"));
                }
                if self.intl_formatters() >= self.sc.limits.max_formatters {
                    return Err(Error::Limit(LimitKind::Sc(ScLimit::Formatters)));
                }
                let s = self.intl_seg_create(a0, a1)?;
                let o = h!(self, self.heap.new_object(Some(self.intl.seg_proto)));
                self.intl.segs.insert(o, s);
                Ok(Value::Object(o))
            }
            IntlFn::SegResolved => {
                let s = self.this_intl_seg(this, "Intl.Segmenter.prototype.resolvedOptions")?;
                let props: Vec<(&str, Value)> = vec![("locale", self.str_value(intl::locale_name(s.loc))?), ("granularity", self.str_value(GRANULARITIES[s.gran as usize])?)];
                let o = h!(self, self.heap.new_object(Some(self.object_proto)));
                self.intl_fill(o, props)?;
                Ok(Value::Object(o))
            }
            IntlFn::SegSegment => {
                let s = self.this_intl_seg(this, "Intl.Segmenter.prototype.segment")?;
                let text = self.coerce_string(a0)?;
                self.seg_track()?;
                let o = h!(self, self.heap.new_object(Some(self.intl.segments_proto)));
                self.intl.segments.insert(o, SegData { gran: s.gran, s: text, brk: None });
                Ok(Value::Object(o))
            }
            IntlFn::SegsContaining | IntlFn::SegsIter => {
                let method = if f == IntlFn::SegsIter { "%Segments%.prototype[Symbol.iterator]" } else { "%Segments%.prototype.containing" };
                let Some((o, gran, s, brk)) = (match this {
                    Value::Object(o) => self.intl.segments.get(&o).map(|d| (o, d.gran, d.s, d.brk.clone())),
                    _ => None,
                }) else {
                    let t = self.intl_show(this);
                    return Err(self.type_err(&format!("Method {method} called on incompatible receiver {t}")));
                };
                if f == IntlFn::SegsContaining {
                    let n = self.int_arg(a0)?;
                    let len = h!(self, self.heap.str_len(s)) as f64;
                    if n < 0.0 || n >= len {
                        return Ok(Value::Undefined);
                    }
                    let brk = match brk {
                        Some(b) => b,
                        None => self.seg_breaks_for(o, gran, s)?,
                    };
                    let i = brk.b.partition_point(|&b| b as f64 <= n) - 1;
                    return self.seg_data_object(gran, s, &brk, i);
                }
                let brk = match brk {
                    Some(b) => b,
                    None => self.seg_breaks_for(o, gran, s)?,
                };
                self.seg_track()?;
                let it = h!(self, self.heap.new_object(Some(self.intl.seg_iter_proto)));
                self.intl.seg_iters.insert(it, SegIter { gran, s, brk, i: 0 });
                Ok(Value::Object(it))
            }
            IntlFn::SegIterNext => {
                let Some(&mut SegIter { gran, s, ref brk, ref mut i }) = (match this {
                    Value::Object(o) => self.intl.seg_iters.get_mut(&o),
                    _ => None,
                }) else {
                    let t = self.intl_show(this);
                    return Err(self.type_err(&format!("Method %SegmentIterator%.prototype.next called on incompatible receiver {t}")));
                };
                let (brk, at) = (brk.clone(), *i);
                if at + 1 >= brk.b.len() {
                    return self.sc_iter_result(Value::Undefined, true);
                }
                *i += 1;
                let v = self.seg_data_object(gran, s, &brk, at)?;
                self.sc_iter_result(v, false)
            }
            _ => unreachable!("handled in call_intl_num"),
        }
    }

    /// The boundaries of the `%Segments%` object `o`, made once and kept on it.
    fn seg_breaks_for(&mut self, o: ObjId, gran: u8, s: StrId) -> Res<Rc<Brk>> {
        let brk = self.seg_breaks(gran, s)?;
        if let Some(d) = self.intl.segments.get_mut(&o) {
            d.brk = Some(brk.clone());
        }
        Ok(brk)
    }
}
