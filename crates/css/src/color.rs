//! Colors: `#fff`, `red`, `rgb(0 128 255 / 50%)`, `hsl(120, 50%, 50%)`.
//!
//! A color is 4 bytes: red, green, blue and alpha, each 0 to 255. Alpha 255 is solid, 0 is clear.
//! Paint (module 5) needs nothing more, so we turn every form into this one when we parse.
//!
//! Forms:
//! - `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`. One hex digit `f` means `ff`.
//! - A name: 148 names (the table in `colors.rs` is generated), and `transparent`. Not case sensitive.
//! - `rgb()` and `rgba()`. The old form with commas (`rgb(0, 128, 255)`) and the new form with spaces
//!   and a slash for the alpha (`rgb(0 128 255 / 0.5)`). Numbers are 0 to 255, or percents.
//!   Both names work for both forms: `rgba(0 0 0)` is good.
//! - `hsl()` and `hsla()`. Hue is a number of degrees, or a unit (`deg`, `rad`, `grad`, `turn`).
//!   Saturation and lightness are percents.
//!
//! Left out: `hwb()`, `lab()`, `lch()`, `oklab()`, `color()`, `color-mix()`, `none`, relative colors, system colors.
//! A style that uses them is dropped like any value that we do not know.
//!
//! Comments compare Rust with JavaScript, because you know JS.

use crate::colors::NAMED_COLORS;
use crate::tokenizer::{Token, Tokens};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const BLACK: Color = Color { r: 0, g: 0, b: 0, a: 255 };
    pub const TRANSPARENT: Color = Color { r: 0, g: 0, b: 0, a: 0 };

    /// A named color. `name` must be in lowercase.
    pub fn named(name: &str) -> Option<Color> {
        if name == "transparent" {
            return Some(Color::TRANSPARENT);
        }
        let i = NAMED_COLORS.binary_search_by(|&(n, ..)| n.cmp(name)).ok()?;
        // JS: like `Array.prototype.findIndex`, but for a sorted list, and it halves the search each step.
        let (_, r, g, b) = NAMED_COLORS[i];
        Some(Color { r, g, b, a: 255 })
    }

    /// The text after the `#` of a hex color: 3, 4, 6 or 8 hex digits.
    pub fn hex(digits: &str) -> Option<Color> {
        let d: Vec<u8> = digits.bytes().map(|b| (b as char).to_digit(16).map(|d| d as u8)).collect::<Option<_>>()?;
        // `collect::<Option<_>>` gives `None` if one digit is not hex. JS: like `every` and `map` in one.
        Some(match d[..] {
            [r, g, b] => Color { r: r * 17, g: g * 17, b: b * 17, a: 255 },
            [r, g, b, a] => Color { r: r * 17, g: g * 17, b: b * 17, a: a * 17 },
            // `f * 17 = 255`, and `a * 17` is the digit written twice: `0xaa = 10 * 17`.
            [r1, r2, g1, g2, b1, b2] => Color { r: r1 << 4 | r2, g: g1 << 4 | g2, b: b1 << 4 | b2, a: 255 },
            [r1, r2, g1, g2, b1, b2, a1, a2] => Color { r: r1 << 4 | r2, g: g1 << 4 | g2, b: b1 << 4 | b2, a: a1 << 4 | a2 },
            _ => return None,
        })
    }
}

/// The name of a color function, in lowercase, or `None` for a name that is not one.
pub fn is_function(name: &str) -> bool {
    ["rgb", "rgba", "hsl", "hsla"].iter().any(|f| name.eq_ignore_ascii_case(f))
}

/// One argument: a number, a percent, or a number with a unit (a hue like `120deg`).
#[derive(Clone, Copy)]
enum Arg {
    Number(f32),
    Percent(f32),
    Angle(f32),
    // The angle is stored in degrees already.
}

