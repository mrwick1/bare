//! Values: turn `margin: 0 auto` into typed data that layout and paint can use.
//!
//! The parser (`parser.rs`) gives a declaration as two texts: a name and a value. This file reads them:
//!
//! ```text
//! margin: 0 auto        four `Declared` items: margin-top 0, margin-right auto, margin-bottom 0, margin-left auto
//! color: #f80           one `Declared`: color, a `Color`
//! border: 1px solid red three longhands each for the four sides... 12 items
//! ```
//!
//! Decisions for speed and memory:
//! - A value is 8 bytes (`size_of::<Value>()`), a `Declared` is 12, and it has no `String` and no `Vec`.
//!   The style sheet text is read once, and what we keep is a flat list of numbers.
//! - No allocation while we read a value: the parts of a value go in an array on the stack (at most 8).
//! - A shorthand is split here, once, into its longhands. The cascade only knows longhands.
//! - Units that are fixed (`cm`, `pt`, `in`, ...) become `px` here. `em`, `rem`, `%`, `vw`, `vh`, `vmin` and `vmax` wait:
//!   they need the font or the window, and the cascade or layout knows those.
//!
//! The rules we follow (CSS Values 3, and the standard of each property):
//! - A value that is not good, in any way, throws away the whole declaration, and the older one for that
//!   property stays. `width: 10` (a number with no unit) is not good. `margin: 1px 2px 3px 4px 5px` is not good.
//! - `0` with no unit is a length.
//! - A negative number is not good for `width`, `padding`, `border-width` and so on, and it is good for `margin`.
//! - `inherit`, `initial` and `unset` work for every property, and for a shorthand they work for each longhand.
//! - Names and keywords are not case sensitive.
//!
//! Left out:
//! - `calc()`, `var()`, `env()`, `min()`, `max()`, `clamp()`. A declaration with one is dropped.
//! - `!important` is kept as a flag. The cascade uses it.
//! - Properties that we do not know. They are dropped. The list is what layout and paint will need in module 4 and 5.
//!   To add one, add a line in `properties!` and a case in `longhand`.
//! - `font-family`, `background-image`, `border-radius`, `box-shadow`, `position`, `overflow`, `opacity`: later, when something needs them.
//! - Approximations that we know: `ex` and `ch` are half an `em` (the real value needs the font). In `background`,
//!   only the color is used, and the other parts are checked a little and ignored. In `text-decoration`, only the line kind is used.
//!   `list-style-type` takes only the five words that we draw, and Chrome takes any word (a name of a counter style).
//!   In `font-size`, `larger` and `smaller` are kept as keywords for the cascade.
//!
//! Comments compare Rust with JavaScript, because you know JS.

use crate::color::{self, Color};
use crate::tokenizer::{Token, Tokens, tokenize};

/// Make the `Property` enum, with its list and its names, from one list. JS: like a map from the name to a number,
/// and back, but made when we build the program, so it costs nothing when it runs.
macro_rules! properties {
    ($($variant:ident => $name:literal,)*) => {
        /// A property that we know. `repr(u8)`: it is one byte, and `property as usize` is its place in `Property::ALL`.
        /// The cascade can keep the style of an element as an array with one place for each property.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[repr(u8)]
        pub enum Property {
            $($variant,)*
        }

        impl Property {
            /// Every property, in order.
            pub const ALL: &'static [Property] = &[$(Property::$variant,)*];
            pub const COUNT: usize = Property::ALL.len();

            /// The name in CSS, like `margin-top`.
            pub fn name(self) -> &'static str {
                match self {
                    $(Property::$variant => $name,)*
                }
            }

            /// The property for a name in lowercase. `None` if we do not know it, or if it is a shorthand.
            pub fn from_name(name: &str) -> Option<Property> {
                match name {
                    $($name => Some(Property::$variant),)*
                    _ => None,
                }
            }
        }
    };
}

properties! {
    Display => "display",
    Float => "float",
    Clear => "clear",
    Visibility => "visibility",
    WhiteSpace => "white-space",
    TextAlign => "text-align",
    TextDecoration => "text-decoration-line",
    TextTransform => "text-transform",
    ListStyleType => "list-style-type",
    BoxSizing => "box-sizing",
    TextColor => "color",
    BackgroundColor => "background-color",
    FontSize => "font-size",
    FontWeight => "font-weight",
    FontStyle => "font-style",
    LineHeight => "line-height",
    Width => "width",
    Height => "height",
    MinWidth => "min-width",
    MaxWidth => "max-width",
    MinHeight => "min-height",
    MaxHeight => "max-height",
    MarginTop => "margin-top",
    MarginRight => "margin-right",
    MarginBottom => "margin-bottom",
    MarginLeft => "margin-left",
    PaddingTop => "padding-top",
    PaddingRight => "padding-right",
    PaddingBottom => "padding-bottom",
    PaddingLeft => "padding-left",
    BorderTopWidth => "border-top-width",
    BorderRightWidth => "border-right-width",
    BorderBottomWidth => "border-bottom-width",
    BorderLeftWidth => "border-left-width",
    BorderTopStyle => "border-top-style",
    BorderRightStyle => "border-right-style",
    BorderBottomStyle => "border-bottom-style",
    BorderLeftStyle => "border-left-style",
    BorderTopColor => "border-top-color",
    BorderRightColor => "border-right-color",
    BorderBottomColor => "border-bottom-color",
    BorderLeftColor => "border-left-color",
}

/// A length or a percent. The fixed units are already `Px`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Length {
    Px(f32),
    /// Times the font size of this element (of the parent, for `font-size`).
    Em(f32),
    /// Times the font size of the `html` element.
    Rem(f32),
    /// A percent of something that depends on the property: the width of the container, the font size, ...
    /// 50% is `Percent(50.0)`.
    Percent(f32),
    /// 1% of the width of the window, so `10vw` is `Vw(10.0)`.
    Vw(f32),
    Vh(f32),
    /// 1% of the smaller side of the window, and of the larger side.
    Vmin(f32),
    Vmax(f32),
}

