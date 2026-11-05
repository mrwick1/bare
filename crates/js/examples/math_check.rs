//! Reads rows from tools/js/math_gen.mjs on stdin and compares each fdlibm function with Node bit for bit. It prints
//! one line per function and exits with status 1 when a result differs.
//!
//!   node tools/js/math_gen.mjs | math_check

#[path = "../src/ieee754.rs"]
mod ieee754;
use ieee754 as m;
use std::io::BufRead;

fn num(s: &str) -> f64 {
    match s {
        "NaN" => f64::NAN,
        "Infinity" => f64::INFINITY,
        "-Infinity" => f64::NEG_INFINITY,
        _ => s.parse().expect("number"),
    }
}

fn main() {
    let names = [
        "sin", "cos", "tan", "asin", "acos", "atan", "sinh", "cosh", "tanh", "asinh", "acosh", "atanh", "exp", "expm1", "log", "log10", "log2", "log1p",
        "cbrt", "sqrt", "atan2", "pow",
    ];
    let mut bad = [0usize; 22];
    let mut first = [None; 22];
    let mut rows = 0;
    for line in std::io::stdin().lock().lines() {
        let v: Vec<f64> = line.expect("stdin").split(' ').map(num).collect();
        let (x, y) = (v[0], v[24]);
        rows += 1;
        let got = [
            m::sin(x), m::cos(x), m::tan(x), m::asin(x), m::acos(x), m::atan(x), m::sinh(x), m::cosh(x), m::tanh(x), m::asinh(x), m::acosh(x),
            m::atanh(x), m::exp(x), m::expm1(x), m::log(x), m::log10(x), m::log2(x), m::log1p(x), m::cbrt(x), x.sqrt(), m::atan2(x, y),
            m::pow(x, y / 1000.0),
        ];
        for (i, &g) in got.iter().enumerate() {
            let w = v[i + 1];
            if g.to_bits() != w.to_bits() && !(g.is_nan() && w.is_nan()) {
                bad[i] += 1;
                first[i].get_or_insert((x, y, w, g));
            }
        }
    }
    for (i, name) in names.iter().enumerate() {
        match first[i] {
            Some((x, y, w, g)) => println!("{name}: {}/{rows} differ, first x={x:e} y={y:e} node={w:e} bare={g:e}", bad[i]),
            None => println!("{name}: {rows} match"),
        }
    }
    if bad.iter().any(|&b| b > 0) {
        std::process::exit(1);
    }
}
