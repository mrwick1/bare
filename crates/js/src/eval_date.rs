// Date of the evaluator. Included by `eval.rs` (`include!`). The calendar arithmetic, the parser and the formatter are in
// `date.rs` (pure functions, UTC only). This file holds the native state, the clock and the builtins.
//
// Observed need: the cached app bundle calls `Date.now()` (prefetch, react-query, toaster).
//
// Supported (ECMA-262 section 21.4, UTC only)
// - `new Date()`, `new Date(timeValue)`, `new Date(dateObject)`, `new Date(string)`. A value goes through ToPrimitive (default
//   hint, so `@@toPrimitive`, `valueOf` and `toString` of an object run), a string is parsed, anything else is ToNumber, then
//   TimeClip (NaN outside +-8.64e15, the integer part, +0 for -0).
// - `Date.now()`, `Date.parse(string)`, `Date.UTC(year[, month, ...])` (the 7 arguments are coerced in order; a year 0..99 is 19xx).
// - `getTime`, `valueOf`, `setTime`, `toISOString` (RangeError on an invalid date, `+YYYYYY` outside 0..9999), `toJSON` (generic: a
//   non-finite number gives `null`, else `toISOString` of the receiver), `toUTCString` and `toGMTString` (the same function),
//   `@@toPrimitive`, the 8 `getUTC*` getters and the 7 `setUTC*` setters (arguments coerced in order and before the NaN check).
// - `toLocaleString`, `toLocaleDateString` and `toLocaleTimeString` are in `eval_intl.rs`.
// - `Object.prototype.toString` of a Date is `[object Date]`. Every method checks its receiver: another object is a TypeError.
// - Strings: ISO 8601 as the standard writes it (`YYYY`, `YYYY-MM`, `YYYY-MM-DD`, `THH:mm[:ss[.s+]]`, `Z` or `+HH:mm`, years
//   `+YYYYYY`/`-YYYYYY`, `24:00`) and the `toUTCString` form. An out-of-range field is NaN (V8 accepts `2021-02-29`).
//
// Gates (never silent): `Unsupported`, which a script cannot catch.
// - Local time. bare has no time zone database, so nothing here is local: `new Date(y, m, ...)` (2 or more arguments), a call
//   `Date()` (a local string), an ISO date-time with no offset (`2020-01-01T00:00:00`), and these methods when the date is valid:
//   `getDate`, `getDay`, `getFullYear`, `getHours`, `getMilliseconds`, `getMinutes`, `getMonth`, `getSeconds`, `getTimezoneOffset`,
//   `getYear`, `setDate`, `setFullYear`, `setHours`, `setMilliseconds`, `setMinutes`, `setMonth`, `setSeconds`, `setYear`,
//   `toString`, `toDateString`, `toTimeString`. On an invalid date the getters give NaN and the string
//   methods give `Invalid Date`: no zone changes that. They are functions, so `typeof` and feature tests work.
//   `"" + date` and `date + 1` (hint default) call `toString`, so they are Unsupported for a valid date. `date - 1`, `+date`
//   and comparisons use `valueOf` and work.
// - A string with a digit that is not ISO and not the `toUTCString` form (`Jan 1 2020`, `2020-1-1`, `+0100`) is Unsupported. A string with no
//   digit is NaN.
// - A subclass (`class extends Date`, `Reflect.construct(Date, [], X)`) is refused with `Unsupported` when constructed: the constructor
//   has no `new.target` yet (`native_branded`), so a brand with a patched prototype would be wrong.
//
// Clock: `Date.now()` and `new Date()` read the wall clock of the realm (`WallClock`). The default is the system clock
// (`SystemTime`). It can go backward (an NTP step), and a script may see that. It is not `performance.now()`, which is
// monotonic and has its own origin in the embedder. A test sets `WallClock::Virtual(epoch_ms)` with `Interp::set_wall_clock`: it
// reads that value until the test sets another. It never follows the scheduler clock by itself.
//
// Storage: one time value per Date object in `date.times`, keyed by the object handle (generation checked, so a reused slot
// cannot match an old Date). `Interp::collect` roots the prototype and the constructor (they outlive a deleted `Date` global),
// and `date_prune` drops the value of every object the collector did not reach. Dates are capped (`max_dates`) before an
// object is made. A date string is capped (`max_date_string`) before it is read, and charged as steps.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DateFn {
    Ctor,
    Now,
    Parse,
    Utc,
    GetTime,
    SetTime,
    ToIso,
    ToJson,
    ToUtc,
    ToPrim,
    /// `getUTC*`: 0 FullYear, 1 Month, 2 Date, 3 Day, 4 Hours, 5 Minutes, 6 Seconds, 7 Milliseconds.
    Get(u8),
    /// `setUTC*`: 0 Milliseconds, 1 Seconds, 2 Minutes, 3 Hours, 4 Date, 5 Month, 6 FullYear.
    Set(u8),
    /// An index into `DATE_LOCAL`.
    Local(u8),
}

