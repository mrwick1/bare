//! The regex cost rows (design-js2-regexp, step R0): the engine alone, on the old engine (`bare-js`) and on the new one (`bare-js2`),
//! with the same patterns and inputs. A pattern that one engine refuses is left out of both rows.
//!   regexp_row ENGINE WORK REPS      ENGINE: old | new      WORK: staging | porter | compile | size | bad
//! `staging`: 47 patterns of the staging pages on 20 short inputs. `porter`: 19 rules of the Porter stemmer on 2,000 words. Each is one
//! `exec` with all spans (the work of `test`). `size` prints the bytes of the new images. `bad` is `(a+)+b` on `a` repeated REPS times. Count with `tools/js/instr.sh`, for REPS
//! and for 0, and divide the difference by the runs. The line it prints has a checksum of all spans: the two engines must agree.
use std::time::Instant;

const STAGING: &str = include_str!("data/regexp/staging.tsv");
const PORTER: &str = include_str!("data/regexp/porter.tsv");
const INPUTS: &str = include_str!("data/regexp/inputs.txt");
const WORDS: &str = include_str!("data/regexp/words.txt");

fn u16s(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn mix(h: u64, a: Option<(usize, usize)>) -> u64 {
    match a {
        Some((a, b)) => h.wrapping_mul(31).wrapping_add((a as u64) << 20 | b as u64),
        None => h.wrapping_mul(31),
    }
}

enum Eng {
    Old(bare_js::regexp::Regex),
    New(Vec<u64>),
}

fn build(eng: &str, pat: &str, flags: &str) -> Option<Eng> {
    if eng == "old" {
        bare_js::regexp::Regex::from_str(pat, flags).ok().map(Eng::Old)
    } else {
        bare_js2::regexp::compile(&u16s(pat), flags).ok().map(Eng::New)
    }
}

struct Run {
    old: bare_js::regexp::Scratch,
    new: bare_js2::regexp::Scratch,
    out: Vec<u32>,
}

/// One `exec` with all spans; the checksum of the spans (0: no match, 7: an engine limit).
fn run(e: &Eng, input: &[u16], s: &mut Run) -> u64 {
    match e {
        Eng::Old(r) => match r.exec(input, 0, &bare_js::regexp::Limits::default(), None, &mut s.old) {
            Ok(Some(m)) => m.spans.iter().fold(1, |h, sp| mix(h, sp.map(|(a, b)| (a, b)))),
            Ok(None) => 0,
            Err(_) => 7,
        },
        Eng::New(img) => {
            // `exec` writes all the spans of a match, so a buffer that is long enough needs no reset.
            let n = 2 * (bare_js2::regexp::image_groups(img) + 1);
            if s.out.len() < n {
                s.out.resize(n, u32::MAX);
            }
            let mut fuel = u64::MAX;
            match bare_js2::regexp::exec(img, input, 0, &mut s.new, &mut fuel, &mut s.out) {
                Ok(true) => s.out.chunks(2).fold(1, |h, p| mix(h, (p[0] != u32::MAX).then(|| (p[0] as usize, p[1] as usize)))),
                Ok(false) => 0,
                Err(_) => 7,
            }
        }
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (eng, work, reps) = (a[1].as_str(), a[2].as_str(), a[3].parse::<usize>().expect("REPS"));
    let pairs = |t: &'static str| t.lines().filter_map(|l| l.split_once('\t')).collect::<Vec<_>>();
    let (staging, porter) = (pairs(STAGING), pairs(PORTER));
    let both = |set: &[(&str, &str)]| -> Vec<Eng> {
        // The patterns that both engines accept.
        set.iter().filter(|(f, p)| build("old", p, f).is_some() && build("new", p, f).is_some()).filter_map(|(f, p)| build(eng, p, f)).collect()
    };
    let mut s = Run { old: bare_js::regexp::Scratch::new(), new: bare_js2::regexp::Scratch::new(), out: Vec::new() };
    let (mut sum, mut runs) = (0u64, 0u64);
    match work {
        "compile" => {
            for _ in 0..reps {
                for (f, p) in staging.iter().chain(porter.iter()) {
                    std::hint::black_box(build(eng, p, f));
                    runs += 1;
                }
            }
        }
        "staging" | "porter" => {
            let (set, data): (Vec<Eng>, Vec<Vec<u16>>) =
                if work == "staging" { (both(&staging), INPUTS.lines().map(u16s).collect()) } else { (both(&porter), WORDS.lines().map(u16s).collect()) };
            for _ in 0..reps {
                for e in &set {
                    for d in &data {
                        sum = sum.wrapping_add(run(e, d, &mut s));
                        runs += 1;
                    }
                }
            }
        }
        "size" => {
            // The bytes of the images of the new engine: a blob cell is 8 bytes of header and the words, rounded up to 16.
            let imgs: Vec<usize> = staging.iter().chain(porter.iter()).filter_map(|(f, p)| bare_js2::regexp::compile(&u16s(p), f).ok()).map(|w| (8 + 8 * w.len()).next_multiple_of(16)).collect();
            println!("new size patterns={} bytes={} per_pattern={}", imgs.len(), imgs.iter().sum::<usize>(), imgs.iter().sum::<usize>() / imgs.len());
            return;
        }
        "bad" => {
            let e = build(eng, "(a+)+b", "").expect("compile");
            let input = u16s(&"a".repeat(reps));
            let t = Instant::now();
            let r = run(&e, &input, &mut s);
            println!("{eng} bad n={reps} result={r} ms={:.1} steps={}", t.elapsed().as_secs_f64() * 1e3, s.new.steps);
            return;
        }
        _ => panic!("WORK: staging | porter | compile | size | bad"),
    }
    println!("{eng} {work} reps={reps} runs={runs} sum={sum}");
}
