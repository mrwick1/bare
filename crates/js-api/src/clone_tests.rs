// Tests of the clone record format, the cost model and the limits. No engine is needed: a small value model is written and read back.
use crate::clone::*;
use crate::Abort;

fn u(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

#[derive(Clone, Debug, PartialEq)]
enum K {
    Idx(u32),
    Name(Vec<u16>),
}

#[derive(Clone, Debug, PartialEq)]
enum V {
    Undef,
    Null,
    Bool(bool),
    /// The bits of the number.
    Num(u64),
    Str(Vec<u16>),
    Big(bool, Vec<u64>),
    Ref(u32),
    Obj(Vec<(K, V)>),
    ArrD(Vec<Option<V>>, Vec<(K, V)>),
    Arr(u32, Vec<(K, V)>),
    Date(u64),
    RegExp(Vec<u16>, Vec<u16>),
    Map(Vec<(V, V)>),
    Set(Vec<V>),
    Error(Vec<u16>, Vec<(ErrorField, V)>),
    Wrapper(Box<V>),
    Buf(Vec<u8>),
    View(u8, usize, usize, Box<V>),
    Moved(u32),
}

fn num(n: f64) -> V {
    V::Num(n.to_bits())
}

fn name(s: &str) -> K {
    K::Name(u(s))
}

/// The identity of a key in the test: its text. The engines use an atom.
fn ident(t: &[u16]) -> u64 {
    t.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &c| (h ^ c as u64).wrapping_mul(0x100_0000_01b3))
}

fn put_key(w: &mut RecordWriter, k: &K) {
    match k {
        K::Idx(i) => w.key_index(*i),
        K::Name(t) => w.key(ident(t), t),
    }
}

fn put_props(w: &mut RecordWriter, ps: &[(K, V)]) {
    for (k, v) in ps {
        put_key(w, k);
        put(w, v);
    }
    w.end();
}

fn put(w: &mut RecordWriter, v: &V) {
    match v {
        V::Undef => w.undefined(),
        V::Null => w.null(),
        V::Bool(b) => w.boolean(*b),
        V::Num(b) => w.number(f64::from_bits(*b)),
        V::Str(t) => w.string(t),
        V::Big(n, l) => w.bigint(*n, l),
        V::Ref(i) => w.reference(*i),
        V::Obj(ps) => {
            w.object();
            put_props(w, ps);
        }
        V::ArrD(items, ps) => {
            w.array_dense(items.len());
            for i in items {
                match i {
                    Some(v) => put(w, v),
                    None => w.hole(),
                }
            }
            put_props(w, ps);
        }
        V::Arr(len, ps) => {
            w.array_sparse(*len);
            put_props(w, ps);
        }
        V::Date(b) => {
            w.date(f64::from_bits(*b));
        }
        V::RegExp(s, f) => {
            w.regexp(s, f);
        }
        V::Map(es) => {
            w.map();
            for (k, v) in es {
                put(w, k);
                put(w, v);
            }
            w.end();
        }
        V::Set(vs) => {
            w.set();
            for v in vs {
                put(w, v);
            }
            w.end();
        }
        V::Error(n, fs) => {
            w.error(n);
            for (f, v) in fs {
                w.error_field(*f);
                put(w, v);
            }
            w.end();
        }
        V::Wrapper(p) => {
            w.wrapper();
            put(w, p);
        }
        V::Buf(b) => {
            w.buffer(b);
        }
        V::View(ty, off, len, buf) => {
            w.view(*ty, *off, *len);
            put(w, buf);
            w.view_done();
        }
        V::Moved(i) => {
            w.moved(*i);
        }
    }
}

fn encode(v: &V) -> Box<[u8]> {
    let mut w = RecordWriter::new();
    put(&mut w, v);
    w.finish()
}

type R<T> = Result<T, ReadError>;

#[derive(Default)]
struct Ctx {
    table: Vec<Vec<u16>>,
    /// Every tag that the reader met, with the key and field tags.
    tags: Vec<Tag>,
}

fn read_key(r: &mut RecordReader, c: &mut Ctx) -> R<Option<K>> {
    Ok(match r.key()? {
        None => {
            c.tags.push(Tag::End);
            None
        }
        Some(Key::New(t)) => {
            c.tags.push(if matches!(t, Text::Latin1(_)) { Tag::KNew8 } else { Tag::KNew16 });
            c.table.push(t.to_vec());
            Some(K::Name(t.to_vec()))
        }
        Some(Key::Ref(n)) => {
            c.tags.push(Tag::KRef);
            Some(K::Name(c.table[n as usize].clone()))
        }
        Some(Key::Index(i)) => {
            c.tags.push(Tag::KIdx);
            Some(K::Idx(i))
        }
    })
}

fn read_props(r: &mut RecordReader, c: &mut Ctx) -> R<Vec<(K, V)>> {
    let mut ps = Vec::new();
    while let Some(k) = read_key(r, c)? {
        ps.push((k, read(r, c)?));
    }
    Ok(ps)
}

fn end(r: &mut RecordReader, c: &mut Ctx) -> R<bool> {
    let e = r.end()?;
    if e {
        c.tags.push(Tag::End);
    }
    Ok(e)
}

