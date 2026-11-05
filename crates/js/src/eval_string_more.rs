// The String built-ins of batch 1. Included by `eval.rs` (`include!`), so it shares that module's imports and private items.
// The natives are `Nat::Ext(Bi::..)`; `call_ext` (in `eval_builtins.rs`) forwards the new ones to `call_str_more`.
//
// Supported
// - `String.raw`.
// - `String.prototype`: at, codePointAt, lastIndexOf, padStart, padEnd, repeat, replaceAll, search, normalize, trimStart,
//   trimEnd, trimLeft and trimRight (the same function objects as trimStart and trimEnd), isWellFormed, toWellFormed, substr,
//   toLocaleLowerCase, toLocaleUpperCase. `RegExp.prototype[Symbol.search]`.
// - `replaceAll` and `search` use the hook of `str_hook`, like `replace`: a user method of the argument runs, a builtin regexp
//   takes the builtin path, anything else is a string. `replaceAll` of a regexp without `g` is a TypeError, checked before the
//   hook, as in the specification. All matches of a string pattern are found before one callback runs.
// - `normalize`: NFC and NFD, through `bare-unicode`. A string of units that no form changes (below U+0300 for NFC, below U+00C0
//   for NFD) is returned as it is, without a copy. A lone surrogate stays where it is.
// - `trimStart` and `trimEnd` name `trimLeft` and `trimRight` in a TypeError for a null `this`, as V8 does.
// - `toLocale*Case` take the root locale. The locale list is validated by `intl_locales`.
// - A result that is longer than the string limits is a limit error, as for every other string builder; it is checked
//   before the units are made, so `"a".repeat(2 ** 30)` allocates nothing.
//
// Gates (never silent)
// - `normalize("NFKC")` and `normalize("NFKD")`: the compatibility tables are not in `bare-unicode`.
// - `toLocaleLowerCase` and `toLocaleUpperCase` with a locale whose language is `tr`, `az` or `lt`: these have their own
//   case rules.
//
// Left out: `matchAll`. It needs a RegExp String Iterator, which has its own state in the iterator machinery of
// `eval_symbols.rs`.

/// Work in units that a scan costs, charged 1 step per 8. `false` when the budget ran out.
fn charge_work(work: &mut u64, budget: &mut u64) -> bool {
    let steps = *work / 8;
    *work -= steps * 8;
    if steps > *budget {
        *budget = 0;
        return false;
    }
    *budget -= steps;
    true
}

/// Work-bounded `lastIndexOf` on code units: the last match at or before `from`. `None` when the budget ran out.
fn rfind_units(hay: &[u16], needle: &[u16], from: usize, budget: &mut u64) -> Option<Option<usize>> {
    let n = needle.len();
    if hay.len() < n {
        return Some(None);
    }
    let mut i = from.min(hay.len() - n);
    if n == 0 {
        return Some(Some(i));
    }
    let mut work = 0u64;
    loop {
        work += 1;
        if hay[i] == needle[0] {
            let mut m = 0;
            while m < n {
                let end = (m + 256).min(n);
                let matched = hay[i + m..i + end].iter().zip(&needle[m..end]).take_while(|(a, b)| a == b).count();
                work += matched as u64;
                if !charge_work(&mut work, budget) {
                    return None;
                }
                m += matched;
                if m < end {
                    break;
                }
            }
            if m == n {
                return charge_work(&mut work, budget).then_some(Some(i));
            }
        }
        if !charge_work(&mut work, budget) {
            return None;
        }
        if i == 0 {
            return Some(None);
        }
        i -= 1;
    }
}

/// The index of the first lone surrogate of `u`.
fn first_lone_surrogate(u: &[u16]) -> Option<usize> {
    let mut i = 0;
    while i < u.len() {
        match u[i] {
            0xD800..=0xDBFF if matches!(u.get(i + 1), Some(0xDC00..=0xDFFF)) => i += 2,
            0xD800..=0xDFFF => return Some(i),
            _ => i += 1,
        }
    }
    None
}