/// Every word that a value can be, for all the properties.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keyword {
    Auto, None, Normal, CurrentColor,
    // display
    Block, Inline, InlineBlock, ListItem, Table, TableRow, TableCell, TableRowGroup, TableHeaderGroup,
    TableFooterGroup, TableColumn, TableColumnGroup, TableCaption, Flex, InlineFlex, Grid, InlineGrid,
    // float, clear, text-align
    Left, Right, Both, Center, Justify, Start, End,
    // visibility, border-style
    Visible, Hidden, Collapse, Solid, Dashed, Dotted, Double, Groove, Ridge, Inset, Outset,
    // white-space
    Pre, Nowrap, PreWrap, PreLine, BreakSpaces,
    // text-decoration, text-transform
    Underline, Overline, LineThrough, Uppercase, Lowercase, Capitalize,
    // font
    Italic, Oblique, Bold, Bolder, Lighter, Larger, Smaller,
    XxSmall, XSmall, Small, Medium, Large, XLarge, XxLarge, XxxLarge,
    // border-width
    Thin, Thick,
    // list-style-type, box-sizing
    Disc, Circle, Square, Decimal, ContentBox, BorderBox,
}

/// The value of one property. It does not say which property: a `Declared` has that.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Value {
    /// `inherit`: take the value of the parent.
    Inherit,
    /// `initial`: take the starting value of the property.
    Initial,
    /// `unset`: `inherit` for a property that is inherited, `initial` for the others.
    Unset,
    Color(Color),
    Length(Length),
    /// `font-weight: 700`, `line-height: 1.5`.
    Number(f32),
    Keyword(Keyword),
}

/// One property with its value, after the shorthands are split.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Declared {
    pub property: Property,
    pub value: Value,
    pub important: bool,
}

/// Read one declaration, and add what it means to `out`: one item for a longhand, more for a shorthand.
/// Gives `false`, and adds nothing, when the name is not known or the value is not good.
/// `name` must be in lowercase (the parser does it).
pub fn parse_declaration(name: &str, value: &str, important: bool, out: &mut Vec<Declared>) -> bool {
    let Some(parts) = Parts::read(value) else { return false };
    let start = out.len();
    let ok = expand(name, &parts, &mut |property, value| out.push(Declared { property, value, important }));
    if !ok {
        out.truncate(start);
        // A shorthand may have added some items before it found the mistake. Take them back.
    }
    ok
}

/// One piece of a value, after the tokens are read. A function like `rgb(...)` is one piece.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Part {
    Color(Color),
    Length(Length),
    Number { value: f32, int: bool },
    Percent(f32),
    Keyword(Keyword),
    /// `inherit`, `initial` or `unset`.
    Wide(Value),
    Slash,
    /// Something that is wrong in every property: a bad color (`#ff`, `rgb(1,2)`), or a `!` or other mark with no meaning.
    Bad,
    /// Anything else that may be good: a string, a url, a comma, a name that we do not know, a function that we do not know.
    Other,
}

/// The parts of a value. At most 8 are kept, in an array: no allocation.
struct Parts {
    items: [Part; 8],
    len: usize,
    /// There were more than 8. Only `font` takes this (its family list is long).
    more: bool,
}

impl Parts {
    /// `None` when the value has no part, or a part that is a mistake in every property (a `{` block, for example).
    fn read(value: &str) -> Option<Parts> {
        let mut parts = Parts { items: [Part::Other; 8], len: 0, more: false };
        let mut tokens = tokenize(value);
        while let Some(token) = tokens.next() {
            let part = match token {
                Token::Whitespace => continue,
                Token::Ident(name) => ident(&name),
                Token::Hash { value, .. } => Color::hex(&value).map_or(Part::Bad, Part::Color),
                Token::Number { value, is_int } => Part::Number { value, int: is_int },
                Token::Percentage(v) => Part::Percent(v),
                Token::Dimension { value, unit, .. } => length(value, &unit).map_or(Part::Bad, Part::Length),
                Token::Delim('/') => Part::Slash,
                Token::Function(name) if color::is_function(&name) => color::parse_function(&name, &mut tokens).map_or(Part::Bad, Part::Color),
                Token::Function(_) | Token::OpenParen | Token::OpenSquare | Token::OpenCurly => {
                    skip_block(&mut tokens);
                    Part::Other
                }
                Token::Semicolon | Token::CloseParen | Token::CloseSquare | Token::CloseCurly => return None,
                Token::Delim(_) | Token::BadString | Token::BadUrl | Token::Colon => Part::Bad,
                _ => Part::Other,
            };
            if parts.len < 8 {
                parts.items[parts.len] = part;
                parts.len += 1;
            } else {
                parts.more = true;
            }
        }
        (parts.len > 0).then_some(parts)
    }

    fn as_slice(&self) -> &[Part] {
        &self.items[..self.len]
    }
}

/// Read to the end of the block that was just opened. A stack of depth only: a deep `((((` cannot overflow the call stack.
fn skip_block(tokens: &mut Tokens<'_>) {
    let mut depth = 1u32;
    while depth > 0 {
        match tokens.next() {
            Some(Token::Function(_) | Token::OpenParen | Token::OpenSquare | Token::OpenCurly) => depth += 1,
            Some(Token::CloseParen | Token::CloseSquare | Token::CloseCurly) => depth -= 1,
            Some(_) => {}
            None => return,
        }
    }
}

/// A word: a keyword, a color name, `inherit`, or something we do not know.
fn ident(name: &str) -> Part {
    // The longest word that we know has 20 letters. A longer one is not ours. This way we lowercase on the stack.
    let mut buf = [0u8; 24];
    let Some(slot) = buf.get_mut(..name.len()) else { return Part::Other };
    slot.copy_from_slice(name.as_bytes());
    slot.make_ascii_lowercase();
    let Ok(lower) = std::str::from_utf8(slot) else { return Part::Other };
    match lower {
        "inherit" => return Part::Wide(Value::Inherit),
        "initial" => return Part::Wide(Value::Initial),
        "unset" => return Part::Wide(Value::Unset),
        _ => {}
    }
    if let Some(keyword) = keyword(lower) {
        return Part::Keyword(keyword);
    }
    Color::named(lower).map_or(Part::Other, Part::Color)
}

