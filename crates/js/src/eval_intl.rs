// Intl of the evaluator: `Intl.DateTimeFormat` and `Date.prototype.toLocale*String`. Included by `eval.rs` (`include!`). The
// patterns and the formatting are in `intl.rs`, the zones in `tz.rs`.
//
// Observed need: the staging app formats dates with `new Intl.DateTimeFormat("en-US", { timeZone: "Asia/Kolkata", ... })`
// and `formatToParts` at module level, and calls `toLocaleDateString` and `toLocaleTimeString` with a `timeZone`.
//
// Supported (ECMA-402 sections 11 and 20.3)
// - `Intl` with `Symbol.toStringTag`, and `Intl.DateTimeFormat` (with or without `new`; length 0) with `supportedLocalesOf`.
//   The prototype has `format` (a getter that returns one bound function per formatter), `formatToParts` and `resolvedOptions`.
// - Options in the order of CreateDateTimeFormat, with its RangeErrors and TypeErrors: `localeMatcher`, `calendar`,
//   `numberingSystem`, `hour12`, `hourCycle`, `timeZone`, the field widths (weekday, year, month, day, hour, minute, second),
//   `formatMatcher`, `dateStyle` and `timeStyle`, then the defaults (ToDateTimeOptions).
// - Locales: see `intl.rs`. The calendar is always `gregory` and the numbering system `latn` (other values are valid and
//   ignored, as for a locale that does not have them).
// - Time zones: IANA names from the system tzdata, offsets (`+05:30`), and the UTC names. With no `timeZone`, the zone of the
//   system: `TZ`, else the target of `/etc/localtime`, else UTC. `Interp::set_default_time_zone` replaces it (tests).
// - `timeZoneName` (all six values) and the full and long time styles: the names are in `tz_names.rs` (a table of common zones,
//   the offset form for the others). `fractionalSecondDigits` (1 to 3) when the pattern has seconds.
//
// Gates (Unsupported): the options `era` and `dayPeriod`; `fractionalSecondDigits` without `second`; a calendar other than
// `gregory` and a numbering system other than `latn` that ICU knows; a year before 1; a subclass of DateTimeFormat;
// `formatRange`, the other Intl constructors and the local-time methods of Date stay absent or gated as before.
//
// Locale extensions: `-u-` keywords `hc`, `ca` and `nu` are read (ECMA-402 ResolveLocale; an option wins over a keyword; the
// first requested tag that bare has decides). Other keys (`co`, `kn`, `kf`, ...) are ignored. `-t-` and `-x-` are checked and
// then dropped from the resolved locale. A kept keyword goes in `resolvedOptions().locale` (`exts`).
//
// Known deviations: `resolvedOptions().timeZone` is the name as given (the current spec), where V8 gives the old ICU name for
// some links (`Asia/Kolkata` is `Asia/Calcutta` in V8). A zone name must have the case of its file (`asia/kolkata` is a
// RangeError; V8 accepts it). A locale that ICU has and bare has not resolves to its English parent or to `en-US`. With `-u-hc-h12` or `h11` in `en-GB`, a time style
// pads the hour (`03:06 pm`); V8 does not.
//
// Storage: one `Dtf` per formatter object in `intl.dtfs`, capped by `max_formatters`, pruned by `collect`. Zones are read once
// and kept (up to 64) by name. A zone file read is charged as steps.

use crate::icu;
use crate::intl;
use crate::tz::{self, Zone};
use crate::tz_names;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum IntlFn {
    DtfCtor,
    DtfSupported,
    /// The `format` getter.
    DtfFormatGet,
    /// The function that the getter binds to a formatter.
    DtfFormat,
    DtfToParts,
    DtfResolved,
    /// `Date.prototype.toLocaleString` (0), `toLocaleDateString` (1), `toLocaleTimeString` (2).
    DateLocale(u8),
    /// The getter and the setter of the global `Intl` until it is first read (see `install_intl`).
    LazyIntl,
    NfCtor,
    CollCtor,
    PrCtor,
    /// `supportedLocalesOf` of NumberFormat (0), Collator (1) and PluralRules (2).
    Supported(u8),
    NfFormatGet,
    NfFormat,
    NfToParts,
    NfResolved,
    /// `formatRange` (0), `formatRangeToParts` (1) and `selectRange` (2): gates.
    NfRange(u8),
    CollCompareGet,
    CollCompare,
    CollResolved,
    PrSelect,
    PrResolved,
    GetCanonical,
    NumLocale,
    StrLocaleCompare,
    SegCtor,
    SegResolved,
    SegSegment,
    SegsContaining,
    SegsIter,
    SegIterNext,
    LocCtor,
    LocToString,
    /// `maximize` (true) and `minimize` (false).
    LocMaximize(bool),
    /// The getter of `LOCALE_GETTERS[i]`.
    LocGet(u8),
    RtfCtor,
    /// `format` (false) and `formatToParts` (true).
    RtfFormat(bool),
    RtfResolved,
    /// The getter and the setter of a constructor of `LAZY_CTORS` until it is first read.
    LazyLoc(u8),
    LfCtor,
    /// `format` (false) and `formatToParts` (true).
    LfFormat(bool),
    LfResolved,
    DnCtor,
    DnOf,
    DnResolved,
}

impl IntlFn {
    /// A constructor that `new` can call.
    fn is_ctor(self) -> bool {
        matches!(self, IntlFn::DtfCtor | IntlFn::NfCtor | IntlFn::CollCtor | IntlFn::PrCtor | IntlFn::SegCtor | IntlFn::LocCtor | IntlFn::RtfCtor | IntlFn::LfCtor | IntlFn::DnCtor)
    }
}

/// The `-u-` keywords that a `DateTimeFormat` uses: `ca` and `nu` (true when bare's own value is asked) and `hc` (an index of `CYCLES`).
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct DtfKw {
    ca: bool,
    nu: bool,
    hc: Option<u8>,
}

/// A formatter: what `resolvedOptions` reports and what `format` needs.
#[derive(Clone)]
struct Dtf {
    loc: u8,
    pat: intl::Pattern,
    /// `dateStyle` and `timeStyle` (0 none).
    styles: (u8, u8),
    zone_name: Rc<str>,
    zone: Rc<Zone>,
    /// The kind of the time zone name plus one (`tz_names`), 0 for none: the option `timeZoneName`, or the style.
    zn: u8,
    /// `fractionalSecondDigits`, 0 for none.
    fsd: u8,
    /// `zn` or `fsd` is set: the format goes by the cold path.
    extras: bool,
    /// The bound `format` function, made by the first `format` read.
    bound: Option<ObjId>,
}