impl<'p> Interp<'p> {
    fn install_string_more(&mut self, sc: ObjId, sp: ObjId, rp: ObjId) -> Res<()> {
        self.method(sc, "raw", Nat::Ext(Bi::StrRaw), 1)?;
        for (name, x, len) in [
            ("at", Bi::StrAt, 1), ("codePointAt", Bi::StrCodePointAt, 1), ("lastIndexOf", Bi::StrLastIndexOf, 1),
            ("padStart", Bi::StrPadStart, 1), ("padEnd", Bi::StrPadEnd, 1), ("repeat", Bi::StrRepeat, 1),
            ("replaceAll", Bi::StrReplaceAll, 2), ("search", Bi::StrSearch, 1), ("normalize", Bi::StrNormalize, 0),
            ("isWellFormed", Bi::StrIsWellFormed, 0), ("toWellFormed", Bi::StrToWellFormed, 0), ("substr", Bi::StrSubstr, 2),
            ("toLocaleLowerCase", Bi::StrToLocaleLower, 0), ("toLocaleUpperCase", Bi::StrToLocaleUpper, 0),
        ] {
            self.method(sp, name, Nat::Ext(x), len)?;
        }
        for (name, alias, x) in [("trimStart", "trimLeft", Bi::StrTrimStart), ("trimEnd", "trimRight", Bi::StrTrimEnd)] {
            let f = self.method(sp, name, Nat::Ext(x), 0)?;
            let k = h!(self, self.heap.key_from_str(alias));
            self.def(sp, k, Value::Object(f), true, false, true)?;
        }
        let f = self.native(Nat::Ext(Bi::ReSymSearch), "[Symbol.search]", 1)?;
        self.def(rp, self.wk_key(WK_SEARCH), Value::Object(f), true, false, true)
    }

    /// Fail before a string of `n` units is made when it is over the string or byte limit.
    fn sm_check_len(&self, n: f64) -> Res<()> {
        let l = self.heap.limits();
        if n > l.max_string_units as f64 {
            return Err(Error::Limit(LimitKind::Heap(Limit::StringLength)));
        }
        if n > (l.max_bytes / 2) as f64 {
            return Err(Error::Limit(LimitKind::Heap(Limit::Bytes)));
        }
        Ok(())
    }

    /// Budgeted `lastIndexOf` of `needle` in `hay` from `from`.
    fn rsearch(&mut self, hay: StrId, needle: StrId, from: usize) -> Res<Option<usize>> {
        let mut budget = self.step_limit.saturating_sub(self.steps);
        let before = budget;
        let r = {
            let (h, n) = (h!(self, self.heap.str_units(hay)), h!(self, self.heap.str_units(needle)));
            rfind_units(h, n, from, &mut budget)
        };
        self.steps += before - budget;
        r.ok_or(Error::Limit(LimitKind::Steps))
    }