fn keyword(word: &str) -> Option<Keyword> {
    use Keyword::*;
    Some(match word {
        "auto" => Auto, "none" => None, "normal" => Normal, "currentcolor" => CurrentColor,
        "block" => Block, "inline" => Inline, "inline-block" => InlineBlock, "list-item" => ListItem,
        "table" => Table, "table-row" => TableRow, "table-cell" => TableCell, "table-row-group" => TableRowGroup,
        "table-header-group" => TableHeaderGroup, "table-footer-group" => TableFooterGroup,
        "table-column" => TableColumn, "table-column-group" => TableColumnGroup, "table-caption" => TableCaption,
        "flex" => Flex, "inline-flex" => InlineFlex, "grid" => Grid, "inline-grid" => InlineGrid,
        "left" => Left, "right" => Right, "both" => Both, "center" => Center, "justify" => Justify, "start" => Start, "end" => End,
        "visible" => Visible, "hidden" => Hidden, "collapse" => Collapse, "solid" => Solid, "dashed" => Dashed,
        "dotted" => Dotted, "double" => Double, "groove" => Groove, "ridge" => Ridge, "inset" => Inset, "outset" => Outset,
        "pre" => Pre, "nowrap" => Nowrap, "pre-wrap" => PreWrap, "pre-line" => PreLine, "break-spaces" => BreakSpaces,
        "underline" => Underline, "overline" => Overline, "line-through" => LineThrough,
        "uppercase" => Uppercase, "lowercase" => Lowercase, "capitalize" => Capitalize,
        "italic" => Italic, "oblique" => Oblique, "bold" => Bold, "bolder" => Bolder, "lighter" => Lighter,
        "larger" => Larger, "smaller" => Smaller,
        "xx-small" => XxSmall, "x-small" => XSmall, "small" => Small, "medium" => Medium, "large" => Large,
        "x-large" => XLarge, "xx-large" => XxLarge, "xxx-large" => XxxLarge, "thin" => Thin, "thick" => Thick,
        "disc" => Disc, "circle" => Circle, "square" => Square, "decimal" => Decimal,
        "content-box" => ContentBox, "border-box" => BorderBox,
        _ => return Option::None,
    })
}

/// A number with a unit. `None` for a unit that we do not know.
fn length(value: f32, unit: &str) -> Option<Length> {
    // 96 px in an inch: this is fixed by the CSS standard, for every screen.
    let px = |per_unit: f32| Some(Length::Px(value * per_unit));
    let is = |name: &str| unit.eq_ignore_ascii_case(name);
    if is("px") {
        px(1.0)
    } else if is("em") {
        Some(Length::Em(value))
    } else if is("rem") {
        Some(Length::Rem(value))
    } else if is("vw") {
        Some(Length::Vw(value))
    } else if is("vh") {
        Some(Length::Vh(value))
    } else if is("vmin") {
        Some(Length::Vmin(value))
    } else if is("vmax") {
        Some(Length::Vmax(value))
    } else if is("pt") {
        px(96.0 / 72.0)
    } else if is("pc") {
        px(16.0)
    } else if is("in") {
        px(96.0)
    } else if is("cm") {
        px(96.0 / 2.54)
    } else if is("mm") {
        px(96.0 / 25.4)
    } else if is("q") {
        px(96.0 / 101.6)
    } else if is("ex") || is("ch") {
        Some(Length::Em(value * 0.5))
    } else {
        Option::None
    }
}

/// Give each longhand of the declaration `name` to `emit`. `false` if the name or the value is not good.
fn expand(name: &str, parts: &Parts, emit: &mut dyn FnMut(Property, Value)) -> bool {
    use Property::*;
    let slice = parts.as_slice();
    // `inherit`, `initial` and `unset` are the whole value, for a longhand and for a shorthand.
    if let [Part::Wide(wide)] = slice {
        return match shorthand_longhands(name) {
            Some(list) => {
                list.iter().for_each(|&p| emit(p, *wide));
                true
            }
            None => Property::from_name(name).map(|p| emit(p, *wide)).is_some(),
        };
    }
    if parts.more && name != "font" {
        return false;
    }
    match name {
        "margin" => sides(slice, [MarginTop, MarginRight, MarginBottom, MarginLeft], emit),
        "padding" => sides(slice, [PaddingTop, PaddingRight, PaddingBottom, PaddingLeft], emit),
        "border-width" => sides(slice, [BorderTopWidth, BorderRightWidth, BorderBottomWidth, BorderLeftWidth], emit),
        "border-style" => sides(slice, [BorderTopStyle, BorderRightStyle, BorderBottomStyle, BorderLeftStyle], emit),
        "border-color" => sides(slice, [BorderTopColor, BorderRightColor, BorderBottomColor, BorderLeftColor], emit),
        "border" => border(slice, &[(BorderTopWidth, BorderTopStyle, BorderTopColor), (BorderRightWidth, BorderRightStyle, BorderRightColor), (BorderBottomWidth, BorderBottomStyle, BorderBottomColor), (BorderLeftWidth, BorderLeftStyle, BorderLeftColor)], emit),
        "border-top" => border(slice, &[(BorderTopWidth, BorderTopStyle, BorderTopColor)], emit),
        "border-right" => border(slice, &[(BorderRightWidth, BorderRightStyle, BorderRightColor)], emit),
        "border-bottom" => border(slice, &[(BorderBottomWidth, BorderBottomStyle, BorderBottomColor)], emit),
        "border-left" => border(slice, &[(BorderLeftWidth, BorderLeftStyle, BorderLeftColor)], emit),
        "background" => background(slice, emit),
        "font" => font(slice, emit),
        "list-style" => match slice {
            [one] => longhand(ListStyleType, *one).map(|v| emit(ListStyleType, v)).is_some(),
            _ => false,
        },
        "text-decoration" => text_decoration(slice, emit),
        _ => {
            let Some(property) = Property::from_name(name) else { return false };
            let [one] = slice else { return false };
            longhand(property, *one).map(|v| emit(property, v)).is_some()
        }
    }
}

/// The longhands of a shorthand, for `inherit` and the like. `None` if `name` is not a shorthand.
fn shorthand_longhands(name: &str) -> Option<&'static [Property]> {
    use Property::*;
    let list: &'static [Property] = match name {
        "margin" => &[MarginTop, MarginRight, MarginBottom, MarginLeft],
        "padding" => &[PaddingTop, PaddingRight, PaddingBottom, PaddingLeft],
        "border-width" => &[BorderTopWidth, BorderRightWidth, BorderBottomWidth, BorderLeftWidth],
        "border-style" => &[BorderTopStyle, BorderRightStyle, BorderBottomStyle, BorderLeftStyle],
        "border-color" => &[BorderTopColor, BorderRightColor, BorderBottomColor, BorderLeftColor],
        "border" => &[
            BorderTopWidth, BorderRightWidth, BorderBottomWidth, BorderLeftWidth, BorderTopStyle, BorderRightStyle,
            BorderBottomStyle, BorderLeftStyle, BorderTopColor, BorderRightColor, BorderBottomColor, BorderLeftColor,
        ],
        "border-top" => &[BorderTopWidth, BorderTopStyle, BorderTopColor],
        "border-right" => &[BorderRightWidth, BorderRightStyle, BorderRightColor],
        "border-bottom" => &[BorderBottomWidth, BorderBottomStyle, BorderBottomColor],
        "border-left" => &[BorderLeftWidth, BorderLeftStyle, BorderLeftColor],
        "background" => &[BackgroundColor],
        "font" => &[FontStyle, FontWeight, FontSize, LineHeight],
        "list-style" => &[ListStyleType],
        "text-decoration" => &[TextDecoration],
        _ => return Option::None,
    };
    Some(list)
}

