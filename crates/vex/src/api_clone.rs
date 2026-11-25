//! Native structured clone using the shared record and cost model.
use crate::bi_buffer::View;
use crate::bi_object::{Desc, Own};
use crate::bi_typed::Element;
use crate::bigint::Sv;
use crate::blob::Blob;
use crate::interp_rt::{Key, Realm};
use crate::object::{F_HOST, K_BUFFER, K_DATA_VIEW, K_DATE, K_MAP, K_NONE, K_REGEXP, K_SET, K_TYPED, K_WRAP, Obj};
use crate::shapes::{A_C, A_E, A_W};
use crate::value::Value;
use bare_js_api::clone::{self as cl, CloneHost, CloneIn, CloneOpts, Cloned, Moved, Refused, Tag};
use bare_js_api::{Abort, Engine, Error, enter};

type R<'s, T> = Result<T, Error<'s>>;

struct Walk<'a> {
    out: cl::RecordWriter,
    meter: cl::Meter,
    seen: cl::IdMap,
    transfer: cl::IdMap,
    host: &'a mut dyn CloneHost<Realm>,
    /// The listed items: a serializable host object is the item after them.
    listed: u32,
    /// What `write_host_object` gave, in walk order.
    data: Vec<Box<dyn std::any::Any>>,
}

fn id(o: Obj) -> u64 {
    o.cell().as_ptr() as usize as u64
}