    fn call_str_more(&mut self, x: Bi, this: Value, args: &[Value]) -> Res<Value> {
        let (a0, a1) = (arg(args, 0), arg(args, 1));
        match x {
            Bi::StrRaw => self.str_raw(args),
            Bi::StrAt => {
                let s = self.this_str(this, "at")?;
                let len = h!(self, self.heap.str_len(s)) as f64;
                let rel = self.int_arg(a0)?;
                let k = if rel >= 0.0 { rel } else { len + rel };
                if k < 0.0 || k >= len {
                    return Ok(Value::Undefined);
                }
                Ok(Value::String(self.sub(s, k as usize, k as usize + 1)?))
            }
            Bi::StrCodePointAt => {
                let s = self.this_str(this, "codePointAt")?;
                let len = h!(self, self.heap.str_len(s));
                let p = self.int_arg(a0)?;
                if p < 0.0 || p >= len as f64 {
                    return Ok(Value::Undefined);
                }
                let u = h!(self, self.heap.str_units(s));
                let (i, c) = (p as usize, u[p as usize] as u32);
                Ok(Value::Number(match u.get(i + 1) {
                    Some(&d) if (0xD800..0xDC00).contains(&c) && (0xDC00..0xE000).contains(&(d as u32)) => ((c - 0xD800) << 10 | (d as u32 - 0xDC00)) as f64 + 65536.0,
                    _ => c as f64,
                }))
            }
            Bi::StrLastIndexOf => {
                let s = self.this_str(this, "lastIndexOf")?;
                let needle = self.coerce_string(a0)?;
                let n = self.coerce_number(a1)?;
                let (len, m) = (h!(self, self.heap.str_len(s)), h!(self, self.heap.str_len(needle)));
                if m > len {
                    return Ok(Value::Number(-1.0));
                }
                let from = (if n.is_nan() { f64::INFINITY } else { n.trunc() }).clamp(0.0, len as f64) as usize;
                Ok(Value::Number(self.rsearch(s, needle, from)?.map_or(-1.0, |i| i as f64)))
            }
            Bi::StrPadStart | Bi::StrPadEnd => {
                let s = self.this_str(this, if x == Bi::StrPadStart { "padStart" } else { "padEnd" })?;
                let len = h!(self, self.heap.str_len(s));
                let n = self.coerce_number(a0)?;
                let max = if n.is_nan() || n <= 0.0 { 0.0 } else { n.trunc().min(9007199254740991.0) };
                if max <= len as f64 {
                    return Ok(Value::String(s));
                }
                let fill = if matches!(a1, Value::Undefined) { h!(self, self.heap.intern_str(" ")) } else { self.coerce_string(a1)? };
                if h!(self, self.heap.str_len(fill)) == 0 {
                    return Ok(Value::String(s));
                }
                self.sm_check_len(max)?;
                let total = max as usize;
                self.spend_units(total)?;
                let (su, fu) = (h!(self, self.heap.str_units(s)), h!(self, self.heap.str_units(fill)));
                let mut out = Vec::with_capacity(total);
                let pad = fu.iter().copied().cycle().take(total - len);
                if x == Bi::StrPadStart {
                    out.extend(pad);
                    out.extend_from_slice(su);
                } else {
                    out.extend_from_slice(su);
                    out.extend(pad);
                }
                self.new_str(out)
            }
            Bi::StrRepeat => {
                let s = self.this_str(this, "repeat")?;
                let num = self.coerce_number(a0)?;
                let n = if num.is_nan() { 0.0 } else { num.trunc() };
                if n < 0.0 || n == f64::INFINITY {
                    return Err(self.range_error(&format!("Invalid count value: {}", number_to_string(num))));
                }
                let len = h!(self, self.heap.str_len(s));
                if len == 0 || n == 0.0 {
                    return self.str_value("");
                }
                let total = len as f64 * n;
                self.sm_check_len(total)?;
                self.spend_units(total as usize)?;
                let out = h!(self, self.heap.str_units(s)).repeat(n as usize);
                self.new_str(out)
            }
            Bi::StrReplaceAll => self.str_replace_all(this, a0, a1),
            Bi::StrSearch => {
                let hook = self.str_hook(this, "search", a0, WK_SEARCH, &[this])?;
                let s = self.this_str(this, "search")?;
                let r = match hook {
                    Hook::Done(v) => return Ok(v),
                    Hook::Builtin(r) => r,
                    Hook::Absent => {
                        let p = if matches!(a0, Value::Undefined) { h!(self, self.heap.intern_str("")) } else { self.coerce_string(a0)? };
                        let empty = h!(self, self.heap.intern_str(""));
                        self.regexp_create(p, empty)?
                    }
                };
                self.re_search(r, s)
            }
            Bi::ReSymSearch => {
                let r = self.search_receiver(this)?;
                let s = self.coerce_string(a0)?;
                self.re_search(r, s)
            }
            Bi::StrNormalize => self.str_normalize(this, a0),
            Bi::StrTrimStart | Bi::StrTrimEnd => {
                let s = self.this_str(this, if x == Bi::StrTrimStart { "trimLeft" } else { "trimRight" })?;
                let len = h!(self, self.heap.str_len(s));
                self.spend_units(len)?;
                let (a, b) = {
                    let u = h!(self, self.heap.str_units(s));
                    if x == Bi::StrTrimStart {
                        (u.iter().position(|&c| !is_js_space(c)).unwrap_or(len), len)
                    } else {
                        (0, u.iter().rposition(|&c| !is_js_space(c)).map_or(0, |i| i + 1))
                    }
                };
                if (a, b) == (0, len) {
                    return Ok(Value::String(s));
                }
                Ok(Value::String(self.sub(s, a, b)?))
            }
            Bi::StrIsWellFormed | Bi::StrToWellFormed => {
                let s = self.this_str(this, if x == Bi::StrIsWellFormed { "isWellFormed" } else { "toWellFormed" })?;
                let len = h!(self, self.heap.str_len(s));
                self.spend_units(len)?;
                let first = first_lone_surrogate(h!(self, self.heap.str_units(s)));
                if x == Bi::StrIsWellFormed {
                    return Ok(Value::Bool(first.is_none()));
                }
                let Some(mut i) = first else { return Ok(Value::String(s)) };
                let mut out = h!(self, self.heap.str_units(s)).to_vec();
                loop {
                    out[i] = 0xFFFD;
                    match first_lone_surrogate(&out[i + 1..]) {
                        Some(k) => i += 1 + k,
                        None => break,
                    }
                }
                self.new_str(out)
            }
            Bi::StrSubstr => {
                let s = self.this_str(this, "substr")?;
                let size = h!(self, self.heap.str_len(s)) as f64;
                let start = self.int_arg(a0)?;
                let start = if start < 0.0 { (size + start).max(0.0) } else { start.min(size) };
                let count = if matches!(a1, Value::Undefined) { size } else { self.int_arg(a1)?.clamp(0.0, size) };
                let end = (start + count).min(size);
                if start >= end {
                    return self.str_value("");
                }
                Ok(Value::String(self.sub(s, start as usize, end as usize)?))
            }
            Bi::StrToLocaleLower | Bi::StrToLocaleUpper => {
                let upper = x == Bi::StrToLocaleUpper;
                let s = self.this_str(this, if upper { "toLocaleUpperCase" } else { "toLocaleLowerCase" })?;
                if let Some(t) = self.intl_locales(a0)?.first()
                    && matches!(t.split('-').next(), Some("tr" | "az" | "lt"))
                {
                    return Err(unsupported("toLocaleLowerCase and toLocaleUpperCase for tr, az and lt", Span::default()));
                }
                let out = self.case_map(s, upper)?;
                self.new_str(out)
            }
            _ => unreachable!("call_ext handles the other natives"),
        }
    }

