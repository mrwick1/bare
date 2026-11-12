// Values as short text for tools (the `eval` tool of bare-mcp). Included by `eval.rs` (`include!`), so it shares that module's imports.
//
// `Interp::inspect` gives a string as it is, a number, boolean, null or undefined as `display` does, and an object as compact JSON.
// It walks the engine values in Rust; it does not run `JSON.stringify`. Rules that differ from `JSON.stringify`:
// - A function is the string `"function name()"`. A Date is its ISO string. A Map is a list of `[key, value]` pairs; a Set is a list of its values.
// - A host object that `node` names (a DOM node) is its description, for example `"<div#out.a>"`. A NodeList or an HTMLCollection is a list.
// - An object that is already open (a cycle) is the string `"[Circular]"`. A getter that throws gives `"[threw]"`. `toJSON` is not called.
// - The text stops at `cap` characters; then a marker gives the full length. The walk visits at most `INSPECT_VALUES` values and
//   `INSPECT_DEPTH` levels, so a huge or deep value cannot hang the tool.
// At the top level a function, a Date and a node show without the JSON quotes.

/// The most values that one `inspect` visits.
const INSPECT_VALUES: u32 = 100_000;
/// The deepest nesting that `inspect` opens; a deeper object is `"…"`.
const INSPECT_DEPTH: usize = 64;

/// The state of one `inspect` call.
struct Ins<'a> {
    out: String,
    /// The characters of the whole text, also the ones after the cap.
    chars: usize,
    cap: usize,
    /// The objects that are open now (the cycle check).
    open: Vec<ObjId>,
    values: u32,
    node: &'a dyn Fn(ObjId) -> Option<String>,
}

impl Ins<'_> {
    fn put(&mut self, s: &str) {
        let n = s.chars().count();
        if self.chars < self.cap {
            // `chars().take(k)` keeps whole characters, unlike a byte slice (JavaScript `slice` counts UTF-16 units instead).
            self.out.extend(s.chars().take(self.cap - self.chars));
        }
        self.chars += n;
    }

    /// A JSON string: quotes, and escapes for `"`, `\` and control characters.
    fn quote(&mut self, s: &str) {
        let mut q = String::with_capacity(s.len() + 2);
        q.push('"');
        for c in s.chars() {
            match c {
                '"' => q.push_str("\\\""),
                '\\' => q.push_str("\\\\"),
                '\n' => q.push_str("\\n"),
                '\r' => q.push_str("\\r"),
                '\t' => q.push_str("\\t"),
                c if (c as u32) < 0x20 => q.push_str(&format!("\\u{:04x}", c as u32)),
                c => q.push(c),
            }
        }
        q.push('"');
        self.put(&q);
    }
}

