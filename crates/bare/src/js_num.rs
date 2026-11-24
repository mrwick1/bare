//! The number conversions of ECMAScript that the browser needs outside the engine.

/// ToUint32: wrap modulo 2^32. NaN and infinity give 0.
pub fn to_uint32(n: f64) -> u32 {
    let i = n as i32;
    if i as f64 == n {
        return i as u32;
    }
    if !n.is_finite() {
        return 0;
    }
    n.trunc().rem_euclid(4294967296.0) as u32
}

/// ToInt32.
pub fn to_int32(n: f64) -> i32 {
    to_uint32(n) as i32
}

/// ECMAScript WhiteSpace and LineTerminator code units.
pub fn is_js_space(u: u16) -> bool {
    matches!(u, 9..=13 | 0x20 | 0xA0 | 0x1680 | 0x2000..=0x200A | 0x2028 | 0x2029 | 0x202F | 0x205F | 0x3000 | 0xFEFF)
}

/// Number::toString for radix 10.
pub fn number_to_string(n: f64) -> String {
    bare_vex::interp_rt::Realm::num_to_string(n)
}