fn read(r: &mut RecordReader, c: &mut Ctx) -> R<V> {
    let t = r.tag()?;
    c.tags.push(t);
    read_tagged(r, c, t)
}

fn read_tagged(r: &mut RecordReader, c: &mut Ctx, t: Tag) -> R<V> {
    Ok(match t {
        Tag::Undef => V::Undef,
        Tag::Null => V::Null,
        Tag::True => V::Bool(true),
        Tag::False => V::Bool(false),
        Tag::Int => num(r.int()? as f64),
        Tag::Num => num(r.num()?),
        t @ (Tag::Str8 | Tag::Str16) => V::Str(r.text(t)?.to_vec()),
        Tag::Big => {
            let mut l = Vec::new();
            let n = r.bigint(&mut l)?;
            V::Big(n, l)
        }
        Tag::Ref => V::Ref(r.reference()?),
        Tag::Obj => V::Obj(read_props(r, c)?),
        Tag::ArrD => {
            let n = r.dense_len()?;
            let mut items = Vec::with_capacity(n);
            for _ in 0..n {
                let t = r.tag()?;
                c.tags.push(t);
                items.push(if t == Tag::Hole { None } else { Some(read_tagged(r, c, t)?) });
            }
            V::ArrD(items, read_props(r, c)?)
        }
        Tag::Arr => {
            let len = r.sparse_len()?;
            V::Arr(len, read_props(r, c)?)
        }
        Tag::Date => V::Date(r.num()?.to_bits()),
        Tag::RegExp => {
            let (s, f) = r.regexp()?;
            V::RegExp(s.to_vec(), f.to_vec())
        }
        Tag::Map => {
            let mut es = Vec::new();
            while !end(r, c)? {
                let k = read(r, c)?;
                es.push((k, read(r, c)?));
            }
            V::Map(es)
        }
        Tag::Set => {
            let mut vs = Vec::new();
            while !end(r, c)? {
                vs.push(read(r, c)?);
            }
            V::Set(vs)
        }
        Tag::Error => {
            let n = r.string()?.to_vec();
            let mut fs = Vec::new();
            while let Some(f) = r.error_field()? {
                c.tags.push(match f {
                    ErrorField::Message => Tag::ErrMessage,
                    ErrorField::Stack => Tag::ErrStack,
                    ErrorField::Cause => Tag::ErrCause,
                });
                fs.push((f, read(r, c)?));
            }
            c.tags.push(Tag::End);
            V::Error(n, fs)
        }
        Tag::Wrapper => V::Wrapper(Box::new(read(r, c)?)),
        Tag::Buf => V::Buf(r.buffer()?.to_vec()),
        Tag::View => {
            let (ty, off, len) = r.view()?;
            let buf = read(r, c)?;
            r.view_done();
            V::View(ty, off, len, Box::new(buf))
        }
        Tag::Moved => V::Moved(r.moved()?),
        t => return Err(ReadError::BadTag(t as u8)),
    })
}

/// Decode one value. A `Hole` outside an array is a bad tag.
fn decode(b: &[u8]) -> R<V> {
    decode_with(b, &mut Ctx::default())
}

fn decode_with(b: &[u8], c: &mut Ctx) -> R<V> {
    let mut r = RecordReader::new(b);
    let v = read(&mut r, c)?;
    r.finish()?;
    Ok(v)
}

/// A value of every tag (`Host` is reserved), with a cycle (`Ref(0)`).
fn every_tag() -> V {
    let long = "x".repeat(200);
    V::Obj(vec![
        (name("undef"), V::Undef),
        (name("\u{4e2d}"), V::Null),
        (name("null"), V::Null),
        (name("t"), V::Bool(true)),
        (name("f"), V::Bool(false)),
        (name("int"), num(-5.0)),
        (name("num"), num(1.5)),
        (name("nan"), num(f64::NAN)),
        (name("negzero"), num(-0.0)),
        (name("s8"), V::Str(u("h\u{e9}llo"))),
        (name("s16"), V::Str(vec![0x4e2d, 0x6587, 0xd83d, 0xde00, 0xd800])),
        (name("long"), V::Str(u(&long))),
        (name("big"), V::Big(true, vec![1, u64::MAX, 0])),
        (name("self"), V::Ref(0)),
        (name("dense"), V::ArrD(vec![Some(num(1.0)), None, Some(V::Str(u("a")))], vec![(name("extra"), V::Null)])),
        (name("sparse"), V::Arr(1000, vec![(K::Idx(999), num(2.0)), (name("p"), V::Undef)])),
        (name("date"), V::Date(86_400_000f64.to_bits())),
        (name("re"), V::RegExp(u("a+b"), u("gi"))),
        (name("map"), V::Map(vec![(V::Str(u("k")), V::Ref(0)), (num(1.0), V::Null)])),
        (name("set"), V::Set(vec![num(3.0), V::Str(u("s"))])),
        (name("err"), V::Error(u("TypeError"), vec![(ErrorField::Message, V::Str(u("m"))), (ErrorField::Stack, V::Str(u("s"))), (ErrorField::Cause, V::Ref(0))])),
        (name("wrap"), V::Wrapper(Box::new(V::Str(u("w"))))),
        (name("buf"), V::Buf((0..=255).collect())),
        (name("view"), V::View(2, 4, 3, Box::new(V::Buf(vec![0; 16])))),
        (name("moved"), V::Moved(0)),
        (name("undef"), V::Null),
    ])
}

