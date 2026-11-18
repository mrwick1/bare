//! Micro row for `Value` (plan step S1.2): encode and decode N values of mixed kinds (double, integer, boolean, pointer) and
//! print a checksum. Count with `tools/js/instr.sh`; subtract the run with N = 0 for the cost per value.
//!   value_load N
use bare_js2::value::{Kind, Value};
use std::hint::black_box;
use std::ptr::NonNull;

fn main() {
    let n: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).expect("usage: value_load N");
    let cell = Box::new(7u64);
    let p = NonNull::from(&*cell).cast::<u8>();
    let mut sum = 0.0;
    for i in 0..n {
        let v = match i & 3 {
            0 => Value::number(black_box(i as f64 * 0.5)),
            1 => Value::int(black_box(i as i32)),
            2 => Value::bool(black_box(i & 4 == 0)),
            _ => Value::pointer(Kind::Object, black_box(p)),
        };
        let v = black_box(v);
        sum += if let Some(f) = v.as_f64() {
            f
        } else if let Some(b) = v.as_bool() {
            b as u8 as f64
        } else if let Some((_, q)) = v.as_pointer() {
            (q == p) as u8 as f64
        } else {
            0.0
        };
    }
    println!("values={n} checksum={sum}");
}