/// How a local-time method answers for an invalid date.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LocalKind {
    /// NaN
    Number,
    /// "Invalid Date"
    Text,
    /// Unsupported (a setter reads its arguments in an order that depends on the zone only for valid dates, but is gated whole)
    Setter,
}

macro_rules! local {
    ($n:literal, $len:literal, $k:ident) => {
        ($n, concat!("Date.prototype.", $n, " (local time: bare has no time zone)"), $len, LocalKind::$k)
    };
}

const DATE_LOCAL: [(&str, &str, u32, LocalKind); 21] = [
    local!("getDate", 0, Number),
    local!("getDay", 0, Number),
    local!("getFullYear", 0, Number),
    local!("getHours", 0, Number),
    local!("getMilliseconds", 0, Number),
    local!("getMinutes", 0, Number),
    local!("getMonth", 0, Number),
    local!("getSeconds", 0, Number),
    local!("getTimezoneOffset", 0, Number),
    local!("getYear", 0, Number),
    local!("setDate", 1, Setter),
    local!("setFullYear", 3, Setter),
    local!("setHours", 4, Setter),
    local!("setMilliseconds", 1, Setter),
    local!("setMinutes", 3, Setter),
    local!("setMonth", 2, Setter),
    local!("setSeconds", 2, Setter),
    local!("setYear", 1, Setter),
    local!("toString", 0, Text),
    local!("toDateString", 0, Text),
    local!("toTimeString", 0, Text),
];

const DATE_GETTERS: [&str; 8] = ["getUTCFullYear", "getUTCMonth", "getUTCDate", "getUTCDay", "getUTCHours", "getUTCMinutes", "getUTCSeconds", "getUTCMilliseconds"];
/// Name and length of each `setUTC*` method, in the order of `DateFn::Set`.
const DATE_SETTERS: [(&str, u32); 7] = [("setUTCMilliseconds", 1), ("setUTCSeconds", 2), ("setUTCMinutes", 3), ("setUTCHours", 4), ("setUTCDate", 1), ("setUTCMonth", 2), ("setUTCFullYear", 3)];

/// The wall clock a realm reads for `Date.now()` and `new Date()`: milliseconds since 1970-01-01T00:00:00Z.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WallClock {
    /// `std::time::SystemTime`. It can jump backward.
    System,
    /// A fixed value that only `Interp::set_wall_clock` changes (a whole number of milliseconds within +-8.64e15).
    Virtual(f64),
}

struct DateState {
    times: FxMap<ObjId, f64>,
    proto: ObjId,
    ctor: ObjId,
    clock: WallClock,
}

impl DateState {
    fn new(o: ObjId) -> DateState {
        DateState { times: FxMap::default(), proto: o, ctor: o, clock: WallClock::System }
    }
}

impl<'p> Interp<'p> {
    /// Choose the wall clock of this realm. A virtual value must be a whole number of milliseconds within +-8.64e15,
    /// else `Unsupported` and the clock stays as it was. It may move backward: the wall clock is not monotonic.
    pub fn set_wall_clock(&mut self, clock: WallClock) -> Result<(), Error> {
        if let WallClock::Virtual(t) = clock
            && !(t.is_finite() && t.fract() == 0.0 && t.abs() <= date::MAX_TIME)
        {
            return Err(unsupported("virtual wall clock must be a whole number of milliseconds within +-8.64e15", Span::default()));
        }
        self.date.clock = clock;
        Ok(())
    }

