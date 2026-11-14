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
//! - `env()`, `min()`, `max()`, `clamp()`. A declaration with one is dropped. (`calc()` is in `calc.rs`, and `var()` in `var.rs`.)
//! - `!important` is kept as a flag. The cascade uses it.
//! - Properties that we do not know. They are dropped. The list is what layout and paint will need in module 4 and 5.
//!   To add one, add a line in `properties!` and a case in `longhand`.
//! - `font-family`, `background-image`, `box-shadow`: later, when something needs them.
//! - `border-radius` takes one radius for each corner (a circle). The part after a `/` is read and dropped.
//! - Approximations that we know: `ch` is half an `em` (the real value needs the font; `ex` is 0.536 em, the x-height of the one font). In `background`,
//!   only the color is used, and the other parts are checked a little and ignored. In `text-decoration`, only the line kind is used.
//!   `list-style-type` takes only the five words that we draw, and Chrome takes any word (a name of a counter style).
//!   In `font-size`, `larger` and `smaller` are kept as keywords for the cascade.
//!
//! Comments compare Rust with JavaScript, because you know JS.

use crate::calc::{self, Calc, Linear};
use crate::color::{self, Color};
use crate::var::{self, VarDecl};
use crate::tokenizer::{Token, Tokens, tokenize};

/// Make the `Property` enum, with its list and its names, from one list. JS: like a map from the name to a number,
/// and back, but made when we build the program, so it costs nothing when it runs.
macro_rules! properties {
    ($($variant:ident => $name:literal,)* ; $($pic:ident => $pic_name:literal,)*) => {
        /// A property that we know. `repr(u8)`: it is one byte, and `property as usize` is its place in `Property::ALL`.
        /// The cascade can keep the style of an element as an array with one place for each property.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[repr(u8)]
        pub enum Property {
            $($variant,)*
            $($pic,)*
        }

        impl Property {
            /// Every property, in order.
            pub const ALL: &'static [Property] = &[$(Property::$variant,)* $(Property::$pic,)*];
            pub const COUNT: usize = Property::ALL.len();

            /// The name in CSS, like `margin-top`.
            pub fn name(self) -> &'static str {
                match self {
                    $(Property::$variant => $name,)*
                    $(Property::$pic => $pic_name,)*
                }
            }

            /// The property for a name in lowercase. `None` if we do not know it, or if it is a shorthand.
            pub fn from_name(name: &str) -> Option<Property> {
                // `-webkit-user-select` is a legacy alias: the same property.
                Property::from_core_name(name).or_else(|| Property::from_picture_name(name)).or_else(|| (name == "-webkit-user-select").then_some(Property::UserSelect))
            }

            /// `from_name` for the properties before `Property::CORE`. The parser asks this first: a name of a picture property costs it nothing.
            #[inline(always)]
            pub fn from_core_name(name: &str) -> Option<Property> {
                match name {
                    $($name => Some(Property::$variant),)*
                    _ => None,
                }
            }

            #[cold]
            #[inline(never)]
            fn from_picture_name(name: &str) -> Option<Property> {
                match name {
                    $($pic_name => Some(Property::$pic),)*
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
    LetterSpacing => "letter-spacing",
    WordSpacing => "word-spacing",
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
    OverflowX => "overflow-x",
    OverflowY => "overflow-y",
    Position => "position",
    Top => "top",
    Right => "right",
    Bottom => "bottom",
    Left => "left",
    FlexDirection => "flex-direction",
    FlexWrap => "flex-wrap",
    FlexGrow => "flex-grow",
    FlexShrink => "flex-shrink",
    FlexBasis => "flex-basis",
    Order => "order",
    JustifyContent => "justify-content",
    AlignContent => "align-content",
    AlignItems => "align-items",
    AlignSelf => "align-self",
    RowGap => "row-gap",
    ColumnGap => "column-gap",
    GridTemplateColumns => "grid-template-columns",
    GridTemplateRows => "grid-template-rows",
    GridTemplateAreas => "grid-template-areas",
    GridAutoColumns => "grid-auto-columns",
    GridAutoRows => "grid-auto-rows",
    GridAutoFlow => "grid-auto-flow",
    GridRowStart => "grid-row-start",
    GridRowEnd => "grid-row-end",
    GridColumnStart => "grid-column-start",
    GridColumnEnd => "grid-column-end",
    JustifyItems => "justify-items",
    JustifySelf => "justify-self",
    BorderCollapse => "border-collapse",
    BorderSpacingH => "-webkit-border-horizontal-spacing",
    BorderSpacingV => "-webkit-border-vertical-spacing",
    TableLayout => "table-layout",
    CaptionSide => "caption-side",
    VerticalAlign => "vertical-align",
    Direction => "direction",
    ZIndex => "z-index",
    Opacity => "opacity",
    BorderTopLeftRadius => "border-top-left-radius",
    BorderTopRightRadius => "border-top-right-radius",
    BorderBottomRightRadius => "border-bottom-right-radius",
    BorderBottomLeftRadius => "border-bottom-left-radius",
    Appearance => "appearance",
    PointerEvents => "pointer-events",
    AspectRatio => "aspect-ratio",
    Fill => "fill",
    Stroke => "stroke",
    ObjectFit => "object-fit",
    Cursor => "cursor",
    UserSelect => "user-select",
    TransitionProperty => "transition-property",
    TransitionDuration => "transition-duration",
    TransitionDelay => "transition-delay",
    TransitionTimingFunction => "transition-timing-function",
    AnimationName => "animation-name",
    AnimationDuration => "animation-duration",
    AnimationDelay => "animation-delay",
    AnimationIterationCount => "animation-iteration-count",
    TranslateX => "-bare-translate-x",
    TranslateY => "-bare-translate-y",
    ;
    BackgroundImage => "background-image",
    BackgroundSizeX => "background-size-x",
    BackgroundSizeY => "background-size-y",
    BackgroundPositionX => "background-position-x",
    BackgroundPositionY => "background-position-y",
    BackgroundRepeat => "background-repeat",
    MaskImage => "mask-image",
    MaskSizeX => "mask-size-x",
    MaskSizeY => "mask-size-y",
    MaskPositionX => "mask-position-x",
    MaskPositionY => "mask-position-y",
    MaskRepeat => "mask-repeat",
}

impl Property {
    /// The properties that the CSS object model of a page (`style`, `getComputedStyle`) lists and has an accessor for. The picture
    /// properties (`CORE..`) come last. The engine makes an accessor for each listed property each time a page starts, and that costs
    /// instructions for each page. The two last core properties (`TranslateX`, `TranslateY`, the parts of `transform` and `translate`) are not listed:
    /// the page has no accessor for them. `from_name` still knows them, so `setProperty` and `getPropertyValue` work for them.
    pub const LISTED: &'static [Property] = Property::ALL.split_at(Property::CORE - 2).0;
    /// The number of properties before the picture properties (`background-image`, `mask-image` and their size, position and repeat). A
    /// computed style has one slot for each of these. The others (`COUNT - CORE`) are one set that most elements share: it is one number
    /// in the style (`cascade::ComputedStyle::picture`). A declaration of one is kept apart from the others (`cascade::Entry::pics`).
    pub const CORE: usize = Property::BackgroundImage as usize;

    /// Is this a picture property (`CORE..`)?
    #[inline(always)]
    pub const fn is_picture(self) -> bool {
        self as usize >= Property::CORE
    }
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
    /// A `calc()` that mixes units, like `100% - 20px`. The number is the place of its `Linear` in a list. In a declared value
    /// that is the list that `parse_declaration` fills. In a computed value it is the list of the `Styles`: a pair of pixels and percent.
    /// A `min()`, `max()` or `clamp()` is also a `Calc`: its place holds a tree (`calc::encode`, `calc::flatten`), and the first entry has NaN pixels.
    /// Then only the cold code reads the tree, and the hot code of the layout has no other case.
    Calc(u32),
}

/// Every word that a value can be, for all the properties. One list makes the enum, the name of each word, and the way back.
macro_rules! keywords {
    ($($variant:ident => $name:literal,)*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Keyword {
            $($variant,)*
        }

        impl Keyword {
            /// The word in CSS, like `inline-block`.
            pub fn name(self) -> &'static str {
                match self {
                    $(Keyword::$variant => $name,)*
                }
            }

            /// The keyword for a word in lowercase.
            pub fn from_name(word: &str) -> Option<Keyword> {
                match word {
                    $($name => Some(Keyword::$variant),)*
                    _ => None,
                }
            }
        }
    };
}

keywords! {
    Auto => "auto", None => "none", Normal => "normal", CurrentColor => "currentcolor",
    Block => "block", Inline => "inline", InlineBlock => "inline-block", ListItem => "list-item",
    Table => "table", TableRow => "table-row", TableCell => "table-cell", TableRowGroup => "table-row-group",
    TableHeaderGroup => "table-header-group", TableFooterGroup => "table-footer-group", TableColumn => "table-column", TableColumnGroup => "table-column-group",
    TableCaption => "table-caption", Flex => "flex", InlineFlex => "inline-flex", Grid => "grid",
    InlineGrid => "inline-grid", Left => "left", Right => "right", Both => "both",
    Center => "center", Justify => "justify", Start => "start", End => "end", InlineStart => "inline-start", InlineEnd => "inline-end",
    Visible => "visible", Hidden => "hidden", Collapse => "collapse", Solid => "solid",
    Dashed => "dashed", Dotted => "dotted", Double => "double", Groove => "groove",
    Ridge => "ridge", Inset => "inset", Outset => "outset", Pre => "pre",
    Nowrap => "nowrap", PreWrap => "pre-wrap", PreLine => "pre-line", BreakSpaces => "break-spaces",
    Underline => "underline", Overline => "overline", LineThrough => "line-through", Uppercase => "uppercase",
    Lowercase => "lowercase", Capitalize => "capitalize", Italic => "italic", Oblique => "oblique",
    Bold => "bold", Bolder => "bolder", Lighter => "lighter", Larger => "larger",
    Smaller => "smaller", XxSmall => "xx-small", XSmall => "x-small", Small => "small",
    Medium => "medium", Large => "large", XLarge => "x-large", XxLarge => "xx-large",
    XxxLarge => "xxx-large", Thin => "thin", Thick => "thick", Disc => "disc",
    Circle => "circle", Square => "square", Decimal => "decimal", ContentBox => "content-box",
    BorderBox => "border-box", FlowRoot => "flow-root", Clip => "clip", Scroll => "scroll",
    Static => "static", Relative => "relative", Absolute => "absolute", Fixed => "fixed",
    Sticky => "sticky", Row => "row", RowReverse => "row-reverse", Column => "column",
    ColumnReverse => "column-reverse", Wrap => "wrap", WrapReverse => "wrap-reverse", Content => "content",
    FlexStart => "flex-start", FlexEnd => "flex-end", SpaceBetween => "space-between", SpaceAround => "space-around",
    SpaceEvenly => "space-evenly", Stretch => "stretch", Baseline => "baseline", SelfStart => "self-start",
    SelfEnd => "self-end", First => "first", Last => "last", LastBaseline => "last baseline",
    Safe => "safe", Unsafe => "unsafe", InlineTable => "inline-table", Separate => "separate",
    Top => "top", Bottom => "bottom", Middle => "middle", Sub => "sub",
    Super => "super", TextTop => "text-top", TextBottom => "text-bottom", Ltr => "ltr",
    Rtl => "rtl", Button => "button", Checkbox => "checkbox", Radio => "radio",
    Textfield => "textfield", Textarea => "textarea", Menulist => "menulist", Text => "text",
    All => "all", Painted => "painted", Fill => "fill", Stroke => "stroke", VisiblePainted => "visiblepainted",
    VisibleFill => "visiblefill", VisibleStroke => "visiblestroke",
    Contain => "contain", Cover => "cover", ScaleDown => "scale-down", Repeat => "repeat", NoRepeat => "no-repeat",
    RepeatX => "repeat-x", RepeatY => "repeat-y", Default => "default", Pointer => "pointer",
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
    ColorExpr(u32),
    Length(Length),
    /// `font-weight: 700`, `line-height: 1.5`.
    Number(f32),
    Keyword(Keyword),
    /// The value has `var()` in it. The number is the place of its `VarDecl` in `Side::deferred`. The cascade reads the text when it knows
    /// the custom properties of the element.
    Var(u32),
    /// A track list, a line or area names: the place of a `grid::Grid` in `Side::grids` (declared) or `Styles::grids` (computed).
    Grid(u32),
}

/// One property with its value, after the shorthands are split.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Declared {
    pub property: Property,
    pub value: Value,
    pub important: bool,
    /// A logical declaration of the inline axis (`margin-inline-start`). `property` is the one for `ltr`: the cascade swaps left and right in an `rtl` element.
    pub mirror: bool,
}

/// Read one declaration, and add what it means to `out`: one item for a longhand, more for a shorthand.
/// Gives `false`, and adds nothing, when the name is not known or the value is not good.
/// A `calc()` that mixes units goes in `side.calcs`, and a value with `var()` goes in `side.deferred`: the value points to them.
/// `name` must be in lowercase (the parser does it).
pub fn parse_declaration(mut name: &str, value: &str, important: bool, side: &mut Side, out: &mut Vec<Declared>) -> bool {
    // `-webkit-mask-image` is `mask-image`: the same property with the prefix taken off. The second round runs only after a name or a value
    // was not good, so a good declaration never pays for it. (One call of `parse_known`, so that it is inlined once.)
    loop {
        if parse_known(name, value, important, side, out) {
            return true;
        }
        match name.strip_prefix("-webkit-") {
            Some(n) if n.starts_with("mask") || n == "user-select" => name = n,
            _ => return false,
        }
    }
}

/// `transform` and `translate`: the move of the box, as `TranslateX` and `TranslateY`. `Some(false)`: the value is not good.
/// Only the translation counts: `translate()`, `translateX()`, `translateY()` and `translate3d()` (its z is read and dropped), and the
/// `translate` property. The other functions (`rotate`, `scale`, `skew`, `matrix`, `perspective`) are good values and do nothing. Several
/// translations add up when their units agree (two `px`, two `%`); a mix of units is not good (it needs `calc()`).
fn transform_declaration(name: &str, value: &str, important: bool, side: &mut Side, out: &mut Vec<Declared>) -> Option<bool> {
    let text = value.trim();
    let zero = Value::Length(Length::Px(0.0));
    let mut emit = |x: Value, y: Value| {
        out.push(Declared { property: Property::TranslateX, value: x, important, mirror: false });
        out.push(Declared { property: Property::TranslateY, value: y, important, mirror: false });
    };
    if text.eq_ignore_ascii_case("none") {
        emit(zero, zero);
        return Some(true);
    }
    // One argument: a length, a percent or `0`.
    fn arg(t: &str, side: &mut Side) -> Option<Length> {
        match *Parts::read(t, side)?.as_slice() {
            [Part::Length(l)] => Some(l),
            [Part::Percent(p)] => Some(Length::Percent(p)),
            [Part::Number { value: 0.0, .. }] => Some(Length::Px(0.0)),
            _ => Option::None,
        }
    }
    let (mut x, mut y) = (Length::Px(0.0), Length::Px(0.0));
    if name == "translate" {
        // `translate: x y z`: one to three parts.
        let parts: Vec<&str> = text.split_whitespace().collect();
        if parts.is_empty() || parts.len() > 3 || parts.iter().any(|p| p.contains('(')) && parts.len() > 1 {
            return Some(false);
        }
        let Some(a) = arg(parts[0], side) else {
            // `inherit`, `initial` and `unset`.
            return Some(match Parts::read(text, side).as_ref().map(Parts::as_slice) {
                Some([Part::Wide(w)]) => {
                    emit(*w, *w);
                    true
                }
                _ => false,
            });
        };
        x = a;
        for (i, p) in parts.iter().enumerate().skip(1) {
            match (arg(p, side), i) {
                (Some(l), 1) => y = l,
                (Some(_), _) => {}
                _ => return Some(false),
            }
        }
        emit(Value::Length(x), Value::Length(y));
        return Some(true);
    }
    if let Some(Part::Wide(w)) = Parts::read(text, side).as_ref().and_then(|p| p.as_slice().first().copied().filter(|_| p.len == 1)) {
        emit(w, w);
        return Some(true);
    }
    let mut rest = text;
    while !rest.is_empty() {
        let Some(open) = rest.find('(') else { return Some(false) };
        let func = rest[..open].trim().to_ascii_lowercase();
        // The matching `)`.
        let mut depth = 0;
        let mut close = None;
        for (i, c) in rest[open..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(open + i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(close) = close else { return Some(false) };
        let inner = &rest[open + 1..close];
        rest = rest[close + 1..].trim_start();
        // The arguments, cut at the commas that are not inside a function.
        let mut args = Vec::new();
        let (mut depth, mut from) = (0, 0);
        for (i, c) in inner.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                ',' if depth == 0 => {
                    args.push(&inner[from..i]);
                    from = i + 1;
                }
                _ => {}
            }
        }
        args.push(&inner[from..]);
        let (dx, dy) = match (func.as_str(), args.len()) {
            ("translatex", 1) => (arg(args[0], side), Some(Length::Px(0.0))),
            ("translatey", 1) => (Some(Length::Px(0.0)), arg(args[0], side)),
            ("translate", 1) => (arg(args[0], side), Some(Length::Px(0.0))),
            ("translate", 2) => (arg(args[0], side), arg(args[1], side)),
            // The z of `translate3d` must be a length and counts for nothing.
            ("translate3d", 3) if arg(args[2], side).is_some_and(|z| !matches!(z, Length::Percent(_))) => (arg(args[0], side), arg(args[1], side)),
            ("matrix" | "matrix3d" | "rotate" | "rotatex" | "rotatey" | "rotatez" | "rotate3d" | "scale" | "scalex" | "scaley" | "scalez" | "scale3d" | "skew" | "skewx"
            | "skewy" | "perspective", _) => (Some(Length::Px(0.0)), Some(Length::Px(0.0))),
            _ => return Some(false),
        };
        let (Some(dx), Some(dy)) = (dx, dy) else { return Some(false) };
        let (Some(nx), Some(ny)) = (add(x, dx), add(y, dy)) else { return Some(false) };
        (x, y) = (nx, ny);
    }
    emit(Value::Length(x), Value::Length(y));
    Some(true)
}

/// Add two lengths of the same unit. A zero `px` goes with any unit. `None` for a mix.
fn add(a: Length, b: Length) -> Option<Length> {
    use Length::*;
    match (a, b) {
        (Px(x), l) | (l, Px(x)) if x == 0.0 => Some(l),
        (Px(x), Px(y)) => Some(Px(x + y)),
        (Percent(x), Percent(y)) => Some(Percent(x + y)),
        (Em(x), Em(y)) => Some(Em(x + y)),
        (Rem(x), Rem(y)) => Some(Rem(x + y)),
        (Vw(x), Vw(y)) => Some(Vw(x + y)),
        (Vh(x), Vh(y)) => Some(Vh(x + y)),
        _ => Option::None,
    }
}

#[inline(always)]
fn parse_known(name: &str, value: &str, important: bool, side: &mut Side, out: &mut Vec<Declared>) -> bool {
    if var::has_var(value) {
        // A `var()` that is not written well (`var(--a`, `var(x)`) is not good now. We test it with any value for the names.
        if var::substitute(value, &mut |_| Some(String::new())).is_none() {
            return false;
        }
        // The value is not known until the custom properties are. A shorthand gives all its longhands the same value.
        let single = Property::from_name(name).map(|p| [p]);
        let Some(list) = all_longhands(name).or_else(|| logical(name)).or(single.as_ref().map(|p| &p[..])) else { return false };
        let mirror = mirrors(name);
        side.deferred.push(Deferred::Variable(VarDecl { name: name.into(), text: value.trim().into() }));
        let index = side.deferred.len() as u32 - 1;
        out.extend(list.iter().map(|&property| Declared { property, value: Value::Var(index), important, mirror }));
        return true;
    }
    if (name == "transform" || name == "translate") && let Some(ok) = transform_declaration(name, value, important, side, out) {
        return ok;
    }
    if let Some(ok) = crate::grid::declaration(name, value, important, side, out) {
        return ok;
    }
    if let Some(ok) = crate::motion::declaration(name, value, important, side, out) {
        return ok;
    }
    let Some(parts) = Parts::read(value, side) else { return false };
    let start = out.len();
    // `expand` says so when `name` is a logical property of the inline axis. It is the last arm there: a physical name never pays for it.
    let mirror = std::cell::Cell::new(false);
    let ok = expand(name, &parts, &mirror, &mut |property, value| out.push(Declared { property, value, important, mirror: mirror.get() }));
    if !ok {
        out.truncate(start);
        // A shorthand may have added some items before it found the mistake. Take them back.
    }
    ok
}

/// A typed deferred entry. Variable declarations and currentColor expressions share
/// one arena so ordinary Side values need no extra empty collection.
#[derive(Debug, Clone, PartialEq)]
pub enum Deferred {
    Variable(VarDecl),
    Color(crate::modern_color::Expression),
}

/// Indexed calculations, deferred declarations and color expressions, and grid lists.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Side {
    pub calcs: Vec<Linear>,
    pub deferred: Vec<Deferred>,
    pub grids: Vec<crate::grid::Grid>,
}

