// RegExp methods over a user `exec`. Included by `eval.rs` (`include!`), so it shares that module's items.
//
// The builtin paths (`re_match`, `re_replace`, ...) run the engine directly. They are exact only while `exec` is the intrinsic one.
// When `exec` is another callable (an own property, a method of a subclass, a patched `RegExp.prototype.exec`), each path calls
// the algorithm below. It is the one in the spec: RegExpExec, with `flags` and `lastIndex` read and written as properties.
// The species constructor makes the splitter of `split` and the matcher of `matchAll`.

impl<'p> Interp<'p> {
    /// True when `r.exec` is a callable other than the intrinsic one. The caller then takes the generic path.
    fn user_exec(&mut self, r: ObjId) -> Res<bool> {
        let e = self.get_prop(Value::Object(r), self.ext.k_exec)?;
        Ok(match (e, self.ext.exec_fn) {
            (Value::Object(a), Some(b)) if a == b => false,
            _ => self.is_callable(e),
        })
    }

    /// RegExpExec: the result of `exec` (an object or `null`).
    fn re_exec_v(&mut self, r: ObjId, s: StrId) -> Res<Value> {
        let rv = Value::Object(r);
        let e = self.get_prop(rv, self.ext.k_exec)?;
        if self.is_callable(e) && !matches!((e, self.ext.exec_fn), (Value::Object(a), Some(b)) if a == b) {
            let res = self.call_fn(e, rv, &[Value::String(s)])?;
            return match res {
                Value::Object(_) | Value::Null => Ok(res),
                _ => Err(self.type_err("RegExp exec method returned something other than an Object or null")),
            };
        }
        let o = self.this_regexp(rv, "exec")?;
        match self.re_exec_raw(o, s)? {
            None => Ok(Value::Null),
            Some((m, named)) => self.match_result(o, s, &m, named),
        }
    }

    /// `ToString(Get(r, "flags"))` as `(global, unicode, flags)`. `unicode` is true for `u` and `v`.
    fn re_flags_of(&mut self, r: ObjId) -> Res<(bool, bool, StrId)> {
        let k = h!(self, self.heap.key_from_str("flags"));
        let f = self.get_prop(Value::Object(r), k)?;
        let f = self.coerce_string(f)?;
        let u = h!(self, self.heap.str_units(f));
        let has = |c: u8| u.contains(&(c as u16));
        Ok((has(b'g'), has(b'u') || has(b'v'), f))
    }

    /// `ToLength(Get(r, "lastIndex"))`.
    fn re_last_index(&mut self, r: ObjId) -> Res<f64> {
        let v = self.get_prop(Value::Object(r), self.ext.k_last_index)?;
        let n = self.coerce_number(v)?;
        Ok(if n.is_nan() || n <= 0.0 { 0.0 } else { n.trunc().min(9007199254740991.0) })
    }

    /// AdvanceStringIndex.
    fn re_advance(&self, s: StrId, i: f64, unicode: bool) -> Res<f64> {
        let u = self.heap.str_units(s).map_err(Error::Heap)?;
        if !unicode || i + 1.0 >= u.len() as f64 {
            return Ok(i + 1.0);
        }
        let i = i as usize;
        let (hi, lo) = (u[i], u[i + 1]);
        Ok(if (0xD800..0xDC00).contains(&hi) && (0xDC00..0xE000).contains(&lo) { i as f64 + 2.0 } else { i as f64 + 1.0 })
    }

    /// After an empty match: `lastIndex = AdvanceStringIndex(s, lastIndex)`.
    fn re_step_empty(&mut self, r: ObjId, s: StrId, unicode: bool) -> Res<()> {
        let t = self.re_last_index(r)?;
        let n = self.re_advance(s, t, unicode)?;
        self.set_last_index(r, n)
    }

    fn re_room(&self, n: usize) -> Res<()> {
        if n >= self.ext.bl.max_items {
            return Err(Error::Limit(LimitKind::Regexp(RegexpLimit::Items)));
        }
        Ok(())
    }