struct IntlState {
    dtfs: FxMap<ObjId, Dtf>,
    nfs: FxMap<ObjId, IntlNf>,
    colls: FxMap<ObjId, IntlColl>,
    prs: FxMap<ObjId, IntlPr>,
    segs: FxMap<ObjId, IntlSeg>,
    /// The `-u-` extension that `resolvedOptions().locale` adds to the locale of a formatter (`-u-hc-h23`): only the keywords
    /// that were used. Most formatters have none. `pending_ext` carries one from a create function to its constructor.
    exts: FxMap<ObjId, Box<str>>,
    pending_ext: Option<Box<str>>,
    /// True while a constructor runs. Only then does a create function leave a `pending_ext`.
    in_ctor: bool,
    segments: FxMap<ObjId, SegData>,
    seg_iters: FxMap<ObjId, SegIter>,
    /// The ICU4X formatters and collators, made on first use.
    icu: icu::Cache,
    intl: ObjId,
    proto: ObjId,
    ctor: ObjId,
    format_fn: ObjId,
    /// The other constructors, their prototypes and the shared `format` and `compare` functions. All `object_proto` until the
    /// first read of `Intl` makes them.
    nf_proto: ObjId,
    nf_ctor: ObjId,
    nf_format: ObjId,
    coll_proto: ObjId,
    coll_ctor: ObjId,
    coll_compare: ObjId,
    pr_proto: ObjId,
    pr_ctor: ObjId,
    seg_proto: ObjId,
    seg_ctor: ObjId,
    segments_proto: ObjId,
    seg_iter_proto: ObjId,
    /// The constructors that are made on their first read (`LAZY_CTORS`).
    loc: IntlLoc,
    zones: FxMap<Rc<str>, Rc<Zone>>,
    default_zone: Option<(Rc<str>, Rc<Zone>)>,
    /// The keys of `OPTION_NAMES`, made once (rooted).
    keys: Vec<PropertyKey>,
    /// The UTC zone, shared by every formatter that uses it.
    utc: Option<(Rc<str>, Rc<Zone>)>,
}

impl IntlState {
    fn new(o: ObjId) -> IntlState {
        IntlState {
            dtfs: FxMap::default(),
            nfs: FxMap::default(),
            colls: FxMap::default(),
            prs: FxMap::default(),
            segs: FxMap::default(),
            exts: FxMap::default(),
            pending_ext: None,
            in_ctor: false,
            segments: FxMap::default(),
            seg_iters: FxMap::default(),
            icu: icu::Cache::default(),
            intl: o,
            proto: o,
            ctor: o,
            format_fn: o,
            nf_proto: o,
            nf_ctor: o,
            nf_format: o,
            coll_proto: o,
            coll_ctor: o,
            coll_compare: o,
            pr_proto: o,
            pr_ctor: o,
            seg_proto: o,
            seg_ctor: o,
            segments_proto: o,
            seg_iter_proto: o,
            loc: IntlLoc::new(o),
            zones: FxMap::default(),
            default_zone: None,
            keys: Vec::new(),
            utc: None,
        }
    }
}

const MAX_ZONES: usize = 64;
const NO_EXTRAS: intl::Extras<'static> = intl::Extras { zone: "", fsd: 0 };
/// The options that the constructors read. Their keys are made once, on the first read of an option.
const OPTION_NAMES: [&str; 47] = [
    "localeMatcher", "calendar", "numberingSystem", "hour12", "hourCycle", "timeZone", "weekday", "era", "year", "month", "day",
    "dayPeriod", "hour", "minute", "second", "fractionalSecondDigits", "timeZoneName", "formatMatcher", "dateStyle", "timeStyle",
    // NumberFormat, PluralRules and Collator
    "style", "currency", "currencyDisplay", "currencySign", "unit", "unitDisplay", "notation", "minimumIntegerDigits",
    "minimumFractionDigits", "maximumFractionDigits", "minimumSignificantDigits", "maximumSignificantDigits", "roundingIncrement",
    "roundingMode", "roundingPriority", "trailingZeroDisplay", "compactDisplay", "useGrouping", "signDisplay", "usage",
    "collation", "numeric", "caseFirst", "sensitivity", "ignorePunctuation", "type", "granularity",
];

/// The index of `name` in `OPTION_NAMES`. A `match` on the text compares the length first: a scan of the array cost 10% on
/// `new Intl.DateTimeFormat` with options.
fn option_index(name: &str) -> Option<usize> {
    Some(match name {
        "localeMatcher" => 0,
        "calendar" => 1,
        "numberingSystem" => 2,
        "hour12" => 3,
        "hourCycle" => 4,
        "timeZone" => 5,
        "weekday" => 6,
        "era" => 7,
        "year" => 8,
        "month" => 9,
        "day" => 10,
        "dayPeriod" => 11,
        "hour" => 12,
        "minute" => 13,
        "second" => 14,
        "fractionalSecondDigits" => 15,
        "timeZoneName" => 16,
        "formatMatcher" => 17,
        "dateStyle" => 18,
        "timeStyle" => 19,
        "style" => 20,
        "currency" => 21,
        "currencyDisplay" => 22,
        "currencySign" => 23,
        "unit" => 24,
        "unitDisplay" => 25,
        "notation" => 26,
        "minimumIntegerDigits" => 27,
        "minimumFractionDigits" => 28,
        "maximumFractionDigits" => 29,
        "minimumSignificantDigits" => 30,
        "maximumSignificantDigits" => 31,
        "roundingIncrement" => 32,
        "roundingMode" => 33,
        "roundingPriority" => 34,
        "trailingZeroDisplay" => 35,
        "compactDisplay" => 36,
        "useGrouping" => 37,
        "signDisplay" => 38,
        "usage" => 39,
        "collation" => 40,
        "numeric" => 41,
        "caseFirst" => 42,
        "sensitivity" => 43,
        "ignorePunctuation" => 44,
        "type" => 45,
        "granularity" => 46,
        _ => return None,
    })
}

/// The calendars and numbering systems of ICU other than `gregory` and `latn`. bare formats only those two and ignores an
/// unknown name, as V8 does.
static NON_GREGORY: [&str; 17] = [
    "buddhist", "chinese", "coptic", "dangi", "ethioaa", "ethiopic", "hebrew", "indian", "islamic", "islamic-civil", "islamic-rgsa",
    "islamic-tbla", "islamic-umalqura", "iso8601", "japanese", "persian", "roc",
];
static NON_LATN: [&str; 20] = [
    "arab", "arabext", "beng", "deva", "fullwide", "gujr", "guru", "hanidec", "khmr", "knda", "laoo", "mlym", "mong", "mymr", "orya",
    "tamldec", "telu", "thai", "tibt", "adlm",
];

/// Steps charged for each zone file read.
const ZONE_READ_STEPS: u64 = 10_000;

/// The option names that each part of CreateDateTimeFormat reads, with their values.
const WIDTHS_TEXT: [&str; 3] = ["narrow", "short", "long"];
const WIDTHS_NUM: [&str; 2] = ["numeric", "2-digit"];
const WIDTHS_MONTH: [&str; 5] = ["numeric", "2-digit", "narrow", "short", "long"];
const STYLES: [&str; 4] = ["full", "long", "medium", "short"];
const CYCLES: [&str; 4] = ["h11", "h12", "h23", "h24"];