/// One piece of a value, after the tokens are read. A function like `rgb(...)` is one piece.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Part {
    Color(Color),
    ColorExpr(u32),
    Length(Length),
    Number { value: f32, int: bool },
    Percent(f32),
    Keyword(Keyword),
    /// `inherit`, `initial` or `unset`.
    Wide(Value),
    Slash,
    /// `url(...)`: the number of the address (`urls::intern`; 0 when it was not kept).
    Url(u32),
    /// An image that we do not draw: `linear-gradient(...)`, `image-set(...)`. It is `none` for the image properties.
    Gradient,
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
    /// The number of parts before the first comma of the value (all of them if there is none): the first layer of `a, b`.
    layer: usize,
}

impl Parts {
    /// `None` when the value has no part, or a part that is a mistake in every property (a `{` block, for example).
    #[inline(always)]
    fn read(value: &str, side: &mut Side) -> Option<Parts> {
        let mut parts = Parts { items: [Part::Other; 8], len: 0, more: false, layer: usize::MAX };
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
                Token::Function(name) if crate::modern_color::is_function(&name) => match crate::modern_color::read(&name, &mut tokens) {
                    None => Part::Bad,
                    Some(expr) if !expr.dependent() => Part::Color(expr.resolve(Color::BLACK)),
                    Some(expr) => { let i = side.deferred.len() as u32; side.deferred.push(Deferred::Color(expr)); Part::ColorExpr(i) }
                },
                Token::Function(name) if calc::is_calc(&name) || calc::is_math(&name) => read_calc(&name, &mut tokens, &mut side.calcs),
                Token::Function(name) => picture_function(&name, &mut tokens),
                Token::Url(address) => Part::Url(crate::urls::intern(address.trim())),
                Token::Comma => {
                    parts.layer = parts.layer.min(parts.len);
                    Part::Other
                }
                Token::OpenParen | Token::OpenSquare | Token::OpenCurly => {
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

    /// The parts of the first layer of a list: up to the first comma.
    fn first_layer(&self) -> &[Part] {
        &self.items[..self.layer.min(self.len)]
    }
}

/// After `calc(`: read to the matching `)` and compute. A number is a number. One unit is a plain length. A mix goes in `calcs`.
#[inline(never)]
fn read_calc(name: &str, tokens: &mut Tokens<'_>, calcs: &mut Vec<Linear>) -> Part {
    let mut inner = Vec::new();
    let mut depth = 1u32;
    while let Some(token) = tokens.next() {
        match token {
            Token::Function(_) | Token::OpenParen | Token::OpenSquare | Token::OpenCurly => depth += 1,
            Token::CloseParen | Token::CloseSquare | Token::CloseCurly => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        inner.push(token);
    }
    // A `calc(` that is not closed ends with the value: the standard closes it there.
    let result = if calc::is_calc(name) { calc::parse(&inner, length) } else { calc::parse_fn(name, &inner, length) };
    match result {
        Some(Calc::Math(node)) => {
            let at = calcs.len() as u32;
            calc::encode(&node, calcs);
            Part::Length(Length::Calc(at))
        }
        Some(Calc::Number(value)) => Part::Number { value, int: value.fract() == 0.0 },
        Some(Calc::Length(lin)) => match lin.single() {
            Some(Length::Percent(p)) => Part::Percent(p),
            Some(l) => Part::Length(l),
            None => {
                calcs.push(lin);
                Part::Length(Length::Calc(calcs.len() as u32 - 1))
            }
        },
        None => Part::Bad,
    }
}

/// After `url(`: `"address")`. A function with anything else inside is not an address that we know.
fn read_url(tokens: &mut Tokens<'_>) -> Part {
    let mut address = Part::Other;
    while let Some(token) = tokens.next() {
        match token {
            Token::Whitespace => {}
            Token::String(text) if matches!(address, Part::Other) => address = Part::Url(crate::urls::intern(text.trim())),
            Token::CloseParen => return address,
            _ => {
                skip_block(tokens);
                return Part::Other;
            }
        }
    }
    address
}

/// A function that no other property reads: `url(...)`, a gradient or `image-set(...)` are pictures, the others are skipped.
#[inline(never)]
fn picture_function(name: &str, tokens: &mut Tokens<'_>) -> Part {
    if name.eq_ignore_ascii_case("url") {
        return read_url(tokens);
    }
    skip_block(tokens);
    if is_gradient(name) { Part::Gradient } else { Part::Other }
}

/// `linear-gradient`, `radial-gradient`, `conic-gradient`, their `repeating-` forms, and `image-set`.
fn is_gradient(name: &str) -> bool {
    let name = name.strip_prefix("repeating-").unwrap_or(name);
    let b = name.as_bytes();
    name.eq_ignore_ascii_case("image-set") || b.len() > 8 && b[b.len() - 8..].eq_ignore_ascii_case(b"gradient")
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
    if let Some(keyword) = Keyword::from_name(lower) {
        return Part::Keyword(keyword);
    }
    Color::named(lower).map_or(Part::Other, Part::Color)
}

/// A number with a unit. `None` for a unit that we do not know.
pub(crate) fn length(value: f32, unit: &str) -> Option<Length> {
    // 96 px in an inch: this is fixed by the CSS standard, for every screen.
    let px = |per_unit: f32| Some(Length::Px(value * per_unit));
    let is = |name: &str| unit.eq_ignore_ascii_case(name);
    if is("px") {
        px(1.0)
    } else if is("em") {
        Some(Length::Em(value))
    } else if is("rem") {
        Some(Length::Rem(value))
    } else if is("vw") || is("dvw") || is("svw") || is("lvw") {
        Some(Length::Vw(value))
    } else if is("vh") || is("dvh") || is("svh") || is("lvh") {
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
    } else if is("ex") {
        // The x-height of Noto Sans Mono, the one font (`OS/2` `sxHeight` 536 of 1000).
        Some(Length::Em(value * 0.536))
    } else if is("ch") {
        Some(Length::Em(value * 0.5))
    } else {
        Option::None
    }
}

/// Give each longhand of the declaration `name` to `emit`. `false` if the name or the value is not good.
#[inline(always)]
fn expand(name: &str, parts: &Parts, mirror: &std::cell::Cell<bool>, emit: &mut dyn FnMut(Property, Value)) -> bool {
    use Property::*;
    let slice = parts.as_slice();
    // `inherit`, `initial` and `unset` are the whole value, for a longhand and for a shorthand.
    if let [Part::Wide(wide)] = slice {
        return match all_longhands(name) {
            Some(list) => {
                list.iter().for_each(|&p| emit(p, *wide));
                true
            }
            None => match Property::from_name(name) {
                Some(p) => {
                    emit(p, *wide);
                    true
                }
                None => logical(name).is_some_and(|list| {
                    mirror.set(mirrors(name));
                    list.iter().for_each(|&p| emit(p, *wide));
                    true
                }),
            },
        };
    }
    if parts.more && name != "font" {
        return false;
    }
    match name {
        "margin" => sides(slice, [MarginTop, MarginRight, MarginBottom, MarginLeft], emit),
        "inset" => sides(slice, [Top, Right, Bottom, Left], emit),
        "flex" => flex(slice, emit),
        "flex-flow" => {
            let (mut dir, mut wrap) = (Option::None, Option::None);
            for &part in slice {
                match (longhand(FlexDirection, part), longhand(FlexWrap, part)) {
                    (Some(v), _) if dir.is_none() => dir = Some(v),
                    (_, Some(v)) if wrap.is_none() => wrap = Some(v),
                    _ => return false,
                }
            }
            if slice.is_empty() || slice.len() > 2 {
                return false;
            }
            emit(FlexDirection, dir.unwrap_or(Value::Keyword(Keyword::Row)));
            emit(FlexWrap, wrap.unwrap_or(Value::Keyword(Keyword::Nowrap)));
            true
        }
        "gap" => match *slice {
            [r] | [r, _] => {
                let c = slice[slice.len() - 1];
                let (Some(r), Some(c)) = (longhand(RowGap, r), longhand(ColumnGap, c)) else { return false };
                emit(RowGap, r);
                emit(ColumnGap, c);
                true
            }
            _ => false,
        },
        // `border-spacing: h v`. With one value, both are the same.
        "border-spacing" => match *slice {
            [h] | [h, _] => {
                let v = slice[slice.len() - 1];
                let (Some(h), Some(v)) = (longhand(BorderSpacingH, h), longhand(BorderSpacingV, v)) else { return false };
                emit(BorderSpacingH, h);
                emit(BorderSpacingV, v);
                true
            }
            _ => false,
        },
        // `first baseline`, `last baseline`, `safe center`: two words for one value.
        "align-items" | "align-self" | "align-content" | "justify-content" | "justify-items" | "justify-self" if slice.len() == 2 => {
            let Some(property) = Property::from_name(name) else { return false };
            let value = match *slice {
                [Part::Keyword(Keyword::First), Part::Keyword(Keyword::Baseline)] => Part::Keyword(Keyword::Baseline),
                [Part::Keyword(Keyword::Last), Part::Keyword(Keyword::Baseline)] => Part::Keyword(Keyword::LastBaseline),
                [Part::Keyword(Keyword::Safe | Keyword::Unsafe), p @ Part::Keyword(Keyword::Center | Keyword::Start | Keyword::End | Keyword::FlexStart | Keyword::FlexEnd | Keyword::SelfStart | Keyword::SelfEnd | Keyword::Left | Keyword::Right)] => p,
                _ => return false,
            };
            longhand(property, value).map(|v| emit(property, v)).is_some()
        }
        "place-items" | "place-self" | "place-content" => {
            let list = crate::grid::longhands(name).unwrap();
            let (first, second) = match *slice {
                [a] => (a, a),
                [a, b] => (a, b),
                _ => return false,
            };
            let (Some(a), Some(b)) = (longhand(list[0], first), longhand(list[1], second)) else { return false };
            emit(list[0], a);
            emit(list[1], b);
            true
        }
        "padding" => sides(slice, [PaddingTop, PaddingRight, PaddingBottom, PaddingLeft], emit),
        "border-width" => sides(slice, [BorderTopWidth, BorderRightWidth, BorderBottomWidth, BorderLeftWidth], emit),
        "border-style" => sides(slice, [BorderTopStyle, BorderRightStyle, BorderBottomStyle, BorderLeftStyle], emit),
        "border-color" => sides(slice, [BorderTopColor, BorderRightColor, BorderBottomColor, BorderLeftColor], emit),
        "border" => border(slice, &[(BorderTopWidth, BorderTopStyle, BorderTopColor), (BorderRightWidth, BorderRightStyle, BorderRightColor), (BorderBottomWidth, BorderBottomStyle, BorderBottomColor), (BorderLeftWidth, BorderLeftStyle, BorderLeftColor)], emit),
        "border-top" => border(slice, &[(BorderTopWidth, BorderTopStyle, BorderTopColor)], emit),
        "border-right" => border(slice, &[(BorderRightWidth, BorderRightStyle, BorderRightColor)], emit),
        "border-bottom" => border(slice, &[(BorderBottomWidth, BorderBottomStyle, BorderBottomColor)], emit),
        "border-left" => border(slice, &[(BorderLeftWidth, BorderLeftStyle, BorderLeftColor)], emit),
        "border-inline-start" => {
            mirror.set(true);
            border(slice, &[(BorderLeftWidth, BorderLeftStyle, BorderLeftColor)], emit)
        }
        "border-inline-end" => {
            mirror.set(true);
            border(slice, &[(BorderRightWidth, BorderRightStyle, BorderRightColor)], emit)
        }
        "border-block-start" => border(slice, &[(BorderTopWidth, BorderTopStyle, BorderTopColor)], emit),
        "border-block-end" => border(slice, &[(BorderBottomWidth, BorderBottomStyle, BorderBottomColor)], emit),
        "border-inline" => {
            mirror.set(true);
            border(slice, &[(BorderLeftWidth, BorderLeftStyle, BorderLeftColor), (BorderRightWidth, BorderRightStyle, BorderRightColor)], emit)
        }
        "border-block" => border(slice, &[(BorderTopWidth, BorderTopStyle, BorderTopColor), (BorderBottomWidth, BorderBottomStyle, BorderBottomColor)], emit),
        // The vertical radii after a `/` are dropped: a corner is a circle here.
        "border-radius" => {
            let end = slice.iter().position(|p| matches!(p, Part::Slash)).unwrap_or(slice.len());
            sides(&slice[..end], [BorderTopLeftRadius, BorderTopRightRadius, BorderBottomRightRadius, BorderBottomLeftRadius], emit)
        }
        // `auto`, `<w> / <h>` or both words. The value is `auto` or the ratio as a number. A ratio with a 0 is `auto` (CSS Sizing 4).
        "aspect-ratio" => {
            let ratio = match *slice {
                [Part::Keyword(Keyword::Auto), ref rest @ ..] | [ref rest @ .., Part::Keyword(Keyword::Auto)] if !rest.is_empty() => rest,
                ref all => all,
            };
            let value = match *ratio {
                [Part::Keyword(Keyword::Auto)] => 0.0,
                [Part::Number { value: w, .. }] if w >= 0.0 => w,
                [Part::Number { value: w, .. }, Part::Slash, Part::Number { value: h, .. }] if w >= 0.0 && h >= 0.0 => if h == 0.0 { 0.0 } else { w / h },
                _ => return false,
            };
            emit(AspectRatio, if value > 0.0 && value.is_finite() { Value::Number(value) } else { Value::Keyword(Keyword::Auto) });
            true
        }
        "font" => font(slice, emit),
        "list-style" => match slice {
            [one] => longhand(ListStyleType, *one).map(|v| emit(ListStyleType, v)).is_some(),
            _ => false,
        },
        "text-decoration" => text_decoration(slice, emit),
        // `overflow: x y`. With one value, both are the same.
        "overflow" => match *slice {
            [x] | [x, _] => {
                let y = slice[slice.len() - 1];
                let (Some(x), Some(y)) = (longhand(OverflowX, x), longhand(OverflowY, y)) else { return false };
                emit(OverflowX, x);
                emit(OverflowY, y);
                true
            }
            _ => false,
        },
        _ => {
            // A physical name goes first: it pays nothing for the table of logical names.
            if let Some(property) = Property::from_core_name(name) {
                let [one] = slice else { return false };
                return longhand(property, *one).map(|v| emit(property, v)).is_some();
            }
            // The picture names are not core properties: they are looked up after this miss, in a cold function.
            if let Some(ok) = picture_expand(name, parts, emit) {
                return ok;
            }
            let Some(list) = logical(name) else { return false };
            mirror.set(mirrors(name));
            match (list, slice) {
                (&[p], &[one]) => longhand(p, one).map(|v| emit(p, v)).is_some(),
                // `margin-inline: a b`: the start and the end. One value is for both.
                (&[a, b], &[x] | &[x, _]) => {
                    let y = slice[slice.len() - 1];
                    let (Some(x), Some(y)) = (longhand(a, x), longhand(b, y)) else { return false };
                    emit(a, x);
                    emit(b, y);
                    true
                }
                _ => false,
            }
        }
    }
}

/// The physical longhands of a logical name, for a horizontal writing mode and `ltr`: the inline axis is left to right, the block axis top to bottom.
/// `None` if `name` is not a logical property. The cascade swaps left and right in an `rtl` element (`mirrors`). Other writing modes are not built.
fn logical(name: &str) -> Option<&'static [Property]> {
    use Property::*;
    Some(match name {
        "margin-inline" => &[MarginLeft, MarginRight],
        "margin-inline-start" => &[MarginLeft],
        "margin-inline-end" => &[MarginRight],
        "margin-block" => &[MarginTop, MarginBottom],
        "margin-block-start" => &[MarginTop],
        "margin-block-end" => &[MarginBottom],
        "padding-inline" => &[PaddingLeft, PaddingRight],
        "padding-inline-start" => &[PaddingLeft],
        "padding-inline-end" => &[PaddingRight],
        "padding-block" => &[PaddingTop, PaddingBottom],
        "padding-block-start" => &[PaddingTop],
        "padding-block-end" => &[PaddingBottom],
        "inset-inline" => &[Left, Right],
        "inset-inline-start" => &[Left],
        "inset-inline-end" => &[Right],
        "inset-block" => &[Top, Bottom],
        "inset-block-start" => &[Top],
        "inset-block-end" => &[Bottom],
        "border-inline-width" => &[BorderLeftWidth, BorderRightWidth],
        "border-inline-start-width" => &[BorderLeftWidth],
        "border-inline-end-width" => &[BorderRightWidth],
        "border-block-width" => &[BorderTopWidth, BorderBottomWidth],
        "border-block-start-width" => &[BorderTopWidth],
        "border-block-end-width" => &[BorderBottomWidth],
        "border-inline-style" => &[BorderLeftStyle, BorderRightStyle],
        "border-inline-start-style" => &[BorderLeftStyle],
        "border-inline-end-style" => &[BorderRightStyle],
        "border-block-style" => &[BorderTopStyle, BorderBottomStyle],
        "border-block-start-style" => &[BorderTopStyle],
        "border-block-end-style" => &[BorderBottomStyle],
        "border-inline-color" => &[BorderLeftColor, BorderRightColor],
        "border-inline-start-color" => &[BorderLeftColor],
        "border-inline-end-color" => &[BorderRightColor],
        "border-block-color" => &[BorderTopColor, BorderBottomColor],
        "border-block-start-color" => &[BorderTopColor],
        "border-block-end-color" => &[BorderBottomColor],
        "border-inline" => &[BorderLeftWidth, BorderRightWidth, BorderLeftStyle, BorderRightStyle, BorderLeftColor, BorderRightColor],
        "border-inline-start" => &[BorderLeftWidth, BorderLeftStyle, BorderLeftColor],
        "border-inline-end" => &[BorderRightWidth, BorderRightStyle, BorderRightColor],
        "border-block" => &[BorderTopWidth, BorderBottomWidth, BorderTopStyle, BorderBottomStyle, BorderTopColor, BorderBottomColor],
        "border-block-start" => &[BorderTopWidth, BorderTopStyle, BorderTopColor],
        "border-block-end" => &[BorderBottomWidth, BorderBottomStyle, BorderBottomColor],
        "border-start-start-radius" => &[BorderTopLeftRadius],
        "border-start-end-radius" => &[BorderTopRightRadius],
        "border-end-start-radius" => &[BorderBottomLeftRadius],
        "border-end-end-radius" => &[BorderBottomRightRadius],
        "inline-size" => &[Width],
        "block-size" => &[Height],
        "min-inline-size" => &[MinWidth],
        "max-inline-size" => &[MaxWidth],
        "min-block-size" => &[MinHeight],
        "max-block-size" => &[MaxHeight],
        _ => return None,
    })
}

/// `expand` for the picture properties and the shorthands of `background` and `mask`. `None` for any other name. Only a name that is not a
/// core property comes here (`expand`), so a declaration of the other properties pays for none of this. A value with a comma is a list of
/// layers: the first layer is read (the color of `background` is the one of the last layer).
#[cold]
#[inline(never)]
fn picture_expand(name: &str, parts: &Parts, emit: &mut dyn FnMut(Property, Value)) -> Option<bool> {
    use Property::*;
    if !matches!(name.as_bytes().first(), Some(b'b' | b'm')) {
        return None;
    }
    let slice = parts.first_layer();
    Some(match name {
        "background" => layer_shorthand(slice, &parts.as_slice()[slice.len()..], &BACKGROUND, emit),
        "mask" => layer_shorthand(slice, &[], &MASK, emit),
        "background-size" => size(slice, [BackgroundSizeX, BackgroundSizeY], emit),
        "mask-size" => size(slice, [MaskSizeX, MaskSizeY], emit),
        "background-position" => position(slice, [BackgroundPositionX, BackgroundPositionY], emit),
        "mask-position" => position(slice, [MaskPositionX, MaskPositionY], emit),
        "background-repeat" => repeat(slice, BackgroundRepeat, emit),
        "mask-repeat" => repeat(slice, MaskRepeat, emit),
        "background-image" | "mask-image" | "background-position-x" | "background-position-y" | "mask-position-x" | "mask-position-y" => {
            let ([one], Some(p)) = (slice, Property::from_name(name)) else { return Some(false) };
            return Some(longhand(p, *one).map(|v| emit(p, v)).is_some());
        }
        _ => return None,
    })
}

/// Does the logical `name` follow the inline axis, so that the cascade swaps its left and right in an `rtl` element? The sizes and the block axis do not.
fn mirrors(name: &str) -> bool {
    (name.contains("-inline") && !name.ends_with("-size")) || name.starts_with("border-start-") || name.starts_with("border-end-")
}

/// The property on the other side: left for right, `border-top-left-radius` for `border-top-right-radius`. The others stay.
pub fn mirror(p: Property) -> Property {
    use Property::*;
    match p {
        MarginLeft => MarginRight,
        MarginRight => MarginLeft,
        PaddingLeft => PaddingRight,
        PaddingRight => PaddingLeft,
        Left => Right,
        Right => Left,
        BorderLeftWidth => BorderRightWidth,
        BorderRightWidth => BorderLeftWidth,
        BorderLeftStyle => BorderRightStyle,
        BorderRightStyle => BorderLeftStyle,
        BorderLeftColor => BorderRightColor,
        BorderRightColor => BorderLeftColor,
        BorderTopLeftRadius => BorderTopRightRadius,
        BorderTopRightRadius => BorderTopLeftRadius,
        BorderBottomLeftRadius => BorderBottomRightRadius,
        BorderBottomRightRadius => BorderBottomLeftRadius,
        other => other,
    }
}

/// The shorthands that `parse_declaration` splits into longhands: every name for which `shorthand_longhands` answers.
pub const SHORTHANDS: &[&str] = &[
    "margin", "padding", "inset", "flex", "flex-flow", "gap", "border-spacing", "border-width", "border-style", "border-color",
    "border", "border-top", "border-right", "border-bottom", "border-left", "background", "font", "list-style", "text-decoration",
    "overflow", "border-radius", "grid-row", "grid-column", "grid-area", "place-items", "place-self", "place-content", "transition", "animation",
];

/// The longhands of a shorthand, for `inherit` and the like. `None` if `name` is not a shorthand.
/// `shorthand_longhands`, and for the shorthands of the background and mask properties all the longhands that the parser sets. (The object
/// model lists only `background-color` for `background`: see `Property::LISTED`.)
pub fn all_longhands(name: &str) -> Option<&'static [Property]> {
    use Property::*;
    Some(match name {
        "background" => &[BackgroundColor, BackgroundImage, BackgroundSizeX, BackgroundSizeY, BackgroundPositionX, BackgroundPositionY, BackgroundRepeat],
        "mask" => &[MaskImage, MaskSizeX, MaskSizeY, MaskPositionX, MaskPositionY, MaskRepeat],
        "background-size" => &[BackgroundSizeX, BackgroundSizeY],
        "mask-size" => &[MaskSizeX, MaskSizeY],
        "background-position" => &[BackgroundPositionX, BackgroundPositionY],
        "mask-position" => &[MaskPositionX, MaskPositionY],
        "transform" | "translate" => &[TranslateX, TranslateY],
        _ => return shorthand_longhands(name),
    })
}

pub fn shorthand_longhands(name: &str) -> Option<&'static [Property]> {
    use Property::*;
    let list: &'static [Property] = match name {
        "margin" => &[MarginTop, MarginRight, MarginBottom, MarginLeft],
        "padding" => &[PaddingTop, PaddingRight, PaddingBottom, PaddingLeft],
        "inset" => &[Top, Right, Bottom, Left],
        "flex" => &[FlexGrow, FlexShrink, FlexBasis],
        "flex-flow" => &[FlexDirection, FlexWrap],
        "gap" => &[RowGap, ColumnGap],
        "border-spacing" => &[BorderSpacingH, BorderSpacingV],
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
        "overflow" => &[OverflowX, OverflowY],
        "border-radius" => &[BorderTopLeftRadius, BorderTopRightRadius, BorderBottomRightRadius, BorderBottomLeftRadius],
        _ => return crate::grid::longhands(name).or_else(|| crate::motion::longhands(name)),
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

/// The longhands of a shorthand with layers: `background` and `mask`.
struct Layer {
    image: Property,
    size: [Property; 2],
    position: [Property; 2],
    repeat: Property,
    /// `background` has a color, and `mask` does not.
    color: Option<Property>,
}

const BACKGROUND: Layer = Layer {
    image: Property::BackgroundImage,
    size: [Property::BackgroundSizeX, Property::BackgroundSizeY],
    position: [Property::BackgroundPositionX, Property::BackgroundPositionY],
    repeat: Property::BackgroundRepeat,
    color: Some(Property::BackgroundColor),
};

const MASK: Layer = Layer {
    image: Property::MaskImage,
    size: [Property::MaskSizeX, Property::MaskSizeY],
    position: [Property::MaskPositionX, Property::MaskPositionY],
    repeat: Property::MaskRepeat,
    color: None,
};

/// The image of a layer: a `url()` is its number, `none` and an image that we do not draw (a gradient) are `none`.
fn image(part: Part) -> Option<Value> {
    match part {
        Part::Keyword(Keyword::None) | Part::Gradient | Part::Url(0) => Some(Value::Keyword(Keyword::None)),
        Part::Url(id) => Some(Value::Number(id as f32)),
        _ => Option::None,
    }
}

/// A length or percent that is not negative (`0` is a length).
fn non_negative(part: Part) -> Option<Length> {
    match part {
        Part::Length(Length::Px(v) | Length::Em(v) | Length::Rem(v) | Length::Vw(v) | Length::Vh(v) | Length::Vmin(v) | Length::Vmax(v)) if v < 0.0 => Option::None,
        Part::Length(l) => Some(l),
        Part::Percent(p) if p >= 0.0 => Some(Length::Percent(p)),
        Part::Number { value: 0.0, .. } => Some(Length::Px(0.0)),
        _ => Option::None,
    }
}

/// `auto`, `cover`, `contain`, or one or two lengths. The values are the width and the height: `cover` and `contain` are in the width.
fn size_values(slice: &[Part]) -> Option<[Value; 2]> {
    let one = |p: Part| match p {
        Part::Keyword(Keyword::Auto) => Some(Value::Keyword(Keyword::Auto)),
        p => non_negative(p).map(Value::Length),
    };
    let auto = Value::Keyword(Keyword::Auto);
    match *slice {
        [Part::Keyword(k @ (Keyword::Cover | Keyword::Contain))] => Some([Value::Keyword(k), auto]),
        [x] => Some([one(x)?, auto]),
        [x, y] => Some([one(x)?, one(y)?]),
        _ => Option::None,
    }
}

fn size(slice: &[Part], to: [Property; 2], emit: &mut dyn FnMut(Property, Value)) -> bool {
    let Some([x, y]) = size_values(slice) else { return false };
    emit(to[0], x);
    emit(to[1], y);
    true
}

/// A position as a percent or a length: `left` is 0%, `center` is 50%, `right` is 100%. Edge offsets (`right 10px`) are not read.
/// Gives the value, and which side it can be: 1 horizontal only, 2 vertical only, 3 either.
fn place(part: Part) -> Option<(Value, u8)> {
    let pct = |p| Value::Length(Length::Percent(p));
    match part {
        Part::Keyword(Keyword::Left) => Some((pct(0.0), 1)),
        Part::Keyword(Keyword::Right) => Some((pct(100.0), 1)),
        Part::Keyword(Keyword::Top) => Some((pct(0.0), 2)),
        Part::Keyword(Keyword::Bottom) => Some((pct(100.0), 2)),
        Part::Keyword(Keyword::Center) => Some((pct(50.0), 3)),
        Part::Length(l) => Some((Value::Length(l), 3)),
        Part::Percent(p) => Some((pct(p), 3)),
        Part::Number { value: 0.0, .. } => Some((Value::Length(Length::Px(0.0)), 3)),
        _ => Option::None,
    }
}

/// One or two places, in any order when they are words (`top left`, `center right`): the horizontal and the vertical one.
fn position_values(slice: &[Part]) -> Option<[Value; 2]> {
    let half = Value::Length(Length::Percent(50.0));
    match *slice {
        [a] => match place(a)? {
            (v, 2) => Some([half, v]),
            (v, _) => Some([v, half]),
        },
        [a, b] => {
            let ((x, xs), (y, ys)) = (place(a)?, place(b)?);
            match (xs, ys) {
                (1 | 3, 2 | 3) => Some([x, y]),
                (2 | 3, 1 | 3) => Some([y, x]),
                _ => Option::None,
            }
        }
        _ => Option::None,
    }
}

fn position(slice: &[Part], to: [Property; 2], emit: &mut dyn FnMut(Property, Value)) -> bool {
    let Some([x, y]) = position_values(slice) else { return false };
    emit(to[0], x);
    emit(to[1], y);
    true
}

/// `repeat`, `no-repeat`, `repeat-x`, `repeat-y`, or two of `repeat` and `no-repeat`. (`space` and `round` are not read.)
fn repeat_value(slice: &[Part]) -> Option<Value> {
    use Keyword as K;
    let k = match *slice {
        [Part::Keyword(k @ (K::Repeat | K::NoRepeat | K::RepeatX | K::RepeatY))] => k,
        [Part::Keyword(x @ (K::Repeat | K::NoRepeat)), Part::Keyword(y @ (K::Repeat | K::NoRepeat))] => match (x, y) {
            (K::Repeat, K::NoRepeat) => K::RepeatX,
            (K::NoRepeat, K::Repeat) => K::RepeatY,
            (k, _) => k,
        },
        _ => return Option::None,
    };
    Some(Value::Keyword(k))
}

fn repeat(slice: &[Part], to: Property, emit: &mut dyn FnMut(Property, Value)) -> bool {
    repeat_value(slice).map(|v| emit(to, v)).is_some()
}

/// `background: ...` and `mask: ...`: an image, a position with a size after a `/`, a repeat, and for `background` a color. The words that
/// we do not use (the attachment, the boxes, the mask mode and compositing) are checked a little and ignored.
/// The check: no bad part, no word of another property (`bold`), no number with no unit (but `0`).
/// This is not the whole grammar, and so some wrong values pass. We checked 2,600 values against Chrome: see `docs/progress.md`.
/// All the longhands are set, so that `background: red` takes an image, a size, a position and a repeat away.
/// `slice` is the first layer. `later` is what follows it (`, ...`): the color of `background` is the one of the last layer.
fn layer_shorthand(slice: &[Part], later: &[Part], l: &Layer, emit: &mut dyn FnMut(Property, Value)) -> bool {
    use Keyword as K;
    let mut color = l.color.map(|_| Value::Color(Color::TRANSPARENT));
    let (mut img, mut rep, mut rep_len) = (Value::Keyword(K::None), [Part::Other; 2], 0);
    let (mut at, mut at_len, mut by, mut by_len, mut slash) = ([Part::Other; 4], 0, [Part::Other; 2], 0, false);
    for &part in slice {
        match part {
            Part::Color(c) if color.is_some() => color = Some(Value::Color(c)),
            Part::ColorExpr(i) if color.is_some() => color = Some(Value::ColorExpr(i)),
            Part::Keyword(K::CurrentColor) if color.is_some() => color = Some(Value::Keyword(K::CurrentColor)),
            Part::Keyword(K::None) | Part::Gradient | Part::Url(_) => img = image(part).unwrap_or(img),
            Part::Keyword(K::Repeat | K::NoRepeat | K::RepeatX | K::RepeatY) => {
                if rep_len == 2 {
                    return false;
                }
                rep[rep_len] = part;
                rep_len += 1;
            }
            Part::Keyword(K::Scroll | K::Fixed | K::ContentBox | K::BorderBox) | Part::Other => {}
            Part::Slash if !slash && at_len > 0 => slash = true,
            Part::Keyword(K::Auto | K::Cover | K::Contain) | Part::Length(_) | Part::Percent(_) | Part::Number { value: 0.0, .. } if slash => {
                if by_len == 2 {
                    return false;
                }
                by[by_len] = part;
                by_len += 1;
            }
            Part::Keyword(K::Center | K::Left | K::Right | K::Top | K::Bottom) | Part::Length(_) | Part::Percent(_) | Part::Number { value: 0.0, .. } if !slash => {
                if at_len == 4 {
                    return false;
                }
                at[at_len] = part;
                at_len += 1;
            }
            _ => return false,
        }
    }
    for &part in later {
        match part {
            Part::Color(c) if color.is_some() => color = Some(Value::Color(c)),
            Part::ColorExpr(i) if color.is_some() => color = Some(Value::ColorExpr(i)),
            Part::Keyword(K::CurrentColor) if color.is_some() => color = Some(Value::Keyword(K::CurrentColor)),
            _ => {}
        }
    }
    let (pos, sz) = (position_values(&at[..at_len]), size_values(&by[..by_len]));
    if (at_len > 0 && pos.is_none()) || (by_len > 0 && sz.is_none()) || (slash && by_len == 0) || (rep_len > 0 && repeat_value(&rep[..rep_len]).is_none()) {
        return false;
    }
    if let (Some(p), Some(c)) = (l.color, color) {
        emit(p, c);
    }
    emit(l.image, img);
    {
        let [px, py] = pos.unwrap_or([Value::Length(Length::Percent(0.0)); 2]);
        let [sx, sy] = sz.unwrap_or([Value::Keyword(K::Auto); 2]);
        emit(l.position[0], px);
        emit(l.position[1], py);
        emit(l.size[0], sx);
        emit(l.size[1], sy);
        emit(l.repeat, repeat_value(&rep[..rep_len]).unwrap_or(Value::Keyword(K::Repeat)));
    }
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
            | Part::ColorExpr(_)
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

/// `flex: none | auto | <grow> [<shrink>] || <basis>`. A number alone sets the basis to 0, not `auto` (CSS Flexbox 1, 7.1).
fn flex(slice: &[Part], emit: &mut dyn FnMut(Property, Value)) -> bool {
    use Keyword as K;
    use Property::{FlexBasis, FlexGrow, FlexShrink};
    let n = Value::Number;
    let (grow, shrink, basis) = match *slice {
        [Part::Keyword(K::None)] => (n(0.0), n(0.0), Value::Keyword(K::Auto)),
        [Part::Keyword(K::Auto)] => (n(1.0), n(1.0), Value::Keyword(K::Auto)),
        _ => {
            let (mut numbers, mut basis) = (Vec::new(), Option::None);
            let mut last_number = false;
            for &part in slice {
                match part {
                    // The two numbers go together: `flex: 1 10px 2` is not valid. A 0 is a number here, unless both
                    // numbers are there already: then it is the basis.
                    Part::Number { value, .. } if value >= 0.0 && numbers.len() < 2 && (numbers.is_empty() || last_number) => {
                        numbers.push(value);
                        last_number = true;
                        continue;
                    }
                    _ if basis.is_none() => {
                        let Some(v) = longhand(FlexBasis, part) else { return false };
                        basis = Some(v);
                    }
                    _ => return false,
                }
                last_number = false;
            }
            if numbers.is_empty() && basis.is_none() {
                return false;
            }
            let grow = numbers.first().copied().unwrap_or(1.0);
            let shrink = numbers.get(1).copied().unwrap_or(1.0);
            (n(grow), n(shrink), basis.unwrap_or(Value::Length(Length::Percent(0.0))))
        }
    };
    emit(FlexGrow, grow);
    emit(FlexShrink, shrink);
    emit(FlexBasis, basis);
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
    if i >= slice.len() || slice[i..].iter().any(|p| !matches!(p, Part::Keyword(_) | Part::Color(_) | Part::ColorExpr(_) | Part::Other)) {
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
            Length::Calc(_) => true,
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
            K::Grid, K::InlineGrid, K::FlowRoot, K::InlineTable,
        ]),
        Float => one_of(part, &[K::None, K::Left, K::Right, K::InlineStart, K::InlineEnd]),
        Clear => one_of(part, &[K::None, K::Left, K::Right, K::Both]),
        Visibility => one_of(part, &[K::Visible, K::Hidden, K::Collapse]),
        Direction => one_of(part, &[K::Ltr, K::Rtl]),
        WhiteSpace => one_of(part, &[K::Normal, K::Nowrap, K::Pre, K::PreWrap, K::PreLine, K::BreakSpaces]),
        TextAlign => one_of(part, &[K::Left, K::Right, K::Center, K::Justify, K::Start, K::End]),
        TextDecoration => one_of(part, &[K::None, K::Underline, K::Overline, K::LineThrough]),
        TextTransform => one_of(part, &[K::None, K::Uppercase, K::Lowercase, K::Capitalize]),
        ListStyleType => one_of(part, &[K::None, K::Disc, K::Circle, K::Square, K::Decimal]),
        BoxSizing => one_of(part, &[K::ContentBox, K::BorderBox]),
        FontStyle => one_of(part, &[K::Normal, K::Italic, K::Oblique]),
        FlexDirection => one_of(part, &[K::Row, K::RowReverse, K::Column, K::ColumnReverse]),
        FlexWrap => one_of(part, &[K::Nowrap, K::Wrap, K::WrapReverse]),
        FlexGrow | FlexShrink => match part {
            Part::Number { value, .. } if value >= 0.0 => Some(Value::Number(value)),
            _ => Option::None,
        },
        FlexBasis => match part {
            Part::Keyword(k @ (K::Auto | K::Content)) => Some(Value::Keyword(k)),
            _ => positive(part).map(Value::Length),
        },
        Order => match part {
            Part::Number { value, int: true } => Some(Value::Number(value)),
            _ => Option::None,
        },
        JustifyContent => one_of(part, &[K::Normal, K::FlexStart, K::FlexEnd, K::Center, K::SpaceBetween, K::SpaceAround, K::SpaceEvenly, K::Start, K::End, K::Left, K::Right, K::Stretch]),
        AlignContent => one_of(part, &[K::Normal, K::FlexStart, K::FlexEnd, K::Center, K::SpaceBetween, K::SpaceAround, K::SpaceEvenly, K::Start, K::End, K::Stretch, K::Baseline, K::LastBaseline]),
        AlignItems => one_of(part, &[K::Normal, K::Stretch, K::FlexStart, K::FlexEnd, K::Center, K::Baseline, K::LastBaseline, K::Start, K::End, K::SelfStart, K::SelfEnd]),
        AlignSelf => one_of(part, &[K::Auto, K::Normal, K::Stretch, K::FlexStart, K::FlexEnd, K::Center, K::Baseline, K::LastBaseline, K::Start, K::End, K::SelfStart, K::SelfEnd]),
        JustifyItems => one_of(part, &[K::Normal, K::Stretch, K::FlexStart, K::FlexEnd, K::Center, K::Baseline, K::LastBaseline, K::Start, K::End, K::SelfStart, K::SelfEnd, K::Left, K::Right]),
        JustifySelf => one_of(part, &[K::Auto, K::Normal, K::Stretch, K::FlexStart, K::FlexEnd, K::Center, K::Baseline, K::LastBaseline, K::Start, K::End, K::SelfStart, K::SelfEnd, K::Left, K::Right]),
        ObjectFit => one_of(part, &[K::Fill, K::Contain, K::Cover, K::None, K::ScaleDown]),
        // A ratio has more than one part: `expand` reads it.
        AspectRatio => Option::None,
        // The paint of an SVG shape: a color or `none`. A paint server (`url(#a)`) is not built, so it is not good.
        Fill | Stroke => match part {
            Part::Color(c) => Some(Value::Color(c)),
            Part::ColorExpr(i) => Some(Value::ColorExpr(i)),
            Part::Keyword(k @ (K::None | K::CurrentColor)) => Some(Value::Keyword(k)),
            _ => Option::None,
        },
        BackgroundImage | MaskImage => image(part),
        BackgroundSizeX | MaskSizeX => size_values(&[part]).map(|v| v[0]),
        BackgroundSizeY | MaskSizeY => match part {
            Part::Keyword(K::Auto) => Some(Value::Keyword(K::Auto)),
            _ => non_negative(part).map(Value::Length),
        },
        BackgroundPositionX | MaskPositionX => place(part).filter(|p| p.1 & 1 != 0).map(|p| p.0),
        BackgroundPositionY | MaskPositionY => place(part).filter(|p| p.1 & 2 != 0).map(|p| p.0),
        BackgroundRepeat | MaskRepeat => one_of(part, &[K::Repeat, K::NoRepeat, K::RepeatX, K::RepeatY]),
        // These are read as text (`grid.rs`), and never come here.
        GridTemplateColumns | GridTemplateRows | GridTemplateAreas | GridAutoColumns | GridAutoRows | GridAutoFlow | GridRowStart | GridRowEnd | GridColumnStart | GridColumnEnd => Option::None,
        // Read as text (`motion.rs`).
        TransitionProperty | TransitionDuration | TransitionDelay | TransitionTimingFunction | AnimationName | AnimationDuration | AnimationDelay | AnimationIterationCount => Option::None,
        RowGap | ColumnGap => match part {
            Part::Keyword(K::Normal) => Some(Value::Keyword(K::Normal)),
            _ => positive(part).map(Value::Length),
        },
        BorderCollapse => one_of(part, &[K::Separate, K::Collapse]),
        BorderSpacingH | BorderSpacingV => match part {
            Part::Percent(_) => Option::None,
            _ => positive(part).map(Value::Length),
        },
        TableLayout => one_of(part, &[K::Auto, K::Fixed]),
        CaptionSide => one_of(part, &[K::Top, K::Bottom]),
        // A table cell reads `baseline`, `top`, `middle` and `bottom` (the others act as `baseline`). An inline box and an atomic inline read all of them (`layout/src/inline.rs`).
        VerticalAlign => match part {
            Part::Keyword(k @ (K::Baseline | K::Top | K::Middle | K::Bottom | K::Sub | K::Super | K::TextTop | K::TextBottom)) => Some(Value::Keyword(k)),
            _ => length(part).map(Value::Length),
        },
        Position => one_of(part, &[K::Static, K::Relative, K::Absolute, K::Fixed, K::Sticky]),
        ZIndex => match part {
            Part::Keyword(K::Auto) => Some(Value::Keyword(K::Auto)),
            Part::Number { value, int: true } => Some(Value::Number(value.clamp(-2_147_483_648.0, 2_147_483_647.0))),
            _ => Option::None,
        },
        // A percent is the same as the number (`50%` is 0.5). Over 1 is 1, and under 0 is 0.
        Opacity => match part {
            Part::Number { value, .. } => Some(Value::Number(value.clamp(0.0, 1.0))),
            Part::Percent(p) => Some(Value::Number((p / 100.0).clamp(0.0, 1.0))),
            _ => Option::None,
        },
        // Set by `transform_declaration`; a plain value is a length or a percent.
        TranslateX | TranslateY => length(part).map(Value::Length),
        BorderTopLeftRadius | BorderTopRightRadius | BorderBottomRightRadius | BorderBottomLeftRadius => positive(part).map(Value::Length),
        Appearance => one_of(part, &[K::None, K::Auto, K::Button, K::Checkbox, K::Radio, K::Textfield, K::Textarea, K::Menulist]),
        Cursor => one_of(part, &[K::Auto, K::Default, K::Pointer]),
        UserSelect => one_of(part, &[K::Auto, K::Text, K::None, K::Contain, K::All]),
        PointerEvents => one_of(part, &[K::Auto, K::None, K::Visible, K::VisiblePainted, K::VisibleFill, K::VisibleStroke, K::Painted, K::Fill, K::Stroke, K::All]),
        Top | Right | Bottom | Left => match part {
            Part::Keyword(K::Auto) => Some(Value::Keyword(K::Auto)),
            _ => length(part).map(Value::Length),
        },
        OverflowX | OverflowY => one_of(part, &[K::Visible, K::Hidden, K::Clip, K::Scroll, K::Auto]),
        TextColor | BackgroundColor | BorderTopColor | BorderRightColor | BorderBottomColor | BorderLeftColor => match part {
            Part::Color(c) => Some(Value::Color(c)),
            Part::ColorExpr(i) => Some(Value::ColorExpr(i)),
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
        // `normal` is no extra space. A length may be negative. A percent is not valid.
        LetterSpacing | WordSpacing => match part {
            Part::Keyword(K::Normal) => Some(Value::Keyword(K::Normal)),
            Part::Percent(_) => Option::None,
            _ => length(part).map(Value::Length),
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

    #[test]
    fn object_fit_keywords() {
        for value in ["fill", "contain", "cover", "none", "scale-down"] {
            assert_eq!(parse("object-fit", value).len(), 1, "{value}");
        }
    }

    /// `SHORTHANDS` is the list of every shorthand name, and no longhand is in it.
    #[test]
    fn shorthand_list_matches_longhands() {
        for n in SHORTHANDS {
            assert!(shorthand_longhands(n).is_some_and(|l| !l.is_empty()), "{n}");
            assert!(Property::from_name(n).is_none(), "{n}");
        }
        for p in Property::ALL {
            assert!(shorthand_longhands(p.name()).is_none(), "{}", p.name());
        }
    }

    fn parse(name: &str, value: &str) -> Vec<(Property, Value)> {
        let mut out = Vec::new();
        if parse_declaration(name, value, false, &mut Side::default(), &mut out) {
            out.into_iter().map(|d| (d.property, d.value)).collect()
        } else {
            assert!(out.is_empty(), "{name}: {value} added items and still failed");
            vec![]
        }
    }

    #[test]
    fn transform_translate() {
        let px = |x| Value::Length(Length::Px(x));
        let pct = |x| Value::Length(Length::Percent(x));
        assert_eq!(parse("transform", "translateX(-100%)"), vec![(TranslateX, pct(-100.0)), (TranslateY, px(0.0))]);
        assert_eq!(parse("transform", "translateY(12px)"), vec![(TranslateX, px(0.0)), (TranslateY, px(12.0))]);
        assert_eq!(parse("transform", "translate(10px, 50%)"), vec![(TranslateX, px(10.0)), (TranslateY, pct(50.0))]);
        assert_eq!(parse("transform", "translate3d(-50%, 2px, 9px) scale(2)"), vec![(TranslateX, pct(-50.0)), (TranslateY, px(2.0))]);
        assert_eq!(parse("transform", "translateX(5px) translateX(7px)"), vec![(TranslateX, px(12.0)), (TranslateY, px(0.0))]);
        assert_eq!(parse("transform", "none"), vec![(TranslateX, px(0.0)), (TranslateY, px(0.0))]);
        assert_eq!(parse("transform", "inherit"), vec![(TranslateX, Value::Inherit), (TranslateY, Value::Inherit)]);
        assert_eq!(parse("translate", "10px 20px"), vec![(TranslateX, px(10.0)), (TranslateY, px(20.0))]);
        assert_eq!(parse("translate", "-100%"), vec![(TranslateX, pct(-100.0)), (TranslateY, px(0.0))]);
        assert_eq!(parse("translate", "none"), vec![(TranslateX, px(0.0)), (TranslateY, px(0.0))]);
        // Not good: a mix of units, a name that is not a transform, a bare number.
        assert!(parse("transform", "translateX(5px) translateX(7%)").is_empty());
        assert!(parse("transform", "wobble(1px)").is_empty());
        assert!(parse("transform", "translateX(5)").is_empty());
        assert!(parse("translate", "5").is_empty());
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
        assert_eq!(std::mem::size_of::<Side>(), 3 * std::mem::size_of::<Vec<()>>());
        assert_eq!(std::mem::size_of::<Declared>(), 12);
        assert_eq!(std::mem::size_of::<Property>(), 1);
    }

    #[test]
    fn keyword_names_round_trip() {
        for word in ["auto", "none", "inline-block", "xxx-large", "currentcolor", "thick"] {
            assert_eq!(Keyword::from_name(word).map(Keyword::name), Some(word));
        }
        assert_eq!(Keyword::from_name("Auto"), None);
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
        for unit in ["dvh", "svh", "lvh"] {
            assert_eq!(one("height", &format!("10{unit}")), Some(Value::Length(Length::Vh(10.0))));
        }
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
        for bad in ["10", "10 px", "px", "10xx", "1.5", "-5px", "-1%", "var(--x", "var(x)", "auto auto", "#fff", "red", "10px 20px", "\"10px\"", "url(a)", "10px;", "10px)", "{10px}"] {
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
    fn table_properties() {
        let k = |w| Value::Keyword(w);
        let px = |v| Value::Length(Length::Px(v));
        assert_eq!(parse("border-spacing", "2px"), vec![(BorderSpacingH, px(2.0)), (BorderSpacingV, px(2.0))]);
        assert_eq!(parse("border-spacing", "1px 4px"), vec![(BorderSpacingH, px(1.0)), (BorderSpacingV, px(4.0))]);
        assert_eq!(parse("border-spacing", "0"), vec![(BorderSpacingH, px(0.0)), (BorderSpacingV, px(0.0))]);
        assert_eq!(parse("border-spacing", "1px 2px 3px"), vec![]);
        assert_eq!(parse("border-spacing", "-1px"), vec![]);
        assert_eq!(parse("border-spacing", "5%"), vec![]);
        assert_eq!(parse("border-spacing", "inherit").len(), 2);
        assert_eq!(parse("border-collapse", "collapse"), vec![(BorderCollapse, k(K::Collapse))]);
        assert_eq!(parse("border-collapse", "auto"), vec![]);
        assert_eq!(parse("table-layout", "fixed"), vec![(TableLayout, k(K::Fixed))]);
        assert_eq!(parse("table-layout", "collapse"), vec![]);
        assert_eq!(parse("caption-side", "bottom"), vec![(CaptionSide, k(K::Bottom))]);
        assert_eq!(parse("vertical-align", "middle"), vec![(VerticalAlign, k(K::Middle))]);
        assert_eq!(parse("vertical-align", "2px"), vec![(VerticalAlign, px(2.0))]);
        assert_eq!(parse("vertical-align", "bogus"), vec![]);
        assert_eq!(parse("display", "inline-table"), vec![(Display, k(K::InlineTable))]);
    }

    #[test]
    fn flex_shorthands() {
        let k = |w| Value::Keyword(w);
        let n = Value::Number;
        let pct = |v| Value::Length(Length::Percent(v));
        let px = |v| Value::Length(Length::Px(v));
        let f = |v: &str| parse("flex", v).into_iter().map(|(_, v)| v).collect::<Vec<_>>();
        assert_eq!(f("1"), [n(1.0), n(1.0), pct(0.0)]);
        assert_eq!(f("none"), [n(0.0), n(0.0), k(K::Auto)]);
        assert_eq!(f("auto"), [n(1.0), n(1.0), k(K::Auto)]);
        assert_eq!(f("2 3"), [n(2.0), n(3.0), pct(0.0)]);
        assert_eq!(f("10px"), [n(1.0), n(1.0), px(10.0)]);
        assert_eq!(f("0 0 auto"), [n(0.0), n(0.0), k(K::Auto)]);
        assert_eq!(f("1 1 0"), [n(1.0), n(1.0), px(0.0)]);
        assert_eq!(f("30px 2 3"), [n(2.0), n(3.0), px(30.0)]);
        assert_eq!(f("1 10px 2"), vec![]);
        assert_eq!(f("-1"), vec![]);
        assert_eq!(parse("flex-flow", "wrap column"), vec![(FlexDirection, k(K::Column)), (FlexWrap, k(K::Wrap))]);
        assert_eq!(parse("flex-flow", "wrap wrap"), vec![]);
        assert_eq!(parse("gap", "10px 5%"), vec![(RowGap, px(10.0)), (ColumnGap, pct(5.0))]);
        assert_eq!(parse("align-items", "first baseline"), vec![(AlignItems, k(K::Baseline))]);
        assert_eq!(parse("justify-content", "safe center"), vec![(JustifyContent, k(K::Center))]);
        assert_eq!(parse("order", "-2"), vec![(Order, n(-2.0))]);
        assert_eq!(parse("order", "1.5"), vec![]);
        assert_eq!(parse("aspect-ratio", "5 / 2"), vec![(AspectRatio, n(2.5))]);
        assert_eq!(parse("aspect-ratio", "auto 4/1"), vec![(AspectRatio, n(4.0))]);
        assert_eq!(parse("aspect-ratio", "2 auto"), vec![(AspectRatio, n(2.0))]);
        assert_eq!(parse("aspect-ratio", "auto"), vec![(AspectRatio, k(K::Auto))]);
        assert_eq!(parse("aspect-ratio", "0 / 1"), vec![(AspectRatio, k(K::Auto))]);
        assert_eq!(parse("aspect-ratio", "1 / 0"), vec![(AspectRatio, k(K::Auto))]);
        assert_eq!(parse("aspect-ratio", "-1"), vec![]);
        assert_eq!(parse("aspect-ratio", "1 / 2 / 3"), vec![]);
        assert_eq!(parse("align-self", "space-between"), vec![]);
    }

    #[test]
    fn position() {
        let k = |w| Value::Keyword(w);
        assert_eq!(parse("position", "sticky"), vec![(Position, k(K::Sticky))]);
        assert_eq!(one("z-index", "-3"), Some(Value::Number(-3.0)));
        assert_eq!(one("z-index", "auto"), Some(Value::Keyword(K::Auto)));
        assert_eq!(one("z-index", "1.5"), None);
        assert_eq!(one("z-index", "10px"), None);
        assert_eq!(one("opacity", "0.5"), Some(Value::Number(0.5)));
        assert_eq!(one("opacity", "50%"), Some(Value::Number(0.5)));
        assert_eq!(one("opacity", "7"), Some(Value::Number(1.0)));
        assert_eq!(one("opacity", "-1"), Some(Value::Number(0.0)));
        assert_eq!(one("opacity", "1px"), None);
        assert_eq!(one("appearance", "none"), Some(Value::Keyword(K::None)));
        assert_eq!(one("appearance", "textfield"), Some(Value::Keyword(K::Textfield)));
        // `user-select` takes the CSS UI 4 keywords. `-webkit-user-select` is the same property.
        assert_eq!(one("user-select", "none"), Some(Value::Keyword(K::None)));
        assert_eq!(one("user-select", "text"), Some(Value::Keyword(K::Text)));
        assert_eq!(one("user-select", "contain"), Some(Value::Keyword(K::Contain)));
        assert_eq!(one("user-select", "all"), Some(Value::Keyword(K::All)));
        assert_eq!(one("user-select", "element"), None);
        assert_eq!(parse("-webkit-user-select", "none"), [(Property::UserSelect, Value::Keyword(K::None))]);
        assert_eq!(parse("-webkit-user-select", "none"), parse("user-select", "none"));
        assert_eq!(Property::from_name("-webkit-user-select"), Some(Property::UserSelect));
        assert_eq!(one("border-top-left-radius", "4px"), px(4.0));
        assert_eq!(one("border-top-left-radius", "-4px"), None);
        let all = |v: f32| vec![BorderTopLeftRadius, BorderTopRightRadius, BorderBottomRightRadius, BorderBottomLeftRadius].into_iter().map(|p| (p, Value::Length(Length::Px(v)))).collect::<Vec<_>>();
        assert_eq!(parse("border-radius", "6px"), all(6.0));
        let two = parse("border-radius", "1px 2px");
        assert_eq!(two.iter().map(|&(_, v)| v).collect::<Vec<_>>(), [1.0, 2.0, 1.0, 2.0].map(|v| Value::Length(Length::Px(v))));
        let four = parse("border-radius", "1px 2px 3px 4px / 9px");
        assert_eq!(four.iter().map(|&(_, v)| v).collect::<Vec<_>>(), [1.0, 2.0, 3.0, 4.0].map(|v| Value::Length(Length::Px(v))));
        assert_eq!(parse("border-radius", "1px 2px 3px 4px 5px"), vec![]);
        assert_eq!(parse("border-radius", "50%")[0].1, Value::Length(Length::Percent(50.0)));
        assert_eq!(parse("position", "auto"), vec![]);
        assert_eq!(parse("top", "-10px"), vec![(Top, Value::Length(Length::Px(-10.0)))]);
        assert_eq!(parse("left", "50%"), vec![(Left, Value::Length(Length::Percent(50.0)))]);
        assert_eq!(parse("bottom", "auto"), vec![(Bottom, k(K::Auto))]);
        assert_eq!(parse("right", "3"), vec![]);
        let px = |v| Value::Length(Length::Px(v));
        assert_eq!(parse("inset", "0 auto"), vec![(Top, px(0.0)), (Right, k(K::Auto)), (Bottom, px(0.0)), (Left, k(K::Auto))]);
    }

    #[test]
    fn overflow() {
        let k = |w| Value::Keyword(w);
        assert_eq!(parse("overflow", "hidden"), vec![(OverflowX, k(K::Hidden)), (OverflowY, k(K::Hidden))]);
        assert_eq!(parse("overflow", "clip scroll"), vec![(OverflowX, k(K::Clip)), (OverflowY, k(K::Scroll))]);
        assert_eq!(parse("overflow-y", "auto"), vec![(OverflowY, k(K::Auto))]);
        assert_eq!(parse("overflow", "hidden auto visible"), vec![]);
        assert_eq!(parse("overflow", "none"), vec![]);
        assert_eq!(parse("overflow-x", "clip"), vec![(OverflowX, k(K::Clip))]);
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
        for bad in ["#ff", "#ggg", "rgb(1,2)", "notacolor", "10px", "red blue", "rgb(1 2 3) red", "1"] {
            assert_eq!(parse("color", bad), vec![], "{bad}");
        }
    }

    #[test]
    fn keyword_properties() {
        assert_eq!(one("display", "block"), Some(Value::Keyword(K::Block)));
        assert_eq!(one("display", "Inline-Block"), Some(Value::Keyword(K::InlineBlock)));
        assert_eq!(one("display", "flow-root"), Some(Value::Keyword(K::FlowRoot)));
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
        let none = Value::Keyword(K::None);
        assert_eq!(parse("background", "red").len(), 7);
        assert_eq!(parse("background", "red")[..2], [(BackgroundColor, red), (BackgroundImage, none)]);
        assert_eq!(parse("background", "#f00 url(a.png)").len(), 7);
        assert_eq!(parse("background", "none")[..2], [(BackgroundColor, Value::Color(Color::TRANSPARENT)), (BackgroundImage, none)]);
        assert_eq!(parse("background", "bold"), vec![]);
        assert_eq!(parse("background", "#ff"), vec![]);
        assert_eq!(parse("background", "1px solid red"), vec![]);
        assert_eq!(parse("background", "red 10"), vec![]);
        assert_eq!(parse("background", "linear-gradient(red, blue)")[..2], [(BackgroundColor, Value::Color(Color::TRANSPARENT)), (BackgroundImage, none)]);
    }

    #[test]
    fn image_properties() {
        let (none, auto, pct) = (Value::Keyword(K::None), Value::Keyword(K::Auto), |p| Value::Length(Length::Percent(p)));
        let px = |v| Value::Length(Length::Px(v));
        let id = |u: &str| Value::Number(crate::urls::intern(u) as f32);
        assert_eq!(parse("background-image", "url(a.png)"), [(BackgroundImage, id("a.png"))]);
        assert_eq!(parse("background-image", "URL( \"b.png\" )"), [(BackgroundImage, id("b.png"))]);
        assert_eq!(parse("background-image", "url('data:image/svg+xml,<svg a=\"1\"/>')"), [(BackgroundImage, id("data:image/svg+xml,<svg a=\"1\"/>"))]);
        assert_eq!(parse("background-image", "none"), [(BackgroundImage, none)]);
        // An image that is not drawn is `none`, and the other declarations stay.
        assert_eq!(parse("background-image", "linear-gradient(red, blue)"), [(BackgroundImage, none)]);
        assert_eq!(parse("background-image", "repeating-radial-gradient(red 0, blue 10px), url(a.png)"), [(BackgroundImage, none)]);
        assert_eq!(parse("background-image", "url(a.png), url(b.png)"), [(BackgroundImage, id("a.png"))]);
        assert_eq!(parse("background-image", "url(\"a,b.png\"), none"), [(BackgroundImage, id("a,b.png"))]);
        assert_eq!(parse("background-image", "bold"), vec![]);
        assert_eq!(parse("background-image", "url(a.png) url(b.png)"), vec![]);
        assert_eq!(parse("background-size", "cover"), [(BackgroundSizeX, Value::Keyword(K::Cover)), (BackgroundSizeY, auto)]);
        assert_eq!(parse("background-size", "contain"), [(BackgroundSizeX, Value::Keyword(K::Contain)), (BackgroundSizeY, auto)]);
        assert_eq!(parse("background-size", "20px"), [(BackgroundSizeX, px(20.0)), (BackgroundSizeY, auto)]);
        assert_eq!(parse("background-size", "50% auto"), [(BackgroundSizeX, pct(50.0)), (BackgroundSizeY, auto)]);
        assert_eq!(parse("background-size", "auto 2em"), [(BackgroundSizeX, auto), (BackgroundSizeY, Value::Length(Length::Em(2.0)))]);
        assert_eq!(parse("background-size", "-1px"), vec![]);
        assert_eq!(parse("background-size", "cover cover"), vec![]);
        assert_eq!(parse("background-position", "center"), [(BackgroundPositionX, pct(50.0)), (BackgroundPositionY, pct(50.0))]);
        assert_eq!(parse("background-position", "right"), [(BackgroundPositionX, pct(100.0)), (BackgroundPositionY, pct(50.0))]);
        assert_eq!(parse("background-position", "bottom"), [(BackgroundPositionX, pct(50.0)), (BackgroundPositionY, pct(100.0))]);
        assert_eq!(parse("background-position", "10px 25%"), [(BackgroundPositionX, px(10.0)), (BackgroundPositionY, pct(25.0))]);
        assert_eq!(parse("background-position", "bottom left"), [(BackgroundPositionX, pct(0.0)), (BackgroundPositionY, pct(100.0))]);
        assert_eq!(parse("background-position", "right 5px"), [(BackgroundPositionX, pct(100.0)), (BackgroundPositionY, px(5.0))]);
        assert_eq!(parse("background-position", "left right"), vec![]);
        assert_eq!(parse("background-position", "right 10px bottom 5px"), vec![]);
        assert_eq!(parse("background-position-x", "right"), [(BackgroundPositionX, pct(100.0))]);
        assert_eq!(parse("background-position-y", "right"), vec![]);
        assert_eq!(parse("background-repeat", "no-repeat"), [(BackgroundRepeat, Value::Keyword(K::NoRepeat))]);
        assert_eq!(parse("background-repeat", "repeat-x"), [(BackgroundRepeat, Value::Keyword(K::RepeatX))]);
        assert_eq!(parse("background-repeat", "repeat no-repeat"), [(BackgroundRepeat, Value::Keyword(K::RepeatX))]);
        assert_eq!(parse("background-repeat", "no-repeat repeat"), [(BackgroundRepeat, Value::Keyword(K::RepeatY))]);
        assert_eq!(parse("background-repeat", "no-repeat no-repeat"), [(BackgroundRepeat, Value::Keyword(K::NoRepeat))]);
        assert_eq!(parse("background-repeat", "repeat-x repeat"), vec![]);
        // The shorthand.
        let all = parse("background", "#f00 url(a.png) no-repeat 10px 20% / 30px auto");
        assert_eq!(all.len(), 7);
        assert!(all.contains(&(BackgroundImage, id("a.png"))));
        assert!(all.contains(&(BackgroundPositionX, px(10.0))) && all.contains(&(BackgroundPositionY, pct(20.0))));
        assert!(all.contains(&(BackgroundSizeX, px(30.0))) && all.contains(&(BackgroundSizeY, auto)));
        assert!(all.contains(&(BackgroundRepeat, Value::Keyword(K::NoRepeat))));
        assert_eq!(parse("background", "url(a.png) center / cover").len(), 7);
        assert_eq!(parse("background", "url(a.png) / cover"), vec![]);
        assert_eq!(parse("background", "center cover"), vec![]);
        assert_eq!(parse("background", "url(a.png) top left right bottom 1px"), vec![]);
        // `mask` has the same parts and no color; `-webkit-mask` is the same property.
        let mask = parse("mask", "url(m.svg) no-repeat center / 20px");
        assert_eq!(mask.len(), 6);
        assert!(mask.contains(&(MaskImage, id("m.svg"))) && mask.contains(&(MaskSizeX, px(20.0))) && mask.contains(&(MaskRepeat, Value::Keyword(K::NoRepeat))));
        assert_eq!(parse("mask", "red url(m.svg)"), vec![]);
        assert_eq!(parse("-webkit-mask", "url(m.svg) no-repeat"), parse("mask", "url(m.svg) no-repeat"));
        assert_eq!(parse("-webkit-mask-image", "url(m.svg)"), [(MaskImage, id("m.svg"))]);
        assert_eq!(parse("-webkit-mask-size", "contain"), parse("mask-size", "contain"));
        assert_eq!(parse("mask-position", "center"), [(MaskPositionX, pct(50.0)), (MaskPositionY, pct(50.0))]);
        assert_eq!(parse("mask-repeat", "repeat-y"), [(MaskRepeat, Value::Keyword(K::RepeatY))]);
        assert_eq!(parse("mask", "none").len(), 6);
        // A `calc()` that mixes units keeps its place in the list.
        let mut side = Side::default();
        let mut out = Vec::new();
        assert!(parse_declaration("mask-size", "calc(100% - 4px)", false, &mut side, &mut out));
        assert_eq!(out[0].value, Value::Length(Length::Calc(0)));
    }

    #[test]
    fn css_wide_keywords() {
        assert_eq!(one("color", "inherit"), Some(Value::Inherit));
        assert_eq!(one("width", "INITIAL"), Some(Value::Initial));
        assert_eq!(one("display", "unset"), Some(Value::Unset));
        assert_eq!(parse("margin", "inherit").len(), 4);
        assert_eq!(parse("border", "initial").len(), 12);
        assert_eq!(parse("font", "inherit").len(), 4);
        assert_eq!(parse("background", "inherit").len(), 7);
        assert_eq!(parse("mask", "initial").len(), 6);
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
        assert!(parse_declaration("margin", "0", true, &mut Side::default(), &mut out));
        assert_eq!(out.len(), 4);
        assert!(out.iter().all(|d| d.important));
        let mut out = Vec::new();
        assert!(parse_declaration("margin", "0", false, &mut Side::default(), &mut out));
        assert!(out.iter().all(|d| !d.important));
    }

    #[test]
    fn a_wrong_value_leaves_the_list_as_it_was() {
        let mut out = Vec::new();
        parse_declaration("width", "1px", false, &mut Side::default(), &mut out);
        assert!(!parse_declaration("border", "1px solid red red", false, &mut Side::default(), &mut out));
        assert!(!parse_declaration("margin", "1px 2px 3px 4px 5px", false, &mut Side::default(), &mut out));
        assert!(!parse_declaration("padding", "1px -1px", false, &mut Side::default(), &mut out));
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
            let _ = parse_declaration(names[(next() % names.len() as u64) as usize], &text, false, &mut Side::default(), &mut out);
        }
    }

    #[test]
    fn calc_values() {
        let w = |v: &str| one("width", v);
        assert_eq!(w("calc(10px + 5px)"), Some(Value::Length(Length::Px(15.0))));
        assert_eq!(w("calc(2 * 1em)"), Some(Value::Length(Length::Em(2.0))));
        assert_eq!(w("calc(50% / 2)"), Some(Value::Length(Length::Percent(25.0))));
        assert_eq!(w("CALC( 1in - 4px )"), Some(Value::Length(Length::Px(92.0))));
        assert_eq!(w("-webkit-calc(1px + 1px)"), Some(Value::Length(Length::Px(2.0))));
        assert_eq!(w("calc(1px + calc(1px + 1px))"), Some(Value::Length(Length::Px(3.0))));
        assert_eq!(w("calc(100% - 20px)"), Some(Value::Length(Length::Calc(0))));
        assert_eq!(one("line-height", "calc(1 + 0.5)"), Some(Value::Number(1.5)));
        assert_eq!(one("font-weight", "calc(300 + 400)"), Some(Value::Number(700.0)));
        // Not good: the whole declaration goes.
        for bad in ["calc(1 + 2px)", "calc(1px + )", "calc(1px*2px)", "calc(1px/0)", "calc()", "calc(1px 2px)", "calc(1px+2px)"] {
            assert_eq!(parse("width", bad), vec![], "{bad}");
        }
        // A `calc()` that is not closed ends with the value.
        assert_eq!(w("calc(1px + 1px"), Some(Value::Length(Length::Px(2.0))));
    }

    #[test]
    fn mixed_calc_goes_to_the_list() {
        let mut side = Side::default();
        let mut out = Vec::new();
        assert!(parse_declaration("margin", "calc(100% - 1px) calc(50% + 2em)", false, &mut side, &mut out));
        assert_eq!(side.calcs.len(), 2);
        assert_eq!(out[0].value, Value::Length(Length::Calc(0)));
        assert_eq!(out[1].value, Value::Length(Length::Calc(1)));
        assert_eq!(out[2].value, Value::Length(Length::Calc(0)));
        assert!(!parse_declaration("width", "calc(1px +)", false, &mut side, &mut out));
        assert_eq!(side.calcs.len(), 2);
    }
}