/// Read the arguments of `rgb(`, `rgba(`, `hsl(` or `hsla(`, up to and with the `)`, and make the color.
/// Call it after the tokenizer gave the `Function` token. On an error, it gives `None`, and it reads to the `)` anyway.
pub fn parse_function(name: &str, tokens: &mut Tokens<'_>) -> Option<Color> {
    let mut args: [Option<Arg>; 4] = [None; 4];
    let (mut count, mut commas, mut slash, mut ok) = (0, 0, false, true);
    let mut after_arg = false;
    // A function or a `(` inside, like `rgb(var(--x))`: we do not know it, but we must read to the end of it.
    let mut depth = 0;
    loop {
        match tokens.next() {
            None => break,
            Some(Token::CloseParen) if depth == 0 => break,
            Some(Token::CloseParen) => depth -= 1,
            Some(Token::Function(_) | Token::OpenParen) => {
                depth += 1;
                ok = false;
            }
            Some(_) if depth > 0 => {}
            Some(Token::Whitespace) => continue,
            Some(Token::Comma) => {
                // The old form: `a, b, c` or `a, b, c, d`. A comma must come after an argument.
                ok &= after_arg && !slash;
                commas += 1;
                after_arg = false;
                continue;
            }
            Some(Token::Delim('/')) => {
                ok &= after_arg && !slash && count == 3 && commas == 0;
                slash = true;
                after_arg = false;
                continue;
            }
            Some(token) => {
                let arg = match token {
                    Token::Number { value, .. } => Some(Arg::Number(value)),
                    Token::Percentage(v) => Some(Arg::Percent(v)),
                    Token::Dimension { value, unit, .. } => angle_degrees(value, &unit).map(Arg::Angle),
                    _ => None,
                };
                // A mix of commas and spaces is a mistake. `commas` and `count` check this at the end.
                if count < 4 && arg.is_some() {
                    args[count] = arg;
                } else {
                    ok = false;
                }
                count += 1;
                after_arg = true;
            }
        }
    }
    // Old form: 3 or 4 arguments and 2 or 3 commas. New form: 3 arguments, or 4 with a slash. Not both.
    let legacy = commas > 0;
    let shape = if legacy { commas + 1 == count && !slash } else { (count == 3 && !slash) || (count == 4 && slash) };
    if !ok || !shape || !(3..=4).contains(&count) {
        return None;
    }
    let alpha = match args[3] {
        None => 255,
        Some(Arg::Number(v)) => to_byte(v * 255.0),
        Some(Arg::Percent(v)) => to_byte(v * 2.55),
        Some(Arg::Angle(_)) => return None,
    };
    let (a, b, c) = (args[0]?, args[1]?, args[2]?);
    if name.eq_ignore_ascii_case("rgb") || name.eq_ignore_ascii_case("rgba") {
        let channel = |arg: Arg| match arg {
            Arg::Number(v) => Some(to_byte(v)),
            Arg::Percent(v) => Some(to_byte(v * 2.55)),
            Arg::Angle(_) => None,
        };
        Some(Color { r: channel(a)?, g: channel(b)?, b: channel(c)?, a: alpha })
    } else {
        let hue = match a {
            Arg::Number(v) | Arg::Angle(v) => v,
            Arg::Percent(_) => return None,
        };
        let percent = |arg: Arg| match arg {
            Arg::Percent(v) | Arg::Number(v) => Some(v.clamp(0.0, 100.0) / 100.0),
            // The new form allows a plain number here, and means a percent.
            Arg::Angle(_) => None,
        };
        let (r, g, b) = hsl_to_rgb(hue, percent(b)?, percent(c)?);
        Some(Color { r: to_byte(r * 255.0), g: to_byte(g * 255.0), b: to_byte(b * 255.0), a: alpha })
    }
}

/// A number to a byte: round it, and keep it between 0 and 255.
fn to_byte(v: f32) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
    // `as u8` after `clamp` cannot lose anything. For NaN, Rust gives 0.
}

/// `120deg`, `0.5turn`, `1rad`, `100grad` in degrees.
fn angle_degrees(value: f32, unit: &str) -> Option<f32> {
    let k = match &*unit.to_ascii_lowercase() {
        "deg" => 1.0,
        "grad" => 0.9,
        "rad" => 180.0 / std::f32::consts::PI,
        "turn" => 360.0,
        _ => return None,
    };
    Some(value * k)
}