impl<'p> Interp<'p> {
    /// Use zone `name` (an IANA name, an offset or a UTC name) when a formatter has no `timeZone`. Errors: the name is not a
    /// zone.
    pub fn set_default_time_zone(&mut self, name: &str) -> Result<(), Error> {
        match self.intl_zone(name)? {
            Some(z) => {
                self.intl.default_zone = Some(z);
                Ok(())
            }
            None => Err(self.range_error(&format!("Invalid time zone specified: {name}"))),
        }
    }

    /// Formatter objects the evaluator tracks (until `collect` drops the dead ones).
    pub fn intl_tracked(&self) -> usize {
        self.intl.dtfs.len()
    }

    // ------------------------------------------------------------ GC

    fn intl_roots(&self, out: &mut Vec<Value>) {
        let i = &self.intl;
        out.extend([i.intl, i.proto, i.ctor, i.format_fn, i.nf_proto, i.nf_ctor, i.nf_format, i.coll_proto, i.coll_ctor, i.coll_compare, i.pr_proto, i.pr_ctor, i.seg_proto, i.seg_ctor, i.segments_proto, i.seg_iter_proto, i.loc.loc_proto, i.loc.loc_ctor, i.loc.rtf_proto, i.loc.rtf_ctor, i.loc.lf_proto, i.loc.lf_ctor, i.loc.dn_proto, i.loc.dn_ctor].map(Value::Object));
        out.extend(self.intl.keys.iter().filter_map(|k| if let PropertyKey::String(s) = k { Some(Value::String(*s)) } else { None }));
    }

    fn intl_all_values(&self, out: &mut Vec<Value>) {
        for (&o, d) in &self.intl.dtfs {
            out.push(Value::Object(o));
            out.extend(d.bound.map(Value::Object));
        }
        for (&o, n) in &self.intl.nfs {
            out.push(Value::Object(o));
            out.extend(n.bound.map(Value::Object));
        }
        for (&o, c) in &self.intl.colls {
            out.push(Value::Object(o));
            out.extend(c.bound.map(Value::Object));
        }
        out.extend(self.intl.prs.keys().map(|&o| Value::Object(o)));
        out.extend(self.intl.segs.keys().map(|&o| Value::Object(o)));
        for (&o, d) in &self.intl.segments {
            out.extend([Value::Object(o), Value::String(d.s)]);
        }
        for (&o, d) in &self.intl.seg_iters {
            out.extend([Value::Object(o), Value::String(d.s)]);
        }
        out.extend(self.intl.loc.locales.keys().chain(self.intl.loc.rtfs.keys()).chain(self.intl.loc.lfs.keys()).chain(self.intl.loc.dns.keys()).map(|&o| Value::Object(o)));
    }

    fn intl_trace(&self, o: ObjId, out: &mut Vec<Value>) {
        let bound = self.intl.dtfs.get(&o).and_then(|d| d.bound).or_else(|| self.intl.nfs.get(&o).and_then(|n| n.bound)).or_else(|| self.intl.colls.get(&o).and_then(|c| c.bound));
        if let Some(b) = bound {
            out.push(Value::Object(b));
        }
        if let Some(d) = self.intl.segments.get(&o) {
            out.push(Value::String(d.s));
        }
        if let Some(d) = self.intl.seg_iters.get(&o) {
            out.push(Value::String(d.s));
        }
    }

    fn intl_prune(&mut self, seen: &Seen) {
        self.intl.dtfs.retain(|o, _| seen.contains(o));
        self.intl.nfs.retain(|o, _| seen.contains(o));
        self.intl.colls.retain(|o, _| seen.contains(o));
        self.intl.prs.retain(|o, _| seen.contains(o));
        self.intl.segs.retain(|o, _| seen.contains(o));
        self.intl.exts.retain(|o, _| seen.contains(o));
        self.intl.segments.retain(|o, _| seen.contains(o));
        self.intl.seg_iters.retain(|o, _| seen.contains(o));
        self.intl.loc.locales.retain(|o, _| seen.contains(o));
        self.intl.loc.rtfs.retain(|o, _| seen.contains(o));
        self.intl.loc.lfs.retain(|o, _| seen.contains(o));
        self.intl.loc.dns.retain(|o, _| seen.contains(o));
        if self.intl.dtfs.capacity() > 64 && self.intl.dtfs.len() * 4 < self.intl.dtfs.capacity() {
            self.intl.dtfs.shrink_to_fit();
        }
    }

    // ------------------------------------------------------------ install

    /// The global `Intl` is an accessor until a script first reads it, so a realm that never uses `Intl` pays for one function
    /// and not for the 25 of the namespace (`intl_make`). The read replaces the accessor with the data property of the spec.
    /// The date, number and string methods are installed at once: they need no object.
    fn install_intl(&mut self) -> Res<()> {
        let k = h!(self, self.heap.key_from_str("Intl"));
        let g = self.new_func_object(Callee::Native(Nat::Intl(IntlFn::LazyIntl)))?;
        let d = PropertyDescriptor { get: Some(Some(g)), set: Some(Some(g)), enumerable: Some(false), configurable: Some(true), ..Default::default() };
        h!(self, self.heap.define_own_property(self.global, k, &d));
        for (i, n) in ["toLocaleString", "toLocaleDateString", "toLocaleTimeString"].iter().enumerate() {
            self.method(self.date.proto, n, Nat::Intl(IntlFn::DateLocale(i as u8)), 0)?;
        }
        if let Some(np) = self.ao.number_proto {
            self.method(np, "toLocaleString", Nat::Intl(IntlFn::NumLocale), 0)?;
        }
        let sp = self.ext.string_proto;
        self.method(sp, "localeCompare", Nat::Intl(IntlFn::StrLocaleCompare), 1)?;
        Ok(())
    }