#[test]
fn every_tag_round_trips() {
    let v = every_tag();
    let b = encode(&v);
    assert_eq!(decode(&b), Ok(v));
}

#[test]
fn every_tag_is_covered() {
    // The reader met every tag of the table, in the payloads and the key, field and `End` positions. `Host` is reserved.
    let mut c = Ctx::default();
    decode_with(&encode(&every_tag()), &mut c).unwrap();
    for b in 0..32u8 {
        if b == Tag::Host as u8 {
            continue;
        }
        let t = RecordReader::new(&[b]).tag().unwrap();
        assert!(c.tags.contains(&t), "{t:?} is not in the record");
    }
}

#[test]
fn tag_values_are_stable_and_host_is_reserved() {
    for b in 0..=255u8 {
        let mut r = RecordReader::new(std::slice::from_ref(&b));
        match b {
            28 => assert_eq!(r.tag(), Err(ReadError::Reserved)),
            0..=31 => assert_eq!(r.tag().map(|t| t as u8), Ok(b)),
            _ => assert_eq!(r.tag(), Err(ReadError::BadTag(b))),
        }
    }
}

#[test]
fn numbers_choose_int_or_num() {
    let tag = |n: f64| {
        let mut w = RecordWriter::new();
        w.number(n);
        w.finish()[0]
    };
    for n in [0.0, 1.0, -1.0, 63.0, 64.0, i32::MAX as f64, i32::MIN as f64] {
        assert_eq!(tag(n), Tag::Int as u8, "{n}");
    }
    for n in [-0.0, 1.5, f64::NAN, f64::INFINITY, f64::NEG_INFINITY, i32::MAX as f64 + 1.0, i32::MIN as f64 - 1.0, 4294967296.0, f64::MIN_POSITIVE] {
        assert_eq!(tag(n), Tag::Num as u8, "{n}");
    }
    for n in [0.0, -0.0, 1.5, f64::NAN, f64::INFINITY, -7.0, i32::MAX as f64, i32::MIN as f64, 1e300, f64::from_bits(1)] {
        let b = encode(&num(n));
        assert_eq!(decode(&b), Ok(num(n)), "{n}: the bits survive (-0, NaN)");
    }
}

#[test]
fn varint_edges() {
    let cases: [(u64, usize); 12] = [(0, 1), (1, 1), (127, 1), (128, 2), (16383, 2), (16384, 3), (u32::MAX as u64, 5), (1 << 35, 6), (1 << 56, 9), (1 << 63, 10), (u64::MAX - 1, 10), (u64::MAX, 10)];
    for (n, len) in cases {
        let mut w = RecordWriter::new();
        w.varint(n);
        let b = w.finish();
        assert_eq!(b.len(), len, "{n}");
        let mut r = RecordReader::new(&b);
        assert_eq!(r.varint(), Ok(n));
        r.finish().unwrap();
    }
    // A cut varint, an 11-byte varint and a 10th byte with bits above 64.
    assert_eq!(RecordReader::new(&[0x80]).varint(), Err(ReadError::Truncated));
    assert_eq!(RecordReader::new(&[0x80; 10]).varint(), Err(ReadError::BadVarint));
    assert_eq!(RecordReader::new(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x02]).varint(), Err(ReadError::BadVarint));
    assert_eq!(RecordReader::new(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01]).varint(), Ok(u64::MAX));
}

#[test]
fn zigzag_ints() {
    for (i, len) in [(0, 1), (-1, 1), (1, 1), (63, 1), (-64, 1), (64, 2), (-65, 2), (8191, 2), (-8192, 2), (i32::MAX, 5), (i32::MIN, 5)] {
        let mut w = RecordWriter::new();
        w.int(i);
        let b = w.finish();
        assert_eq!(b.len(), 1 + len, "{i}");
        let mut r = RecordReader::new(&b);
        assert_eq!(r.tag(), Ok(Tag::Int));
        assert_eq!(r.int(), Ok(i));
    }
    // 0 -> 0, -1 -> 1, 1 -> 2, as zigzag says.
    for (i, z) in [(0, 0u8), (-1, 1), (1, 2), (-2, 3)] {
        let mut w = RecordWriter::new();
        w.int(i);
        assert_eq!(&*w.finish(), &[Tag::Int as u8, z]);
    }
    // An `Int` above 32 bits is damage.
    let mut w = RecordWriter::new();
    w.varint(1 << 32);
    let b = w.finish();
    assert_eq!(RecordReader::new(&b).int(), Err(ReadError::BadVarint));
}

#[test]
fn lengths_cross_the_varint_edges() {
    for n in [0usize, 1, 127, 128, 129, 16383, 16384] {
        let v = V::Buf(vec![7; n]);
        let b = encode(&v);
        let head = if n < 128 { 1 } else if n < 16384 { 2 } else { 3 };
        assert_eq!(b.len(), 1 + head + n, "{n}");
        assert_eq!(decode(&b), Ok(v));
        let s = V::Str(vec![0x41; n]);
        assert_eq!(decode(&encode(&s)), Ok(s));
    }
}

