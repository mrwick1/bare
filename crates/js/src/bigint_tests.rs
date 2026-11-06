// Tests of the BigInt arithmetic in `bigint.rs`. The values are checked against `i128`, against algebraic identities at
// sizes that reach Karatsuba and divide and conquer, and against known texts. Node comparison is in `tools/js/bigint_check.sh`.

use super::*;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    /// A magnitude of `limbs` limbs with the edge patterns that find carry bugs.
    fn mag(&mut self, limbs: usize) -> Vec<u64> {
        let v = (0..limbs)
            .map(|_| match self.next() % 6 {
                0 => 0,
                1 => u64::MAX,
                2 => 1 << 63,
                _ => self.next(),
            })
            .collect();
        trim(v)
    }
    fn int(&mut self, limbs: usize) -> Int {
        let neg = self.next() & 1 == 1;
        Int::new(neg, self.mag(limbs))
    }
}

fn from_i128(v: i128) -> Int {
    let m = v.unsigned_abs();
    Int::new(v < 0, trim(vec![m as u64, (m >> 64) as u64]))
}

fn to_i128(a: &Int) -> Option<i128> {
    if a.mag.len() > 2 {
        return None;
    }
    let m = a.mag.first().copied().unwrap_or(0) as u128 | (a.mag.get(1).copied().unwrap_or(0) as u128) << 64;
    if a.neg { if m <= 1 << 127 { Some((m as i128).wrapping_neg()) } else { None } } else { i128::try_from(m).ok() }
}

fn text(a: &Int) -> String {
    to_string_radix(a.sv(), 10)
}

fn parse(s: &str) -> Int {
    let (neg, d) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s),
    };
    let mut i = parse_radix(d.as_bytes(), 10);
    i.neg = neg && !i.mag.is_empty();
    i
}

#[test]
fn known_values() {
    let mut f = Int::from_u64(1);
    for k in 1..=30u64 {
        f = mul(f.sv(), Int::from_u64(k).sv());
    }
    assert_eq!(text(&f), "265252859812191058636308480000000");
    let p = pow(Int::from_u64(2).sv(), 200);
    assert_eq!(text(&p), "1606938044258990275541962092341162602522202993782792835301376");
    assert_eq!(to_string_radix(p.sv(), 16), format!("1{}", "0".repeat(50)));
    assert_eq!(to_string_radix(p.sv(), 36), "bnklg118comha6gqury14067gur54n8won6guf4");
    assert_eq!(to_string_radix(Int::from_i64(-255).sv(), 16), "-ff");
    assert_eq!(to_string_radix(Int::zero().sv(), 2), "0");
    assert_eq!(text(&parse("-340282366920938463463374607431768211456")), "-340282366920938463463374607431768211456");
    assert_eq!(text(&pow(Int::from_i64(-3).sv(), 41)), "-36472996377170786403");
    assert_eq!(text(&pow(Int::from_i64(-3).sv(), 40)), "12157665459056928801");
    assert_eq!(text(&pow(Int::from_i64(7).sv(), 0)), "1");
    assert_eq!(text(&pow(Int::zero().sv(), 0)), "1");
    assert_eq!(text(&pow(Int::zero().sv(), 5)), "0");
}

#[test]
fn small_values_match_i128() {
    let mut r = Rng(1);
    for _ in 0..20000 {
        let (x, y) = ((r.next() as i64 >> (r.next() % 64)) as i128, (r.next() as i64 >> (r.next() % 64)) as i128);
        let (a, b) = (from_i128(x), from_i128(y));
        assert_eq!(to_i128(&add(a.sv(), b.sv())), Some(x + y));
        assert_eq!(to_i128(&sub(a.sv(), b.sv())), Some(x - y));
        assert_eq!(to_i128(&mul(a.sv(), b.sv())), Some(x * y));
        if y != 0 {
            let (q, rm) = divrem(a.sv(), b.sv());
            assert_eq!((to_i128(&q), to_i128(&rm)), (Some(x / y), Some(x % y)), "{x} / {y}");
        }
        assert_eq!(to_i128(&bitop(Bit::And, a.sv(), b.sv())), Some(x & y));
        assert_eq!(to_i128(&bitop(Bit::Or, a.sv(), b.sv())), Some(x | y));
        assert_eq!(to_i128(&bitop(Bit::Xor, a.sv(), b.sv())), Some(x ^ y));
        assert_eq!(to_i128(&not(a.sv())), Some(!x));
        let s = r.next() % 60;
        assert_eq!(to_i128(&shr(a.sv(), s)), Some(x >> s));
        assert_eq!(to_i128(&shl(a.sv(), s)), Some(x << s));
        assert_eq!(cmp(a.sv(), b.sv()), x.cmp(&y));
        assert_eq!(to_i64(a.sv()), i64::try_from(x).ok());
        assert_eq!(low_u64(a.sv()), x as u64);
    }
}

