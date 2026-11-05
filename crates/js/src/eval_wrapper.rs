// Primitive wrapper objects: `new String(x)`, `new Number(x)`, `new Boolean(x)`, `Object(x)` and ToObject of a string, number,
// boolean or symbol. Included by `eval.rs` (`include!`), so it shares that module's imports and private items.
//
// Storage and rules of the objects are in `heap_prim.rs` (one `Exotic::Prim` entry, no cost for ordinary objects). The evaluator
// adds the construction and the `this` checks of the prototype methods:
// - `new String(x)` is ToString(x) in a String wrapper, `new Number(x)` ToNumber(x) (0 with no argument) and `new Boolean(x)` ToBoolean(x).
//   A symbol is a `TypeError` for `new String(sym)` and a string `Symbol(desc)` for `String(sym)`. `new.target` gives the prototype
//   (`class A extends Number`). `Object(x)` and `new Object(x)` are ToObject; `Object(undefined)` and `Object(null)` make a plain object.
// - `thisNumberValue`, `thisStringValue`, `thisBooleanValue` and `thisSymbolValue` accept a primitive, a wrapper and the prototype itself
//   (`Number.prototype` is a Number object for `+0`, `Boolean.prototype` for `false`, `String.prototype` for `""`), else a `TypeError`.
//   The prototype is not a real wrapper here: it is an ordinary object that these four functions treat as one.
// - A primitive `this` of a sloppy function becomes a wrapper (`this` of `function () { return typeof this }.call(1)` is "object"), and
//   so does a primitive receiver of an Array method (`Array.prototype.map.call("ab", f)` passes a String wrapper to `f`). Strict
//   functions and native methods see the primitive. A primitive that gets a property read, a method call or `length` is never boxed:
//   `"a".length` and `(1).toFixed()` take the same path as before.
//
// Not done: BigInt wrappers (`Object(1n)`: this engine has no BigInt kind), `structuredClone` (not in this engine), `Symbol.prototype`
// and `String.prototype` as exotic objects (they are ordinary objects with a special case in the four `this` functions, so
// `Object.getOwnPropertyNames(String.prototype)` has no `length` and no indices).

impl<'p> Interp<'p> {
    /// The prototype of the wrapper of a primitive. `None` for an object, `undefined` and `null`.
    fn wrapper_proto(&self, v: Value) -> Option<ObjId> {
        match v {
            Value::String(_) => Some(self.ext.string_proto),
            Value::Number(_) => Some(self.ao.number_proto.unwrap_or(self.object_proto)),
            Value::Bool(_) => Some(self.sc.bool_proto),
            Value::Symbol(_) => Some(self.sc.symbol_proto),
            _ => None,
        }
    }

    /// A wrapper object of the primitive `v` with prototype `proto`.
    fn wrap(&mut self, proto: ObjId, v: Value) -> Res<Value> {
        Ok(Value::Object(h!(self, self.heap.new_primitive(Some(proto), v))))
    }

    /// ToObject.
    fn to_object(&mut self, v: Value) -> Res<Value> {
        match self.wrapper_proto(v) {
            Some(p) => self.wrap(p, v),
            None if matches!(v, Value::Object(_)) => Ok(v),
            None => Err(self.type_err("Cannot convert undefined or null to object")),
        }
    }

    /// The `String`, `Number` and `Boolean` constructors called with `new`. `nt` is the `new.target` of a subclass: it gives the
    /// prototype, after the value is converted (the order of the specification).
    fn wrapper_ctor(&mut self, n: Nat, args: &[Value], nt: Option<ObjId>) -> Res<Value> {
        let a0 = arg(args, 0);
        let prim = match n {
            Nat::StringFn => Value::String(if args.is_empty() { h!(self, self.heap.intern_str("")) } else { self.coerce_string(a0)? }),
            Nat::NumberFn => Value::Number(if args.is_empty() { 0.0 } else { self.coerce_number(a0)? }),
            _ => Value::Bool(self.truthy(a0)?),
        };
        let default = self.wrapper_proto(prim).expect("a primitive");
        let proto = match nt {
            Some(nt) => self.proto_from_ctor_or(nt, default)?,
            None => default,
        };
        self.wrap(proto, prim)
    }

    /// thisNumberValue: `Some` for a number, a Number wrapper and `Number.prototype`.
    fn this_number_value(&self, this: Value) -> Option<f64> {
        match this {
            Value::Number(x) => Some(x),
            Value::Object(o) => match self.heap.primitive_value(o) {
                Some(Value::Number(x)) => Some(x),
                _ => (Some(o) == self.ao.number_proto).then_some(0.0),
            },
            _ => None,
        }
    }

    /// thisBooleanValue.
    fn this_bool_value(&self, this: Value) -> Option<bool> {
        match this {
            Value::Bool(b) => Some(b),
            Value::Object(o) => match self.heap.primitive_value(o) {
                Some(Value::Bool(b)) => Some(b),
                _ => (o == self.sc.bool_proto).then_some(false),
            },
            _ => None,
        }
    }

    /// thisStringValue.
    fn this_string_value(&mut self, this: Value) -> Res<Option<StrId>> {
        match this {
            Value::String(s) => Ok(Some(s)),
            Value::Object(o) => match self.heap.primitive_value(o) {
                Some(Value::String(s)) => Ok(Some(s)),
                _ if o == self.ext.string_proto => Ok(Some(h!(self, self.heap.intern_str("")))),
                _ => Ok(None),
            },
            _ => Ok(None),
        }
    }

    /// thisSymbolValue.
    fn this_symbol_value(&self, this: Value) -> Option<SymId> {
        match this {
            Value::Symbol(s) => Some(s),
            Value::Object(o) => match self.heap.primitive_value(o) {
                Some(Value::Symbol(s)) => Some(s),
                _ => None,
            },
            _ => None,
        }
    }

    /// The receiver of an Array method: a primitive other than `undefined` and `null` becomes a wrapper (ToObject).
    fn ao_recv(&mut self, this: Value) -> Res<Value> {
        if matches!(this, Value::Object(_)) || is_nullish(this) {
            return Ok(this);
        }
        self.to_object(this)
    }
}