#[test]
fn strings_pick_latin1_or_utf16() {
    let tag = |t: &[u16]| {
        let mut w = RecordWriter::new();
        w.string(t);
        w.finish()[0]
    };
    assert_eq!(tag(&u("")), Tag::Str8 as u8);
    assert_eq!(tag(&u("abc\u{ff}")), Tag::Str8 as u8);
    assert_eq!(tag(&u("a\u{100}")), Tag::Str16 as u8);
    assert_eq!(tag(&[0xd800]), Tag::Str16 as u8, "a lone surrogate needs UTF-16");
    for t in [u(""), u("\u{ff}"), u("\u{100}"), vec![0xd800], vec![0xdc00, 0xd800], u("\u{1f600}")] {
        assert_eq!(decode(&encode(&V::Str(t.clone()))), Ok(V::Str(t)));
    }
    let mut w = RecordWriter::new();
    w.string_latin1(b"\xe9a");
    let b = w.finish();
    assert_eq!(decode(&b), Ok(V::Str(vec![0xe9, 0x61])));
    // `Text` reports units.
    let b = encode(&V::Str(u("\u{4e2d}a")));
    let mut r = RecordReader::new(&b);
    let t = r.string().unwrap();
    assert_eq!((t.len(), t.is_empty()), (2, false));
    assert_eq!(t.to_vec(), u("\u{4e2d}a"));
}

/// How many times the reader meets `t` in a record.
fn count(b: &[u8], t: Tag) -> usize {
    let mut c = Ctx::default();
    decode_with(b, &mut c).unwrap();
    c.tags.iter().filter(|&&x| x == t).count()
}

#[test]
fn the_key_table_reuses_keys() {
    // Three objects with the same two keys: each key is written once.
    let o = |a: i32, b: i32| V::Obj(vec![(name("alpha"), num(a as f64)), (name("beta"), num(b as f64))]);
    let one = encode(&o(1, 2)).len();
    let three = V::ArrD(vec![Some(o(1, 2)), Some(o(3, 4)), Some(o(5, 6))], vec![]);
    let b = encode(&three);
    let again = encode(&o(3, 4)).len();
    assert_eq!(one, again);
    // Objects 2 and 3 use `KRef` (1 byte tag + 1 byte number) for the keys.
    let ref_obj = 1 + 2 * (2 + 2) + 1; // Obj, two (KRef n, Int v), End
    assert_eq!(b.len(), 1 + 1 + one + 2 * ref_obj + 1);
    assert_eq!(count(&b, Tag::KNew8), 2);
    assert_eq!(decode(&b), Ok(three));

    // Many keys: past 8 the table changes its structure. Every reference still finds its key.
    let keys: Vec<String> = (0..40).map(|i| format!("k{i}")).collect();
    let mk = |rot: usize| V::Obj((0..40).map(|i| (name(&keys[(i + rot) % 40]), num(i as f64))).collect());
    let v = V::ArrD(vec![Some(mk(0)), Some(mk(7)), Some(mk(13))], vec![]);
    let b = encode(&v);
    assert_eq!(decode(&b), Ok(v));
    assert_eq!((count(&b, Tag::KNew8), count(&b, Tag::KRef)), (40, 80));

    // Two idents with one text make two entries. Both are valid.
    let mut w = RecordWriter::new();
    w.object();
    w.key(1, &u("a"));
    w.null();
    w.key(2, &u("a"));
    w.null();
    w.key(1, &u("a"));
    w.null();
    w.end();
    let b = w.finish();
    assert_eq!(decode(&b), Ok(V::Obj(vec![(name("a"), V::Null), (name("a"), V::Null), (name("a"), V::Null)])));
    assert_eq!(count(&b, Tag::KNew8), 2);

    // A key of UTF-16 text, an index key and a Latin-1 key.
    let v = V::Obj(vec![(name("\u{4e2d}"), V::Null), (K::Idx(u32::MAX), V::Null), (name("\u{e9}"), V::Null)]);
    assert_eq!(decode(&encode(&v)), Ok(v));
    let mut w = RecordWriter::new();
    w.object();
    w.key_latin1(9, b"ab");
    w.null();
    w.key_latin1(9, b"ab");
    w.null();
    w.end();
    assert_eq!(decode(&w.finish()), Ok(V::Obj(vec![(name("ab"), V::Null), (name("ab"), V::Null)])));
}

#[test]
fn a_key_ref_needs_an_earlier_key() {
    let mut w = RecordWriter::new();
    w.object();
    w.key(1, &u("a"));
    w.null();
    w.end();
    let mut b = w.finish().into_vec();
    // `Obj KNew8 1 'a' Null End`: make the key a `KRef 0` before any `KNew` ...
    let at = b.iter().position(|&c| c == Tag::KNew8 as u8).unwrap();
    b.splice(at..at + 3, [Tag::KRef as u8, 0]);
    assert_eq!(decode(&b), Err(ReadError::BadKey));
    // ... and a `KRef` past the table.
    let mut w = RecordWriter::new();
    w.object();
    w.key(1, &u("a"));
    w.null();
    w.key(1, &u("a"));
    w.null();
    w.end();
    let mut b = w.finish().into_vec();
    let at = b.iter().position(|&c| c == Tag::KRef as u8).unwrap();
    b[at + 1] = 1;
    assert_eq!(decode(&b), Err(ReadError::BadKey));
}

