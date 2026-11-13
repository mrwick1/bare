//! `transition-*` and `animation-*`: the lists that the page asks for. They are read here and kept as `Grid::Motion` values; the
//! engine (`bare/src/browser_motion.rs`) reads them to time the events. Values are not interpolated.
//!
//! - Longhands kept: `transition-property`, `-duration`, `-delay`, `-timing-function`; `animation-name`, `-duration`, `-delay`,
//!   `-iteration-count`. The shorthands `transition` and `animation` read every part of the grammar. Of `animation`, the timing function,
//!   direction, fill mode and play state are checked and dropped (no longhand keeps them).
//! - A time is kept in microseconds (`1.5s` is 1_500_000). An iteration count is kept in thousandths (`infinite` is `i64::MAX`).
//! - The initial value of each of the eight properties is `Value::Keyword(None)`: the engine reads it as the initial list
//!   (`all`, `0s`, `0s`, `ease`; `none`, `0s`, `0s`, `1`).
//! - Not valid, so dropped: a negative duration or iteration count, an unknown unit, a part that appears twice in one item.

use crate::values::{Declared, Property, Side, Value};

/// What a motion property holds: one entry for each item of the comma list.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Motion {
    /// A property name (`all`, `none`, `opacity`), a keyframes name, or the text of a timing function.
    Names(Vec<String>),
    /// Microseconds, or thousandths of an iteration. `i64::MAX` is `infinite`.
    Times(Vec<i64>),
}

pub const INFINITE: i64 = i64::MAX;

/// The longhands of `transition` and `animation`.
pub fn longhands(name: &str) -> Option<&'static [Property]> {
    use Property::*;
    Some(match name {
        "transition" => &[TransitionProperty, TransitionDuration, TransitionDelay, TransitionTimingFunction],
        "animation" => &[AnimationName, AnimationDuration, AnimationDelay, AnimationIterationCount],
        _ => return None,
    })
}

/// Cut `s` at the commas and spaces that are not inside parentheses.
fn split(s: &str, at: fn(char) -> bool) -> Vec<&str> {
    let (mut out, mut depth, mut from) = (Vec::new(), 0i32, 0);
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            c if depth == 0 && at(c) => {
                if from < i {
                    out.push(&s[from..i]);
                }
                from = i + c.len_utf8();
            }
            _ => {}
        }
    }
    if from < s.len() {
        out.push(&s[from..]);
    }
    out
}

fn time(t: &str) -> Option<i64> {
    let t = t.to_ascii_lowercase();
    let (n, scale) = match t.strip_suffix("ms") {
        Some(n) => (n, 1000.0),
        None => (t.strip_suffix('s')?, 1_000_000.0),
    };
    let v: f64 = n.parse().ok().filter(|v: &f64| v.is_finite() && !n.is_empty() && !n.ends_with('.') && n.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'+' | b'e')))?;
    Some((v * scale).round().clamp(-1e15, 1e15) as i64)
}

fn count(t: &str) -> Option<i64> {
    if t.eq_ignore_ascii_case("infinite") {
        return Some(INFINITE);
    }
    let v: f64 = t.parse().ok().filter(|v: &f64| v.is_finite() && *v >= 0.0)?;
    Some((v * 1000.0).round().min(1e12) as i64)
}

fn ident(t: &str) -> bool {
    let b = t.as_bytes();
    !b.is_empty() && (b[0].is_ascii_alphabetic() || b[0] == b'_' || b[0] == b'-' && b.len() > 1 && !b[1].is_ascii_digit() || b[0] >= 0x80) && t.chars().all(|c| c.is_alphanumeric() || matches!(c, '-' | '_'))
}

fn timing(t: &str) -> bool {
    let l = t.to_ascii_lowercase();
    matches!(l.as_str(), "ease" | "linear" | "ease-in" | "ease-out" | "ease-in-out" | "step-start" | "step-end")
        || ["cubic-bezier(", "steps(", "linear("].iter().any(|f| l.starts_with(f) && l.ends_with(')'))
}

fn wide(t: &str) -> Option<Value> {
    match t.to_ascii_lowercase().as_str() {
        "inherit" => Some(Value::Inherit),
        "initial" => Some(Value::Initial),
        "unset" => Some(Value::Unset),
        _ => None,
    }
}

const DIRECTIONS: [&str; 4] = ["normal", "reverse", "alternate", "alternate-reverse"];
const FILLS: [&str; 4] = ["none", "forwards", "backwards", "both"];
const STATES: [&str; 2] = ["running", "paused"];