    /// Make `Intl` and everything on it, and define the global as a data property.
    fn intl_make(&mut self) -> Res<ObjId> {
        let intl = h!(self, self.heap.new_object(Some(self.object_proto)));
        self.sc_tag(intl, "Intl")?;
        let k = h!(self, self.heap.key_from_str("getCanonicalLocales"));
        let f = self.native(Nat::Intl(IntlFn::GetCanonical), "getCanonicalLocales", 1)?;
        self.def(intl, k, Value::Object(f), true, false, true)?;
        // DateTimeFormat
        let proto = h!(self, self.heap.new_object(Some(self.object_proto)));
        let ctor = self.native(Nat::Intl(IntlFn::DtfCtor), "DateTimeFormat", 0)?;
        self.def(ctor, self.k_prototype, Value::Object(proto), false, false, false)?;
        self.def(proto, self.k_constructor, Value::Object(ctor), true, false, true)?;
        let k = h!(self, self.heap.key_from_str("DateTimeFormat"));
        self.def(intl, k, Value::Object(ctor), true, false, true)?;
        self.method(ctor, "supportedLocalesOf", Nat::Intl(IntlFn::DtfSupported), 1)?;
        self.method(proto, "resolvedOptions", Nat::Intl(IntlFn::DtfResolved), 0)?;
        self.method(proto, "formatToParts", Nat::Intl(IntlFn::DtfToParts), 1)?;
        self.getter(proto, "format", Nat::Intl(IntlFn::DtfFormatGet))?;
        self.sc_tag(proto, "Intl.DateTimeFormat")?;
        let format_fn = self.native(Nat::Intl(IntlFn::DtfFormat), "", 1)?;
        // NumberFormat
        let (nf_ctor, nf_proto) = self.intl_ctor(intl, "NumberFormat", IntlFn::NfCtor, IntlFn::Supported(0), "Intl.NumberFormat")?;
        self.method(nf_proto, "resolvedOptions", Nat::Intl(IntlFn::NfResolved), 0)?;
        self.method(nf_proto, "formatToParts", Nat::Intl(IntlFn::NfToParts), 1)?;
        self.getter(nf_proto, "format", Nat::Intl(IntlFn::NfFormatGet))?;
        self.method(nf_proto, "formatRange", Nat::Intl(IntlFn::NfRange(0)), 2)?;
        self.method(nf_proto, "formatRangeToParts", Nat::Intl(IntlFn::NfRange(1)), 2)?;
        let nf_format = self.native(Nat::Intl(IntlFn::NfFormat), "", 1)?;
        // Collator
        let (coll_ctor, coll_proto) = self.intl_ctor(intl, "Collator", IntlFn::CollCtor, IntlFn::Supported(1), "Intl.Collator")?;
        self.method(coll_proto, "resolvedOptions", Nat::Intl(IntlFn::CollResolved), 0)?;
        self.getter(coll_proto, "compare", Nat::Intl(IntlFn::CollCompareGet))?;
        let coll_compare = self.native(Nat::Intl(IntlFn::CollCompare), "", 2)?;
        // PluralRules
        let (pr_ctor, pr_proto) = self.intl_ctor(intl, "PluralRules", IntlFn::PrCtor, IntlFn::Supported(2), "Intl.PluralRules")?;
        self.method(pr_proto, "resolvedOptions", Nat::Intl(IntlFn::PrResolved), 0)?;
        self.method(pr_proto, "select", Nat::Intl(IntlFn::PrSelect), 1)?;
        self.method(pr_proto, "selectRange", Nat::Intl(IntlFn::NfRange(2)), 2)?;
        // Segmenter
        let (seg_ctor, seg_proto) = self.intl_ctor(intl, "Segmenter", IntlFn::SegCtor, IntlFn::Supported(3), "Intl.Segmenter")?;
        self.method(seg_proto, "resolvedOptions", Nat::Intl(IntlFn::SegResolved), 0)?;
        self.method(seg_proto, "segment", Nat::Intl(IntlFn::SegSegment), 1)?;
        let segments_proto = h!(self, self.heap.new_object(Some(self.object_proto)));
        self.method(segments_proto, "containing", Nat::Intl(IntlFn::SegsContaining), 1)?;
        let f = self.native(Nat::Intl(IntlFn::SegsIter), "[Symbol.iterator]", 0)?;
        self.def(segments_proto, self.sc.k_iterator, Value::Object(f), true, false, true)?;
        let seg_iter_proto = h!(self, self.heap.new_object(Some(self.sc.iter_proto)));
        self.method(seg_iter_proto, "next", Nat::Intl(IntlFn::SegIterNext), 0)?;
        self.sc_tag(seg_iter_proto, "Segmenter String Iterator")?;
        let i = &mut self.intl;
        (i.seg_proto, i.seg_ctor, i.segments_proto, i.seg_iter_proto) = (seg_proto, seg_ctor, segments_proto, seg_iter_proto);
        self.intl_loc_lazy(intl)?;
        let i = &mut self.intl;
        (i.intl, i.proto, i.ctor, i.format_fn) = (intl, proto, ctor, format_fn);
        (i.nf_proto, i.nf_ctor, i.nf_format, i.coll_proto, i.coll_ctor, i.coll_compare, i.pr_proto, i.pr_ctor) =
            (nf_proto, nf_ctor, nf_format, coll_proto, coll_ctor, coll_compare, pr_proto, pr_ctor);
        let k = h!(self, self.heap.key_from_str("Intl"));
        self.def(self.global, k, Value::Object(intl), true, false, true)?;
        Ok(intl)
    }

    /// A constructor `name` on `intl` with its prototype, `supportedLocalesOf` and `Symbol.toStringTag`.
    fn intl_ctor(&mut self, intl: ObjId, name: &str, ctor: IntlFn, supported: IntlFn, tag: &str) -> Res<(ObjId, ObjId)> {
        let proto = h!(self, self.heap.new_object(Some(self.object_proto)));
        let c = self.native(Nat::Intl(ctor), name, 0)?;
        self.def(c, self.k_prototype, Value::Object(proto), false, false, false)?;
        self.def(proto, self.k_constructor, Value::Object(c), true, false, true)?;
        let k = h!(self, self.heap.key_from_str(name));
        self.def(intl, k, Value::Object(c), true, false, true)?;
        self.method(c, "supportedLocalesOf", Nat::Intl(supported), 1)?;
        self.sc_tag(proto, tag)?;
        Ok((c, proto))
    }

    /// An accessor `name` with a getter and no setter.
    fn getter(&mut self, on: ObjId, name: &str, n: Nat) -> Res<()> {
        let g = self.native(n, &format!("get {name}"), 0)?;
        let k = h!(self, self.heap.key_from_str(name));
        let d = PropertyDescriptor { get: Some(Some(g)), set: Some(None), enumerable: Some(false), configurable: Some(true), ..Default::default() };
        h!(self, self.heap.define_own_property(on, k, &d));
        Ok(())
    }

    // ------------------------------------------------------------ helpers