#[test]
fn ids_follow_the_tags() {
    // Each object takes an id when its tag is written. A view takes its id after its buffer.
    let mut w = RecordWriter::new();
    assert_eq!(w.object(), 0);
    w.key(1, &u("a"));
    assert_eq!(w.array_dense(0), 1);
    w.end();
    w.key(2, &u("b"));
    w.view(1, 0, 4);
    assert_eq!(w.buffer(&[0; 4]), 2);
    assert_eq!(w.view_done(), 3);
    w.key(3, &u("c"));
    assert_eq!(w.date(0.0), 4);
    w.key(4, &u("d"));
    assert_eq!(w.regexp(&u("a"), &u("g")), 5, "the strings of a RegExp take no id");
    w.key(5, &u("e"));
    w.string(&u("s"));
    w.key(6, &u("f"));
    w.reference(3);
    w.key(7, &u("g"));
    assert_eq!(w.moved(0), 6);
    w.end();
    assert_eq!(w.ids(), 7);
    let b = w.finish();

    let mut r = RecordReader::new(&b);
    let v = read(&mut r, &mut Ctx::default()).unwrap();
    assert_eq!(r.ids(), 7);
    r.finish().unwrap();
    let V::Obj(ps) = v else { panic!() };
    assert_eq!(ps[4].1, V::Str(u("s")));
    assert_eq!(ps[5].1, V::Ref(3));

    // The reader refuses a reference to an id that is not there yet.
    for (id, ok) in [(0, true), (1, false)] {
        let mut w = RecordWriter::new();
        w.array_dense(1);
        w.reference(id);
        w.end();
        let b = w.finish();
        let r = decode(&b);
        if ok {
            assert!(r.is_ok(), "{id}");
        } else {
            assert_eq!(r, Err(ReadError::BadRef), "{id}");
        }
    }
    // The view is not an id until its buffer is read: a reference to it inside its own buffer position fails.
    let mut w = RecordWriter::new();
    w.view(1, 0, 4);
    w.reference(0);
    let b = w.finish();
    assert_eq!(decode(&b), Err(ReadError::BadRef));
    // Ids that the reader gave are the ids of a later `Ref`, also across a view.
    let v = V::ArrD(vec![Some(V::View(1, 0, 4, Box::new(V::Buf(vec![0; 4])))), Some(V::Ref(1)), Some(V::Ref(2)), Some(V::Ref(0))], vec![]);
    assert_eq!(decode(&encode(&v)), Ok(v));
    let bad = V::ArrD(vec![Some(V::View(1, 0, 4, Box::new(V::Buf(vec![0; 4])))), Some(V::Ref(3))], vec![]);
    assert_eq!(decode(&encode(&bad)), Err(ReadError::BadRef));
}

#[test]
fn bigint_limbs() {
    for (neg, l) in [(false, vec![]), (true, vec![0]), (false, vec![u64::MAX; 5])] {
        let v = V::Big(neg, l);
        assert_eq!(decode(&encode(&v)), Ok(v));
    }
}

#[test]
fn a_cut_record_is_an_error_and_never_a_panic() {
    let mut records = vec![encode(&every_tag())];
    for v in [
        V::Undef,
        num(1.0),
        num(1.5),
        V::Str(u("abc")),
        V::Str(u("\u{4e2d}")),
        V::Big(true, vec![1, 2]),
        V::Date(0),
        V::RegExp(u("a"), u("g")),
        V::Buf(vec![1, 2, 3]),
        V::View(1, 0, 3, Box::new(V::Buf(vec![1, 2, 3]))),
        V::Moved(300),
        V::Ref(0),
    ] {
        records.push(encode(&V::ArrD(vec![Some(V::Null), Some(v)], vec![])));
    }
    for b in &records {
        assert!(decode(b).is_ok());
        for n in 0..b.len() {
            let r = decode(&b[..n]);
            assert!(r.is_err(), "a prefix of {n} of {} bytes was read as a record", b.len());
        }
        // A trailing byte is refused.
        let mut more = b.to_vec();
        more.push(Tag::Null as u8);
        assert_eq!(decode(&more), Err(ReadError::Trailing));
    }
}

