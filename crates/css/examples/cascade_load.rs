//! Load test for the cascade (`src/cascade.rs`) with a sheet in the style of Tailwind 3: a `*` rule with 40 custom properties,
//! a base layer, and utility classes with `:hover`, `:focus`, attribute and `group` variants.
//!
//!   cascade_load CASE SCALE      CASE: full | read | restyle      SCALE: 1, 10
//!   cascade_load file PAGE.html SHEET.css REPS
//!
//! full: SCALE*20 passes of `compute` over a page of 600 elements with 4 to 8 classes each.
//! read: SCALE*20 `compute` passes, then a `compute_for` of an element inside `display: none`, as `getComputedStyle` does.
//! restyle: one `compute_kept`, then SCALE*200 class changes, each with a `restyle` of the changed element.
//! file: REPS passes of `compute` over a saved page and sheet. Prints the styles, the passes and a check sum.
//! Only synthetic data.

use bare_css::cascade::{Origin, RuleSet, UA_CSS};
use bare_css::values::Property;
use bare_html::dom::{Atom, Document, NodeData, NodeId};
use bare_html::tree::parse;

const UTIL: [&str; 24] = [
    "flex",
    "items-center",
    "gap-2",
    "p-4",
    "px-3",
    "py-2",
    "text-sm",
    "font-medium",
    "rounded-md",
    "border",
    "bg-white",
    "text-gray-900",
    "hover:bg-gray-100",
    "focus:ring-2",
    "shadow-sm",
    "w-full",
    "h-10",
    "truncate",
    "grid",
    "grid-cols-3",
    "mt-2",
    "space-y-4",
    "group",
    "group-hover:opacity-100",
];

fn sheet() -> String {
    let mut s = String::from("*,:after,:before{box-sizing:border-box;border:0 solid #e5e7eb}");
    s += "*,:after,:before{";
    for i in 0..40 {
        s += &format!("--tw-v{i}:{};", if i % 3 == 0 { " " } else { "0" });
    }
    s += "}";
    s += "html{line-height:1.5;font-family:ui-sans-serif,system-ui}body{margin:0;line-height:inherit}";
    s += "button,[type=button],[type=submit]{-webkit-appearance:button;background-color:transparent}";
    s += "a{color:inherit;text-decoration:inherit}input::placeholder{color:#9ca3af}[hidden]{display:none}";
    s += "button:focus-visible{outline:2px solid}[role=button]{cursor:pointer}";
    // Many utilities that no element uses, as in a large build, and the ones above.
    for i in 0..1200 {
        s += &format!(
            ".u{i}{{margin-left:{}px;color:rgb({},{},0)}}",
            i % 40,
            i % 255,
            (i * 7) % 255
        );
    }
    for (i, u) in UTIL.iter().enumerate() {
        let name = u.replace(':', "\\:");
        match u.split_once(':') {
            Some(("hover", _)) => {
                s += &format!(".{name}:hover{{background-color:rgb(243 244 246/var(--tw-v{i}))}}")
            }
            Some(("focus", _)) => {
                s += &format!(
                    ".{name}:focus{{--tw-ring-offset-shadow:var(--tw-v1) 0 0 0 var(--tw-v2);box-shadow:var(--tw-ring-offset-shadow)}}"
                )
            }
            Some(("group-hover", _)) => s += &format!(".group:hover .{name}{{opacity:1}}"),
            _ => s += &format!(".{name}{{padding-top:{}px;--tw-v{}:{i}}}", i % 9, i % 40),
        }
    }
    s += ".space-y-4>:not([hidden])~:not([hidden]){margin-top:1rem}.hidden{display:none}";
    s
}