    /// CanonicalizeLocaleList.
    fn intl_locales(&mut self, v: Value) -> Res<Vec<String>> {
        let tags: Vec<Value> = match v {
            Value::Undefined => return Ok(Vec::new()),
            Value::String(_) => vec![v],
            Value::Null => return Err(self.type_err("Cannot convert undefined or null to object")),
            Value::Object(o) => {
                let len = self.length_of(v)?;
                self.spend(len)?;
                let mut out = Vec::new();
                for i in 0..len {
                    let k = PropertyKey::Index(i as u32);
                    if !hx!(self, self.heap.has_property(o, k)) {
                        continue;
                    }
                    let x = self.get_prop(v, k)?;
                    if !matches!(x, Value::String(_) | Value::Object(_)) {
                        return Err(self.type_err("Language ID should be string or object."));
                    }
                    out.push(x);
                }
                out
            }
            _ => Vec::new(),
        };
        let mut out: Vec<String> = Vec::new();
        for t in tags {
            let s = self.coerce_string(t)?;
            let s = h!(self, self.heap.to_string_lossy(s));
            match intl::canonical_tag(&s) {
                Ok(c) => {
                    if !out.contains(&c) {
                        out.push(c);
                    }
                }
                Err(intl::TagError::Malformed) => return Err(self.range_error("Incorrect locale information provided")),
                Err(intl::TagError::Invalid) => return Err(self.range_error(&format!("Invalid language tag: {}", s.to_ascii_lowercase()))),
            }
        }
        Ok(out)
    }

    /// Keep the extension of the formatter just made (`pending_ext`) for its `resolvedOptions`. At most `max_formatters` entries.
    #[inline]
    fn intl_keep_ext(&mut self, o: ObjId) {
        self.intl.in_ctor = false;
        if let Some(e) = self.intl.pending_ext.take() {
            self.intl.exts.insert(o, e);
        }
    }

    /// A constructor failed: drop what its create function left.
    #[cold]
    #[inline(never)]
    pub(crate) fn intl_ctor_failed(&mut self) {
        self.intl.in_ctor = false;
        self.intl.pending_ext = None;
    }

    /// Set `pending_ext`, if a constructor runs (a `toLocaleString` call keeps no extension).
    #[cold]
    #[inline(never)]
    pub(crate) fn set_ext(&mut self, e: Option<Box<str>>) {
        if self.intl.in_ctor {
            self.intl.pending_ext = e;
        }
    }

    /// The locale that `resolvedOptions` reports: the name of `loc` and the kept extension of `this`.
    fn intl_locale_value(&mut self, this: Value, loc: u8) -> Res<Value> {
        if let Value::Object(o) = this
            && let Some(e) = self.intl.exts.get(&o)
        {
            let s = format!("{}{}", intl::locale_name(loc), e);
            return self.str_value(&s);
        }
        self.str_value(intl::locale_name(loc))
    }

    /// The `-u-` keywords of the first requested locale that bare has (the one the formatter uses): (key, type).
    #[cold]
    #[inline(never)]
    fn intl_keywords(requested: &[String]) -> Vec<(String, String)> {
        requested.iter().find(|t| intl::lookup(t).is_some()).map_or_else(Vec::new, |t| intl::unicode_keywords(t))
    }

