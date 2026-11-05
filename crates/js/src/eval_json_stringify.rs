// JSON.stringify for the evaluator. Included by `eval.rs` (`include!`), so it shares that module's imports.
//
// What is bound: `JSON.stringify(value, replacer, space)` as in ECMA-262 (SerializeJSONProperty, -Object, -Array):
// `toJSON`, a callable replacer (receiver = holder, args = key, value), a replacer array (property list: strings
// and numbers, deduplicated, in order), `space` as a number (clamped to 10) or a string (first 10 units), own
// enumerable string keys in `[[OwnPropertyKeys]]` order read BEFORE any value, getters and inherited array
// elements through ordinary `Get`, `undefined`, functions and symbols omitted from objects and `null` in arrays,
// non-finite numbers `null`, `-0` as `0`, lone surrogates as `\udxxx`, a root that serializes to nothing returns
// `undefined`, an active-path cycle is a `TypeError` (a shared acyclic subgraph is legal).
//
// Wrapper objects: a Number, String or Boolean wrapper is written as ToNumber, ToString or its boolean (`sj_value`), also as
// `space` and as an entry of the replacer array. A Symbol wrapper is an object with no keys (`{}`).
// Gates: BigInt wrappers and `BigInt` values do not exist in this evaluator (no BigInt), so those spec steps are not written.
//
// How: no Rust recursion. A stack of `Frame`s (one per open object or array) lives in the call. Callbacks run
// through `call_fn`, so they may re-enter `JSON.stringify`; every piece of state is local to one call. The heap
// never collects during a call, so the handles in the frames stay valid. Nothing persists after the call.
//
// Bounds (all checked BEFORE the growth, all `Error::Limit(Stringify(..))`, never catchable, never `undefined`):
// output units, nesting depth, values visited, and keys / array length / property-list entries of one container.
// Work is also charged to the evaluator step budget: one step per value and per key, one per 32 output units.

/// Which `JSON.stringify` bound ended a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StringifyLimit {
    /// The result would be longer than `max_output_units`.
    Output,
    /// Open objects and arrays nested deeper than `max_depth`.
    Depth,
    /// More than `max_nodes` values visited by one call.
    Nodes,
    /// An object with more than `max_keys` own keys, an array longer than that, or a replacer array longer than that.
    Keys,
}

/// Bounds of `JSON.stringify`, part of [`BuiltinLimits`].
#[derive(Clone, Copy, Debug)]
pub struct StringifyLimits {
    pub max_output_units: usize,
    pub max_depth: usize,
    pub max_nodes: u64,
    pub max_keys: usize,
}

impl Default for StringifyLimits {
    fn default() -> Self {
        StringifyLimits { max_output_units: 1 << 24, max_depth: 1024, max_nodes: 1 << 22, max_keys: 1 << 22 }
    }
}

/// One open object or array. `keys` is `None` for an array (then `len` is its length at open time).
struct SjFrame {
    holder: ObjId,
    keys: Option<Rc<Vec<PropertyKey>>>,
    len: usize,
    next: usize,
    any: bool,
}

/// The state of one `JSON.stringify` call.
struct Sj {
    out: Vec<u16>,
    stack: Vec<SjFrame>,
    /// Objects of the open frames (cycle check).
    active: FxSet<ObjId>,
    replacer: Option<Value>,
    plist: Option<Rc<Vec<PropertyKey>>>,
    gap: Vec<u16>,
    k_to_json: PropertyKey,
    nodes: u64,
    written: usize,
    charged: usize,
    lim: StringifyLimits,
}

enum SjOut {
    Undef,
    Done,
    Open,
}

fn out_limit() -> Error {
    Error::Limit(LimitKind::Stringify(StringifyLimit::Output))
}

impl Sj {
    fn put(&mut self, u: &[u16]) -> Res<()> {
        if u.len() > self.lim.max_output_units - self.out.len() {
            return Err(out_limit());
        }
        self.written += u.len();
        self.out.extend_from_slice(u);
        Ok(())
    }