fn is_in(l: &[&str], t: &str) -> bool {
    l.iter().any(|k| k.eq_ignore_ascii_case(t))
}

/// Read one declaration of this file. `None`: `name` is not one of them. `Some(false)`: the value is not good, and `out` is as before.
pub fn declaration(name: &str, value: &str, important: bool, side: &mut Side, out: &mut Vec<Declared>) -> Option<bool> {
    use Property::*;
    if !(name.starts_with("transition") || name.starts_with("animation")) {
        return None;
    }
    let one: &[Property] = match name {
        "transition-property" => &[TransitionProperty],
        "transition-duration" => &[TransitionDuration],
        "transition-delay" => &[TransitionDelay],
        "transition-timing-function" => &[TransitionTimingFunction],
        "animation-name" => &[AnimationName],
        "animation-duration" => &[AnimationDuration],
        "animation-delay" => &[AnimationDelay],
        "animation-iteration-count" => &[AnimationIterationCount],
        _ => longhands(name)?,
    };
    let text = value.trim();
    let mut emit = |p: Property, v: Value| out.push(Declared { property: p, value: v, important, mirror: false });
    if let Some(w) = wide(text) {
        one.iter().for_each(|&p| emit(p, w));
        return Some(true);
    }
    let mut keep = |m: Motion| {
        side.grids.push(crate::grid::Grid::Motion(m));
        Value::Grid(side.grids.len() as u32 - 1)
    };
    let items = split(text, |c| c == ',');
    if items.is_empty() {
        return Some(false);
    }
    let ok = match name {
        "transition-property" => items.iter().all(|i| ident(i.trim())).then(|| emit(TransitionProperty, keep(Motion::Names(items.iter().map(|i| i.trim().to_string()).collect())))),
        "animation-name" => items.iter().all(|i| ident(i.trim())).then(|| emit(AnimationName, keep(Motion::Names(items.iter().map(|i| i.trim().to_string()).collect())))),
        "transition-timing-function" => items.iter().all(|i| timing(i.trim())).then(|| emit(TransitionTimingFunction, keep(Motion::Names(items.iter().map(|i| i.trim().to_ascii_lowercase()).collect())))),
        "transition-duration" | "animation-duration" | "transition-delay" | "animation-delay" => {
            let delay = name.ends_with("delay");
            let times: Option<Vec<i64>> = items.iter().map(|i| time(i.trim()).filter(|t| delay || *t >= 0)).collect();
            times.map(|t| emit(one[0], keep(Motion::Times(t))))
        }
        "animation-iteration-count" => {
            let times: Option<Vec<i64>> = items.iter().map(|i| count(i.trim())).collect();
            times.map(|t| emit(AnimationIterationCount, keep(Motion::Times(t))))
        }
        "transition" => shorthand(&items, false).map(|(n, d, l, t)| {
            emit(TransitionProperty, keep(Motion::Names(n)));
            emit(TransitionDuration, keep(Motion::Times(d)));
            emit(TransitionDelay, keep(Motion::Times(l)));
            emit(TransitionTimingFunction, keep(Motion::Names(t)));
        }),
        "animation" => shorthand(&items, true).map(|(n, d, l, c)| {
            emit(AnimationName, keep(Motion::Names(n)));
            emit(AnimationDuration, keep(Motion::Times(d)));
            emit(AnimationDelay, keep(Motion::Times(l)));
            emit(AnimationIterationCount, keep(Motion::Times(c.iter().map(|s| s.parse().unwrap_or(1000)).collect())));
        }),
        _ => return None,
    };
    Some(ok.is_some())
}

type Lists = (Vec<String>, Vec<i64>, Vec<i64>, Vec<String>);

