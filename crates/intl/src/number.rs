/// Number::toString(10) (section 6.1.6.1.20): the shortest digits that read back to `x`.
pub fn number_to_string(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x == 0.0 {
        return "0".into(); // also for -0
    }
    if x.is_infinite() {
        return if x < 0.0 { "-Infinity" } else { "Infinity" }.into();
    }
    // Rust's `{:e}` prints the shortest round-trip digits as `d.ddde-7`. Take digits and exponent.
    let mut sci = format!("{:e}", x.abs());
    // With 16 or 17 digits several decimals can read back to `x`. The spec wants the closest
    // to `x`; Rust's shortest output does not always pick it. The correctly rounded decimal of
    // the same length is the closest, so use it when it also reads back to `x`.
    let len = sci.split('e').next().unwrap().len() - sci.contains('.') as usize;
    if len >= 16 {
        let exact = format!("{:.*e}", len - 1, x.abs());
        if exact.parse::<f64>() == Ok(x.abs()) {
            sci = exact;
        }
    }
    let (mant, exp) = sci.split_once('e').unwrap();
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let n = exp.parse::<i32>().unwrap() + 1; // value = 0.digits * 10^n
    let mut s = String::new();
    if x < 0.0 {
        s.push('-');
    }
    if k <= n && n <= 21 {
        s += &digits;
        s.extend(std::iter::repeat_n('0', (n - k) as usize));
    } else if 0 < n && n <= 21 {
        s += &digits[..n as usize];
        s.push('.');
        s += &digits[n as usize..];
    } else if -6 < n && n <= 0 {
        s += "0.";
        s.extend(std::iter::repeat_n('0', (-n) as usize));
        s += &digits;
    } else {
        s += &digits[..1];
        if k > 1 {
            s.push('.');
            s += &digits[1..];
        }
        s.push('e');
        s.push(if n - 1 < 0 { '-' } else { '+' });
        s += &(n - 1).abs().to_string();
    }
    s
}