    /// `String.raw(template, ...substitutions)`.
    fn str_raw(&mut self, args: &[Value]) -> Res<Value> {
        let obj_err = |s: &mut Self| s.type_err("Cannot convert undefined or null to object");
        let t = arg(args, 0);
        if is_nullish(t) {
            return Err(obj_err(self));
        }
        let kr = h!(self, self.heap.key_from_str("raw"));
        let raw = self.get_prop(t, kr)?;
        if is_nullish(raw) {
            return Err(obj_err(self));
        }
        let len = self.length_of(raw)?;
        let mut out: Vec<u16> = Vec::new();
        for i in 0..len {
            self.tick()?;
            let k = self.index_key(i)?;
            let v = self.get_prop(raw, k)?;
            let seg = self.coerce_string(v)?;
            self.append_string(&mut out, h!(self, self.heap.str_units(seg)))?;
            if i + 1 < len && (i as usize) + 1 < args.len() {
                let sub = self.coerce_string(args[i as usize + 1])?;
                self.append_string(&mut out, h!(self, self.heap.str_units(sub)))?;
            }
        }
        self.spend_units(out.len())?;
        self.new_str(out)
    }

    /// `String.prototype.replaceAll`.
    fn str_replace_all(&mut self, this: Value, sv: Value, rv: Value) -> Res<Value> {
        if is_nullish(this) {
            return Err(self.type_err("String.prototype.replaceAll called on null or undefined"));
        }
        if !is_nullish(sv) && self.is_regexp(sv)? {
            let kf = h!(self, self.heap.key_from_str("flags"));
            let fl = self.get_prop(sv, kf)?;
            if is_nullish(fl) {
                return Err(self.type_err("String.prototype.replaceAll called on null or undefined"));
            }
            let fl = self.coerce_string(fl)?;
            if !h!(self, self.heap.str_units(fl)).contains(&(b'g' as u16)) {
                return Err(self.type_err("String.prototype.replaceAll called with a non-global RegExp argument"));
            }
        }
        let hook = self.str_hook(this, "replaceAll", sv, WK_REPLACE, &[this, rv])?;
        let s = self.this_str(this, "replaceAll")?;
        let func = if self.is_callable(rv) { Some(rv) } else { None };
        match hook {
            Hook::Done(v) => return Ok(v),
            Hook::Builtin(r) => return self.re_replace(r, s, func, rv),
            Hook::Absent => {}
        }
        let search = self.coerce_string(sv)?;
        let tpl = if func.is_none() { Some(self.coerce_string(rv)?) } else { None };
        let (len, m) = (h!(self, self.heap.str_len(s)), h!(self, self.heap.str_len(search)));
        self.ao_temp(len + 1, 8)?;
        let step = m.max(1);
        let mut v: Vec<(u32, u32)> = Vec::new();
        let mut at = 0;
        while let Some(p) = self.search(s, search, at)? {
            self.tick()?;
            v.push((p as u32, (p + m) as u32));
            at = p + step;
        }
        if v.is_empty() {
            return Ok(Value::String(s));
        }
        self.build_replacement(&MatchList { n: 1, v }, s, func, tpl)
    }