    /// `ToString(Get(result, "0"))`.
    fn re_matched(&mut self, res: Value) -> Res<StrId> {
        let k = self.index_key(0)?;
        let m = self.get_prop(res, k)?;
        self.coerce_string(m)
    }

    /// `RegExp.prototype.test`.
    fn re_test_g(&mut self, r: ObjId, s: StrId) -> Res<Value> {
        Ok(Value::Bool(!matches!(self.re_exec_v(r, s)?, Value::Null)))
    }

    /// `RegExp.prototype[@@match]`.
    fn re_match_g(&mut self, r: ObjId, s: StrId) -> Res<Value> {
        let (global, unicode, _) = self.re_flags_of(r)?;
        if !global {
            return self.re_exec_v(r, s);
        }
        self.set_last_index(r, 0.0)?;
        let mut items = Vec::new();
        loop {
            self.tick()?;
            let res = self.re_exec_v(r, s)?;
            if matches!(res, Value::Null) {
                return if items.is_empty() { Ok(Value::Null) } else { self.new_array_from(&items) };
            }
            let m = self.re_matched(res)?;
            self.re_room(items.len())?;
            items.push(Value::String(m));
            if h!(self, self.heap.str_len(m)) == 0 {
                self.re_step_empty(r, s, unicode)?;
            }
        }
    }

    /// `RegExp.prototype[@@search]`.
    fn re_search_g(&mut self, r: ObjId, s: StrId) -> Res<Value> {
        let (rv, k) = (Value::Object(r), self.ext.k_last_index);
        let prev = self.get_prop(rv, k)?;
        if !prev.same_value(Value::Number(0.0)) {
            self.set_last_index(r, 0.0)?;
        }
        let res = self.re_exec_v(r, s)?;
        let cur = self.get_prop(rv, k)?;
        if !cur.same_value(prev) {
            let saved = std::mem::replace(&mut self.strict, true);
            let put = self.put_prop(rv, k, prev);
            self.strict = saved;
            put?;
        }
        if matches!(res, Value::Null) {
            return Ok(Value::Number(-1.0));
        }
        let ki = self.ext.k_index;
        self.get_prop(res, ki)
    }

    /// `RegExp.prototype[@@replace]`. `func` is a callable replacement, else `tpl` is the template.
    fn re_replace_g(&mut self, r: ObjId, s: StrId, func: Option<Value>, tpl: Option<StrId>) -> Res<Value> {
        let (global, unicode, _) = self.re_flags_of(r)?;
        if global {
            self.set_last_index(r, 0.0)?;
        }
        let mut results = Vec::new();
        loop {
            self.tick()?;
            let res = self.re_exec_v(r, s)?;
            if matches!(res, Value::Null) {
                break;
            }
            self.re_room(results.len())?;
            results.push(res);
            if !global {
                break;
            }
            let m = self.re_matched(res)?;
            if h!(self, self.heap.str_len(m)) == 0 {
                self.re_step_empty(r, s, unicode)?;
            }
        }
        let len = h!(self, self.heap.str_len(s));
        let (mut out, mut next) = (Vec::new(), 0usize);
        let kg = self.ext.k_groups;
        for res in results {
            self.tick()?;
            let ncap = self.length_of(res)?.saturating_sub(1);
            let matched = self.re_matched(res)?;
            let mlen = h!(self, self.heap.str_len(matched));
            let ki = self.ext.k_index;
            let pos = self.get_prop(res, ki)?;
            let pos = self.int_arg(pos)?.clamp(0.0, len as f64) as usize;
            let mut caps = Vec::new();
            for i in 1..=ncap {
                self.tick()?;
                let k = self.index_key(i)?;
                let c = self.get_prop(res, k)?;
                caps.push(if matches!(c, Value::Undefined) { c } else { Value::String(self.coerce_string(c)?) });
            }
            let groups = self.get_prop(res, kg)?;
            let repl = if let Some(f) = func {
                let mut a = vec![Value::String(matched)];
                a.extend_from_slice(&caps);
                a.extend([Value::Number(pos as f64), Value::String(s)]);
                if !matches!(groups, Value::Undefined) {
                    a.push(groups);
                }
                let v = self.call_fn(f, Value::Undefined, &a)?;
                self.coerce_string(v)?
            } else {
                let named = if matches!(groups, Value::Undefined) { None } else { Some(self.to_object(groups)?) };
                self.get_substitution(matched, s, pos, &caps, named, tpl.unwrap())?
            };
            if pos >= next {
                self.append_string(&mut out, &h!(self, self.heap.str_units(s))[next..pos])?;
                self.append_string(&mut out, h!(self, self.heap.str_units(repl)))?;
                next = pos + mlen;
            }
        }
        if next < len {
            self.append_string(&mut out, &h!(self, self.heap.str_units(s))[next..])?;
        }
        self.spend_units(out.len())?;
        self.new_str(out)
    }