    pub fn wall_clock(&self) -> WallClock {
        self.date.clock
    }

    /// Milliseconds since the Unix epoch on the wall clock: what `Date.now()` returns.
    pub fn wall_now_ms(&self) -> f64 {
        match self.date.clock {
            WallClock::Virtual(t) => t,
            WallClock::System => match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
                Ok(d) => d.as_millis() as f64,
                Err(e) => -(e.duration().as_millis() as f64),
            },
        }
    }

    /// Date objects the evaluator tracks (until `collect` drops the dead ones).
    pub fn date_tracked(&self) -> usize {
        self.date.times.len()
    }

    // ------------------------------------------------------------ GC

    /// Intrinsics that must outlive a deleted `Date` global.
    fn date_roots(&self, out: &mut Vec<Value>) {
        out.extend([self.date.proto, self.date.ctor].map(Value::Object));
    }

    /// Conservative storage roots for direct heap collection. `Interp::collect` prunes instead.
    fn date_all_values(&self, out: &mut Vec<Value>) {
        out.extend(self.date.times.keys().map(|&o| Value::Object(o)));
    }

    fn date_prune(&mut self, seen: &Seen) {
        self.date.times.retain(|o, _| seen.contains(o));
        if self.date.times.capacity() > 64 && self.date.times.len() * 4 < self.date.times.capacity() {
            self.date.times.shrink_to_fit();
        }
    }

    // ------------------------------------------------------------ install

    fn install_date(&mut self) -> Res<()> {
        let proto = h!(self, self.heap.new_object(Some(self.object_proto)));
        let ctor = self.native(Nat::Date(DateFn::Ctor), "Date", 7)?;
        self.sc_global("Date", ctor, proto)?;
        self.date.proto = proto;
        self.date.ctor = ctor;
        for (n, f, l) in [("now", DateFn::Now, 0), ("parse", DateFn::Parse, 1), ("UTC", DateFn::Utc, 7)] {
            self.method(ctor, n, Nat::Date(f), l)?;
        }
        for (n, f, l) in [("getTime", DateFn::GetTime, 0), ("valueOf", DateFn::GetTime, 0), ("setTime", DateFn::SetTime, 1), ("toISOString", DateFn::ToIso, 0), ("toJSON", DateFn::ToJson, 1)] {
            self.method(proto, n, Nat::Date(f), l)?;
        }
        let utc = self.method(proto, "toUTCString", Nat::Date(DateFn::ToUtc), 0)?;
        let k = h!(self, self.heap.key_from_str("toGMTString"));
        self.def(proto, k, Value::Object(utc), true, false, true)?;
        for (i, n) in DATE_GETTERS.iter().enumerate() {
            self.method(proto, n, Nat::Date(DateFn::Get(i as u8)), 0)?;
        }
        for (i, &(n, l)) in DATE_SETTERS.iter().enumerate() {
            self.method(proto, n, Nat::Date(DateFn::Set(i as u8)), l)?;
        }
        for (i, &(n, _, l, _)) in DATE_LOCAL.iter().enumerate() {
            self.method(proto, n, Nat::Date(DateFn::Local(i as u8)), l)?;
        }
        let tp = self.native(Nat::Date(DateFn::ToPrim), "[Symbol.toPrimitive]", 1)?;
        self.def(proto, self.sc.k_to_primitive, Value::Object(tp), false, false, true)
    }

    // ------------------------------------------------------------ helpers

    /// The time value of a Date receiver, or the TypeError that V8 throws.
    fn this_date(&mut self, this: Value) -> Res<(ObjId, f64)> {
        if let Value::Object(o) = this
            && let Some(&t) = self.date.times.get(&o)
        {
            return Ok((o, t));
        }
        Err(self.type_err("this is not a Date object."))
    }

    /// A new Date with time value `t` (already clipped). Checks the cap before the object is made.
    fn date_new(&mut self, t: f64, proto: ObjId) -> Res<Value> {
        if self.date.times.len() >= self.sc.limits.max_dates {
            return Err(Error::Limit(LimitKind::Sc(ScLimit::Dates)));
        }
        let o = h!(self, self.heap.new_object(Some(proto)));
        self.date.times.insert(o, t);
        Ok(Value::Object(o))
    }

    /// Parse a date string to a time value. The length is checked, and charged, before anything reads the text.
    fn date_parse_str(&mut self, s: StrId) -> Res<f64> {
        let len = h!(self, self.heap.str_len(s));
        if len > self.sc.limits.max_date_string {
            return Err(Error::Limit(LimitKind::Sc(ScLimit::DateString)));
        }
        self.charge_steps(len as u64)?;
        match date::parse(h!(self, self.heap.str_units(s))) {
            date::Parsed::Time(t) => Ok(t),
            date::Parsed::Local => Err(unsupported("Date string with a time and no offset (local time: bare has no time zone)", Span::default())),
            date::Parsed::Unrecognized => Err(unsupported("Date string that is not ISO 8601 or toUTCString format", Span::default())),
        }
    }

    fn date_string(&mut self, s: Option<String>) -> Res<Value> {
        Ok(Value::String(h!(self, self.heap.intern_str(s.as_deref().unwrap_or("Invalid Date")))))
    }

    // ------------------------------------------------------------ natives

    fn call_date(&mut self, f: DateFn, this: Value, args: &[Value], is_new: bool) -> Res<Value> {
        let a0 = arg(args, 0);
        match f {
            DateFn::Ctor => {
                if !is_new {
                    return Err(unsupported("Date() called as a function (a local time string)", Span::default()));
                }
                let nt = self.cls.native_nt.take();
                let t = match args.len() {
                    0 => date::time_clip(self.wall_now_ms()),
                    1 => match a0 {
                        Value::Object(o) if self.date.times.contains_key(&o) => self.date.times[&o],
                        _ => match self.coerce_primitive(a0, Hint::Default)? {
                            Value::String(s) => self.date_parse_str(s)?,
                            p => date::time_clip(self.coerce_number(p)?),
                        },
                    },
                    _ => return Err(unsupported("new Date(year, month, ...) is local time (bare has no time zone); use Date.UTC", Span::default())),
                };
                let proto = self.nt_proto(nt, self.date.proto)?;
                self.date_new(t, proto)
            }
            DateFn::Now => Ok(Value::Number(date::time_clip(self.wall_now_ms()))),
            DateFn::Parse => {
                let s = self.coerce_string(a0)?;
                Ok(Value::Number(self.date_parse_str(s)?))
            }
            DateFn::Utc => {
                let mut n = [f64::NAN, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
                for (i, slot) in n.iter_mut().enumerate() {
                    if i == 0 || i < args.len() {
                        *slot = self.coerce_number(arg(args, i))?;
                    }
                }
                let y = if n[0].is_nan() { n[0] } else if (0.0..=99.0).contains(&n[0].trunc()) { 1900.0 + n[0].trunc() } else { n[0] };
                Ok(Value::Number(date::time_clip(date::make_date(date::make_day(y, n[1], n[2]), date::make_time(n[3], n[4], n[5], n[6])))))
            }
            DateFn::GetTime => Ok(Value::Number(self.this_date(this)?.1)),
            DateFn::SetTime => {
                let (o, _) = self.this_date(this)?;
                let t = date::time_clip(self.coerce_number(a0)?);
                self.date.times.insert(o, t);
                Ok(Value::Number(t))
            }
            DateFn::ToIso => {
                let (_, t) = self.this_date(this)?;
                match date::to_iso(t) {
                    Some(s) => self.date_string(Some(s)),
                    None => Err(self.range_error("Invalid time value")),
                }
            }
            DateFn::ToUtc => {
                let (_, t) = self.this_date(this)?;
                self.date_string(date::to_utc_string(t))
            }
            DateFn::ToJson => {
                if is_nullish(this) {
                    return Err(self.type_err("Date.prototype.toJSON called on null or undefined"));
                }
                if let Value::Number(n) = self.coerce_primitive(this, Hint::Number)?
                    && !n.is_finite()
                {
                    return Ok(Value::Null);
                }
                let k = h!(self, self.heap.key_from_str("toISOString"));
                let m = self.get_prop(this, k)?;
                if !self.is_callable(m) {
                    return Err(self.type_err("toISOString is not a function"));
                }
                self.call_fn(m, this, &[])
            }
            DateFn::ToPrim => {
                if !matches!(this, Value::Object(_)) {
                    return Err(self.type_err("Date.prototype[Symbol.toPrimitive] called on non-object"));
                }
                let number = match a0 {
                    Value::String(s) => match h!(self, self.heap.str_units(s)) {
                        [110, 117, 109, 98, 101, 114] => true,
                        [115, 116, 114, 105, 110, 103] | [100, 101, 102, 97, 117, 108, 116] => false,
                        _ => return Err(self.type_err("Invalid hint")),
                    },
                    _ => return Err(self.type_err("Invalid hint")),
                };
                let order = if number { [self.k_value_of, self.k_to_string] } else { [self.k_to_string, self.k_value_of] };
                for k in order {
                    let m = self.get_prop(this, k)?;
                    if self.is_callable(m) {
                        let r = self.call_fn(m, this, &[])?;
                        if !matches!(r, Value::Object(_)) {
                            return Ok(r);
                        }
                    }
                }
                Err(self.type_err("Cannot convert object to primitive value"))
            }
            DateFn::Get(i) => {
                let (_, t) = self.this_date(this)?;
                let Some(f) = date::fields(t) else { return Ok(Value::Number(f64::NAN)) };
                Ok(Value::Number([f.year, f.month, f.day, f.weekday, f.hour, f.minute, f.second, f.ms][i as usize] as f64))
            }
            DateFn::Set(i) => self.date_set_utc(i, this, args),
            DateFn::Local(i) => {
                let (_, t) = self.this_date(this)?;
                let (_, what, _, kind) = DATE_LOCAL[i as usize];
                if t.is_nan() {
                    match kind {
                        LocalKind::Number => return Ok(Value::Number(f64::NAN)),
                        LocalKind::Text => return self.date_string(None),
                        LocalKind::Setter => {}
                    }
                }
                Err(unsupported(what, Span::default()))
            }
        }
    }

    /// `setUTC*` (section 21.4.4.20 and its siblings). The time value is read first, every argument present is coerced in order,
    /// then an invalid date stays invalid (except `setUTCFullYear`, which starts from +0).
    fn date_set_utc(&mut self, i: u8, this: Value, args: &[Value]) -> Res<Value> {
        let (o, t) = self.this_date(this)?;
        let max = [1, 2, 3, 4, 1, 2, 3][i as usize];
        let mut n = [f64::NAN; 4];
        for (k, slot) in n.iter_mut().enumerate().take(max) {
            if k == 0 || k < args.len() {
                *slot = self.coerce_number(arg(args, k))?;
            }
        }
        let t = if t.is_nan() && i == 6 { 0.0 } else { t };
        let Some(f) = date::fields(t) else { return Ok(Value::Number(f64::NAN)) };
        let given = |k: usize, default: i64| if k < args.len() { n[k] } else { default as f64 };
        let (day, year, month) = (date::day(t), f.year as f64, f.month as f64);
        let (h, mi, s) = (f.hour as f64, f.minute as f64, f.second as f64);
        let v = match i {
            0 => date::make_date(day, date::make_time(h, mi, s, n[0])),
            1 => date::make_date(day, date::make_time(h, mi, n[0], given(1, f.ms))),
            2 => date::make_date(day, date::make_time(h, n[0], given(1, f.second), given(2, f.ms))),
            3 => date::make_date(day, date::make_time(n[0], given(1, f.minute), given(2, f.second), given(3, f.ms))),
            4 => date::make_date(date::make_day(year, month, n[0]), date::time_within_day(t)),
            5 => date::make_date(date::make_day(year, n[0], given(1, f.day)), date::time_within_day(t)),
            _ => date::make_date(date::make_day(n[0], given(1, f.month), given(2, f.day)), date::time_within_day(t)),
        };
        let v = date::time_clip(v);
        self.date.times.insert(o, v);
        Ok(Value::Number(v))
    }
}
