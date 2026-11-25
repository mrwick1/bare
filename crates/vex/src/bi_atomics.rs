use crate::bi_buffer::{View, integer, range, tag, to_index};
use crate::bi_typed::Element;
use crate::builtins::arg;
use crate::interp_rt::{ErrKind, R, Realm};
use crate::lazy::{late_table, m};
use crate::value::Value;
use std::time::Duration;

pub(crate) fn install(rt: &mut Realm) {
    let atomics = crate::bi_buffer::plain_proto(rt, rt.object_proto);
    tag(rt, atomics, "Atomics");
    rt.lazy_table(atomics, late_table![
        m("add", 3, access::<1>), m("and", 3, access::<2>),
        m("compareExchange", 4, access::<3>), m("exchange", 3, access::<4>),
        m("isLockFree", 1, is_lock_free), m("load", 2, access::<0>),
        m("notify", 3, notify), m("or", 3, access::<5>), m("pause", 0, pause),
        m("store", 3, access::<6>), m("sub", 3, access::<7>),
        m("wait", 4, wait::<false>), m("waitAsync", 4, wait::<true>),
        m("xor", 3, access::<8>),
    ]);
    rt.hidden(rt.global, "Atomics", atomics.value());
}

fn view(rt: &mut Realm, value: Value, waitable: bool, write: bool) -> R<(View, Element, usize)> {
    let Some(v) = Realm::obj(value).and_then(View::of) else {
        return rt.type_error("Atomics requires an integer typed array");
    };
    let elem = match v.elem {
        Some(e @ (Element::I32 | Element::BI64)) => e,
        Some(e @ (Element::I8 | Element::U8 | Element::I16 | Element::U16 | Element::U32 | Element::BU64)) if !waitable => e,
        _ => return rt.type_error("Atomics requires an integer typed array"),
    };
    if write && v.immutable() {
        return rt.type_error("immutable ArrayBuffer");
    }
    let Some(len) = v.len(rt) else { return rt.type_error("typed array is out of bounds") };
    Ok((v, elem, len))
}

fn access_index(rt: &mut Realm, index: Value, len: usize) -> R<usize> {
    let i = to_index(rt, index)?;
    if i >= len { return range(rt, "Atomics index is out of bounds") }
    Ok(i)
}

fn checked_offset(rt: &mut Realm, v: View, i: usize) -> R<usize> {
    if !v.len(rt).is_some_and(|len| i < len) {
        return rt.type_error("typed array is out of bounds");
    }
    Ok(v.offset + i * v.width())
}

fn raw(e: Element, n: u64) -> u64 {
    let mut bytes = [0; 8];
    e.write(&mut bytes[..e.width()], n, true);
    u64::from_le_bytes(bytes)
}

fn access<const OP: u8>(rt: &mut Realm, _: Value, args: &[Value]) -> R<Value> {
    let (view, elem, len) = view(rt, arg(args, 0), false, OP != 0)?;
    let i = access_index(rt, arg(args, 1), len)?;
    if OP == 0 {
        let n = view.read(rt, i).ok_or_else(|| rt.error(ErrKind::Type, "typed array is out of bounds"))?;
        return elem.value(rt, n);
    }
    let value = if elem.is_big() {
        rt.to_bigint(arg(args, 2))?
    } else {
        let n = integer(rt, arg(args, 2))?;
        Realm::num(if n == 0.0 { 0.0 } else { n })
    };
    let input = raw(elem, elem.convert(rt, value)?);
    let replacement = if OP == 3 { raw(elem, elem.convert(rt, arg(args, 3))?) } else { 0 };
    let offset = checked_offset(rt, view, i)?;
    let width = elem.width();
    let bits = rt.write_buffer(view.buffer, |bytes| {
        let bytes = &mut bytes[offset..offset + width];
        let mut raw = [0; 8];
        for (dst, &src) in raw.iter_mut().zip(bytes.iter()) { *dst = src; }
        if cfg!(target_endian = "big") { raw[..width].reverse(); }
        let old = u64::from_le_bytes(raw);
        let new = match OP {
            1 => old.wrapping_add(input),
            2 => old & input,
            3 => if old == input { replacement } else { old },
            4 | 6 => input,
            5 => old | input,
            7 => old.wrapping_sub(input),
            8 => old ^ input,
            _ => unreachable!(),
        };
        let mut raw = new.to_le_bytes();
        if cfg!(target_endian = "big") { raw[..width].reverse(); }
        bytes.copy_from_slice(&raw[..width]);
        elem.read(&old.to_le_bytes()[..width], true)
    });
    if OP == 6 { Ok(value) } else { elem.value(rt, bits) }
}

fn is_lock_free(rt: &mut Realm, _: Value, args: &[Value]) -> R<Value> {
    let width = integer(rt, arg(args, 0))?;
    Ok(Value::bool(width == 4.0))
}

fn pause(rt: &mut Realm, _: Value, args: &[Value]) -> R<Value> {
    let hint = arg(args, 0);
    if !hint.is_undefined() && !hint.as_f64().is_some_and(|n| n.is_finite() && n.fract() == 0.0) {
        return rt.type_error("Atomics.pause requires an integer hint");
    }
    std::hint::spin_loop();
    Ok(Value::UNDEFINED)
}

fn notify(rt: &mut Realm, _: Value, args: &[Value]) -> R<Value> {
    let (v, _, len) = view(rt, arg(args, 0), true, false)?;
    let i = access_index(rt, arg(args, 1), len)?;
    let count = arg(args, 2);
    let count = if count.is_undefined() { usize::MAX } else { integer(rt, count)? as usize };
    let offset = v.offset + i * v.width();
    Ok(Realm::num(rt.notify_shared_waiters(v.buffer, offset, count)? as f64))
}

fn wait<const ASYNC: bool>(rt: &mut Realm, _: Value, args: &[Value]) -> R<Value> {
    let (v, elem, len) = view(rt, arg(args, 0), true, false)?;
    let Some(buffer) = rt.shared_buffer(v.buffer).cloned() else {
        return rt.type_error("Atomics.wait requires a SharedArrayBuffer");
    };
    let i = access_index(rt, arg(args, 1), len)?;
    let value = elem.convert(rt, arg(args, 2))?;
    let millis = rt.to_number(arg(args, 3))?;
    let timeout = if millis.is_nan() || millis == f64::INFINITY { None }
        else { Duration::try_from_secs_f64(millis.max(0.0) / 1000.0).ok() };
    let shared = rt.shared_memory.as_ref().expect("shared memory installed");
    if !ASYNC && !shared.can_block { return rt.type_error("the agent cannot block") }
    let mut expected = [0; 8];
    let expected = &mut expected[..elem.width()];
    elem.write(expected, value, cfg!(target_endian = "little"));
    let offset = v.offset + i * v.width();
    if ASYNC {
        return rt.wait_async(buffer, offset, expected, timeout);
    }
    let outcome = buffer.wait(offset, expected, timeout, shared.control.as_ref());
    let Some(outcome) = outcome else { return rt.type_error("Test262 agent stopped") };
    Ok(rt.string(outcome))
}
