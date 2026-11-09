// Date of the evaluator. Included by `eval.rs` (`include!`). The calendar arithmetic, the parser and the formatter are in
// `date.rs` (pure functions, UTC only: the zone is applied here). This file holds the native state, the clock and the builtins.
//
// Observed need: the cached app bundle calls `Date.now()` (prefetch, react-query, toaster).
//
// Supported (ECMA-262 section 21.4)
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
// Local time (the zone of `Intl.DateTimeFormat().resolvedOptions().timeZone`: `TZ`, else the `/etc/localtime` link, else UTC; the
// zone file is read on the first local-time call, never before)
// - `new Date(y, m, ...)` (2 or more arguments), `Date()` (the local string of now), an ISO date-time with no offset
//   (`2020-01-01T00:00:00`), `getDate`, `getDay`, `getFullYear`, `getHours`, `getMilliseconds`, `getMinutes`, `getMonth`,
//   `getSeconds`, `getTimezoneOffset`, `getYear`, `setDate`, `setFullYear`, `setHours`, `setMilliseconds`, `setMinutes`,
//   `setMonth`, `setSeconds`, `setYear`, `toString`, `toDateString`, `toTimeString`. `"" + date` calls `toString`.
// - A wall-clock time in an overlap (clocks go back) is the first instant. One in a gap (clocks go forward) uses the offset from
//   before the gap (ECMA-262 `UTC(t)`).
// - Known deviation: the text in parentheses after `GMT+hhmm` is `Coordinated Universal Time` for the zone `UTC` and `GMT+hh:mm`
//   for any other zone. V8 writes the long name from ICU (`India Standard Time`); bare has no name table.
//
// Gates (never silent): `Unsupported`, which a script cannot catch.
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

/// What a local-time method does.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LocalFn {
    /// The `getUTC*` field with this index, read from local time.
    Get(u8),
    /// `getTimezoneOffset`
    Offset,
    /// `getYear`
    GetYear,
    /// The `setUTC*` method with this index, on local time.
    Set(u8),
    /// `setYear`
    SetYear,
    /// `toString`, `toDateString`, `toTimeString`
    Text(date::LocalText),
}