/// `margin: a`, `a b`, `a b c`, `a b c d`: the four sides, in the order top, right, bottom, left.
fn sides(slice: &[Part], props: [Property; 4], emit: &mut dyn FnMut(Property, Value)) -> bool {
    let (t, r, b, l) = match *slice {
        [a] => (a, a, a, a),
        [a, b] => (a, b, a, b),
        [a, b, c] => (a, b, c, b),
        [a, b, c, d] => (a, b, c, d),
        _ => return false,
    };
    // Each side has its own property, and the same rules. So the shorthand is good only if each part is good for it.
    // The width, style and color shorthands use the same longhand rules as the longhands.
    let values = [t, r, b, l].map(|part| longhand(props[0], part));
    // The first property is enough to check the part, because the four of a shorthand take the same kind of value.
    if values.iter().any(Option::is_none) {
        return false;
    }
    for (property, value) in props.into_iter().zip(values) {
        emit(property, value.unwrap());
    }
    true
}

/// `border: 1px solid red`: a width, a style and a color, in any order, each at most once. The ones that are missing get the starting values.
fn border(slice: &[Part], which: &[(Property, Property, Property)], emit: &mut dyn FnMut(Property, Value)) -> bool {
    use Property::{BorderTopColor, BorderTopStyle, BorderTopWidth};
    let (mut width, mut style, mut color) = (Option::None, Option::None, Option::None);
    if slice.len() > 3 {
        return false;
    }
    for &part in slice {
        // The first side is enough to find the kind of a part. The kinds are the same for the four sides.
        if let Some(v) = longhand(BorderTopWidth, part) {
            if width.replace(v).is_some() {
                return false;
            }
        } else if let Some(v) = longhand(BorderTopStyle, part) {
            if style.replace(v).is_some() {
                return false;
            }
        } else if let Some(v) = longhand(BorderTopColor, part) {
            if color.replace(v).is_some() {
                return false;
            }
        } else {
            return false;
        }
    }
    for &(w, s, c) in which {
        emit(w, width.unwrap_or(Value::Length(Length::Px(3.0))));
        // `medium` is 3px.
        emit(s, style.unwrap_or(Value::Keyword(Keyword::None)));
        emit(c, color.unwrap_or(Value::Keyword(Keyword::CurrentColor)));
    }
    true
}

/// `background: ...`. We use only the color. The other parts (an image, a position, `no-repeat`) are checked a little and
/// ignored, because we cannot draw them yet. If there is no color, the background color is the starting value, `transparent`.
/// The check: no bad part, no word of another property (`bold`), no number with no unit (but `0`).
/// This is not the whole grammar, and so some wrong values pass. We checked 2,600 values against Chrome: see `docs/progress.md`.
fn background(slice: &[Part], emit: &mut dyn FnMut(Property, Value)) -> bool {
    let mut color = Value::Color(Color::TRANSPARENT);
    let mut slash = false;
    for &part in slice {
        match part {
            Part::Color(c) => color = Value::Color(c),
            Part::Keyword(Keyword::CurrentColor) => color = Value::Keyword(Keyword::CurrentColor),
            Part::Keyword(Keyword::None | Keyword::Center | Keyword::Left | Keyword::Right) | Part::Length(_) | Part::Percent(_) | Part::Other => {}
            Part::Keyword(Keyword::Auto) if slash => {}
            Part::Number { value: 0.0, .. } => {}
            Part::Slash if !slash => slash = true,
            _ => return false,
        }
    }
    emit(Property::BackgroundColor, color);
    true
}

/// `text-decoration: underline red wavy 2px`. We use only the line. The style, the color and the thickness are checked a
/// little and ignored. Two lines, like `underline overline`, are not kept: the first one wins.
fn text_decoration(slice: &[Part], emit: &mut dyn FnMut(Property, Value)) -> bool {
    use Keyword as K;
    let mut line = Value::Keyword(K::None);
    let mut seen = false;
    for &part in slice {
        match part {
            Part::Keyword(K::None | K::Underline | K::Overline | K::LineThrough) => {
                if !seen {
                    line = longhand(Property::TextDecoration, part).unwrap();
                    seen = true;
                }
            }
            Part::Keyword(K::Solid | K::Dashed | K::Dotted | K::Double | K::Auto | K::CurrentColor)
            | Part::Color(_)
            | Part::Length(_)
            | Part::Percent(_)
            | Part::Number { value: 0.0, .. }
            | Part::Other => {}
            _ => return false,
        }
    }
    emit(Property::TextDecoration, line);
    true
}

/// `font: [style] [weight] size[/line-height] family`. The family is checked a little, and thrown away (module 5 has one font).
fn font(slice: &[Part], emit: &mut dyn FnMut(Property, Value)) -> bool {
    use Property::*;
    let mut style = Value::Keyword(Keyword::Normal);
    let mut weight = Value::Number(400.0);
    let (mut style_seen, mut weight_seen, mut i) = (false, false, 0);
    // Up to 3 words before the size: `normal` (which means the starting value), a style, a weight. Each one at most once.
    while i < slice.len() && i < 3 {
        match slice[i] {
            Part::Keyword(Keyword::Normal) => {}
            part @ Part::Keyword(Keyword::Italic | Keyword::Oblique) => {
                if std::mem::replace(&mut style_seen, true) {
                    return false;
                }
                style = longhand(FontStyle, part).unwrap();
            }
            part => match longhand(FontWeight, part) {
                Some(w) => {
                    if std::mem::replace(&mut weight_seen, true) {
                        return false;
                    }
                    weight = w;
                }
                None => break,
            },
        }
        i += 1;
    }
    let Some(&size_part) = slice.get(i) else { return false };
    let Some(size) = longhand(FontSize, size_part) else { return false };
    i += 1;
    let mut line_height = Value::Keyword(Keyword::Normal);
    if slice.get(i) == Some(&Part::Slash) {
        let Some(&lh) = slice.get(i + 1) else { return false };
        let Some(v) = longhand(LineHeight, lh) else { return false };
        line_height = v;
        i += 2;
    }
    // The family: at least one part. We do not keep it, but it must be words, strings and commas, not numbers.
    // (A list that is too long is cut at 8 parts, and that is fine.)
    if i >= slice.len() || slice[i..].iter().any(|p| !matches!(p, Part::Keyword(_) | Part::Color(_) | Part::Other)) {
        return false;
    }
    emit(FontStyle, style);
    emit(FontWeight, weight);
    emit(FontSize, size);
    emit(LineHeight, line_height);
    true
}