    /// `GetSubstitution` with the captures as values (strings or `undefined`) and an optional named-groups object.
    fn get_substitution(&mut self, matched: StrId, s: StrId, pos: usize, caps: &[Value], named: Option<Value>, tpl: StrId) -> Res<StrId> {
        let t = h!(self, self.heap.str_units(tpl)).to_vec();
        let (src, m) = (h!(self, self.heap.str_units(s)).to_vec(), h!(self, self.heap.str_units(matched)).to_vec());
        let dollar = b'$' as u16;
        let digit = |c: Option<&u16>| c.and_then(|&c| (b'0' as u16..=b'9' as u16).contains(&c).then(|| (c - b'0' as u16) as usize));
        let mut out: Vec<u16> = Vec::new();
        let mut i = 0;
        while i < t.len() {
            self.tick()?;
            if t[i] != dollar || i + 1 >= t.len() {
                out.push(t[i]);
                i += 1;
                continue;
            }
            let c = t[i + 1];
            if c == dollar {
                out.push(dollar);
                i += 2;
            } else if c == b'&' as u16 {
                out.extend_from_slice(&m);
                i += 2;
            } else if c == b'`' as u16 {
                out.extend_from_slice(&src[..pos.min(src.len())]);
                i += 2;
            } else if c == b'\'' as u16 {
                out.extend_from_slice(&src[(pos + m.len()).min(src.len())..]);
                i += 2;
            } else if let Some(d1) = digit(Some(&c)) {
                let two = digit(t.get(i + 2)).map(|d2| d1 * 10 + d2).filter(|&n| n <= caps.len());
                let (n, used) = match two {
                    Some(n) => (n, 3),
                    None => (d1, 2),
                };
                if (1..=caps.len()).contains(&n) {
                    if let Value::String(cs) = caps[n - 1] {
                        out.extend_from_slice(h!(self, self.heap.str_units(cs)));
                    }
                    i += used;
                } else {
                    out.push(dollar);
                    i += 1;
                }
            } else if c == b'<' as u16 {
                match (named, t[i + 2..].iter().position(|&x| x == b'>' as u16)) {
                    (Some(g), Some(end)) => {
                        let name = String::from_utf16_lossy(&t[i + 2..i + 2 + end]);
                        let k = h!(self, self.heap.key_from_str(&name));
                        let v = self.get_prop(g, k)?;
                        if !matches!(v, Value::Undefined) {
                            let vs = self.coerce_string(v)?;
                            out.extend_from_slice(h!(self, self.heap.str_units(vs)));
                        }
                        i += end + 3;
                    }
                    _ => {
                        out.push(dollar);
                        i += 1;
                    }
                }
            } else {
                out.push(dollar);
                i += 1;
            }
        }
        self.spend_units(out.len())?;
        Ok(h!(self, self.heap.intern_vec(out)))
    }