impl Interp<'_> {
    /// Text of `v` for tools: see the header of `eval_inspect.rs`. `node` describes a host object, or gives `None`.
    pub fn inspect(&mut self, v: Value, cap: usize, node: &dyn Fn(ObjId) -> Option<String>) -> String {
        let Value::Object(o) = v else { return self.display(v) };
        if let Some(s) = self.ins_leaf(o, node) {
            return s;
        }
        let mut st = Ins { out: String::new(), chars: 0, cap, open: Vec::new(), values: 0, node };
        // `host_scope` starts the step budget, so that a getter that the walk calls cannot run forever.
        let _ = self.host_scope(|it| {
            it.ins_value(&mut st, v, true);
            Ok(())
        });
        let mut out = st.out;
        if st.values > INSPECT_VALUES {
            out.push_str(&format!("… (cut: {} of more than {} chars)", cap.min(st.chars), st.chars));
        } else if st.chars > cap {
            out.push_str(&format!("… (cut: {cap} of {} chars)", st.chars));
        }
        out
    }

    /// The text of an object that shows as one string (a node, a function, a Date), or `None` for a container.
    fn ins_leaf(&mut self, o: ObjId, node: &dyn Fn(ObjId) -> Option<String>) -> Option<String> {
        if let Some(d) = node(o) {
            return Some(d);
        }
        if self.is_callable(Value::Object(o)) {
            let name = self.get_prop(Value::Object(o), self.k_name).ok().filter(|n| matches!(n, Value::String(_))).map(|n| self.display(n)).unwrap_or_default();
            return Some(format!("function {name}()"));
        }
        // `?` on an `Option` returns `None` at once when there is no value, like `if (t === undefined) return null` in JavaScript.
        let t = *self.date.times.get(&o)?;
        Some(date::to_iso(t).unwrap_or_else(|| "Invalid Date".to_string()))
    }

    /// Write `v` as JSON. `in_list`: an `undefined` is `null` (in an object the caller skips it).
    fn ins_value(&mut self, st: &mut Ins, v: Value, in_list: bool) {
        st.values += 1;
        if st.values > INSPECT_VALUES {
            return;
        }
        match v {
            Value::Undefined | Value::Symbol(_) => st.put(if in_list { "null" } else { "" }),
            Value::Number(n) if !n.is_finite() => st.put("null"),
            Value::Number(n) if n == 0.0 => st.put("0"),
            Value::String(_) => {
                let s = self.display(v);
                st.quote(&s);
            }
            Value::Object(o) => self.ins_object(st, o),
            _ => {
                let s = self.display(v);
                st.put(&s);
            }
        }
    }

    fn ins_object(&mut self, st: &mut Ins, o: ObjId) {
        if let Some(s) = self.ins_leaf(o, st.node) {
            return st.quote(&s);
        }
        if st.open.contains(&o) {
            return st.put("\"[Circular]\"");
        }
        if st.open.len() >= INSPECT_DEPTH {
            return st.put("\"…\"");
        }
        st.open.push(o);
        if let Some(c) = self.sc.colls.get(&o) {
            // Copy the live entries first: writing them reads the interpreter again.
            let is_map = c.is_map;
            let items: Vec<(Value, Value)> = c.entries.iter().filter(|e| e.live).map(|e| (e.key, e.val)).collect();
            st.put("[");
            for (i, (k, v)) in items.into_iter().enumerate() {
                if st.values > INSPECT_VALUES {
                    break;
                }
                st.put(if i > 0 { "," } else { "" });
                if is_map {
                    st.put("[");
                    self.ins_value(st, k, true);
                    st.put(",");
                    self.ins_value(st, v, true);
                    st.put("]");
                } else {
                    self.ins_value(st, k, true);
                }
            }
            st.put("]");
        } else if self.o_is_array(o).unwrap_or(false) || self.ins_is_list(o) {
            let len = match self.get_prop(Value::Object(o), self.k_length) {
                Ok(Value::Number(n)) if n >= 0.0 => n as u64,
                _ => 0,
            };
            st.put("[");
            for i in 0..len {
                if st.values > INSPECT_VALUES {
                    break;
                }
                st.put(if i > 0 { "," } else { "" });
                match u32::try_from(i).ok().map(|i| self.get_prop(Value::Object(o), PropertyKey::Index(i))) {
                    Some(Ok(v)) => self.ins_value(st, v, true),
                    _ => st.put("\"[threw]\""),
                }
            }
            st.put("]");
        } else {
            let keys = self.property_keys(o).unwrap_or_default();
            st.put("{");
            let mut any = false;
            for k in keys {
                if st.values > INSPECT_VALUES {
                    break;
                }
                if matches!(k, PropertyKey::Symbol(_)) || !self.o_peek(o, k).ok().flatten().is_some_and(|p| p.enumerable()) {
                    continue;
                }
                let v = self.get_prop(Value::Object(o), k);
                // `undefined` and a symbol are left out of an object, as in JSON.
                if matches!(v, Ok(Value::Undefined | Value::Symbol(_))) {
                    continue;
                }
                st.put(if any { "," } else { "" });
                any = true;
                let name = self.key_text(k);
                st.quote(&name);
                st.put(":");
                match v {
                    Ok(v) => self.ins_value(st, v, false),
                    Err(_) => st.put("\"[threw]\""),
                }
            }
            st.put("}");
        }
        st.open.pop();
    }

    /// Is `o` a NodeList or an HTML collection (by its `Symbol.toStringTag`)? A plain object is not.
    fn ins_is_list(&mut self, o: ObjId) -> bool {
        if self.heap.get_prototype_of(o).ok().flatten() == Some(self.object_proto) {
            return false;
        }
        let tag = self.get_prop(Value::Object(o), self.sc.k_to_string_tag).map(|t| if matches!(t, Value::String(_)) { self.display(t) } else { String::new() }).unwrap_or_default();
        tag == "NodeList" || tag.ends_with("Collection")
    }
}
