//! Compact inspection for the browser driver. A getter may run, but toJSON never runs.

use crate::bi_object::Own;
use crate::interp::STOP;
use crate::interp_rt::{Key, Realm};
use crate::object::Obj;
use crate::shapes::A_E;
use crate::string::Str;
use crate::value::Value;
use bare_js_api::Engine;

const MAX_VALUES: usize = 100_000;
const MAX_DEPTH: usize = 64;
const MAX_STEPS: u64 = 100_000_000;

struct Inspect<'a> {
    out: String,
    chars: usize,
    cap: usize,
    open: Vec<Obj>,
    values: usize,
    node: &'a dyn Fn(Value) -> Option<String>,
}

impl Inspect<'_> {
    fn put(&mut self, s: &str) {
        if self.chars < self.cap {
            self.out.extend(s.chars().take(self.cap - self.chars));
        }
        self.chars += s.chars().count();
    }

    fn quote(&mut self, s: &str) {
        self.put("\"");
        for c in s.chars() {
            match c {
                '"' => self.put("\\\""),
                '\\' => self.put("\\\\"),
                '\n' => self.put("\\n"),
                '\r' => self.put("\\r"),
                '\t' => self.put("\\t"),
                c if c < '\u{20}' => self.put(&format!("\\u{:04x}", c as u32)),
                c => self.put(c.encode_utf8(&mut [0; 4])),
            }
        }
        self.put("\"");
    }

    fn next(&mut self) -> bool {
        self.values += 1;
        self.values <= MAX_VALUES
    }
}

impl Realm {
    /// An object as compact JSON, or the text of a primitive, node, date or function.
    pub fn raw_inspect(&mut self, value: Value, cap: usize, node: &dyn Fn(Value) -> Option<String>) -> String {
        let Some(o) = Realm::obj(value) else { return self.inspect_primitive(value) };
        let mut st = Inspect { out: String::new(), chars: 0, cap, open: Vec::new(), values: 0, node };
        let saved = self.api.abort.take();
        // Keys and collection snapshots contain raw values while a host fill or getter runs.
        let held = self.heap.hold_task();
        let _ = self.task(|rt| {
            let fuel = rt.fuel;
            let steps = fuel.min(MAX_STEPS);
            rt.fuel = steps;
            if let Some(s) = rt.inspect_leaf(o, node) {
                st.out = s;
            } else {
                rt.inspect_value(&mut st, value);
            }
            rt.fuel = fuel.saturating_sub(steps.saturating_sub(rt.fuel));
        });
        self.heap.release_task(held);
        self.api.abort = saved;
        if st.values > MAX_VALUES {
            st.out.push_str(&format!("… (cut: {} of more than {} chars)", cap.min(st.chars), st.chars));
        } else if st.chars > cap {
            st.out.push_str(&format!("… (cut: {cap} of {} chars)", st.chars));
        }
        st.out
    }

    fn inspect_text(&self, s: Str) -> String {
        if s.is_rope() {
            return String::from_utf16_lossy(&self.raw_units(s.value()).expect("a string"));
        }
        self.text(s)
    }

    fn inspect_primitive(&self, v: Value) -> String {
        if let Some(s) = Realm::str_of(v) {
            self.inspect_text(s)
        } else if let Some(n) = v.as_f64() {
            Realm::num_to_string(n)
        } else if let Some(b) = v.as_bool() {
            b.to_string()
        } else if let Some(s) = Realm::sym_of(v) {
            format!("Symbol({})", self.text(s))
        } else if v.is_null() {
            "null".into()
        } else {
            "undefined".into()
        }
    }

    fn inspect_leaf(&mut self, o: Obj, node: &dyn Fn(Value) -> Option<String>) -> Option<String> {
        if let Some(s) = node(o.value()) {
            return Some(s);
        }
        if o.is_callable() {
            let name = self.intern("name");
            let v = self.get_prop(o.value(), Key::Name(name)).ok().and_then(Realm::str_of);
            let name = v.map(|s| self.inspect_text(s)).unwrap_or_default();
            return Some(format!("function {name}()"));
        }
        let (date, slot) = crate::bi_date::date_slot(self, o.value())?;
        let t = date.slot(&self.shapes, slot).as_f64()?;
        Some(crate::bi_date_calc::to_iso(t).unwrap_or_else(|| "Invalid Date".into()))
    }