/// `transition` (names, durations, delays, timing functions) or `animation` (names, durations, delays, counts as text).
fn shorthand(items: &[&str], animation: bool) -> Option<Lists> {
    let (mut names, mut durations, mut delays, mut extra) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for item in items {
        let parts = split(item.trim(), char::is_whitespace);
        if parts.is_empty() {
            return None;
        }
        let (mut name, mut dur, mut delay, mut tf, mut cnt) = (None, None, None, None, None);
        let (mut dir, mut fill, mut state) = (false, false, false);
        for p in parts {
            let dup = if let Some(t) = time(p) {
                if dur.is_none() {
                    dur = Some(t).filter(|t| *t >= 0);
                    dur.is_none()
                } else {
                    delay.replace(t).is_some()
                }
            } else if timing(p) {
                tf.replace(p.to_ascii_lowercase()).is_some()
            } else if animation && count(p).is_some() {
                cnt.replace(count(p)?).is_some()
            } else if animation && is_in(&DIRECTIONS, p) {
                std::mem::replace(&mut dir, true)
            } else if animation && is_in(&FILLS, p) && !p.eq_ignore_ascii_case("none") {
                std::mem::replace(&mut fill, true)
            } else if animation && is_in(&STATES, p) {
                std::mem::replace(&mut state, true)
            } else if ident(p) && wide(p).is_none() && !p.eq_ignore_ascii_case("default") {
                name.replace(p.to_string()).is_some()
            } else {
                return None;
            };
            if dup {
                return None;
            }
        }
        // `animation: 1s` has no name (`none`); `transition: 1s` has the name `all`.
        names.push(name.unwrap_or_else(|| if animation { "none" } else { "all" }.to_string()));
        durations.push(dur.unwrap_or(0));
        delays.push(delay.unwrap_or(0));
        extra.push(if animation { cnt.unwrap_or(1000).to_string() } else { tf.unwrap_or_else(|| "ease".into()) });
    }
    // `none` is the only item, or no item at all.
    if names.len() > 1 && names.iter().any(|n| n.eq_ignore_ascii_case("none")) && !animation {
        return None;
    }
    Some((names, durations, delays, extra))
}

/// The text of a value, as `getComputedStyle` gives it.
pub fn text(p: Property, m: &Motion) -> String {
    fn secs(us: i64) -> String {
        let s = us as f64 / 1e6;
        format!("{}s", (s * 1e6).round() / 1e6)
    }
    match m {
        Motion::Names(n) => n.join(", "),
        Motion::Times(t) if p == Property::AnimationIterationCount => t.iter().map(|&t| if t == INFINITE { "infinite".to_string() } else { format!("{}", t as f64 / 1000.0) }).collect::<Vec<_>>().join(", "),
        Motion::Times(t) => t.iter().map(|&t| secs(t)).collect::<Vec<_>>().join(", "),
    }
}

/// The text of the initial value of a motion property.
pub fn initial_text(p: Property) -> &'static str {
    use Property::*;
    match p {
        TransitionProperty => "all",
        TransitionDuration | TransitionDelay | AnimationDuration | AnimationDelay => "0s",
        TransitionTimingFunction => "ease",
        AnimationName => "none",
        AnimationIterationCount => "1",
        _ => "",
    }
}


/// Is `p` one of the eight properties of this file?
pub fn is_motion(p: Property) -> bool {
    use Property::*;
    matches!(p, TransitionProperty | TransitionDuration | TransitionDelay | TransitionTimingFunction | AnimationName | AnimationDuration | AnimationDelay | AnimationIterationCount)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(name: &str, value: &str) -> Option<Vec<Motion>> {
        let (mut side, mut out) = (Side::default(), Vec::new());
        match declaration(name, value, false, &mut side, &mut out)? {
            true => Some(out.iter().filter_map(|d| match d.value { Value::Grid(i) => match &side.grids[i as usize] { crate::grid::Grid::Motion(m) => Some(m.clone()), _ => None }, _ => None }).collect()),
            false => None,
        }
    }

    #[test]
    fn transition_shorthand() {
        let m = read("transition", "opacity 100ms linear, transform 0.2s ease-in 50ms").unwrap();
        assert_eq!(m[0], Motion::Names(vec!["opacity".into(), "transform".into()]));
        assert_eq!(m[1], Motion::Times(vec![100_000, 200_000]));
        assert_eq!(m[2], Motion::Times(vec![0, 50_000]));
        assert_eq!(m[3], Motion::Names(vec!["linear".into(), "ease-in".into()]));
        assert_eq!(read("transition", "1s").unwrap()[0], Motion::Names(vec!["all".into()]));
    }

    #[test]
    fn animation_shorthand_and_bad_values() {
        let m = read("animation", "k 1s 2s infinite alternate both").unwrap();
        assert_eq!((&m[0], &m[1], &m[2], &m[3]), (&Motion::Names(vec!["k".into()]), &Motion::Times(vec![1_000_000]), &Motion::Times(vec![2_000_000]), &Motion::Times(vec![INFINITE])));
        assert!(read("transition-duration", "-1s").is_none());
        assert!(read("transition", "opacity 1s 1s 1s").is_none());
        assert!(read("animation-iteration-count", "-1").is_none());
        assert!(read("transition-delay", "-1s").is_some());
        assert_eq!(text(Property::TransitionDuration, &Motion::Times(vec![1_500_000, 100_000])), "1.5s, 0.1s");
    }
}