#[test]
fn damaged_lengths_do_not_allocate_or_panic() {
    let huge = |t: Tag| {
        let mut w = RecordWriter::new();
        w.varint(u64::MAX);
        let mut b = vec![t as u8];
        b.extend_from_slice(&w.finish());
        b
    };
    for t in [Tag::Str8, Tag::Str16, Tag::Buf, Tag::ArrD] {
        let b = huge(t);
        assert!(decode(&b).is_err(), "{t:?}");
    }
    // A limb count and a string length that overflow when multiplied.
    let mut w = RecordWriter::new();
    w.varint(1 << 62);
    let n = w.finish();
    for t in [Tag::Str16, Tag::ArrD] {
        let mut b = vec![t as u8];
        b.extend_from_slice(&n);
        assert_eq!(decode(&b), Err(ReadError::Truncated), "{t:?}");
    }
    let mut b = vec![Tag::Big as u8, 0];
    b.extend_from_slice(&n);
    assert_eq!(decode(&b), Err(ReadError::Truncated));
    // A dense array that claims more values than there are bytes.
    assert_eq!(decode(&[Tag::ArrD as u8, 5, Tag::Null as u8, Tag::End as u8]), Err(ReadError::Truncated));
    // A sparse length above `u32`.
    let mut b = vec![Tag::Arr as u8];
    b.extend_from_slice(&n);
    assert_eq!(decode(&b), Err(ReadError::BadVarint));
    // A string where a key is expected, a key where a value is expected, and a field tag in a value.
    assert_eq!(decode(&[Tag::Obj as u8, Tag::Null as u8]), Err(ReadError::BadTag(Tag::Null as u8)));
    assert_eq!(decode(&[Tag::KIdx as u8, 0]), Err(ReadError::BadTag(Tag::KIdx as u8)));
    assert_eq!(decode(&[Tag::Error as u8, Tag::Null as u8]), Err(ReadError::BadTag(Tag::Null as u8)));
    assert_eq!(decode(&[Tag::Error as u8, Tag::Str8 as u8, 0, Tag::Null as u8]), Err(ReadError::BadTag(Tag::Null as u8)));
    assert_eq!(decode(&[Tag::Host as u8]), Err(ReadError::Reserved));
    assert_eq!(decode(&[200]), Err(ReadError::BadTag(200)));
}

#[test]
fn random_bytes_never_panic() {
    let mut x = 0x2545_f491_4f6c_dd1du64;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let valid = encode(&every_tag());
    for round in 0..20_000 {
        let n = (next() % 48) as usize;
        let mut b: Vec<u8> = (0..n).map(|_| (next() % 40) as u8).collect();
        if round % 2 == 1 {
            // Flip some bytes of a valid record.
            b = valid.to_vec();
            for _ in 0..1 + next() % 3 {
                let i = (next() as usize) % b.len();
                b[i] = next() as u8;
            }
            b.truncate(1 + (next() as usize) % b.len());
        }
        let _ = decode(&b);
    }
}

fn limits() -> CloneLimits {
    CloneLimits { bytes: 1000, depth: 3, nodes: 4, names: ["bytes!", "depth!", "nodes!"] }
}

#[test]
fn bytes_limit() {
    let mut m = Meter::new(limits());
    // `string(len)` is `2 * len + 16`: 484 + 484 = 968, and a third string passes 1000.
    assert_eq!(m.string(234), Ok(()));
    assert_eq!(m.bytes(), 484);
    assert_eq!(m.string(234), Ok(()));
    assert_eq!(m.bytes(), 968);
    assert_eq!(m.string(8), Ok(()), "exactly at the cap is allowed: 968 + 32 = 1000");
    assert_eq!(m.bytes(), 1000);
    assert_eq!(m.string(0), Err(Abort::Limit("bytes!")));
    // Each charge checks the bytes cap.
    for f in [
        |m: &mut Meter| m.bigint(0),
        |m: &mut Meter| m.key_name(0),
        |m: &mut Meter| m.key_index(),
        |m: &mut Meter| m.entry(),
        |m: &mut Meter| m.buffer(1),
        |m: &mut Meter| m.node(0),
        |m: &mut Meter| m.moved(0),
        |m: &mut Meter| m.unused(0),
    ] {
        let mut m = Meter::new(limits());
        m.string(242).unwrap();
        assert_eq!(m.bytes(), 500);
        m.string(242).unwrap();
        assert_eq!(f(&mut m), Err(Abort::Limit("bytes!")));
    }
    // A huge length saturates and does not wrap or panic.
    let mut m = Meter::new(limits());
    assert_eq!(m.string(usize::MAX), Err(Abort::Limit("bytes!")));
    assert_eq!(m.buffer(usize::MAX), Err(Abort::Limit("bytes!")));
    assert_eq!(m.bytes(), usize::MAX);
}

#[test]
fn depth_limit() {
    let mut m = Meter::new(limits());
    assert_eq!(m.node(2), Ok(()), "depth 2 is below the cap of 3");
    assert_eq!(m.node(3), Err(Abort::Limit("depth!")), "depth >= cap");
    assert_eq!(m.node(100), Err(Abort::Limit("depth!")));
    // The depth check comes before the node check and before the cost: a refused node costs nothing.
    assert_eq!((m.nodes(), m.bytes()), (1, 48));
    // A moved item has no depth.
    assert_eq!(m.moved(0), Ok(()));
}

#[test]
fn node_limit() {
    let mut m = Meter::new(limits());
    for _ in 0..4 {
        assert_eq!(m.node(0), Ok(()));
    }
    assert_eq!(m.node(0), Err(Abort::Limit("nodes!")));
    assert_eq!(m.moved(0), Err(Abort::Limit("nodes!")), "a moved item is a node, and the node cap comes before its cost");
    assert_eq!((m.nodes(), m.bytes()), (4, 192));
    // A listed item that no node used is no node.
    assert_eq!(m.unused(0), Ok(()));
    assert_eq!((m.nodes(), m.bytes()), (4, 192 + 64));
    // The order for a node when two limits fail: depth first.
    let mut m = Meter::new(CloneLimits { bytes: 0, depth: 1, nodes: 0, names: ["bytes!", "depth!", "nodes!"] });
    assert_eq!(m.node(1), Err(Abort::Limit("depth!")));
    assert_eq!(m.node(0), Err(Abort::Limit("nodes!")));
    let mut m = Meter::new(CloneLimits { bytes: 10, depth: 9, nodes: 9, names: ["bytes!", "depth!", "nodes!"] });
    assert_eq!(m.node(0), Err(Abort::Limit("bytes!")));
}