    fn inspect_value(&mut self, st: &mut Inspect<'_>, v: Value) {
        if let Some(o) = Realm::obj(v) {
            self.inspect_object(st, o);
        } else if let Some(s) = Realm::str_of(v) {
            st.quote(&self.inspect_text(s));
        } else if v.is_undefined() || Realm::sym_of(v).is_some() || v.as_f64().is_some_and(|n| !n.is_finite()) {
            st.put("null");
        } else {
            st.put(&self.inspect_primitive(v));
        }
    }

    fn inspect_object(&mut self, st: &mut Inspect<'_>, o: Obj) {
        if !st.open.is_empty()
            && let Some(s) = self.inspect_leaf(o, st.node)
        {
            return st.quote(&s);
        }
        if st.open.contains(&o) {
            return st.quote("[Circular]");
        }
        if st.open.len() >= MAX_DEPTH {
            return st.quote("…");
        }
        st.open.push(o);
        if let Some((is_map, entries)) = self.inspect_collection(o, MAX_VALUES.saturating_sub(st.values) + 1) {
            self.inspect_entries(st, is_map, entries);
        } else if Realm::is_array(self, o) || self.inspect_is_list(o) {
            self.inspect_list(st, o);
        } else {
            self.inspect_properties(st, o);
        }
        st.open.pop();
    }

    fn inspect_is_list(&mut self, o: Obj) -> bool {
        if o.proto(&self.shapes) == Some(self.object_proto.cell()) {
            return false;
        }
        let tag = self.wk[crate::bi_symbol::TO_STRING_TAG];
        let v = self.get_prop(o.value(), Key::Name(tag)).ok().and_then(Realm::str_of);
        v.is_some_and(|s| {
            let tag = self.text(s);
            tag == "NodeList" || tag.ends_with("Collection")
        })
    }

    fn inspect_list(&mut self, st: &mut Inspect<'_>, o: Obj) {
        let len = self.get_prop(o.value(), Key::Name(self.k_length)).ok().and_then(Value::as_f64).unwrap_or(0.0) as u32;
        st.put("[");
        for i in 0..len {
            if !st.next() {
                break;
            }
            if i != 0 {
                st.put(",");
            }
            let v = self.get_prop(o.value(), Key::Idx(i));
            self.inspect_result(st, v);
            if v == Err(STOP) {
                break;
            }
        }
        st.put("]");
    }

    fn inspect_entries(&mut self, st: &mut Inspect<'_>, is_map: bool, entries: Vec<(Value, Value)>) {
        st.put("[");
        for (i, (key, value)) in entries.into_iter().enumerate() {
            if !st.next() {
                break;
            }
            if i > 0 {
                st.put(",");
            }
            if is_map {
                st.put("[");
                self.inspect_value(st, key);
                st.put(",");
                if st.next() {
                    self.inspect_value(st, value);
                } else {
                    st.put("null");
                }
                st.put("]");
            } else {
                self.inspect_value(st, value);
            }
        }
        st.put("]");
    }

    fn inspect_properties(&mut self, st: &mut Inspect<'_>, o: Obj) {
        let keys = self.own_key_list_h(o, true).unwrap_or_default();
        let mut any = false;
        st.put("{");
        for k in keys {
            if !st.next() {
                break;
            }
            let Some(s) = Realm::str_of(k) else { continue };
            let Ok(key) = self.key_of(k) else { continue };
            let value = match self.get_own_h(o, key) {
                Ok(Some(Own::Data(v, a))) if a & A_E != 0 => Ok(v),
                Ok(Some(Own::Acc(_, _, a))) if a & A_E != 0 => self.get_prop(o.value(), key),
                Ok(_) => continue,
                Err(e) => Err(e),
            };
            if value.is_ok_and(|v| v.is_undefined() || Realm::sym_of(v).is_some()) {
                continue;
            }
            if any {
                st.put(",");
            }
            any = true;
            st.quote(&self.inspect_text(s));
            st.put(":");
            self.inspect_result(st, value);
            if value == Err(STOP) {
                break;
            }
        }
        st.put("}");
    }

    fn inspect_result(&mut self, st: &mut Inspect<'_>, v: Result<Value, Value>) {
        match v {
            Ok(v) => self.inspect_value(st, v),
            Err(_) => st.quote("[threw]"),
        }
    }
}