fn page() -> String {
    let mut h = String::from(
        "<!DOCTYPE html><html><body><div class='hidden'><span id=deep>x</span></div><main class='flex p-4'>",
    );
    for i in 0..100 {
        let c = |k: usize| UTIL[(i * 5 + k * 3) % UTIL.len()];
        h += &format!("<section class='{} {} {} {}'>", c(0), c(1), c(2), c(3));
        h += &format!(
            "<a href='#' class='{} {} {} {} {}'>link {i}</a>",
            c(4),
            c(5),
            c(6),
            c(7),
            c(8)
        );
        h += &format!(
            "<button type=button class='{} {} {} {} {} {} {}'><span class='{}'>b</span></button>",
            c(9),
            c(10),
            c(11),
            c(12),
            c(13),
            c(14),
            c(15),
            c(16)
        );
        h += &format!(
            "<div class='{} {} {} {} {} {}'><p class='{} {}'>t</p></div></section>",
            c(17),
            c(18),
            c(19),
            c(20),
            c(21),
            c(22),
            c(23),
            c(1)
        );
    }
    h + "</main></body></html>"
}

fn elements(doc: &Document) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut stack = vec![doc.root()];
    while let Some(n) = stack.pop() {
        if matches!(doc.data(n), NodeData::Element { .. }) {
            out.push(n);
        }
        stack.extend(doc.children(n));
    }
    out
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |i: usize| {
        args.get(i).cloned().unwrap_or_else(|| {
            panic!("usage: cascade_load full|read|restyle SCALE | file PAGE SHEET REPS")
        })
    };
    let case = arg(1);
    let (html, css, reps) = if case == "file" {
        (
            std::fs::read_to_string(arg(2)).expect("page"),
            std::fs::read_to_string(arg(3)).expect("sheet"),
            arg(4).parse().expect("reps"),
        )
    } else {
        let scale: usize = arg(2).parse().expect("scale");
        (page(), sheet(), 20 * scale)
    };
    let mut doc = parse(&html);
    let mut rules = RuleSet::new((1280.0, 800.0));
    rules.add(&mut doc, UA_CSS, Origin::UserAgent);
    rules.add(&mut doc, &css, Origin::Author);
    let mut sum = 0u64;
    let mut unique = 0;
    let check = |doc: &Document, s: &bare_css::cascade::Styles| {
        elements(doc)
            .iter()
            .filter_map(|&n| s.style(n))
            .map(|c| c.px(Property::PaddingTop) as u64 + c.px(Property::MarginTop) as u64)
            .sum::<u64>()
    };
    match case.as_str() {
        "full" | "file" | "read" => {
            for _ in 0..reps {
                let s = rules.compute(&doc);
                unique = s.unique();
                sum += check(&doc, &s);
            }
            if case == "read" {
                let deep = elements(&doc)
                    .into_iter()
                    .find(|&n| doc.attr(n, Atom::ID) == Some("deep"))
                    .expect("deep");
                for _ in 0..reps {
                    let s = rules.compute_for(&doc, Some(deep));
                    sum += s.style(deep).map_or(0, |c| c.px(Property::FontSize) as u64);
                }
            }
        }
        "restyle" => {
            let mut s = rules.compute_kept(&doc);
            let els = elements(&doc);
            for k in 0..reps * 10 {
                let el = els[(k * 37) % els.len()];
                doc.set_attr(el, "class", UTIL[k % UTIL.len()]);
                let mut changed = Vec::new();
                if !rules.restyle(&doc, &mut s, &[(el, false)], &mut changed) {
                    s = rules.compute_kept(&doc);
                }
                sum += changed.len() as u64;
            }
            unique = s.unique();
            sum += check(&doc, &s);
        }
        _ => panic!("unknown case {case}"),
    }
    if std::env::var_os("CASCADE_DUMP").is_some() {
        let s = rules.compute(&doc);
        print!("{}", s.dump(&doc));
        for n in elements(&doc) {
            if let Some(c) = s.style(n) {
                println!("{n:?} {:?}", s.custom_properties(c));
            }
        }
    }
    println!(
        "{case}: elements {} styles {unique} passes {reps} sum {sum}",
        elements(&doc).len()
    );
}