impl Walk<'_> {
    fn charge<'s>(&mut self, rt: &mut Realm, f: impl FnOnce(&mut cl::Meter) -> Result<(), Abort>) -> R<'s, ()> {
        let result = f(&mut self.meter);
        rt.charge(self.meter.take_steps())?;
        result.map_err(Error::Abort)
    }

    fn refuse<'s>(&mut self, rt: &mut Realm, why: Refused) -> Error<'s> {
        let err = enter(rt, |s| match self.host.data_clone_error(s, why) {
            Error::Throw(e) => Ok(s.engine().a_at(e)),
            Error::Abort(a) => Err(a),
        });
        match err {
            Ok(v) => Error::Throw(rt.a_push(v)),
            Err(a) => Error::Abort(a),
        }
    }

    fn value<'s>(&mut self, rt: &mut Realm, v: Value, depth: usize) -> R<'s, ()> {
        self.meter.value();
        self.charge(rt, |_| Ok(()))?;
        if let Some(o) = Realm::obj(v) {
            return self.object(rt, o, depth);
        }
        if v.is_undefined() {
            self.out.undefined();
        } else if v.is_null() {
            self.out.null();
        } else if let Some(b) = v.as_bool() {
            self.out.boolean(b);
        } else if let Some(n) = v.as_f64() {
            self.out.number(n);
        } else if let Some(s) = Realm::str_of(v) {
            self.charge(rt, |m| m.string(s.len()))?;
            self.out.string(&rt.units(s));
        } else if let Some(b) = Realm::big_of(v) {
            let b = b.bigint();
            self.charge(rt, |m| m.bigint(b.mag.len()))?;
            self.out.bigint(b.neg, b.mag);
        } else {
            return Err(self.refuse(rt, Refused::Symbol));
        }
        Ok(())
    }

    fn object<'s>(&mut self, rt: &mut Realm, o: Obj, depth: usize) -> R<'s, ()> {
        let key = id(o);
        if let Some(n) = self.seen.get(key) {
            self.out.reference(n);
            return Ok(());
        }
        if let Some(i) = self.transfer.get(key) {
            let len = rt.buffer_bytes(o).map_or(0, <[u8]>::len);
            self.charge(rt, |m| m.moved(len))?;
            self.seen.insert(key, self.out.moved(i));
            return Ok(());
        }
        let v = o.value();
        if o.is_callable() {
            return Err(self.refuse(rt, Refused::Function));
        }
        let kind = o.kind();
        // A platform object with data (a `Blob`, a `CryptoKey`) has a host owner. A plain object has none, so it costs one lookup.
        if kind == K_NONE && rt.api.hs.owners.contains_key(&o.cell()) {
            if let Some((data, bytes)) = self.host.write_host_object(&v) {
                self.charge(rt, |m| m.moved(bytes))?;
                let i = self.listed + self.data.len() as u32;
                self.data.push(data);
                self.seen.insert(key, self.out.moved(i));
                return Ok(());
            }
        }
        if o.has_flag(F_HOST) || self.host.is_host_object(&v)
            || !matches!(kind, K_NONE | K_DATE | K_REGEXP | K_MAP | K_SET | K_WRAP | K_BUFFER | K_TYPED | K_DATA_VIEW)
            || (kind == K_WRAP && rt.prim_of(v).is_none_or(|p| Realm::sym_of(p).is_some()))
        {
            return Err(self.refuse(rt, Refused::Object));
        }
        self.charge(rt, |m| m.node(depth))?;
        // A later getter may unlink an object or allocate another cell at its address.
        rt.a_push(v);
        match kind {
            K_NONE if rt.is_error(v) => self.error(rt, o, depth)?,
            K_NONE => {
                let n = if Realm::is_array(rt, o) { self.out.array_sparse(o.len()) } else { self.out.object() };
                self.seen.insert(key, n);
                self.props(rt, o, depth)?;
            }
            K_DATE => {
                let (_, slot) = crate::bi_date::date_slot(rt, v).ok_or_else(|| self.refuse(rt, Refused::Object))?;
                self.seen.insert(key, self.out.date(o.slot(&rt.shapes, slot).as_f64().unwrap_or(f64::NAN)));
            }
            K_REGEXP => {
                let (src, flags) = rt.clone_regexp_parts(o);
                self.charge(rt, |m| m.string(src.len()).and_then(|()| m.string(flags.len())))?;
                self.seen.insert(key, self.out.regexp(&rt.units(src), &flags.encode_utf16().collect::<Vec<_>>()));
            }
            K_MAP | K_SET => {
                let (is_map, entries) = rt.inspect_collection(o, usize::MAX).expect("collection");
                let n = if is_map { self.out.map() } else { self.out.set() };
                self.seen.insert(key, n);
                // Snapshot entries before visiting children that can mutate the collection.
                for &(k, v) in &entries {
                    rt.a_push(k);
                    rt.a_push(v);
                }
                for (k, v) in entries {
                    self.charge(rt, cl::Meter::entry)?;
                    self.value(rt, k, depth + 1)?;
                    if is_map {
                        self.value(rt, v, depth + 1)?;
                    }
                }
                self.out.end();
            }
            K_WRAP => {
                self.seen.insert(key, self.out.wrapper());
                self.value(rt, rt.prim_of(v).expect("wrapper"), depth + 1)?;
            }
            K_BUFFER => {
                if rt.buffer_detached(o) {
                    return Err(self.refuse(rt, Refused::DetachedBuffer));
                }
                let len = rt.buffer_bytes(o).expect("buffer").len();
                self.charge(rt, |m| m.buffer(len))?;
                self.seen.insert(key, self.out.buffer(rt.buffer_bytes(o).expect("buffer")));
            }
            K_TYPED | K_DATA_VIEW => {
                let view = View::of(o).expect("view");
                let len = view.len().ok_or_else(|| self.refuse(rt, Refused::DetachedBuffer))?;
                self.out.view(view.elem.map_or(11, |e| e.api() as u8), view.offset, len);
                self.value(rt, view.buffer.value(), depth + 1)?;
                self.seen.insert(key, self.out.view_done());
            }
            _ => unreachable!(),
        }
        Ok(())
    }

    fn props<'s>(&mut self, rt: &mut Realm, o: Obj, depth: usize) -> R<'s, ()> {
        let keys = rt.all_own_keys(o).map_err(|e| rt.a_err(e))?;
        for &k in &keys {
            rt.a_push(k);
        }
        for k in keys {
            self.meter.step(1);
            self.charge(rt, |_| Ok(()))?;
            if Realm::sym_of(k).is_some() {
                continue;
            }
            let k = rt.key_of(k).map_err(|e| rt.a_err(e))?;
            if !rt.get_own_h(o, k).map_err(|e| rt.a_err(e))?.is_some_and(|p| p.attrs() & A_E != 0) {
                continue;
            }
            match k {
                Key::Idx(i) => {
                    self.charge(rt, cl::Meter::key_index)?;
                    self.out.key_index(i);
                }
                Key::Name(s) => {
                    self.charge(rt, |m| m.key_name(s.len()))?;
                    self.out.key(s.cell().as_ptr() as usize as u64, &rt.units(s));
                }
            }
            let v = rt.get_prop(o.value(), k).map_err(|e| rt.a_err(e))?;
            self.value(rt, v, depth + 1)?;
        }
        self.out.end();
        Ok(())
    }

    fn error<'s>(&mut self, rt: &mut Realm, o: Obj, depth: usize) -> R<'s, ()> {
        let name = rt.get_prop(o.value(), Key::Name(rt.k_name)).map_err(|e| rt.a_err(e))?;
        let name = if let Some(s) = Realm::str_of(name) {
            self.charge(rt, |m| m.string(s.len()))?;
            rt.units(s)
        } else {
            Vec::new()
        };
        self.seen.insert(id(o), self.out.error(&name));
        for (name, field) in [("message", cl::ErrorField::Message), ("stack", cl::ErrorField::Stack), ("cause", cl::ErrorField::Cause)] {
            let key = Key::Name(rt.intern(name));
            let Some(Own::Data(v, _)) = rt.get_own(o, key) else { continue };
            let v = match field {
                cl::ErrorField::Message => rt.to_str(v).map_err(|e| rt.a_err(e))?.value(),
                cl::ErrorField::Stack if Realm::str_of(v).is_none() => continue,
                _ => v,
            };
            self.out.error_field(field);
            self.value(rt, v, depth + 1)?;
        }
        self.out.end();
        Ok(())
    }
}