#[test]
fn as_int_n_and_as_uint_n_match_i128() {
    let mut r = Rng(2);
    for _ in 0..20000 {
        let x = (r.next() as i64 as i128) << (r.next() % 40);
        let bits = r.next() % 100;
        let a = from_i128(x);
        let m = 1i128 << bits;
        let u = x.rem_euclid(m);
        let i = if bits == 0 { 0 } else if u >= m >> 1 { u - m } else { u };
        assert_eq!(to_i128(&as_uint_n(a.sv(), bits, 1 << 30).unwrap()), Some(u), "asUintN({bits}, {x})");
        assert_eq!(to_i128(&as_int_n(a.sv(), bits, 1 << 30).unwrap()), Some(i), "asIntN({bits}, {x})");
    }
    // Huge widths: a non-negative value is unchanged, a negative one needs 2^bits (over the cap).
    let big = Int::from_i64(5);
    assert_eq!(as_uint_n(big.sv(), u64::MAX, 1 << 20).unwrap(), big);
    assert_eq!(as_int_n(big.sv(), u64::MAX, 1 << 20).unwrap(), big);
    assert!(as_uint_n(Int::from_i64(-1).sv(), 1 << 21, 1 << 20).is_none());
    assert_eq!(as_int_n(Int::from_i64(-1).sv(), u64::MAX, 1 << 20).unwrap(), Int::from_i64(-1));
}

#[test]
fn division_recomposes_at_every_size() {
    let mut r = Rng(3);
    for round in 0..1500 {
        let (la, lb) = (1 + (r.next() % 90) as usize, 1 + (r.next() % 70) as usize);
        let (a, b) = (r.mag(la), r.mag(lb));
        if b.is_empty() {
            continue;
        }
        let (q, rem) = divrem_mag(&a, &b);
        assert_eq!(cmp_mag(&rem, &b), Ordering::Less, "round {round}");
        assert_eq!(add_mag(&mul_mag(&q, &b), &rem), trimmed(&a), "round {round}");
    }
    // The add-back step of Knuth D: a divisor that makes the first estimate one too big.
    let b = vec![u64::MAX, u64::MAX, 1 << 63];
    let a = vec![0, 0, 0, u64::MAX, (1 << 63) - 1];
    let (q, rem) = divrem_mag(&a, &b);
    assert_eq!(add_mag(&mul_mag(&q, &b), &rem), a);
}

#[test]
fn karatsuba_equals_schoolbook() {
    let mut r = Rng(4);
    for _ in 0..60 {
        let (la, lb) = (1 + (r.next() % 400) as usize, 1 + (r.next() % 400) as usize);
        let (a, b) = (r.mag(la), r.mag(lb));
        if a.is_empty() || b.is_empty() {
            continue;
        }
        let (x, y) = if a.len() >= b.len() { (&a, &b) } else { (&b, &a) };
        let mut want = vec![0u64; x.len() + y.len()];
        mul_school(&mut want, x, y);
        assert_eq!(mul_mag(&a, &b), trim(want), "{la} x {lb}");
    }
    // The all-ones operand makes every carry happen.
    let a = vec![u64::MAX; 300];
    let sq = mul_mag(&a, &a);
    let mut want = vec![0u64; 600];
    mul_school(&mut want, &a, &a);
    assert_eq!(sq, trim(want));
}

#[test]
fn text_round_trips_in_every_radix() {
    let mut r = Rng(5);
    for radix in 2..=36u32 {
        for limbs in [0usize, 1, 2, 5, 31, 32, 33, 100, 250] {
            let a = Int::new(false, r.mag(limbs));
            let s = to_string_radix(a.sv(), radix);
            assert!(s == "0" || !s.starts_with('0'), "leading zero, radix {radix}, {limbs} limbs");
            assert_eq!(parse_radix(s.as_bytes(), radix), a, "radix {radix}, {limbs} limbs");
        }
    }
    // A power of ten has a long run of zero digits: the divide and conquer pads them.
    let p = pow(Int::from_u64(10).sv(), 5000);
    let s = text(&p);
    assert_eq!(s.len(), 5001);
    assert!(s.starts_with('1') && s[1..].bytes().all(|c| c == b'0'));
    let q = sub(p.sv(), Int::from_u64(1).sv());
    assert!(text(&q).bytes().all(|c| c == b'9') && text(&q).len() == 5000);
}