    fn put1(&mut self, u: u16) -> Res<()> {
        self.put(&[u])
    }

    fn put_ascii(&mut self, s: &str) -> Res<()> {
        if s.len() > self.lim.max_output_units - self.out.len() {
            return Err(out_limit());
        }
        self.written += s.len();
        self.out.extend(s.bytes().map(u16::from));
        Ok(())
    }

    /// `QuoteJSONString`: lone surrogates and controls are escaped, pairs are kept.
    fn quote(&mut self, s: &[u16]) -> Res<()> {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        self.put1(0x22)?;
        let (mut i, mut start) = (0, 0);
        while i < s.len() {
            let u = s[i];
            let short: Option<u8> = match u {
                0x22 => Some(b'"'),
                0x5c => Some(b'\\'),
                0x08 => Some(b'b'),
                0x09 => Some(b't'),
                0x0a => Some(b'n'),
                0x0c => Some(b'f'),
                0x0d => Some(b'r'),
                0xD800..=0xDBFF if i + 1 < s.len() && (0xDC00..=0xDFFF).contains(&s[i + 1]) => {
                    i += 2;
                    continue;
                }
                0..=0x1f | 0xD800..=0xDFFF => None,
                _ => {
                    i += 1;
                    continue;
                }
            };
            self.put(&s[start..i])?;
            match short {
                Some(c) => self.put(&[0x5c, c as u16])?,
                None => self.put(&[0x5c, b'u' as u16, HEX[(u >> 12) as usize & 15] as u16, HEX[(u >> 8) as usize & 15] as u16, HEX[(u >> 4) as usize & 15] as u16, HEX[u as usize & 15] as u16])?,
            }
            i += 1;
            start = i;
        }
        self.put(&s[start..])?;
        self.put1(0x22)
    }

    /// Newline and `depth` copies of the gap (nothing when there is no gap).
    fn indent(&mut self, depth: usize) -> Res<()> {
        if self.gap.is_empty() {
            return Ok(());
        }
        let n = self.gap.len().saturating_mul(depth);
        if n.saturating_add(1) > self.lim.max_output_units - self.out.len() {
            return Err(out_limit());
        }
        self.put1(0x0a)?;
        for _ in 0..depth {
            let g = std::mem::take(&mut self.gap);
            let r = self.put(&g);
            self.gap = g;
            r?;
        }
        Ok(())
    }
}