/// Hue in degrees (any number), saturation and lightness from 0 to 1. Gives red, green, blue from 0 to 1.
/// This is the algorithm of the CSS Color standard.
fn hsl_to_rgb(hue: f32, s: f32, l: f32) -> (f32, f32, f32) {
    let h = hue.rem_euclid(360.0);
    // `rem_euclid` makes -90 into 270. The `%` of Rust, like the one of JS, gives -90.
    let f = |n: f32| {
        let k = (n + h / 30.0) % 12.0;
        l - s * l.min(1.0 - l) * (k - 3.0).min(9.0 - k).clamp(-1.0, 1.0)
    };
    (f(0.0), f(8.0), f(4.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokenizer::tokenize;

    fn rgba(r: u8, g: u8, b: u8, a: u8) -> Option<Color> {
        Some(Color { r, g, b, a })
    }

    /// Parse a color function from the text of a whole function: `rgb(1 2 3)`.
    fn func(text: &str) -> Option<Color> {
        let mut tokens = tokenize(text);
        let Some(Token::Function(name)) = tokens.next() else { panic!("not a function: {text}") };
        let color = parse_function(&name, &mut tokens);
        assert_eq!(tokens.next(), None, "{text}: the function must end at its `)`");
        color
    }

    #[test]
    fn hex_forms() {
        assert_eq!(Color::hex("fff"), rgba(255, 255, 255, 255));
        assert_eq!(Color::hex("F80"), rgba(255, 136, 0, 255));
        assert_eq!(Color::hex("f808"), rgba(255, 136, 0, 136));
        assert_eq!(Color::hex("1a2B3c"), rgba(0x1a, 0x2b, 0x3c, 255));
        assert_eq!(Color::hex("1a2b3c4d"), rgba(0x1a, 0x2b, 0x3c, 0x4d));
        for bad in ["", "f", "ff", "fffff", "fffffff", "fffffffff", "ggg", "12345g", "#fff", " fff"] {
            assert_eq!(Color::hex(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn names() {
        assert_eq!(Color::named("red"), rgba(255, 0, 0, 255));
        assert_eq!(Color::named("rebeccapurple"), rgba(102, 51, 153, 255));
        assert_eq!(Color::named("transparent"), Some(Color::TRANSPARENT));
        assert_eq!(Color::named("RED"), None);
        // The caller makes the name lowercase: that way this function does not copy.
        assert_eq!(Color::named("reds"), None);
        assert_eq!(Color::named("currentcolor"), None);
    }

    #[test]
    fn every_name_is_found() {
        // The binary search needs a sorted table. If the table is not sorted, some names are not found.
        for &(name, r, g, b) in NAMED_COLORS {
            assert_eq!(Color::named(name), rgba(r, g, b, 255), "{name}");
        }
        assert_eq!(NAMED_COLORS.len(), 148);
        assert!(NAMED_COLORS.windows(2).all(|w| w[0].0 < w[1].0));
    }

    #[test]
    fn rgb_old_and_new_forms() {
        assert_eq!(func("rgb(255, 128, 0)"), rgba(255, 128, 0, 255));
        assert_eq!(func("rgb(255 128 0)"), rgba(255, 128, 0, 255));
        assert_eq!(func("RGB(255,128,0)"), rgba(255, 128, 0, 255));
        assert_eq!(func("rgba(255, 128, 0, 0.5)"), rgba(255, 128, 0, 128));
        assert_eq!(func("rgba(255 128 0 / 50%)"), rgba(255, 128, 0, 128));
        assert_eq!(func("rgb(255 128 0 / 0.5)"), rgba(255, 128, 0, 128));
        assert_eq!(func("rgb(  255  ,  128  ,  0  )"), rgba(255, 128, 0, 255));
        assert_eq!(func("rgb(100%, 50%, 0%)"), rgba(255, 128, 0, 255));
        assert_eq!(func("rgb(0 0 0 / 0)"), rgba(0, 0, 0, 0));
        assert_eq!(func("rgb(0 0 0 / 1)"), rgba(0, 0, 0, 255));
    }

    #[test]
    fn rgb_values_are_clamped_and_rounded() {
        assert_eq!(func("rgb(300, -5, 127.5)"), rgba(255, 0, 128, 255));
        assert_eq!(func("rgb(0, 0, 0, 2)"), rgba(0, 0, 0, 255));
        assert_eq!(func("rgb(0, 0, 0, -1)"), rgba(0, 0, 0, 0));
        assert_eq!(func("rgb(10.4, 10.5, 10.6)"), rgba(10, 11, 11, 255));
    }

    #[test]
    fn rgb_errors() {
        for bad in [
            "rgb()", "rgb(1)", "rgb(1, 2)", "rgb(1 2)", "rgb(1, 2, 3, 4, 5)", "rgb(1 2 3 4 5)", "rgb(1, 2 3)", "rgb(1 2, 3)",
            "rgb(1, 2, 3,)", "rgb(,1, 2, 3)", "rgb(1,, 2, 3)", "rgb(1 2 3 4)", "rgb(1, 2, 3 / 4)", "rgb(1 2 3 / )", "rgb(1 2 3 /)",
            "rgb(1 / 2 3)", "rgb(a, b, c)", "rgb(1px, 2, 3)", "rgb(1deg, 2, 3)", "rgb(1, 2, 3, 4deg)", "rgb(1 2 3 / 4 / 5)", "rgb(1 2 3 4 / 5)",
            "rgb(var(--x))", "rgb(1, 2, 3, 4, )", "rgb(1,2,3 4)",
        ] {
            assert_eq!(func(bad), None, "{bad}");
        }
    }

    #[test]
    fn hsl_forms() {
        assert_eq!(func("hsl(0, 100%, 50%)"), rgba(255, 0, 0, 255));
        assert_eq!(func("hsl(120 100% 25%)"), rgba(0, 128, 0, 255));
        assert_eq!(func("hsl(240deg, 100%, 50%)"), rgba(0, 0, 255, 255));
        assert_eq!(func("hsla(240, 100%, 50%, 0.5)"), rgba(0, 0, 255, 128));
        assert_eq!(func("hsl(0.5turn 100% 50%)"), rgba(0, 255, 255, 255));
        assert_eq!(func("hsl(3.14159265rad 100% 50%)"), rgba(0, 255, 255, 255));
        assert_eq!(func("hsl(200grad 100% 50%)"), rgba(0, 255, 255, 255));
        assert_eq!(func("hsl(0 0% 100%)"), rgba(255, 255, 255, 255));
        assert_eq!(func("hsl(0 0% 0%)"), rgba(0, 0, 0, 255));
        assert_eq!(func("hsl(0 0% 50%)"), rgba(128, 128, 128, 255));
        assert_eq!(func("hsl(210 50% 40%)"), rgba(51, 102, 153, 255));
    }

    #[test]
    fn hsl_hue_wraps_around() {
        let red = rgba(255, 0, 0, 255);
        assert_eq!(func("hsl(360, 100%, 50%)"), red);
        assert_eq!(func("hsl(720, 100%, 50%)"), red);
        assert_eq!(func("hsl(-360, 100%, 50%)"), red);
        assert_eq!(func("hsl(-120, 100%, 50%)"), func("hsl(240, 100%, 50%)"));
    }

    #[test]
    fn hsl_clamps_saturation_and_lightness() {
        assert_eq!(func("hsl(0, 200%, 50%)"), rgba(255, 0, 0, 255));
        assert_eq!(func("hsl(0, 100%, 150%)"), rgba(255, 255, 255, 255));
        assert_eq!(func("hsl(0, -50%, 50%)"), rgba(128, 128, 128, 255));
    }

    #[test]
    fn hsl_errors() {
        for bad in ["hsl(50%, 100%, 50%)", "hsl(0, 100%)", "hsl(0, 100%, 50%, 1, 2)", "hsl(0deg 1px 50%)", "hsl(a b c)", "hsl(0, 100%, 50%, 1deg)"] {
            assert_eq!(func(bad), None, "{bad}");
        }
        // A plain number for saturation and lightness is good in the new form.
        assert_eq!(func("hsl(0 100 50)"), rgba(255, 0, 0, 255));
    }

    #[test]
    fn function_names() {
        assert!(is_function("rgb") && is_function("RGBA") && is_function("Hsl") && is_function("hsla"));
        assert!(!is_function("hwb") && !is_function("rgbx") && !is_function(""));
    }

    #[test]
    fn a_function_that_is_not_closed_reads_to_the_end() {
        assert_eq!(func("rgb(1, 2, 3"), rgba(1, 2, 3, 255));
        assert_eq!(func("rgb(1, 2"), None);
    }
}