    /// The receiver of `RegExp.prototype[@@search]`: a builtin regexp. Another object would run its own `exec` (`Unsupported`)
    /// or fail in the builtin `exec`, as V8 does.
    fn search_receiver(&mut self, this: Value) -> Res<ObjId> {
        if let Some(r) = self.regexp_of(this) {
            return Ok(r);
        }
        if !matches!(this, Value::Object(_)) {
            let t = self.display(this);
            return Err(self.type_err(&format!("Method RegExp.prototype.@@search called on incompatible receiver {t}")));
        }
        let e = self.get_prop(this, self.ext.k_exec)?;
        if self.is_callable(e) {
            return Err(unsupported("RegExp.prototype[Symbol.search] on a non-RegExp object", Span::default()));
        }
        let t = self.ro_text(this);
        Err(self.type_err(&format!("Method RegExp.prototype.exec called on incompatible receiver {t}")))
    }

    /// `RegExp.prototype[@@search]` on a builtin regexp: the index of the first match, with `lastIndex` kept.
    fn re_search(&mut self, r: ObjId, s: StrId) -> Res<Value> {
        self.gate_exec(r)?;
        let (rv, k) = (Value::Object(r), self.ext.k_last_index);
        let prev = self.get_prop(rv, k)?;
        if !prev.same_value(Value::Number(0.0)) {
            self.set_last_index(r, 0.0)?;
        }
        let m = self.re_exec_raw(r, s)?;
        let cur = self.get_prop(rv, k)?;
        if !cur.same_value(prev) {
            let saved = std::mem::replace(&mut self.strict, true);
            let res = self.put_prop(rv, k, prev);
            self.strict = saved;
            res?;
        }
        Ok(Value::Number(m.map_or(-1.0, |m| m.spans[0].map_or(0, |x| x.0) as f64)))
    }

    /// `String.prototype.normalize`. NFC and NFD only.
    fn str_normalize(&mut self, this: Value, form: Value) -> Res<Value> {
        let s = self.this_str(this, "normalize")?;
        let name = if matches!(form, Value::Undefined) {
            "NFC".to_string()
        } else {
            let f = self.coerce_string(form)?;
            h!(self, self.heap.to_string_lossy(f))
        };
        let nfc = match name.as_str() {
            "NFC" => true,
            "NFD" => false,
            "NFKC" | "NFKD" => return Err(unsupported("String.prototype.normalize with NFKC or NFKD", Span::default())),
            _ => return Err(self.range_error("The normalization form should be one of NFC, NFD, NFKC, NFKD.")),
        };
        let len = h!(self, self.heap.str_len(s));
        self.spend_units(len.saturating_mul(4))?;
        let stable = if nfc { 0x300 } else { 0xC0 };
        if h!(self, self.heap.str_units(s)).iter().all(|&c| c < stable) {
            return Ok(Value::String(s));
        }
        self.ao_temp(len, 12)?;
        let cap = self.heap.limits().max_string_units;
        let src = h!(self, self.heap.str_units(s));
        let (mut out, mut seg): (Vec<u16>, String) = (Vec::new(), String::new());
        let flush = |seg: &mut String, out: &mut Vec<u16>| -> Res<()> {
            if seg.is_empty() {
                return Ok(());
            }
            let m = if nfc { bare_unicode::nfc(seg) } else { bare_unicode::nfd(seg) };
            if m.encode_utf16().count() > cap.saturating_sub(out.len()) {
                return Err(Error::Limit(LimitKind::Heap(Limit::StringLength)));
            }
            out.extend(m.encode_utf16());
            seg.clear();
            Ok(())
        };
        for r in char::decode_utf16(src.iter().copied()) {
            match r {
                Ok(c) => seg.push(c),
                Err(e) => {
                    flush(&mut seg, &mut out)?;
                    out.push(e.unpaired_surrogate());
                }
            }
        }
        flush(&mut seg, &mut out)?;
        if out[..] == *src {
            return Ok(Value::String(s));
        }
        self.new_str(out)
    }
}