#[test]
fn steps_add_up() {
    let mut m = Meter::new(CloneLimits { bytes: usize::MAX, depth: 9, nodes: 9, names: ["", "", ""] });
    m.value();
    m.string(70).unwrap(); // 70 / 32 = 2
    m.buffer(130).unwrap(); // 130 / 64 = 2
    m.entry().unwrap(); // 1
    m.unused(64).unwrap(); // 1
    assert_eq!(m.take_steps(), 1 + 2 + 2 + 1 + 1);
    assert_eq!(m.take_steps(), 0);
    m.step(u64::MAX);
    m.step(5);
    assert_eq!(m.take_steps(), u64::MAX, "steps saturate");
}

/// The cost of a value of the page, in the old browser model, with the steps. Each charge is the one the old walk made.
fn cost_of(f: impl FnOnce(&mut Meter)) -> (usize, u64) {
    let mut m = Meter::new(CloneLimits { bytes: 64 << 20, depth: 256, nodes: 1 << 20, names: ["clone-bytes", "clone-depth", "clone-nodes"] });
    f(&mut m);
    (m.bytes(), m.take_steps())
}

/// The numbers come from the browser at origin/main 511d3eb4: `history.pushState(<value>, '')` in a page, then
/// `Browser::navigation_stats().state_bytes`, which is `State::bytes` of `serialize` (`browser_navigation_state.rs`). A scratch test
/// ran each source on its own page. The steps are counted by hand from `ser` and `props`: one for each value, one for each own key
/// (also an index key and `length` of an array), `len / 32` for a string and `n / 64` for a buffer.
#[test]
fn the_cost_model_gives_the_numbers_of_the_old_walk() {
    // `1`
    assert_eq!(cost_of(|m| m.value()), (0, 1));
    // `'héllo'`, `'中文'`, `''`
    assert_eq!(cost_of(|m| m.string(5).unwrap()).0, 26);
    assert_eq!(cost_of(|m| m.string(2).unwrap()).0, 20);
    assert_eq!(cost_of(|m| m.string(0).unwrap()).0, 16);
    // `123n`, `1n << 130n`, `-(1n << 64n)`
    assert_eq!(cost_of(|m| m.bigint(1).unwrap()).0, 24);
    assert_eq!(cost_of(|m| m.bigint(3).unwrap()).0, 40);
    assert_eq!(cost_of(|m| m.bigint(2).unwrap()).0, 32);
    // `({a: 1})`
    assert_eq!(
        cost_of(|m| {
            m.value();
            m.node(0).unwrap();
            m.step(1);
            m.key_name(1).unwrap();
            m.value();
        }),
        (66, 3)
    );
    // `[1, 2, 3]`: the array, its keys `length`, 0, 1 and 2 (`length` is not enumerable, so it costs a step and no bytes), 3 numbers.
    assert_eq!(
        cost_of(|m| {
            m.value();
            m.node(0).unwrap();
            m.step(1);
            for _ in 0..3 {
                m.step(1);
                m.key_index().unwrap();
                m.value();
            }
        }),
        (108, 1 + 1 + 3 * 2)
    );
    // `new Date(0)`
    assert_eq!(cost_of(|m| m.node(0).unwrap()).0, 48);
    // `/ab/g`: the node, the source `ab` and the flags `g`.
    assert_eq!(
        cost_of(|m| {
            m.node(0).unwrap();
            m.string(2).unwrap();
            m.string(1).unwrap();
        })
        .0,
        86
    );
    // `new Map([[1, 2]])` and `new Set([1, 2])`
    assert_eq!(
        cost_of(|m| {
            m.node(0).unwrap();
            m.entry().unwrap();
        })
        .0,
        64
    );
    assert_eq!(
        cost_of(|m| {
            m.node(0).unwrap();
            m.entry().unwrap();
            m.entry().unwrap();
        })
        .0,
        80
    );
    // `new ArrayBuffer(100)`: 48 + 100, and 100 / 64 steps.
    assert_eq!(
        cost_of(|m| {
            m.value();
            m.node(0).unwrap();
            m.buffer(100).unwrap();
        }),
        (148, 1 + 1)
    );
    // `new Uint8Array(10)`: the view node and its buffer node.
    assert_eq!(
        cost_of(|m| {
            m.node(0).unwrap();
            m.node(1).unwrap();
            m.buffer(10).unwrap();
        })
        .0,
        106
    );
    // `new Uint16Array(new ArrayBuffer(8), 2, 2)`
    assert_eq!(
        cost_of(|m| {
            m.node(0).unwrap();
            m.node(1).unwrap();
            m.buffer(8).unwrap();
        })
        .0,
        104
    );
    // `new Number(5)`, `new String('ab')`
    assert_eq!(cost_of(|m| m.node(0).unwrap()).0, 48);
    assert_eq!(
        cost_of(|m| {
            m.node(0).unwrap();
            m.string(2).unwrap();
        })
        .0,
        68
    );
    // `({1: 'x', b: 'yy'})`
    assert_eq!(
        cost_of(|m| {
            m.node(0).unwrap();
            m.key_index().unwrap();
            m.string(1).unwrap();
            m.key_name(1).unwrap();
            m.string(2).unwrap();
        })
        .0,
        124
    );
    // `[[1], [1]]`: three arrays, 3 index keys.
    assert_eq!(
        cost_of(|m| {
            m.node(0).unwrap();
            for _ in 0..2 {
                m.key_index().unwrap();
                m.node(1).unwrap();
                m.key_index().unwrap();
            }
        })
        .0,
        224
    );
    // `var a = {}; [a, a]`: the second `a` is a back reference and costs nothing.
    assert_eq!(
        cost_of(|m| {
            m.node(0).unwrap();
            m.key_index().unwrap();
            m.node(1).unwrap();
            m.key_index().unwrap();
        })
        .0,
        136
    );
    // The state of `navigation_load history`: `{i, self, tags: ['a', 'b', i], nested: {k}}` with a cycle. It is 352, and 50 states are
    // `state_bytes=17600` in that row.
    let (bytes, steps) = cost_of(|m| {
        m.value();
        m.node(0).unwrap();
        for k in ["i", "self", "tags", "nested"] {
            m.step(1);
            m.key_name(k.len()).unwrap();
            match k {
                "i" | "self" => m.value(),
                "tags" => {
                    m.value();
                    m.node(1).unwrap();
                    m.step(1); // length
                    for _ in 0..3 {
                        m.step(1);
                        m.key_index().unwrap();
                        m.value();
                    }
                    m.string(1).unwrap();
                    m.string(1).unwrap();
                }
                _ => {
                    m.value();
                    m.node(1).unwrap();
                    m.step(1);
                    m.key_name(1).unwrap();
                    m.value();
                }
            }
        }
    });
    assert_eq!(bytes, 352);
    assert_eq!(bytes * 50, 17600);
    assert!(steps > 0);
}