/// The value of one longhand property from one part. `None` if the part is not good for the property.
fn longhand(property: Property, part: Part) -> Option<Value> {
    use Keyword as K;
    use Property::*;
    // A number that is 0 is a length. Another number with no unit is not.
    let length = |part: Part| match part {
        Part::Length(l) => Some(l),
        Part::Number { value: 0.0, .. } => Some(Length::Px(0.0)),
        Part::Percent(p) => Some(Length::Percent(p)),
        _ => Option::None,
    };
    // A length that is not negative.
    let positive = |part: Part| {
        length(part).filter(|l| match *l {
            Length::Px(v) | Length::Em(v) | Length::Rem(v) | Length::Percent(v) | Length::Vw(v) | Length::Vh(v) | Length::Vmin(v) | Length::Vmax(v) => v >= 0.0,
        })
    };
    let px = |v: f32| Some(Value::Length(Length::Px(v)));
    let one_of = |part: Part, list: &[Keyword]| match part {
        Part::Keyword(k) if list.contains(&k) => Some(Value::Keyword(k)),
        _ => Option::None,
    };
    let border_styles = [K::None, K::Hidden, K::Solid, K::Dashed, K::Dotted, K::Double, K::Groove, K::Ridge, K::Inset, K::Outset];
    match property {
        Display => one_of(part, &[
            K::None, K::Block, K::Inline, K::InlineBlock, K::ListItem, K::Table, K::TableRow, K::TableCell, K::TableRowGroup,
            K::TableHeaderGroup, K::TableFooterGroup, K::TableColumn, K::TableColumnGroup, K::TableCaption, K::Flex, K::InlineFlex,
            K::Grid, K::InlineGrid,
        ]),
        Float => one_of(part, &[K::None, K::Left, K::Right]),
        Clear => one_of(part, &[K::None, K::Left, K::Right, K::Both]),
        Visibility => one_of(part, &[K::Visible, K::Hidden, K::Collapse]),
        WhiteSpace => one_of(part, &[K::Normal, K::Nowrap, K::Pre, K::PreWrap, K::PreLine, K::BreakSpaces]),
        TextAlign => one_of(part, &[K::Left, K::Right, K::Center, K::Justify, K::Start, K::End]),
        TextDecoration => one_of(part, &[K::None, K::Underline, K::Overline, K::LineThrough]),
        TextTransform => one_of(part, &[K::None, K::Uppercase, K::Lowercase, K::Capitalize]),
        ListStyleType => one_of(part, &[K::None, K::Disc, K::Circle, K::Square, K::Decimal]),
        BoxSizing => one_of(part, &[K::ContentBox, K::BorderBox]),
        FontStyle => one_of(part, &[K::Normal, K::Italic, K::Oblique]),
        TextColor | BackgroundColor | BorderTopColor | BorderRightColor | BorderBottomColor | BorderLeftColor => match part {
            Part::Color(c) => Some(Value::Color(c)),
            Part::Keyword(K::CurrentColor) => Some(Value::Keyword(K::CurrentColor)),
            _ => Option::None,
        },
        BorderTopStyle | BorderRightStyle | BorderBottomStyle | BorderLeftStyle => one_of(part, &border_styles),
        BorderTopWidth | BorderRightWidth | BorderBottomWidth | BorderLeftWidth => match part {
            Part::Keyword(K::Thin) => Some(Value::Length(Length::Px(1.0))),
            Part::Keyword(K::Medium) => Some(Value::Length(Length::Px(3.0))),
            Part::Keyword(K::Thick) => Some(Value::Length(Length::Px(5.0))),
            // Border widths take no percent.
            Part::Percent(_) => Option::None,
            _ => positive(part).map(Value::Length),
        },
        FontSize => match part {
            Part::Keyword(k @ (K::Larger | K::Smaller)) => Some(Value::Keyword(k)),
            // The sizes of Chrome, in pixels.
            Part::Keyword(K::XxSmall) => px(9.0),
            Part::Keyword(K::XSmall) => px(10.0),
            Part::Keyword(K::Small) => px(13.0),
            Part::Keyword(K::Medium) => px(16.0),
            Part::Keyword(K::Large) => px(18.0),
            Part::Keyword(K::XLarge) => px(24.0),
            Part::Keyword(K::XxLarge) => px(32.0),
            Part::Keyword(K::XxxLarge) => px(48.0),
            _ => positive(part).map(Value::Length),
        },
        FontWeight => match part {
            Part::Keyword(K::Normal) => Some(Value::Number(400.0)),
            Part::Keyword(K::Bold) => Some(Value::Number(700.0)),
            Part::Keyword(k @ (K::Bolder | K::Lighter)) => Some(Value::Keyword(k)),
            Part::Number { value, .. } if (1.0..=1000.0).contains(&value) => Some(Value::Number(value)),
            _ => Option::None,
        },
        LineHeight => match part {
            Part::Keyword(K::Normal) => Some(Value::Keyword(K::Normal)),
            Part::Number { value, .. } if value >= 0.0 => Some(Value::Number(value)),
            _ => positive(part).map(Value::Length),
        },
        Width | Height => match part {
            Part::Keyword(K::Auto) => Some(Value::Keyword(K::Auto)),
            _ => positive(part).map(Value::Length),
        },
        MinWidth | MinHeight => match part {
            Part::Keyword(K::Auto) => Some(Value::Keyword(K::Auto)),
            _ => positive(part).map(Value::Length),
        },
        MaxWidth | MaxHeight => match part {
            Part::Keyword(K::None) => Some(Value::Keyword(K::None)),
            _ => positive(part).map(Value::Length),
        },
        MarginTop | MarginRight | MarginBottom | MarginLeft => match part {
            Part::Keyword(K::Auto) => Some(Value::Keyword(K::Auto)),
            _ => length(part).map(Value::Length),
        },
        PaddingTop | PaddingRight | PaddingBottom | PaddingLeft => positive(part).map(Value::Length),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Keyword as K;
    use Property::*;

    fn parse(name: &str, value: &str) -> Vec<(Property, Value)> {
        let mut out = Vec::new();
        if parse_declaration(name, value, false, &mut out) {
            out.into_iter().map(|d| (d.property, d.value)).collect()
        } else {
            assert!(out.is_empty(), "{name}: {value} added items and still failed");
            vec![]
        }
    }

    fn one(name: &str, value: &str) -> Option<Value> {
        let list = parse(name, value);
        assert!(list.len() <= 1, "{name}: {value} gave {list:?}");
        list.first().map(|&(_, v)| v)
    }

    fn px(v: f32) -> Option<Value> {
        Some(Value::Length(Length::Px(v)))
    }

    #[test]
    fn sizes_are_small() {
        assert_eq!(std::mem::size_of::<Value>(), 8);
        assert_eq!(std::mem::size_of::<Declared>(), 12);
        assert_eq!(std::mem::size_of::<Property>(), 1);
    }

    #[test]
    fn property_names_round_trip() {
        for &p in Property::ALL {
            assert_eq!(Property::from_name(p.name()), Some(p));
            assert_eq!(Property::ALL[p as usize], p);
        }
        assert_eq!(Property::from_name("margin"), None);
        assert_eq!(Property::from_name("MARGIN-TOP"), None);
        assert_eq!(Property::from_name(""), None);
    }

    #[test]
    fn lengths() {
        assert_eq!(one("width", "10px"), px(10.0));
        assert_eq!(one("width", "0"), px(0.0));
        assert_eq!(one("width", "-0"), px(0.0));
        assert_eq!(one("width", "1.5em"), Some(Value::Length(Length::Em(1.5))));
        assert_eq!(one("width", "2REM"), Some(Value::Length(Length::Rem(2.0))));
        assert_eq!(one("width", "50%"), Some(Value::Length(Length::Percent(50.0))));
        assert_eq!(one("width", "10vw"), Some(Value::Length(Length::Vw(10.0))));
        assert_eq!(one("height", "10vh"), Some(Value::Length(Length::Vh(10.0))));
        assert_eq!(one("width", "auto"), Some(Value::Keyword(K::Auto)));
        assert_eq!(one("width", "AUTO"), Some(Value::Keyword(K::Auto)));
    }

    #[test]
    fn fixed_units_become_pixels() {
        assert_eq!(one("width", "1in"), px(96.0));
        assert_eq!(one("width", "72pt"), px(96.0));
        assert_eq!(one("width", "6pc"), px(96.0));
        assert_eq!(one("width", "2.54cm"), px(96.0));
        assert_eq!(one("width", "25.4mm"), px(96.0));
        assert_eq!(one("width", "101.6q"), px(96.0));
    }

    #[test]
    fn bad_lengths() {
        for bad in ["10", "10 px", "px", "10xx", "1.5", "-5px", "-1%", "calc(1px + 2px)", "var(--x)", "auto auto", "#fff", "red", "10px 20px", "\"10px\"", "url(a)", "10px;", "10px)", "{10px}"] {
            assert_eq!(parse("width", bad), vec![], "{bad}");
        }
    }

    #[test]
    fn margins_may_be_negative_and_paddings_may_not() {
        assert_eq!(one("margin-left", "-5px"), px(-5.0));
        assert_eq!(one("margin-left", "-5%"), Some(Value::Length(Length::Percent(-5.0))));
        assert_eq!(one("margin-left", "auto"), Some(Value::Keyword(K::Auto)));
        assert_eq!(parse("padding-left", "-5px"), vec![]);
        assert_eq!(parse("padding-left", "auto"), vec![]);
        assert_eq!(one("padding-left", "5%"), Some(Value::Length(Length::Percent(5.0))));
    }

    #[test]
    fn min_and_max() {
        assert_eq!(one("max-width", "none"), Some(Value::Keyword(K::None)));
        assert_eq!(one("max-width", "100%"), Some(Value::Length(Length::Percent(100.0))));
        assert_eq!(parse("max-width", "auto"), vec![]);
        assert_eq!(one("min-height", "0"), px(0.0));
        assert_eq!(parse("min-height", "none"), vec![]);
    }

    #[test]
    fn shorthand_sides() {
        let top = |v: &str| parse("margin", v).into_iter().map(|(_, v)| v).collect::<Vec<_>>();
        let p = |v: f32| Value::Length(Length::Px(v));
        assert_eq!(top("1px"), [p(1.0), p(1.0), p(1.0), p(1.0)]);
        assert_eq!(top("1px 2px"), [p(1.0), p(2.0), p(1.0), p(2.0)]);
        assert_eq!(top("1px 2px 3px"), [p(1.0), p(2.0), p(3.0), p(2.0)]);
        assert_eq!(top("1px 2px 3px 4px"), [p(1.0), p(2.0), p(3.0), p(4.0)]);
        assert_eq!(top("0 auto"), [p(0.0), Value::Keyword(K::Auto), p(0.0), Value::Keyword(K::Auto)]);
        assert_eq!(parse("margin", "1px")[0].0, MarginTop);
        assert_eq!(parse("margin", "1px")[3].0, MarginLeft);
        assert_eq!(parse("margin", "1px 2px 3px 4px 5px"), vec![]);
        assert_eq!(parse("margin", "1px red"), vec![]);
        assert_eq!(parse("padding", "1px auto"), vec![]);
        assert_eq!(parse("padding", "1px -2px"), vec![]);
    }

    #[test]
    fn border_shorthands() {
        let p = |v: f32| Value::Length(Length::Px(v));
        let list = parse("border", "1px solid red");
        assert_eq!(list.len(), 12);
        let get = |prop| list.iter().find(|(p, _)| *p == prop).unwrap().1;
        for (w, s, c) in [(BorderTopWidth, BorderTopStyle, BorderTopColor), (BorderLeftWidth, BorderLeftStyle, BorderLeftColor)] {
            assert_eq!(get(w), p(1.0));
            assert_eq!(get(s), Value::Keyword(K::Solid));
            assert_eq!(get(c), Value::Color(Color { r: 255, g: 0, b: 0, a: 255 }));
        }
        // Any order.
        assert_eq!(parse("border", "red 1px solid"), list);
        assert_eq!(parse("border", "solid red 1px"), list);
        // What is missing gets the starting value: medium (3px), none, currentcolor.
        let list = parse("border-top", "dashed");
        assert_eq!(list, [(BorderTopWidth, p(3.0)), (BorderTopStyle, Value::Keyword(K::Dashed)), (BorderTopColor, Value::Keyword(K::CurrentColor))]);
        assert_eq!(parse("border-bottom", "2px")[0], (BorderBottomWidth, p(2.0)));
        assert_eq!(parse("border", "0").len(), 12);
        assert_eq!(parse("border", "none")[1].1, Value::Keyword(K::None));
        // Twice the same kind, too many parts, a part that is no kind.
        assert_eq!(parse("border", "1px 2px"), vec![]);
        assert_eq!(parse("border", "red blue"), vec![]);
        assert_eq!(parse("border", "1px solid red red"), vec![]);
        assert_eq!(parse("border", "1px bold"), vec![]);
        assert_eq!(parse("border", "1px solid red inherit"), vec![]);
        assert_eq!(parse("border-width", "1px 2px 3px 4px").len(), 4);
        assert_eq!(parse("border-style", "solid none").len(), 4);
        assert_eq!(parse("border-color", "red").len(), 4);
        assert_eq!(parse("border-width", "solid"), vec![]);
    }

    #[test]
    fn border_width_words() {
        assert_eq!(one("border-top-width", "thin"), px(1.0));
        assert_eq!(one("border-top-width", "medium"), px(3.0));
        assert_eq!(one("border-top-width", "thick"), px(5.0));
        assert_eq!(parse("border", "thick solid")[0], (BorderTopWidth, Value::Length(Length::Px(5.0))));
        assert_eq!(parse("border-top-width", "-1px"), vec![]);
        assert_eq!(parse("border-top-width", "50%"), vec![]);
    }

    #[test]
    fn colors() {
        let red = Value::Color(Color { r: 255, g: 0, b: 0, a: 255 });
        assert_eq!(one("color", "red"), Some(red));
        assert_eq!(one("color", "RED"), Some(red));
        assert_eq!(one("color", "#f00"), Some(red));
        assert_eq!(one("color", "#FF0000"), Some(red));
        assert_eq!(one("color", "rgb(255, 0, 0)"), Some(red));
        assert_eq!(one("background-color", "rgb(255 0 0 / 100%)"), Some(red));
        assert_eq!(one("color", "hsl(0 100% 50%)"), Some(red));
        assert_eq!(one("color", "transparent"), Some(Value::Color(Color::TRANSPARENT)));
        assert_eq!(one("color", "currentcolor"), Some(Value::Keyword(K::CurrentColor)));
        assert_eq!(one("color", "CurrentColor"), Some(Value::Keyword(K::CurrentColor)));
        for bad in ["#ff", "#ggg", "rgb(1,2)", "notacolor", "10px", "red blue", "rgb(1 2 3) red", "color-mix(in srgb, red, blue)", "1"] {
            assert_eq!(parse("color", bad), vec![], "{bad}");
        }
    }

    #[test]
    fn keyword_properties() {
        assert_eq!(one("display", "block"), Some(Value::Keyword(K::Block)));
        assert_eq!(one("display", "Inline-Block"), Some(Value::Keyword(K::InlineBlock)));
        assert_eq!(one("display", "none"), Some(Value::Keyword(K::None)));
        assert_eq!(parse("display", "bold"), vec![]);
        assert_eq!(parse("display", "block inline"), vec![]);
        assert_eq!(one("float", "left"), Some(Value::Keyword(K::Left)));
        assert_eq!(parse("float", "both"), vec![]);
        assert_eq!(one("clear", "both"), Some(Value::Keyword(K::Both)));
        assert_eq!(one("text-align", "center"), Some(Value::Keyword(K::Center)));
        assert_eq!(one("white-space", "pre-wrap"), Some(Value::Keyword(K::PreWrap)));
        assert_eq!(one("visibility", "hidden"), Some(Value::Keyword(K::Hidden)));
        assert_eq!(one("text-transform", "uppercase"), Some(Value::Keyword(K::Uppercase)));
        assert_eq!(one("box-sizing", "border-box"), Some(Value::Keyword(K::BorderBox)));
        assert_eq!(one("list-style-type", "disc"), Some(Value::Keyword(K::Disc)));
        assert_eq!(one("list-style", "none"), Some(Value::Keyword(K::None)));
        assert_eq!(one("text-decoration", "underline"), Some(Value::Keyword(K::Underline)));
        assert_eq!(one("text-decoration-line", "line-through"), Some(Value::Keyword(K::LineThrough)));
        assert_eq!(one("text-decoration", "underline red"), Some(Value::Keyword(K::Underline)));
    }

    #[test]
    fn fonts() {
        assert_eq!(one("font-size", "12px"), px(12.0));
        assert_eq!(one("font-size", "120%"), Some(Value::Length(Length::Percent(120.0))));
        assert_eq!(one("font-size", "larger"), Some(Value::Keyword(K::Larger)));
        assert_eq!(parse("font-size", "-1px"), vec![]);
        assert_eq!(one("font-weight", "bold"), Some(Value::Number(700.0)));
        assert_eq!(one("font-weight", "normal"), Some(Value::Number(400.0)));
        assert_eq!(one("font-weight", "600"), Some(Value::Number(600.0)));
        assert_eq!(one("font-weight", "bolder"), Some(Value::Keyword(K::Bolder)));
        assert_eq!(parse("font-weight", "0"), vec![]);
        assert_eq!(parse("font-weight", "1001"), vec![]);
        assert_eq!(parse("font-weight", "10px"), vec![]);
        assert_eq!(one("font-style", "italic"), Some(Value::Keyword(K::Italic)));
        assert_eq!(one("line-height", "1.5"), Some(Value::Number(1.5)));
        assert_eq!(one("line-height", "20px"), px(20.0));
        assert_eq!(one("line-height", "150%"), Some(Value::Length(Length::Percent(150.0))));
        assert_eq!(one("line-height", "normal"), Some(Value::Keyword(K::Normal)));
        assert_eq!(parse("line-height", "-1"), vec![]);
    }

    #[test]
    fn font_size_words() {
        for (word, size) in [("xx-small", 9.0), ("x-small", 10.0), ("small", 13.0), ("medium", 16.0), ("large", 18.0), ("x-large", 24.0), ("xx-large", 32.0), ("xxx-large", 48.0)] {
            assert_eq!(one("font-size", word), px(size), "{word}");
        }
    }

    #[test]
    fn font_shorthand() {
        let list = parse("font", "italic bold 12px/1.5 Arial, sans-serif");
        assert_eq!(
            list,
            [
                (FontStyle, Value::Keyword(K::Italic)),
                (FontWeight, Value::Number(700.0)),
                (FontSize, Value::Length(Length::Px(12.0))),
                (LineHeight, Value::Number(1.5)),
            ]
        );
        let list = parse("font", "12px serif");
        assert_eq!(list[0], (FontStyle, Value::Keyword(K::Normal)));
        assert_eq!(list[1], (FontWeight, Value::Number(400.0)));
        assert_eq!(list[3], (LineHeight, Value::Keyword(K::Normal)));
        assert_eq!(parse("font", "bold 100%/120% \"Helvetica Neue\", Arial, sans-serif, serif, x, y, z")[2].1, Value::Length(Length::Percent(100.0)));
        assert_eq!(parse("font", "normal normal normal 1em a")[2].0, FontSize);
        assert_eq!(parse("font", "1px 2px 3px 4px"), vec![]);
        assert_eq!(parse("font", "12px"), vec![]);
        assert_eq!(parse("font", "serif"), vec![]);
        assert_eq!(parse("font", "12px/ serif"), vec![]);
        assert_eq!(parse("font", "bold italic bold 12px serif"), vec![]);
    }

    #[test]
    fn text_decoration_shorthand() {
        let line = |v: &str| parse("text-decoration", v).first().map(|&(_, v)| v);
        assert_eq!(line("underline"), Some(Value::Keyword(K::Underline)));
        assert_eq!(line("underline red wavy 2px"), Some(Value::Keyword(K::Underline)));
        assert_eq!(line("red"), Some(Value::Keyword(K::None)));
        assert_eq!(line("underline overline"), Some(Value::Keyword(K::Underline)));
        assert_eq!(line("1px solid red"), Some(Value::Keyword(K::None)));
        assert_eq!(line("bold"), None);
        assert_eq!(line("#ff"), None);
        assert_eq!(line("5"), None);
    }

    #[test]
    fn vmin_and_vmax() {
        assert_eq!(one("width", "6vmin"), Some(Value::Length(Length::Vmin(6.0))));
        assert_eq!(one("width", "6VMAX"), Some(Value::Length(Length::Vmax(6.0))));
    }

    #[test]
    fn background_shorthand() {
        let red = Value::Color(Color { r: 255, g: 0, b: 0, a: 255 });
        assert_eq!(parse("background", "red"), [(BackgroundColor, red)]);
        assert_eq!(parse("background", "url(a.png) no-repeat red center"), [(BackgroundColor, red)]);
        assert_eq!(parse("background", "#f00 url(a.png)"), [(BackgroundColor, red)]);
        assert_eq!(parse("background", "none"), [(BackgroundColor, Value::Color(Color::TRANSPARENT))]);
        assert_eq!(parse("background", "bold"), vec![]);
        assert_eq!(parse("background", "#ff"), vec![]);
        assert_eq!(parse("background", "1px solid red"), vec![]);
        assert_eq!(parse("background", "red 10"), vec![]);
        assert_eq!(parse("background", "linear-gradient(red, blue)"), [(BackgroundColor, Value::Color(Color::TRANSPARENT))]);
    }

    #[test]
    fn css_wide_keywords() {
        assert_eq!(one("color", "inherit"), Some(Value::Inherit));
        assert_eq!(one("width", "INITIAL"), Some(Value::Initial));
        assert_eq!(one("display", "unset"), Some(Value::Unset));
        assert_eq!(parse("margin", "inherit").len(), 4);
        assert_eq!(parse("border", "initial").len(), 12);
        assert_eq!(parse("font", "inherit").len(), 4);
        assert_eq!(parse("background", "inherit"), [(BackgroundColor, Value::Inherit)]);
        assert_eq!(parse("margin", "inherit auto"), vec![]);
        assert_eq!(parse("nope", "inherit"), vec![]);
    }

    #[test]
    fn unknown_names_and_empty_values() {
        assert_eq!(parse("nope", "1px"), vec![]);
        assert_eq!(parse("--custom", "1px"), vec![]);
        assert_eq!(parse("width", ""), vec![]);
        assert_eq!(parse("width", "   "), vec![]);
        assert_eq!(parse("", "1px"), vec![]);
    }

    #[test]
    fn important_is_kept() {
        let mut out = Vec::new();
        assert!(parse_declaration("margin", "0", true, &mut out));
        assert_eq!(out.len(), 4);
        assert!(out.iter().all(|d| d.important));
        let mut out = Vec::new();
        assert!(parse_declaration("margin", "0", false, &mut out));
        assert!(out.iter().all(|d| !d.important));
    }

    #[test]
    fn a_wrong_value_leaves_the_list_as_it_was() {
        let mut out = Vec::new();
        parse_declaration("width", "1px", false, &mut out);
        assert!(!parse_declaration("border", "1px solid red red", false, &mut out));
        assert!(!parse_declaration("margin", "1px 2px 3px 4px 5px", false, &mut out));
        assert!(!parse_declaration("padding", "1px -1px", false, &mut out));
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn deep_blocks_in_a_value_do_not_overflow() {
        let value = format!("{}{}", "(".repeat(100_000), ")".repeat(100_000));
        assert_eq!(parse("width", &value), vec![]);
        assert_eq!(parse("width", &"var(".repeat(100_000)), vec![]);
    }

    #[test]
    fn a_long_value_is_fine() {
        let value = format!("1px {}", "a ".repeat(10_000));
        assert_eq!(parse("margin", &value), vec![]);
        assert_eq!(parse("font", &format!("12px {}", "a, ".repeat(10_000))).len(), 4);
    }

    #[test]
    fn random_input_never_panics() {
        let alphabet: Vec<char> = "ab1.0%-+/#(),; px emnrgbhsl\"\\{}[]!".chars().collect();
        let names = ["margin", "border", "font", "background", "color", "width", "font-weight", "line-height", "text-decoration", "padding"];
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..20_000 {
            let len = next() % 20;
            let text: String = (0..len).map(|_| alphabet[(next() % alphabet.len() as u64) as usize]).collect();
            let mut out = Vec::new();
            let _ = parse_declaration(names[(next() % names.len() as u64) as usize], &text, false, &mut out);
        }
    }
}