impl<'p> Interp<'p> {
    fn json_stringify(&mut self, value: Value, replacer: Value, space: Value) -> Res<Value> {
        let lim = self.ext.bl.stringify;
        let k_to_json = h!(self, self.heap.key_from_str("toJSON"));
        let mut st = Sj { out: Vec::new(), stack: Vec::new(), active: FxSet::default(), replacer: None, plist: None, gap: Vec::new(), k_to_json, nodes: 0, written: 0, charged: 0, lim };
        if let Value::Object(o) = replacer {
            if self.is_callable(replacer) {
                st.replacer = Some(replacer);
            } else if self.o_is_array(o)? {
                st.plist = Some(Rc::new(self.sj_property_list(o, &lim)?));
            }
        }
        // A Number or String wrapper as `space` is its number or string.
        let space = match space {
            Value::Object(o) => match self.heap.primitive_value(o) {
                Some(Value::Number(_)) => Value::Number(self.coerce_number(space)?),
                Some(Value::String(_)) => Value::String(self.coerce_string(space)?),
                _ => space,
            },
            _ => space,
        };
        match space {
            Value::Number(n) => {
                let k = if n.is_nan() { 0.0 } else { n.trunc().min(10.0) };
                if k >= 1.0 {
                    st.gap = vec![0x20; k as usize];
                }
            }
            Value::String(s) => st.gap = h!(self, self.heap.str_units(s)).iter().take(10).copied().collect(),
            _ => {}
        }
        let wrapper = h!(self, self.heap.new_object(Some(self.object_proto)));
        let k_empty = h!(self, self.heap.key_from_str(""));
        self.def(wrapper, k_empty, value, true, true, true)?;
        let holder = Value::Object(wrapper);
        let value = self.sj_prepare(&mut st, k_empty, holder, value)?;
        if let SjOut::Undef = self.sj_value(&mut st, value)? {
            return Ok(Value::Undefined);
        }
        while let Some(fr) = st.stack.last() {
            let depth = st.stack.len();
            let (holder, is_arr, i, any) = (fr.holder, fr.keys.is_none(), fr.next, fr.any);
            if i >= fr.len {
                st.stack.pop();
                st.active.remove(&holder);
                if any {
                    st.indent(depth - 1)?;
                }
                st.put1(if is_arr { 0x5d } else { 0x7d })?;
                continue;
            }
            self.tick()?;
            let key = match &fr.keys {
                Some(k) => k[i],
                None => PropertyKey::Index(i as u32),
            };
            st.stack[depth - 1].next += 1;
            let holder_v = Value::Object(holder);
            let v = self.get_prop(holder_v, key)?;
            let v = self.sj_prepare(&mut st, key, holder_v, v)?;
            let omitted = matches!(v, Value::Undefined | Value::Symbol(_)) || self.is_callable(v);
            if omitted && !is_arr {
                continue;
            }
            if any {
                st.put1(0x2c)?;
            }
            st.indent(depth)?;
            if !is_arr {
                match key {
                    PropertyKey::String(s) => st.quote(h!(self, self.heap.str_units(s)))?,
                    k => st.quote(&self.key_text(k).encode_utf16().collect::<Vec<u16>>())?,
                }
                st.put1(0x3a)?;
                if !st.gap.is_empty() {
                    st.put1(0x20)?;
                }
            }
            match self.sj_value(&mut st, v)? {
                SjOut::Undef if is_arr => {
                    st.put_ascii("null")?;
                    st.stack[depth - 1].any = true;
                }
                SjOut::Undef => unreachable!("omitted object properties are skipped before writing"),
                _ => st.stack[depth - 1].any = true,
            }
            let d = st.written - st.charged;
            st.charged += d / 32 * 32;
            self.spend(d as u64 / 32)?;
        }
        let d = st.written - st.charged;
        self.spend(d as u64 / 32)?;
        Ok(Value::String(h!(self, self.heap.intern_vec(st.out))))
    }

    /// The replacer array as a property list: strings and numbers, deduplicated, in order.
    fn sj_property_list(&mut self, o: ObjId, lim: &StringifyLimits) -> Res<Vec<PropertyKey>> {
        let len = match hx!(self, self.heap.get_own_property(o, self.k_length), self.px_length_prop(o)?) {
            Some(Property::Data { value: Value::Number(n), .. }) => n as usize,
            _ => 0,
        };
        if len > lim.max_keys {
            return Err(Error::Limit(LimitKind::Stringify(StringifyLimit::Keys)));
        }
        let (mut list, mut seen) = (Vec::new(), FxSet::default());
        for i in 0..len {
            self.tick()?;
            let item = match self.get_prop(Value::Object(o), PropertyKey::Index(i as u32))? {
                Value::String(s) => s,
                Value::Number(n) => h!(self, self.heap.intern_str(&number_to_string(n))),
                v @ Value::Object(o) if matches!(self.heap.primitive_value(o), Some(Value::String(_) | Value::Number(_))) => self.coerce_string(v)?,
                _ => continue,
            };
            let key = h!(self, self.heap.key_from_string(item));
            if seen.insert(key) {
                list.push(key);
            }
        }
        Ok(list)
    }