#[test]
fn a_listed_item_costs_64_plus_its_bytes() {
    // An `ArrayBuffer(200)` in the transfer list that the graph reaches: a node with 64 + 200, and 200 / 64 steps.
    assert_eq!(cost_of(|m| m.moved(200).unwrap()), (264, 3));
    // A port: 64.
    assert_eq!(cost_of(|m| m.moved(0).unwrap()), (64, 0));
    // The same item, not reached: the same cost and no node.
    let mut m = Meter::new(limits());
    m.unused(200).unwrap();
    assert_eq!((m.bytes(), m.nodes(), m.take_steps()), (264, 0, 3));
}

#[test]
fn cloned_keeps_its_parts() {
    let mut c = Cloned::new(Box::from([1u8, 2, 3]), vec![Moved::Buffer(vec![9]), Moved::Host(Box::new(7u64))], 123);
    assert_eq!((c.cost(), c.bytes(), c.moved().len()), (123, &[1u8, 2, 3][..], 2));
    // A host rewrites its own items.
    for m in c.moved_mut() {
        if let Moved::Host(h) = m {
            let n = *h.downcast_ref::<u64>().unwrap();
            *h = Box::new(n + 1);
        }
    }
    let (bytes, moved, cost) = c.into_parts();
    assert_eq!((&*bytes, cost), (&[1u8, 2, 3][..], 123));
    assert!(matches!(&moved[0], Moved::Buffer(b) if b == &[9]));
    let Moved::Host(h) = &moved[1] else { panic!() };
    assert_eq!(h.downcast_ref::<u64>(), Some(&8));
}

#[test]
fn id_map_grows_past_its_inline_pairs() {
    let mut m = IdMap::default();
    assert_eq!(m.get(5), None);
    for i in 0..100u64 {
        m.insert(i * 0x9e37_79b9, i as u32);
        for j in 0..=i {
            assert_eq!(m.get(j * 0x9e37_79b9), Some(j as u32));
        }
        assert_eq!(m.get(i * 0x9e37_79b9 + 1), None);
    }
    // Zero is a key like any other.
    let mut m = IdMap::default();
    assert_eq!(m.get(0), None);
    m.insert(0, 3);
    assert_eq!(m.get(0), Some(3));
}

#[test]
fn reader_state_is_exact() {
    let b = encode(&V::ArrD(vec![Some(V::Null), None], vec![(name("p"), V::Undef)]));
    let mut r = RecordReader::new(&b);
    assert_eq!(r.remaining(), b.len());
    assert_eq!(r.tag(), Ok(Tag::ArrD));
    assert_eq!(r.dense_len(), Ok(2));
    assert_eq!(r.tag(), Ok(Tag::Null));
    assert_eq!(r.tag(), Ok(Tag::Hole));
    assert_eq!(r.end(), Ok(false));
    assert_eq!(r.key(), Ok(Some(Key::New(Text::Latin1(b"p")))));
    assert_eq!(r.tag(), Ok(Tag::Undef));
    assert_eq!(r.end(), Ok(true));
    assert_eq!(r.finish(), Ok(()));
    assert_eq!(r.end(), Err(ReadError::Truncated), "no `End` can be read past the last byte");
    assert_eq!(r.key(), Err(ReadError::Truncated));
}