pub(super) fn write<'s>(rt: &mut Realm, root: Value, opts: &CloneOpts<'_, '_>, host: &mut dyn CloneHost<Realm>) -> R<'s, Cloned> {
    let list: Vec<_> = opts.transfer.iter().filter_map(|&l| Realm::obj(rt.a_at(l))).collect();
    let mut walk = Walk { out: cl::RecordWriter::new(), meter: cl::Meter::new(opts.limits), seen: cl::IdMap::default(), transfer: cl::IdMap::default(), host, listed: list.len() as u32, data: Vec::new() };
    for (i, &o) in list.iter().enumerate() {
        walk.transfer.insert(id(o), i as u32);
    }
    walk.value(rt, root, 0)?;
    for &o in &list {
        if walk.seen.get(id(o)).is_none() {
            let len = rt.buffer_bytes(o).map_or(0, <[u8]>::len);
            walk.charge(rt, |m| m.unused(len))?;
        }
    }
    for &o in &list {
        let ok = if o.is_kind(K_BUFFER) { !rt.buffer_detached(o) } else { walk.host.can_transfer(&o.value()) };
        if !ok {
            return Err(walk.refuse(rt, Refused::DetachedTransfer));
        }
    }
    let mut moved: Vec<Moved> = list.into_iter().map(|o| {
        if o.is_kind(K_BUFFER) {
            Moved::Buffer(rt.clone_detach_buffer(o))
        } else {
            Moved::Host(walk.host.transfer_out(&o.value()))
        }
    }).collect();
    moved.extend(walk.data.into_iter().map(Moved::Data));
    Ok(Cloned::new(walk.out.finish(), moved, walk.meter.bytes()))
}

enum Item {
    Buffer(Vec<u8>),
    Host(Value),
    Used,
}

struct Read<'r> {
    input: cl::RecordReader<'r>,
    objects: Vec<Value>,
    keys: Vec<Key>,
    items: Vec<Item>,
}

fn damaged() -> Error<'static> {
    Error::Abort(Abort::Unsupported("damaged clone record"))
}

macro_rules! read {
    ($expr:expr) => { $expr.map_err(|_| damaged())? };
}