    /// ResolveLocale for `calendar` (`ca`) or `numberingSystem` (`nu`), with the option `opt`: true when the keyword is used and
    /// goes in the resolved locale. The option wins over the keyword. A keyword for a calendar or numbering system that ICU has
    /// and bare has not is `Unsupported`, as the option is; an unknown value is ignored.
    #[cold]
    #[inline(never)]
    fn intl_keyword_used(kw: &[(String, String)], key: &str, opt: Option<&str>, supported: &str) -> Result<bool, &'static str> {
        let Some((_, v)) = kw.iter().find(|k| k.0 == key) else { return Ok(false) };
        if let Some(o) = opt {
            return Ok(o.eq_ignore_ascii_case(v) && v == supported);
        }
        if v == supported {
            return Ok(true);
        }
        if Self::intl_supported_name(key == "ca", v) { Ok(false) } else { Err(if key == "ca" { "calendar" } else { "numberingSystem" }) }
    }

    /// The key of option `name`: made once for `OPTION_NAMES`, on the first option read.
    fn intl_key(&mut self, name: &str) -> Res<PropertyKey> {
        if self.intl.keys.is_empty() {
            for n in OPTION_NAMES {
                let k = h!(self, self.heap.key_from_str(n));
                self.intl.keys.push(k);
            }
        }
        match option_index(name).and_then(|i| self.intl.keys.get(i)) {
            Some(&k) => Ok(k),
            None => Ok(h!(self, self.heap.key_from_str(name))),
        }
    }

    /// GetOption for a string: `None` when absent, else one of `values` (its index) or a RangeError.
    fn intl_opt(&mut self, opts: Option<Value>, name: &str, values: &[&str]) -> Res<Option<usize>> {
        self.intl_opt_for("Intl.DateTimeFormat", opts, name, values)
    }

    /// `intl_opt` for the constructor `who`, which the RangeError names.
    fn intl_opt_for(&mut self, who: &str, opts: Option<Value>, name: &str, values: &[&str]) -> Res<Option<usize>> {
        let Some(o) = opts else { return Ok(None) };
        let k = self.intl_key(name)?;
        let v = self.get_prop_nat(o, k)?;
        if matches!(v, Value::Undefined) {
            return Ok(None);
        }
        let s = self.coerce_string(v)?;
        let s = h!(self, self.heap.to_string_lossy(s));
        match values.iter().position(|x| *x == s) {
            Some(i) => Ok(Some(i)),
            None => Err(self.range_error(&format!("Value {s} out of range for {who} options property {name}"))),
        }
    }

    /// False for a calendar (`calendar`) or a numbering system that ICU has and bare has not: V8 would use it.
    #[cold]
    #[inline(never)]
    fn intl_supported_name(calendar: bool, s: &str) -> bool {
        let known: &[&str] = if calendar { &NON_GREGORY } else { &NON_LATN };
        !known.contains(&s.to_ascii_lowercase().as_str())
    }

    /// GetNumberOption of `fractionalSecondDigits`: 0 when absent, else an integer 1 to 3 (a RangeError otherwise).
    #[inline(never)]
    fn intl_fsd(&mut self, opts: Option<Value>) -> Res<u8> {
        let Some(o) = opts else { return Ok(0) };
        let k = self.intl_key("fractionalSecondDigits")?;
        let v = self.get_prop_nat(o, k)?;
        if matches!(v, Value::Undefined) {
            return Ok(0);
        }
        let n = self.coerce_number(v)?;
        if !(1.0..=3.0).contains(&n) {
            return Err(self.range_error("fractionalSecondDigits value is out of range."));
        }
        Ok(n.floor() as u8)
    }

    /// GetOption for any string (calendar, numberingSystem, timeZone): `None` when absent.
    fn intl_opt_str(&mut self, opts: Option<Value>, name: &str) -> Res<Option<String>> {
        let Some(o) = opts else { return Ok(None) };
        let k = self.intl_key(name)?;
        let v = self.get_prop_nat(o, k)?;
        if matches!(v, Value::Undefined) {
            return Ok(None);
        }
        let s = self.coerce_string(v)?;
        Ok(Some(h!(self, self.heap.to_string_lossy(s))))
    }

    /// A zone by name: the UTC names, an offset, or a tzdata file (read once). `None` when it is not a zone.
    fn intl_zone(&mut self, name: &str) -> Res<Option<(Rc<str>, Rc<Zone>)>> {
        if tz::is_utc_name(name) {
            let utc = self.intl.utc.get_or_insert_with(|| (Rc::from("UTC"), Rc::new(Zone::fixed(0))));
            return Ok(Some(utc.clone()));
        }
        if let Some(off) = tz::parse_offset_name(name) {
            let a = off.abs();
            let n = format!("{}{:02}:{:02}", if off < 0 { '-' } else { '+' }, a / 3600, a / 60 % 60);
            return Ok(Some((Rc::from(n.as_str()), Rc::new(Zone::fixed(off)))));
        }
        if !tz::is_file_name(name) {
            return Ok(None);
        }
        if let Some((k, z)) = self.intl.zones.get_key_value(name) {
            return Ok(Some((k.clone(), z.clone())));
        }
        self.charge_steps(ZONE_READ_STEPS)?;
        let dir = std::env::var_os("TZDIR").map_or_else(|| std::path::PathBuf::from("/usr/share/zoneinfo"), std::path::PathBuf::from);
        let path = dir.join(name);
        let ok = std::fs::metadata(&path).is_ok_and(|m| m.is_file() && m.len() <= tz::MAX_FILE);
        let Some(z) = ok.then(|| std::fs::read(&path).ok()).flatten().and_then(|b| Zone::parse(&b)) else { return Ok(None) };
        let (k, z): (Rc<str>, Rc<Zone>) = (Rc::from(name), Rc::new(z));
        if self.intl.zones.len() < MAX_ZONES {
            self.intl.zones.insert(k.clone(), z.clone());
        }
        Ok(Some((k, z)))
    }

    /// DefaultTimeZone: `set_default_time_zone`, else `TZ`, else the target of `/etc/localtime`, else UTC.
    fn intl_default_zone(&mut self) -> Res<(Rc<str>, Rc<Zone>)> {
        if let Some(z) = &self.intl.default_zone {
            return Ok(z.clone());
        }
        let from_tz = std::env::var("TZ").ok().map(|s| s.trim_start_matches(':').to_string());
        let from_link = || std::fs::read_link("/etc/localtime").ok().and_then(|p| p.to_str().and_then(|s| s.split_once("zoneinfo/").map(|x| x.1.to_string())));
        let mut z = None;
        for name in [from_tz, from_link()].into_iter().flatten() {
            if let Some(found) = self.intl_zone(&name)? {
                z = Some(found);
                break;
            }
        }
        let z = z.unwrap_or_else(|| (Rc::from("UTC"), Rc::new(Zone::fixed(0))));
        self.intl.default_zone = Some(z.clone());
        Ok(z)
    }

    /// CreateDateTimeFormat without the object: `required` 0 any, 1 date, 2 time; `defaults` 0 all, 1 date, 2 time.
    fn intl_create(&mut self, locales: Value, options: Value, required: u8, defaults: u8) -> Res<Dtf> {
        let requested = self.intl_locales(locales)?;
        let opts = match options {
            Value::Undefined => None,
            Value::Null => return Err(self.type_err("Cannot convert undefined or null to object")),
            v => Some(v),
        };
        self.intl_opt(opts, "localeMatcher", &["lookup", "best fit"])?;
        let has_ext = requested.iter().any(|t| intl::has_unicode_ext(t));
        let mut cal_nu = [0u8; 2];
        for (n, name) in ["calendar", "numberingSystem"].into_iter().enumerate() {
            if let Some(s) = self.intl_opt_str(opts, name)? {
                if !s.split('-').all(|p| (3..=8).contains(&p.len()) && p.bytes().all(|c| c.is_ascii_alphanumeric())) {
                    return Err(self.range_error(&format!("Invalid {name} : {s}")));
                }
                if !Self::intl_supported_name(name == "calendar", &s) {
                    return Err(unsupported(if name == "calendar" { "Intl.DateTimeFormat calendar other than gregory" } else { "Intl.DateTimeFormat numberingSystem other than latn" }, Span::default()));
                }
                if has_ext {
                    // 1: the value bare has; 2: another value that bare accepts
                    cal_nu[n] = if s.eq_ignore_ascii_case(["gregory", "latn"][n]) { 1 } else { 2 };
                }
            }
        }
        let hour12 = match opts {
            Some(o) => {
                let k = self.intl_key("hour12")?;
                let v = self.get_prop_nat(o, k)?;
                if matches!(v, Value::Undefined) { None } else { Some(self.truthy(v)?) }
            }
            None => None,
        };
        let hc_opt = self.intl_opt(opts, "hourCycle", &CYCLES)?.map(|i| i as u8);
        let loc = requested.iter().find_map(|t| intl::lookup(t)).unwrap_or(0);
        let kw = if has_ext { self.intl_dtf_keywords(&requested, &cal_nu)? } else { DtfKw::default() };
        let (zone_name, zone) = match self.intl_opt_str(opts, "timeZone")? {
            None => self.intl_default_zone()?,
            Some(n) => match self.intl_zone(&n)? {
                Some(z) => z,
                None => return Err(self.range_error(&format!("Invalid time zone specified: {n}"))),
            },
        };
        let mut r = intl::Request::default();
        let w = self.intl_opt(opts, "weekday", &WIDTHS_TEXT)?;
        r.weekday = w.map_or(0, |i| intl::NARROW + i as u8);
        if self.intl_opt(opts, "era", &WIDTHS_TEXT)?.is_some() {
            return Err(unsupported("Intl.DateTimeFormat option era", Span::default()));
        }
        let num = |i: Option<usize>| i.map_or(0, |i| i as u8 + 1);
        r.year = num(self.intl_opt(opts, "year", &WIDTHS_NUM)?);
        r.month = num(self.intl_opt(opts, "month", &WIDTHS_MONTH)?);
        r.day = num(self.intl_opt(opts, "day", &WIDTHS_NUM)?);
        if self.intl_opt(opts, "dayPeriod", &WIDTHS_TEXT)?.is_some() {
            return Err(unsupported("Intl.DateTimeFormat option dayPeriod", Span::default()));
        }
        r.hour = num(self.intl_opt(opts, "hour", &WIDTHS_NUM)?);
        r.minute = num(self.intl_opt(opts, "minute", &WIDTHS_NUM)?);
        r.second = num(self.intl_opt(opts, "second", &WIDTHS_NUM)?);
        let fsd = self.intl_fsd(opts)?;
        let zn = self.intl_opt(opts, "timeZoneName", &tz_names::KINDS)?.map_or(0, |i| i as u8 + 1);
        r.zone = zn != 0;
        self.intl_opt(opts, "formatMatcher", &["basic", "best fit"])?;
        r.date_style = num(self.intl_opt(opts, "dateStyle", &STYLES)?);
        r.time_style = num(self.intl_opt(opts, "timeStyle", &STYLES)?);
        if fsd != 0 && r.second == 0 && r.date_style == 0 && r.time_style == 0 {
            return Err(unsupported("Intl.DateTimeFormat: fractionalSecondDigits without second", Span::default()));
        }
        let mut time_defaulted = false;
        let explicit = r.weekday | r.year | r.month | r.day | r.hour | r.minute | r.second | zn | fsd != 0;
        if r.date_style != 0 || r.time_style != 0 {
            if explicit {
                return Err(self.type_err("Invalid option : option"));
            }
            if required == 1 && r.time_style != 0 {
                return Err(self.type_err("Invalid option : timeStyle"));
            }
            if required == 2 && r.date_style != 0 {
                return Err(self.type_err("Invalid option : dateStyle"));
            }
        } else {
            let date_set = r.weekday | r.year | r.month | r.day != 0;
            let time_set = r.hour | r.minute | r.second != 0;
            let need = match required { 1 => !date_set, 2 => !time_set, _ => !date_set && !time_set };
            if need && defaults != 2 {
                (r.year, r.month, r.day) = (intl::NUMERIC, intl::NUMERIC, intl::NUMERIC);
            }
            if need && defaults != 1 {
                (r.hour, r.minute, r.second) = (intl::NUMERIC, intl::NUMERIC, intl::NUMERIC);
                // V8 (Node 22): with the default time fields, h11 prints as h12 and h24 as h23
                time_defaulted = true;
            }
        }
        // hour12 replaces hourCycle
        r.hc = intl::hour_cycle(loc, hour12, if hour12.is_some() { None } else { hc_opt });
        if has_ext && kw.hc.is_some() && hour12.is_none() && hc_opt.is_none() {
            // the keyword `hc` is the last choice, and the default time fields do not change it
            r.hc = intl::hour_cycle(loc, None, kw.hc);
        } else if time_defaulted {
            r.hc = [1, 1, 2, 2][r.hc as usize];
        }
        // a time style of full or long writes the long or the short name
        let zn = match r.time_style {
            1 => tz_names::LONG + 1,
            2 => tz_names::SHORT + 1,
            _ => zn,
        };
        let pat = intl::resolve(loc, &r);
        if has_ext {
            self.intl_dtf_extension(kw, hour12.is_some() || hc_opt.is_some(), pat);
        }
        Ok(Dtf { loc, pat, styles: (r.date_style, r.time_style), zone_name, zone, zn, fsd, extras: zn != 0 || fsd != 0, bound: None })
    }

    /// The `-u-` keywords that `DateTimeFormat` reads from the requested locales (`ca`, `hc`, `nu`), after the options `calendar`
    /// and `numberingSystem` (`cal_nu`: 0 none, 1 the value of bare, 2 another). A calendar or numbering system that bare lacks and ICU has is `Unsupported`.
    #[cold]
    #[inline(never)]
    fn intl_dtf_keywords(&mut self, requested: &[String], cal_nu: &[u8; 2]) -> Res<DtfKw> {
        let kw = Self::intl_keywords(requested);
        let mut out = DtfKw::default();
        for (i, (key, supported)) in [("ca", "gregory"), ("nu", "latn")].into_iter().enumerate() {
            match Self::intl_keyword_used(&kw, key, [None, Some(supported), Some("")][cal_nu[i] as usize], supported) {
                Ok(used) => if i == 0 { out.ca = used } else { out.nu = used },
                Err(_) => return Err(unsupported(if i == 0 { "Intl.DateTimeFormat calendar other than gregory" } else { "Intl.DateTimeFormat numberingSystem other than latn" }, Span::default())),
            }
        }
        out.hc = kw.iter().find(|k| k.0 == "hc").and_then(|k| CYCLES.iter().position(|c| *c == k.1)).map(|i| i as u8);
        Ok(out)
    }

    /// The extension for `resolvedOptions().locale` of a new formatter with keywords `kw`. A keyword `hc` stays when no option
    /// chose the cycle, or when the pattern has an hour that has the cycle of the keyword.
    #[cold]
    #[inline(never)]
    fn intl_dtf_extension(&mut self, kw: DtfKw, hc_option: bool, pat: intl::Pattern) {
        let mut s = String::new();
        if kw.ca {
            s.push_str("-ca-gregory");
        }
        if let Some(h) = kw.hc.filter(|h| !hc_option || intl::shown(pat).1 == Some(*h)) {
            s.push_str("-hc-");
            s.push_str(CYCLES[h as usize]);
        }
        if kw.nu {
            s.push_str("-nu-latn");
        }
        self.set_ext(if s.is_empty() { None } else { Some(format!("-u{s}").into()) });
    }

    /// The formatter of `this`, or the TypeError of V8. V8 names `unwrap` (when given) for an object receiver: its legacy
    /// unwrap step runs first there.
    fn this_dtf(&mut self, this: Value, method: &str, unwrap: bool) -> Res<Dtf> {
        if let Value::Object(o) = this
            && let Some(d) = self.intl.dtfs.get(&o)
        {
            return Ok(d.clone());
        }
        let method = if unwrap && matches!(this, Value::Object(_)) { "UnwrapDateTimeFormat" } else { method };
        let t = self.display(this);
        Err(self.type_err(&format!("Method {method} called on incompatible receiver {t}")))
    }

    /// Format time value `t` (valid) with `d`: the text, and the parts when asked.
    fn intl_format(&mut self, d: &Dtf, t: f64, parts: Option<&mut Vec<(intl::Part, usize)>>) -> Res<String> {
        let secs = (t / 1000.0).floor() as i64;
        let local = t + d.zone.offset(secs) as f64 * 1000.0;
        let f = date::fields(local).ok_or_else(|| self.range_error("Invalid time value"))?;
        let mut out = String::new();
        let r = if !d.extras { intl::format(d.loc, d.pat, &f, &NO_EXTRAS, &mut out, parts) } else { Self::intl_format_extras(d, secs, &f, &mut out, parts) };
        match r {
            Ok(()) => Ok(out),
            Err(intl::Gate::ZoneName) => Err(unsupported("Intl.DateTimeFormat: a pattern letter that bare cannot format", Span::default())),
            Err(intl::Gate::Era) => Err(unsupported("Intl.DateTimeFormat: a year before 1 (an era)", Span::default())),
        }
    }

    /// `intl::format` of a formatter with a time zone name or fractional seconds (the cold path of `intl_format`).
    #[cold]
    #[inline(never)]
    fn intl_format_extras(d: &Dtf, secs: i64, f: &date::Fields, out: &mut String, parts: Option<&mut Vec<(intl::Part, usize)>>) -> Result<(), intl::Gate> {
        let name = if d.zn == 0 { String::new() } else { tz_names::name(&d.zone_name, d.zone.offset(secs), d.zn - 1, d.loc) };
        let mut parts = parts;
        intl::format(d.loc, d.pat, f, &intl::Extras { zone: &name, fsd: d.fsd }, out, parts.as_deref_mut())?;
        if d.pat.has_zone() {
            intl::zone_tail(d.pat, &name, out, parts);
        }
        Ok(())
    }

    /// The time value of a `format` argument: now for undefined, else ToNumber and TimeClip; NaN is a RangeError.
    fn intl_time_arg(&mut self, v: Value) -> Res<f64> {
        let t = if matches!(v, Value::Undefined) { self.wall_now_ms() } else { self.coerce_number(v)? };
        let t = date::time_clip(t);
        if t.is_nan() {
            return Err(self.range_error("Invalid time value"));
        }
        Ok(t)
    }

    // ------------------------------------------------------------ natives

    /// A call without `new` makes a formatter too (but not plural rules), so `is_new` is only for PluralRules.
    fn call_intl(&mut self, f: IntlFn, this: Value, args: &[Value], is_new: bool) -> Res<Value> {
        let (a0, a1) = (arg(args, 0), arg(args, 1));
        match f {
            IntlFn::LazyIntl => {
                // the getter has no argument; the setter has the new value
                if let Some(&v) = args.first() {
                    let k = h!(self, self.heap.key_from_str("Intl"));
                    self.def(self.global, k, v, true, false, true)?;
                    return Ok(Value::Undefined);
                }
                Ok(Value::Object(self.intl_make()?))
            }
            IntlFn::DtfCtor => {
                let nt = self.cls.native_nt.take();
                if self.intl_formatters() >= self.sc.limits.max_formatters {
                    return Err(Error::Limit(LimitKind::Sc(ScLimit::Formatters)));
                }
                let proto = self.nt_proto(nt, self.intl.proto)?;
                self.intl.in_ctor = true;
                let d = self.intl_create(a0, a1, 0, 1).inspect_err(|_| self.intl_ctor_failed())?;
                let o = h!(self, self.heap.new_object(Some(proto)));
                self.intl.dtfs.insert(o, d);
                self.intl_keep_ext(o);
                Ok(Value::Object(o))
            }
            IntlFn::DtfSupported => {
                let tags = self.intl_locales(a0)?;
                if !matches!(a1, Value::Undefined) {
                    self.intl_opt(Some(a1), "localeMatcher", &["lookup", "best fit"])?;
                }
                let mut out = Vec::new();
                for t in tags.iter().filter(|t| intl::lookup(t).is_some()) {
                    out.push(self.str_value(t)?);
                }
                self.new_array_from(&out)
            }
            IntlFn::DtfFormatGet => {
                let d = self.this_dtf(this, "get Intl.DateTimeFormat.prototype.format", true)?;
                if let Some(b) = d.bound {
                    return Ok(Value::Object(b));
                }
                let b = self.new_func_object(Callee::Bound { target: self.intl.format_fn, this, args: Vec::new() })?;
                self.def(b, self.k_length, Value::Number(1.0), false, false, true)?;
                let empty = h!(self, self.heap.intern_str(""));
                self.def(b, self.k_name, Value::String(empty), false, false, true)?;
                if let Value::Object(o) = this
                    && let Some(d) = self.intl.dtfs.get_mut(&o)
                {
                    d.bound = Some(b);
                }
                Ok(Value::Object(b))
            }
            IntlFn::DtfFormat => {
                let d = self.this_dtf(this, "Intl.DateTimeFormat.prototype.format", false)?;
                let t = self.intl_time_arg(a0)?;
                let s = self.intl_format(&d, t, None)?;
                self.str_value(&s)
            }
            IntlFn::DtfToParts => {
                let d = self.this_dtf(this, "Intl.DateTimeFormat.prototype.formatToParts", false)?;
                let t = self.intl_time_arg(a0)?;
                let mut parts = Vec::new();
                let s = self.intl_format(&d, t, Some(&mut parts))?;
                let (kt, kv) = (h!(self, self.heap.key_from_str("type")), h!(self, self.heap.key_from_str("value")));
                let mut items = Vec::with_capacity(parts.len());
                let mut start = 0;
                for (p, end) in parts {
                    let o = h!(self, self.heap.new_object(Some(self.object_proto)));
                    let ty = self.str_value(intl::PART_NAMES[p as usize])?;
                    let v = self.str_value(&s[start..end])?;
                    self.def(o, kt, ty, true, true, true)?;
                    self.def(o, kv, v, true, true, true)?;
                    items.push(Value::Object(o));
                    start = end;
                }
                self.new_array_from(&items)
            }
            IntlFn::DtfResolved => {
                let d = self.this_dtf(this, "Intl.DateTimeFormat.prototype.resolvedOptions", true)?;
                let o = h!(self, self.heap.new_object(Some(self.object_proto)));
                let mut props: Vec<(&str, Value)> = vec![
                    ("locale", self.intl_locale_value(this, d.loc)?),
                    ("calendar", self.str_value("gregory")?),
                    ("numberingSystem", self.str_value("latn")?),
                    ("timeZone", self.str_value(&d.zone_name)?),
                ];
                let (fields, hc) = intl::shown(d.pat);
                if let Some(hc) = hc {
                    props.push(("hourCycle", self.str_value(CYCLES[hc as usize])?));
                    props.push(("hour12", Value::Bool(hc < 2)));
                }
                if d.styles == (0, 0) {
                    for (n, v) in fields {
                        props.push((n, self.str_value(v)?));
                    }
                    if d.fsd != 0 {
                        props.push(("fractionalSecondDigits", Value::Number(d.fsd as f64)));
                    }
                    if d.zn != 0 {
                        props.push(("timeZoneName", self.str_value(tz_names::KINDS[d.zn as usize - 1])?));
                    }
                } else {
                    if d.styles.0 != 0 {
                        props.push(("dateStyle", self.str_value(STYLES[d.styles.0 as usize - 1])?));
                    }
                    if d.styles.1 != 0 {
                        props.push(("timeStyle", self.str_value(STYLES[d.styles.1 as usize - 1])?));
                    }
                }
                for (n, v) in props {
                    let k = h!(self, self.heap.key_from_str(n));
                    self.def(o, k, v, true, true, true)?;
                }
                Ok(Value::Object(o))
            }
            IntlFn::DateLocale(kind) => {
                let (_, t) = self.this_date(this)?;
                if t.is_nan() {
                    return self.date_string(None);
                }
                let (required, defaults) = [(0, 0), (1, 1), (2, 2)][kind as usize];
                let d = self.intl_create(a0, a1, required, defaults)?;
                let s = self.intl_format(&d, t, None)?;
                self.str_value(&s)
            }
            _ => self.call_intl_num(f, this, args, is_new),
        }
    }
}

include!("eval_intl_num.rs");
include!("eval_intl_segmenter.rs");
include!("eval_intl_locale.rs");
include!("eval_intl_rtf.rs");
include!("eval_intl_list.rs");