#[test]
fn bitwise_identities_at_large_sizes() {
    let mut r = Rng(6);
    for _ in 0..300 {
        let (la, lb) = ((r.next() % 6) as usize, (r.next() % 6) as usize);
        let (a, b) = (r.int(la), r.int(lb));
        let and = bitop(Bit::And, a.sv(), b.sv());
        let or = bitop(Bit::Or, a.sv(), b.sv());
        let xor = bitop(Bit::Xor, a.sv(), b.sv());
        // a + b == (a & b) + (a | b) and a ^ b == (a | b) - (a & b)
        assert_eq!(add(a.sv(), b.sv()), add(and.sv(), or.sv()));
        assert_eq!(xor, sub(or.sv(), and.sv()));
        // ~~a == a and ~a == -a - 1
        assert_eq!(not(not(a.sv()).sv()), a);
        // a << s >> s == a, and a >> s == floor(a / 2^s)
        let s = r.next() % 200;
        assert_eq!(shr(shl(a.sv(), s).sv(), s), a);
        let p = pow(Int::from_u64(2).sv(), s);
        let (q, rem) = divrem(a.sv(), p.sv());
        let floor = if rem.neg { sub(q.sv(), Int::from_u64(1).sv()) } else { q };
        assert_eq!(shr(a.sv(), s), floor);
    }
}

#[test]
fn number_conversions() {
    let f = |a: &Int| to_f64(a.sv());
    assert_eq!(f(&Int::zero()), 0.0);
    assert_eq!(f(&parse("9007199254740993")), 9007199254740992.0, "tie goes to even");
    assert_eq!(f(&parse("9007199254740995")), 9007199254740996.0, "tie goes to even");
    assert_eq!(f(&parse("-9007199254740993")), -9007199254740992.0);
    assert_eq!(f(&parse("18446744073709551615")), 18446744073709551616.0);
    assert_eq!(f(&pow(Int::from_u64(2).sv(), 1023)), 2f64.powi(1023));
    assert_eq!(f(&pow(Int::from_u64(2).sv(), 1024)), f64::INFINITY);
    assert_eq!(f(&pow(Int::from_i64(-2).sv(), 1025)), f64::NEG_INFINITY);
    // The largest double is 2^1024 - 2^971. Half an ulp more is a tie to an odd mantissa: it rounds up to infinity.
    let max = sub(pow(Int::from_u64(2).sv(), 1024).sv(), pow(Int::from_u64(2).sv(), 971).sv());
    assert_eq!(f(&max), f64::MAX);
    let tie = add(max.sv(), pow(Int::from_u64(2).sv(), 970).sv());
    assert_eq!(f(&tie), f64::INFINITY);
    let below = sub(tie.sv(), Int::from_u64(1).sv());
    assert_eq!(f(&below), f64::MAX);
    let sticky = add(add(pow(Int::from_u64(2).sv(), 200).sv(), pow(Int::from_u64(2).sv(), 147).sv()).sv(), Int::from_u64(1).sv());
    assert_eq!(f(&sticky), 2f64.powi(200) + 2f64.powi(148));
    for x in [0.0, 1.0, -1.0, 4294967296.0, 9007199254740992.0, 1e21, -1e300, f64::MAX, 2f64.powi(64)] {
        assert_eq!(to_f64(from_f64(x).sv()), x);
    }
    assert_eq!(from_f64(1e21), parse("1000000000000000000000"));
    use Ordering::*;
    let c = |a: &str, x: f64| cmp_f64(parse(a).sv(), x);
    assert_eq!(c("1", 1.0), Some(Equal));
    assert_eq!(c("1", 1.5), Some(Less));
    assert_eq!(c("2", 1.5), Some(Greater));
    assert_eq!(c("-1", -1.5), Some(Greater));
    assert_eq!(c("-2", -1.5), Some(Less));
    assert_eq!(c("0", -0.0), Some(Equal));
    assert_eq!(c("0", 0.5), Some(Less));
    assert_eq!(c("0", -0.5), Some(Greater));
    assert_eq!(c("9007199254740993", 9007199254740992.0), Some(Greater));
    assert_eq!(c("1", f64::NAN), None);
    assert_eq!(c("1", f64::INFINITY), Some(Less));
    assert_eq!(c("1", f64::NEG_INFINITY), Some(Greater));
}

#[test]
fn hash_differs_by_sign_and_value() {
    let (a, b, c) = (Int::from_u64(5), Int::from_i64(-5), Int::from_u64(6));
    assert_eq!(hash(a.sv()), hash(Int::from_u64(5).sv()));
    assert_ne!(hash(a.sv()), hash(b.sv()));
    assert_ne!(hash(a.sv()), hash(c.sv()));
}