impl Read<'_> {
    fn value<'s>(&mut self, rt: &mut Realm, depth: usize) -> R<'s, Value> {
        let tag = read!(self.input.tag());
        self.tag(rt, tag, depth)
    }

    fn object<'s>(&mut self, rt: &mut Realm, v: Value) -> R<'s, Value> {
        rt.charge(1)?;
        self.objects.push(v);
        Ok(v)
    }

    fn tag<'s>(&mut self, rt: &mut Realm, tag: Tag, depth: usize) -> R<'s, Value> {
        if depth > 256 { return Err(damaged()); }
        Ok(match tag {
            Tag::Undef => Value::UNDEFINED,
            Tag::Null => Value::NULL,
            Tag::True | Tag::False => Value::bool(tag == Tag::True),
            Tag::Int => Value::int(read!(self.input.int())),
            Tag::Num => Value::number(read!(self.input.num())),
            Tag::Str8 | Tag::Str16 => rt.make_units(&read!(self.input.text(tag)).to_vec()),
            Tag::Big => {
                let mut mag = Vec::new();
                let neg = read!(self.input.bigint(&mut mag));
                Blob::new_bigint(&mut rt.heap, Sv { neg, mag: &mag }).ok_or(Error::Abort(Abort::Limit("heap memory")))?
            }
            Tag::Ref => *self.objects.get(read!(self.input.reference()) as usize).ok_or_else(damaged)?,
            Tag::Obj | Tag::Arr | Tag::ArrD => self.plain(rt, tag, depth)?,
            Tag::Date => {
                let v = crate::bi_date::new_date(rt, rt.date_proto, read!(self.input.num()));
                self.object(rt, v)?
            }
            Tag::RegExp => {
                let (src, flags) = read!(self.input.regexp());
                let src = Realm::str_of(rt.make_units(&src.to_vec())).expect("string");
                let flags = String::from_utf16(&flags.to_vec()).map_err(|_| damaged())?;
                let v = rt.new_regexp(None, src, &flags).map_err(|e| rt.a_err(e))?;
                self.object(rt, v)?
            }
            Tag::Map | Tag::Set => {
                let is_map = tag == Tag::Map;
                let o = rt.clone_new_collection(is_map);
                let v = self.object(rt, o.value())?;
                while !read!(self.input.end()) {
                    let key = self.value(rt, depth + 1)?;
                    let value = if is_map { self.value(rt, depth + 1)? } else { key };
                    rt.charge(1)?;
                    crate::bi_collections::add_pair(rt, o, if is_map { K_MAP } else { K_SET }, key, value).map_err(|e| rt.a_err(e))?;
                }
                v
            }
            Tag::Error => {
                let name = read!(self.input.string());
                let i = crate::builtins::ERROR_NAMES.iter().position(|n| n.encode_utf16().eq(name.units())).unwrap_or(4);
                let v = rt.make_error(rt.error_protos[i], None);
                self.object(rt, v)?;
                let o = Realm::obj(v).expect("error");
                while let Some(f) = read!(self.input.error_field()) {
                    let name = match f {
                        cl::ErrorField::Message => "message",
                        cl::ErrorField::Stack => "stack",
                        cl::ErrorField::Cause => "cause",
                    };
                    let val = self.value(rt, depth + 1)?;
                    let k = Key::Name(rt.intern(name));
                    rt.define_attrs(o, k, A_W | A_C, val).map_err(|e| rt.a_err(e))?;
                }
                v
            }
            Tag::Wrapper => {
                let p = self.value(rt, depth + 1)?;
                if !(p.is_number() || p.is_bigint() || p.as_bool().is_some() || Realm::str_of(p).is_some()) {
                    return Err(damaged());
                }
                let o = rt.to_object(p).map_err(|e| rt.a_err(e))?;
                self.object(rt, o.value())?
            }
            Tag::Buf => {
                let bytes = read!(self.input.buffer());
                self.buffer(rt, bytes)?
            }
            Tag::View => {
                let (ty, offset, len) = read!(self.input.view());
                let elem = match ty {
                    0..=10 => Some(Element::from_id(ty.into())),
                    11 => None,
                    _ => return Err(damaged()),
                };
                let buf = self.value(rt, depth + 1)?;
                let buffer = Realm::obj(buf).filter(|o| o.is_kind(K_BUFFER)).ok_or_else(damaged)?;
                let view = View { buffer, offset, len: Some(len), elem };
                if offset % view.width() != 0 || view.len().is_none() {
                    return Err(damaged());
                }
                self.input.view_done();
                let proto = rt.buffer_intrinsic(elem.map_or("DataView", Element::name));
                let o = rt.new_view(proto, view).map_err(|e| rt.a_err(e))?;
                self.object(rt, o.value())?
            }
            Tag::Moved => {
                let i = read!(self.input.moved()) as usize;
                match std::mem::replace(self.items.get_mut(i).ok_or_else(damaged)?, Item::Used) {
                    Item::Buffer(bytes) => self.buffer(rt, &bytes)?,
                    Item::Host(v) => self.object(rt, v)?,
                    Item::Used => return Err(damaged()),
                }
            }
            _ => return Err(damaged()),
        })
    }

    fn buffer<'s>(&mut self, rt: &mut Realm, bytes: &[u8]) -> R<'s, Value> {
        let proto = rt.buffer_intrinsic("ArrayBuffer");
        let o = rt.new_buffer(proto, bytes.len(), None).map_err(|e| rt.a_err(e))?;
        rt.buffer_bytes_mut(o).expect("buffer").copy_from_slice(bytes);
        self.object(rt, o.value())
    }

    fn plain<'s>(&mut self, rt: &mut Realm, tag: Tag, depth: usize) -> R<'s, Value> {
        let v = if tag == Tag::Obj {
            Obj::new(&mut rt.heap, &rt.shapes, rt.s_object).expect("memory").value()
        } else {
            rt.new_array(&[])
        };
        self.object(rt, v)?;
        let o = Realm::obj(v).expect("object");
        let len = match tag {
            Tag::Arr => read!(self.input.sparse_len()),
            Tag::ArrD => {
                let n = u32::try_from(read!(self.input.dense_len())).map_err(|_| damaged())?;
                for i in 0..n {
                    let t = read!(self.input.tag());
                    if t != Tag::Hole {
                        rt.charge(1)?;
                        let v = self.tag(rt, t, depth + 1)?;
                        Self::define(rt, o, Key::Idx(i), v)?;
                    }
                }
                n
            }
            _ => 0,
        };
        while let Some(k) = read!(self.input.key()) {
            rt.charge(1)?;
            let k = match k {
                cl::Key::Index(i) => Key::Idx(i),
                cl::Key::Ref(i) => self.keys[i as usize],
                cl::Key::New(t) => {
                    let v = rt.make_units(&t.to_vec());
                    let k = rt.key_of(v).map_err(|e| rt.a_err(e))?;
                    self.keys.push(k);
                    k
                }
            };
            if matches!(k, Key::Idx(i) if i == u32::MAX || tag != Tag::Obj && i >= len)
                || matches!(k, Key::Name(n) if tag != Tag::Obj && n == rt.k_length)
            {
                return Err(damaged());
            }
            let val = self.value(rt, depth + 1)?;
            Self::define(rt, o, k, val)?;
        }
        if tag != Tag::Obj {
            o.set_len(&mut rt.heap, len).ok_or(Error::Abort(Abort::Limit("heap memory")))?;
        }
        Ok(v)
    }

    fn define<'s>(rt: &mut Realm, o: Obj, k: Key, v: Value) -> R<'s, ()> {
        let desc = Desc { value: Some(v), w: Some(true), e: Some(true), c: Some(true), ..Default::default() };
        if rt.define_own(o, k, &desc).map_err(|e| rt.a_err(e))? { Ok(()) } else { Err(damaged()) }
    }
}