    /// `new C(r, flags)` where `C` is the species constructor of `r` (`RegExp` by default).
    fn re_species_new(&mut self, r: ObjId, flags: StrId) -> Res<ObjId> {
        let rv = Value::Object(r);
        let v = match self.species_ctor_of(rv, self.ext.regexp_ctor)? {
            Some(c) => self.construct(c, &[rv, Value::String(flags)], ExprId::from_index(0))?,
            None => self.regexp_ctor_nt(rv, Value::String(flags), true, None)?,
        };
        let Value::Object(o) = v else { unreachable!("construct gives an object") };
        Ok(o)
    }

    /// `RegExp.prototype[@@split]`.
    fn re_split_g(&mut self, r: ObjId, s: StrId, lim: u32) -> Res<Value> {
        let (_, unicode, flags) = self.re_flags_of(r)?;
        let sticky = h!(self, self.heap.str_units(flags)).contains(&(b'y' as u16));
        let flags = if sticky {
            flags
        } else {
            let mut u = h!(self, self.heap.str_units(flags)).to_vec();
            u.push(b'y' as u16);
            h!(self, self.heap.intern_vec(u))
        };
        let sp = self.re_species_new(r, flags)?;
        if lim == 0 {
            return self.new_array_from(&[]);
        }
        let size = h!(self, self.heap.str_len(s));
        if size == 0 {
            return match self.re_exec_v(sp, s)? {
                Value::Null => self.new_array_from(&[Value::String(s)]),
                _ => self.new_array_from(&[]),
            };
        }
        let mut items: Vec<Value> = Vec::new();
        let (mut p, mut q) = (0usize, 0usize);
        while q < size {
            self.tick()?;
            self.set_last_index(sp, q as f64)?;
            let z = self.re_exec_v(sp, s)?;
            if matches!(z, Value::Null) {
                q = self.re_advance(s, q as f64, unicode)? as usize;
                continue;
            }
            let e = (self.re_last_index(sp)? as usize).min(size);
            if e == p {
                q = self.re_advance(s, q as f64, unicode)? as usize;
                continue;
            }
            self.re_room(items.len())?;
            items.push(Value::String(self.sub(s, p, q)?));
            if items.len() as u64 == lim as u64 {
                return self.new_array_from(&items);
            }
            p = e;
            let ncap = self.length_of(z)?.saturating_sub(1);
            for i in 1..=ncap {
                self.tick()?;
                let k = self.index_key(i)?;
                let c = self.get_prop(z, k)?;
                self.re_room(items.len())?;
                items.push(c);
                if items.len() as u64 == lim as u64 {
                    return self.new_array_from(&items);
                }
            }
            q = p;
        }
        self.re_room(items.len())?;
        items.push(Value::String(self.sub(s, p, size)?));
        self.new_array_from(&items)
    }

    /// `RegExp.prototype[@@matchAll]`: the matcher is `new C(r, flags)`.
    fn re_match_all_g(&mut self, r: ObjId, s: StrId) -> Res<Value> {
        let (global, unicode, flags) = self.re_flags_of(r)?;
        let m = self.re_species_new(r, flags)?;
        let li = self.re_last_index(r)?;
        self.set_last_index(m, li)?;
        let proto = self.re_iter_proto()?;
        self.sc_track_iter(proto, IterState::ReStr { re: m, s, global, unicode, done: false })
    }

    /// `next` of a RegExp String Iterator whose matcher has a user `exec`.
    fn re_iter_next_g(&mut self, o: ObjId, re: ObjId, s: StrId, global: bool, unicode: bool) -> Res<Value> {
        let res = self.re_exec_v(re, s)?;
        let finish = |me: &mut Self| {
            if let Some(IterState::ReStr { done, .. }) = me.sc.iters.get_mut(&o) {
                *done = true;
            }
        };
        if matches!(res, Value::Null) {
            finish(self);
            return self.sc_iter_result(Value::Undefined, true);
        }
        if !global {
            finish(self);
        } else {
            let m = self.re_matched(res)?;
            if h!(self, self.heap.str_len(m)) == 0 {
                self.re_step_empty(re, s, unicode)?;
            }
        }
        self.sc_iter_result(res, false)
    }
}