const DATE_LOCAL: [(&str, u32, LocalFn); 21] = [
    ("getDate", 0, LocalFn::Get(2)),
    ("getDay", 0, LocalFn::Get(3)),
    ("getFullYear", 0, LocalFn::Get(0)),
    ("getHours", 0, LocalFn::Get(4)),
    ("getMilliseconds", 0, LocalFn::Get(7)),
    ("getMinutes", 0, LocalFn::Get(5)),
    ("getMonth", 0, LocalFn::Get(1)),
    ("getSeconds", 0, LocalFn::Get(6)),
    ("getTimezoneOffset", 0, LocalFn::Offset),
    ("getYear", 0, LocalFn::GetYear),
    ("setDate", 1, LocalFn::Set(4)),
    ("setFullYear", 3, LocalFn::Set(6)),
    ("setHours", 4, LocalFn::Set(3)),
    ("setMilliseconds", 1, LocalFn::Set(0)),
    ("setMinutes", 3, LocalFn::Set(2)),
    ("setMonth", 2, LocalFn::Set(5)),
    ("setSeconds", 2, LocalFn::Set(1)),
    ("setYear", 1, LocalFn::SetYear),
    ("toString", 0, LocalFn::Text(date::LocalText::Full)),
    ("toDateString", 0, LocalFn::Text(date::LocalText::Date)),
    ("toTimeString", 0, LocalFn::Text(date::LocalText::Time)),
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
        for (i, &(n, l, _)) in DATE_LOCAL.iter().enumerate() {
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
            date::Parsed::Local(l) => Ok(date::time_clip(self.date_from_local(l)?)),
            date::Parsed::Unrecognized => Err(unsupported("Date string that is not ISO 8601 or toUTCString format", Span::default())),
        }
    }

    /// The offset of the default zone (seconds east of UTC) at the valid time value `t`.
    fn date_offset(&mut self, t: f64) -> Res<i32> {
        let (_, z) = self.intl_default_zone()?;
        Ok(z.offset((t / 1000.0).floor() as i64))
    }

    /// Wall-clock time of the time value `t` (NaN stays NaN).
    fn date_to_local(&mut self, t: f64) -> Res<f64> {
        if !t.is_finite() {
            return Ok(f64::NAN);
        }
        Ok(t + self.date_offset(t)? as f64 * 1000.0)
    }

    /// The time value of wall-clock time `l` (`UTC(t)` of ECMA-262; NaN stays NaN). The caller clips.
    fn date_from_local(&mut self, l: f64) -> Res<f64> {
        if !l.is_finite() || l.abs() > date::MAX_TIME + 172_800_000.0 {
            return Ok(f64::NAN);
        }
        let (_, z) = self.intl_default_zone()?;
        let secs = (l / 1000.0).floor() as i64;
        Ok(l + (z.to_utc(secs) - secs) as f64 * 1000.0)
    }

    /// Text of a valid or invalid time value in the default zone.
    fn date_local_text(&mut self, t: f64, part: date::LocalText) -> Res<Value> {
        if t.is_nan() {
            return self.date_string(None);
        }
        let (name, z) = self.intl_default_zone()?;
        let off = z.offset((t / 1000.0).floor() as i64);
        let zone = if &*name == "UTC" { "Coordinated Universal Time".to_string() } else { format!("GMT{}{:02}:{:02}", if off < 0 { '-' } else { '+' }, off.abs() / 3600, off.abs() / 60 % 60) };
        self.date_string(date::to_local_string(t, off, &zone, part))
    }

    /// The 7 arguments of `Date.UTC` and of `new Date(y, m, ...)`: MakeDate of them, coerced in order.
    fn date_components(&mut self, args: &[Value]) -> Res<f64> {
        let mut n = [f64::NAN, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
        for (i, slot) in n.iter_mut().enumerate() {
            if i == 0 || i < args.len() {
                *slot = self.coerce_number(arg(args, i))?;
            }
        }
        let y = if n[0].is_nan() { n[0] } else if (0.0..=99.0).contains(&n[0].trunc()) { 1900.0 + n[0].trunc() } else { n[0] };
        Ok(date::make_date(date::make_day(y, n[1], n[2]), date::make_time(n[3], n[4], n[5], n[6])))
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
                    return self.date_local_text(date::time_clip(self.wall_now_ms()), date::LocalText::Full);
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
                    _ => {
                        let l = self.date_components(args)?;
                        date::time_clip(self.date_from_local(l)?)
                    }
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
                let t = self.date_components(args)?;
                Ok(Value::Number(date::time_clip(t)))
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
            DateFn::Set(i) => self.date_set(i, false, this, args),
            DateFn::Local(i) => {
                let (_, t) = self.this_date(this)?;
                match DATE_LOCAL[i as usize].2 {
                    LocalFn::Get(k) => {
                        let l = self.date_to_local(t)?;
                        let Some(f) = date::fields(l) else { return Ok(Value::Number(f64::NAN)) };
                        Ok(Value::Number([f.year, f.month, f.day, f.weekday, f.hour, f.minute, f.second, f.ms][k as usize] as f64))
                    }
                    LocalFn::GetYear => {
                        let l = self.date_to_local(t)?;
                        Ok(Value::Number(date::fields(l).map_or(f64::NAN, |f| (f.year - 1900) as f64)))
                    }
                    LocalFn::Offset if t.is_nan() => Ok(Value::Number(f64::NAN)),
                    // whole minutes, cut toward zero, as V8 does for a zone with seconds in its offset
                    LocalFn::Offset => Ok(Value::Number(-(self.date_offset(t)? / 60) as f64)),
                    LocalFn::Set(k) => self.date_set(k, true, this, args),
                    LocalFn::SetYear => {
                        let y = self.coerce_number(a0)?;
                        if y.is_nan() {
                            let (o, _) = self.this_date(this)?;
                            self.date.times.insert(o, f64::NAN);
                            return Ok(Value::Number(f64::NAN));
                        }
                        let yi = y.trunc();
                        self.date_set(6, true, this, &[Value::Number(if (0.0..=99.0).contains(&yi) { 1900.0 + yi } else { y })])
                    }
                    LocalFn::Text(part) => self.date_local_text(t, part),
                }
            }
        }
    }

    /// `setUTC*` (section 21.4.4.20 and its siblings), and with `local` the setter of the same field on wall-clock time. The time
    /// value is read first, every argument present is coerced in order, then an invalid date stays invalid (except
    /// `setFullYear` and `setUTCFullYear`, which start from +0).
    fn date_set(&mut self, i: u8, local: bool, this: Value, args: &[Value]) -> Res<Value> {
        let (o, t) = self.this_date(this)?;
        let max = [1, 2, 3, 4, 1, 2, 3][i as usize];
        let mut n = [f64::NAN; 4];
        for (k, slot) in n.iter_mut().enumerate().take(max) {
            if k == 0 || k < args.len() {
                *slot = self.coerce_number(arg(args, k))?;
            }
        }
        let t = if t.is_nan() && i == 6 { 0.0 } else if local { self.date_to_local(t)? } else { t };
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
        let v = date::time_clip(if local { self.date_from_local(v)? } else { v });
        self.date.times.insert(o, v);
        Ok(Value::Number(v))
    }
}