    /// `SerializeJSONProperty` after the `Get`: toJSON, replacer, then primitives are written and an object opens a frame.
    fn sj_prepare(&mut self, st: &mut Sj, key: PropertyKey, holder: Value, mut v: Value) -> Res<Value> {
        self.tick()?;
        st.nodes += 1;
        if st.nodes > st.lim.max_nodes {
            return Err(Error::Limit(LimitKind::Stringify(StringifyLimit::Nodes)));
        }
        let mut key_v = None;
        if let Value::Object(_) = v {
            let f = self.get_prop(v, st.k_to_json)?;
            if self.is_callable(f) {
                let kv = *key_v.insert(h!(self, self.heap.key_to_value(key)));
                v = self.call_fn(f, v, &[kv])?;
            }
        }
        if let Some(r) = st.replacer {
            let kv = match key_v {
                Some(k) => k,
                None => h!(self, self.heap.key_to_value(key)),
            };
            v = self.call_fn(r, holder, &[kv, v])?;
        }
        Ok(v)
    }

    fn sj_value(&mut self, st: &mut Sj, v: Value) -> Res<SjOut> {
        match v {
            Value::Null => st.put_ascii("null")?,
            Value::Bool(b) => st.put_ascii(if b { "true" } else { "false" })?,
            Value::Number(n) if n.is_finite() => st.put_ascii(&number_to_string(n))?,
            Value::Number(_) => st.put_ascii("null")?,
            Value::String(s) => {
                self.spend_units(h!(self, self.heap.str_len(s)))?;
                st.quote(h!(self, self.heap.str_units(s)))?
            }
            Value::Object(o) => {
                if self.is_callable(v) {
                    return Ok(SjOut::Undef);
                }
                // A Number, String or Boolean wrapper is written as its primitive. A Symbol wrapper stays an object.
                if self.heap.has_exotics() && let Some(p) = self.heap.primitive_value(o) && !matches!(p, Value::Symbol(_)) {
                    let u = match p {
                        Value::Number(_) => Value::Number(self.coerce_number(v)?),
                        Value::String(_) => Value::String(self.coerce_string(v)?),
                        _ => p,
                    };
                    return self.sj_value(st, u);
                }
                self.sj_open(st, o)?;
                return Ok(SjOut::Open);
            }
            Value::Undefined | Value::Symbol(_) => return Ok(SjOut::Undef),
        }
        Ok(SjOut::Done)
    }

    /// Open an object or an array: cycle and depth checks, key list or length, opening bracket.
    fn sj_open(&mut self, st: &mut Sj, o: ObjId) -> Res<()> {
        if st.active.contains(&o) {
            return Err(self.type_err("Converting circular structure to JSON"));
        }
        if st.stack.len() >= st.lim.max_depth {
            return Err(Error::Limit(LimitKind::Stringify(StringifyLimit::Depth)));
        }
        let keys_limit = || Error::Limit(LimitKind::Stringify(StringifyLimit::Keys));
        let (keys, len, open) = if self.o_is_array(o)? {
            let len = match hx!(self, self.heap.get_own_property(o, self.k_length), self.px_length_prop(o)?) {
                Some(Property::Data { value: Value::Number(n), .. }) => n as usize,
                _ => 0,
            };
            if len > st.lim.max_keys {
                return Err(keys_limit());
            }
            (None, len, 0x5b)
        } else if let Some(p) = &st.plist {
            (Some(p.clone()), p.len(), 0x7b)
        } else {
            let all = self.property_keys(o)?;
            if all.len() > st.lim.max_keys {
                return Err(keys_limit());
            }
            self.spend(all.len() as u64)?;
            let mut list = Vec::new();
            for k in all {
                if !matches!(k, PropertyKey::Symbol(_)) && self.o_peek(o, k)?.is_some_and(|p| p.enumerable()) {
                    list.push(k);
                }
            }
            let n = list.len();
            (Some(Rc::new(list)), n, 0x7b)
        };
        st.put1(open)?;
        st.active.insert(o);
        st.stack.push(SjFrame { holder: o, keys, len, next: 0, any: false });
        Ok(())
    }
}