pub(super) fn load<'s>(rt: &mut Realm, c: &mut Cloned, host: &mut dyn CloneHost<Realm>) -> R<'s, CloneIn<'s>> {
    let mut payloads = Vec::new();
    let mut data = Vec::new();
    let slots: Vec<_> = c.take_moved().into_iter().map(|item| match item {
        Moved::Buffer(b) => Some(Ok(b)),
        Moved::Host(h) => { payloads.push(h); None }
        Moved::Data(d) => { data.push(d); Some(Err(data.len() - 1)) }
    }).collect();
    // The serializable host objects, made before the walk: the host may run script, the walk may not.
    let made = if data.is_empty() {
        Vec::new()
    } else {
        enter(rt, |s| {
            let mut out = Vec::with_capacity(data.len());
            for d in &data {
                match host.read_host_object(s, &**d) {
                    Ok(l) => out.push(s.engine().a_at(l)),
                    Err(Error::Throw(l)) => return Err(Ok(s.engine().a_at(l))),
                    Err(Error::Abort(a)) => return Err(Err(a)),
                }
            }
            Ok(out)
        }).map_err(|e| match e {
            Ok(v) => Error::Throw(rt.a_push(v)),
            Err(a) => Error::Abort(a),
        })?
    };
    for &v in &made {
        rt.a_push(v);
    }
    let hosts = if payloads.is_empty() {
        Vec::new()
    } else {
        enter(rt, |s| match host.transfer_in(s, payloads) {
            Ok(ls) => Ok(ls.iter().map(|&l| s.engine().a_at(l)).collect()),
            Err(Error::Throw(l)) => Err(Ok(s.engine().a_at(l))),
            Err(Error::Abort(a)) => Err(Err(a)),
        }).map_err(|e| match e {
            Ok(v) => Error::Throw(rt.a_push(v)),
            Err(a) => Error::Abort(a),
        })?
    };
    let mut next = hosts.iter().copied();
    let items: Vec<_> = slots.into_iter().map(|slot| match slot {
        Some(Ok(b)) => Ok(Item::Buffer(b)),
        Some(Err(i)) => Ok(Item::Host(made[i])),
        None => next.next().map(Item::Host).ok_or_else(damaged),
    }).collect::<Result<_, _>>()?;
    let mut rd = Read { input: cl::RecordReader::new(c.bytes()), objects: Vec::new(), keys: Vec::new(), items };
    let v = rd.value(rt, 0)?;
    read!(rd.input.finish());
    Ok(CloneIn { value: rt.a_push(v), hosts: hosts.into_iter().map(|v| rt.a_push(v)).collect() })
}
