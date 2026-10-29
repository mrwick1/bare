//! The scaling cases for the URL, HTML and CSS crates. Each one builds a bad input of size `n`.

use crate::Case;
use bare_css::cascade::{Origin, RuleSet, UA_CSS};
use bare_html::tree;
use bare_url::Url;
use std::hint::black_box;

type Work = Box<dyn FnOnce()>;

fn case(name: &'static str, base: usize, build: fn(usize) -> Work) -> Case {
    Case { name, base, build, single: false }
}

/// Parse a page.
fn html(page: String) -> Work {
    Box::new(move || {
        black_box(tree::parse(&page));
    })
}

/// Parse a page (not timed), then add a style sheet and compute all styles (timed).
fn css(page: &str, sheet: String) -> Work {
    let mut doc = tree::parse(page);
    Box::new(move || {
        let mut rules = RuleSet::new((1280.0, 800.0));
        rules.add(&mut doc, UA_CSS, Origin::UserAgent);
        rules.add(&mut doc, &sheet, Origin::Author);
        black_box(rules.compute(&doc));
    })
}

/// A sheet only: no page, so only the parser runs.
fn sheet(text: String) -> Work {
    css("<p>x", text)
}

fn url(text: String) -> Work {
    Box::new(move || {
        black_box(Url::parse(&text).ok());
    })
}

pub fn all() -> Vec<Case> {
    vec![
        // URL
        case("url-long-path", 100_000, |n| url(format!("http://a/{}", "x".repeat(n)))),
        case("url-long-query", 100_000, |n| url(format!("http://a/?{}", "&a=b".repeat(n / 4)))),
        case("url-dot-segments", 20_000, |n| url(format!("http://a/{}", "/..".repeat(n)))),
        case("url-up-down", 20_000, |n| url(format!("http://a/{}", "a/../".repeat(n)))),
        case("url-percent", 50_000, |n| url(format!("http://a/{}", "%41".repeat(n)))),
        case("url-many-labels", 10_000, |n| url(format!("http://{}com/", "a.".repeat(n)))),
        case("url-idna-long-label", 1_000, |n| url(format!("http://{}.com/", "\u{fc}".repeat(n)))),
        case("url-idna-many-labels", 1_000, |n| url(format!("http://{}com/", "\u{fc}.".repeat(n)))),
        case("url-idna-punycode", 1_000, |n| url(format!("http://xn--{}.com/", "a".repeat(n)))),
        case("url-userinfo", 100_000, |n| url(format!("http://{}@a/", "u:".repeat(n)))),
        case("url-ipv6", 10_000, |n| url(format!("http://[{}]/", "1:".repeat(n)))),
        case("url-port", 100_000, |n| url(format!("http://a:{}/", "1".repeat(n)))),
        case("url-join-dots", 20_000, |n| {
            let base = Url::parse(&format!("http://a/{}", "x/".repeat(n))).unwrap();
            let link = "../".repeat(n);
            Box::new(move || {
                black_box(base.join(&link).ok());
            })
        }),
        // HTML tree builder
        case("html-deep-div", 10_000, |n| html("<div>".repeat(n))),
        case("html-deep-span", 10_000, |n| html("<span>".repeat(n))),
        case("html-wide", 50_000, |n| html("<i>x</i>".repeat(n))),
        case("html-unclosed-b", 5_000, |n| html("<b>x".repeat(n))),
        case("html-unclosed-a", 5_000, |n| html("<a href=x>x".repeat(n))),
        case("html-misnested", 5_000, |n| html("<b><i><p>x</b>y</i>".repeat(n))),
        case("html-nested-b-then-p", 2_000, |n| html(format!("{}<p>{}", "<b>".repeat(n), "</b>".repeat(n)))),
        case("html-end-tag-walk", 5_000, |n| html(format!("{}{}", "<div>".repeat(n), "</span>".repeat(n)))),
        case("html-stray-end", 50_000, |n| html("</div></p></b></br>".repeat(n / 4))),
        case("html-many-attrs", 20_000, |n| html(format!("<div{}>", (0..n).map(|i| format!(" a{i}=1")).collect::<String>()))),
        case("html-same-attr", 20_000, |n| html(format!("<div{}>", " a=1".repeat(n)))),
        case("html-long-attr", 1_000_000, |n| html(format!("<a href=\"{}\">", "x".repeat(n)))),
        case("html-long-text", 1_000_000, |n| html("x".repeat(n))),
        case("html-long-tag-name", 1_000_000, |n| html(format!("<{}>", "x".repeat(n)))),
        case("html-entities", 50_000, |n| html("&amp;&lt;&#x41;&notit;&#0;&#xD800;".repeat(n))),
        case("html-entity-prefix", 20_000, |n| html("&notin".repeat(n))),
        case("html-bad-entity", 20_000, |n| html(format!("&{};", "a".repeat(n)))),
        case("html-comments", 50_000, |n| html("<!--x-->".repeat(n))),
        case("html-comment-open", 100_000, |n| html("<!--".repeat(n))),
        case("html-comment-dashes", 100_000, |n| html(format!("<!--{}", "-".repeat(n)))),
        case("html-nested-tables", 2_000, |n| html("<table><tr><td>".repeat(n))),
        case("html-table-rows", 20_000, |n| html(format!("<table>{}", "<tr><td>x</td></tr>".repeat(n)))),
        case("html-table-foster", 5_000, |n| html(format!("<table>{}", "<b>x".repeat(n)))),
        case("html-table-cell-formatting", 5_000, |n| html(format!("<b>{}", "<table><td><i>x".repeat(n)))),
        case("html-p-chain", 50_000, |n| html("<p>x".repeat(n))),
        case("html-li-chain", 50_000, |n| html("<li>x".repeat(n))),
        case("html-deep-lists", 5_000, |n| html("<ul><li>".repeat(n))),
        case("html-headings", 20_000, |n| html("<h1><h2><h3>".repeat(n))),
        case("html-script-open", 100_000, |n| html(format!("<script>{}", "a<".repeat(n)))),
        case("html-script-comment", 20_000, |n| html(format!("<script><!--{}", "<script>".repeat(n)))),
        case("html-unclosed-tag", 50_000, |n| html("<div ".repeat(n))),
        case("html-unclosed-quote", 100_000, |n| html(format!("<a b=\"{}", "x".repeat(n)))),
        case("html-textarea", 50_000, |n| html(format!("<textarea>{}", "<b>".repeat(n)))),
        case("html-nobr-reopen", 5_000, |n| html("<nobr>x<nobr>".repeat(n))),
        case("html-decode-bytes", 5_000_000, |n| {
            let bytes: Vec<u8> = (0..n).map(|i| (i * 7 % 251) as u8).collect();
            Box::new(move || {
                black_box(bare_html::encoding::decode(&bytes, Some("text/html; charset=windows-1252")));
            })
        }),
        case("html-nested-i-classes", 20_000, |n| html((0..n).map(|i| format!("<i class=c{i}>")).collect::<String>())),
        // CSS parser
        case("css-rules", 100_000, |n| sheet(".a{color:red}".repeat(n))),
        case("css-selector-list", 50_000, |n| sheet(format!("{}{{color:red}}", (0..n).map(|i| format!(".a{i},")).collect::<String>() + ".z"))),
        case("css-declarations", 100_000, |n| sheet(format!("p{{{}}}", "color:red;".repeat(n)))),
        case("css-important", 50_000, |n| sheet(format!("p{{{}}}", "color:red!important;".repeat(n)))),
        case("css-long-value", 1_000_000, |n| sheet(format!("p{{color:{}}}", "x".repeat(n)))),
        case("css-open-braces", 50_000, |n| sheet("{".repeat(n))),
        case("css-open-parens", 50_000, |n| sheet(format!("p{{width:{}", "(".repeat(n)))),
        case("css-open-brackets", 50_000, |n| sheet(format!("p{}", "[".repeat(n)))),
        case("css-open-functions", 50_000, |n| sheet(format!("p{{width:{}", "calc(".repeat(n)))),
        case("css-open-comment", 100_000, |n| sheet(format!("/*{}", "*".repeat(n)))),
        case("css-open-string", 100_000, |n| sheet(format!("p{{content:\"{}", "x".repeat(n)))),
        case("css-escapes", 100_000, |n| sheet(format!(".{}{{color:red}}", "\\41 ".repeat(n)))),
        case("css-bad-rules", 50_000, |n| sheet("p{color}q{:red}r{;;;}".repeat(n))),
        // CSS selectors and matching
        case("css-is-nested", 500, |n| sheet(format!("{}a{} {{color:red}}", ":is(".repeat(n), ")".repeat(n)))),
        case("css-not-nested", 500, |n| sheet(format!("{}a{} {{color:red}}", ":not(".repeat(n), ")".repeat(n)))),
        case("css-is-wide", 20_000, |n| sheet(format!(":is({}) {{color:red}}", (0..n).map(|i| format!(".a{i}")).collect::<Vec<_>>().join(",")))),
        case("css-long-descendant", 5_000, |n| css(&"<div>".repeat(1000), format!("{}p{{color:red}}", "div ".repeat(n)))),
        case("css-descendant-deep-doc", 1_000, |n| css(&"<div>".repeat(n), "div div div div div div div div x {color:red}".to_string())),
        case("css-descendant-miss", 1_000, |n| css(&format!("{}<p>", "<div>".repeat(n)), "a b c d e f g h p{color:red}".to_string())),
        case("css-nth-child", 5_000, |n| css(&format!("<ul>{}", "<li>".repeat(n)), "li:nth-child(odd){color:red}li:nth-last-child(2n+1){color:blue}".to_string())),
        case("css-nth-of-type", 5_000, |n| css(&format!("<ul>{}", "<li>".repeat(n)), "li:nth-of-type(3n){color:red}li:last-of-type{color:blue}".to_string())),
        case("css-sibling", 5_000, |n| css(&format!("<ul>{}", "<li>".repeat(n)), "li + li + li ~ li{color:red}li ~ li ~ li ~ li{color:blue}".to_string())),
        case("css-universal", 50_000, |n| css(&"<i>x</i>".repeat(n), "*{color:red}* *{color:blue}".to_string())),
        case("css-class-index", 20_000, |n| {
            css(&(0..n).map(|i| format!("<i class=c{i}>")).collect::<String>(), (0..n).map(|i| format!(".c{i}{{color:red}}")).collect())
        }),
        case("css-same-class", 20_000, |n| css(&"<i class=a>".repeat(n), ".a{color:red}".repeat(n))),
        case("css-attr-substring", 5_000, |n| {
            css(&format!("<a href=\"{}\">", "x".repeat(2000)), (0..n).map(|i| format!("[href*=\"y{i}\"]{{color:red}}")).collect())
        }),
        case("css-many-classes", 5_000, |n| {
            css(&format!("<i class=\"{}\">", (0..n).map(|i| format!("c{i}")).collect::<Vec<_>>().join(" ")), (0..n).map(|i| format!(".c{i}.c{}{{color:red}}", n - i)).collect())
        }),
        case("css-unique-styles", 20_000, |n| css(&(0..n).map(|i| format!("<i style=\"width:{i}px\">")).collect::<String>(), String::new())),
        case("css-inline-big", 50_000, |n| css(&format!("<i style=\"{}\">", "color:red;".repeat(n)), String::new())),
        case("css-inherit-chain", 5_000, |n| css(&"<div>".repeat(n), "div{font-size:1.1em;color:red}".to_string())),
        // CSS values
        case("css-calc-nested", 5_000, |n| sheet(format!("p{{width:{}1px{}}}", "calc(".repeat(n), ")".repeat(n)))),
        case("css-calc-terms", 20_000, |n| sheet(format!("p{{width:calc(1px{})}}", " + 1px".repeat(n)))),
        case("css-calc-mixed", 5_000, |n| sheet(format!("p{{width:calc(1px{})}}", " + 1em * 2 - 3%".repeat(n)))),
        case("css-var-chain", 5_000, |n| {
            let decls: String = (1..=n).map(|i| format!("--a{i}:var(--a{});", i - 1)).collect();
            css("<p>x", format!("p{{--a0:1px;{decls}width:var(--a{n})}}"))
        }),
        case("css-var-fanout", 100, |n| {
            let decls: String = (1..=n).map(|i| format!("--a{i}:var(--a{}) var(--a{});", i - 1, i - 1)).collect();
            css("<p>x", format!("p{{--a0:1px;{decls}width:var(--a{n})}}"))
        }),
        case("css-var-cycle", 5_000, |n| {
            let decls: String = (0..n).map(|i| format!("--a{i}:var(--a{});", (i + 1) % n)).collect();
            css("<p>x", format!("p{{{decls}width:var(--a0)}}"))
        }),
        case("css-var-many", 20_000, |n| css("<p>x", format!("p{{{}}}", (0..n).map(|i| format!("--a{i}:{i}px;")).collect::<String>()))),
        case("css-var-many-sets", 5_000, |n| css(&(0..n).map(|i| format!("<i style=\"--a:{i}\">")).collect::<String>(), "i{width:var(--a)}".to_string())),
        case("css-var-fallback", 5_000, |n| sheet(format!("p{{width:{}1px{}}}", "var(--x,".repeat(n), ")".repeat(n)))),
        case("css-var-uses", 20_000, |n| css("<p>x", format!("p{{--a:1px;margin:{}}}", "var(--a) ".repeat(n)))),
        // At-rules
        case("css-layers-many", 20_000, |n| sheet((0..n).map(|i| format!("@layer l{i}{{p{{color:red}}}}")).collect())),
        case("css-layers-statement", 20_000, |n| sheet(format!("@layer {};", (0..n).map(|i| format!("l{i}")).collect::<Vec<_>>().join(",")))),
        case("css-layers-nested", 500, |n| sheet(format!("{}p{{color:red}}{}", "@layer a{".repeat(n), "}".repeat(n)))),
        case("css-layers-anon", 20_000, |n| sheet("@layer{p{color:red}}".repeat(n))),
        case("css-media-nested", 500, |n| sheet(format!("{}p{{color:red}}{}", "@media screen{".repeat(n), "}".repeat(n)))),
        case("css-media-many", 20_000, |n| sheet("@media (min-width:100px) and (max-width:2000px){p{color:red}}".repeat(n))),
        case("css-media-long-query", 5_000, |n| sheet(format!("@media {}screen{{p{{color:red}}}}", "screen, ".repeat(n)))),
        case("css-supports-nested", 500, |n| sheet(format!("{}p{{color:red}}{}", "@supports (color:red){".repeat(n), "}".repeat(n)))),
        case("css-supports-condition", 2_000, |n| sheet(format!("@supports {}(color:red){}{{p{{color:red}}}}", "(not ".repeat(n), ")".repeat(n)))),
        case("css-import-many", 20_000, |n| sheet("@import url(x.css);".repeat(n))),
        // The whole path: bytes, tokens, tree, styles
        case("page-everything", 20_000, |n| {
            let page = format!("<style>{}</style>{}", ".c{color:red}p{margin:1em}".repeat(n / 10), "<p class=c>a<b>b<i>c</b>d".repeat(n));
            Box::new(move || {
                let mut doc = tree::parse(&page);
                let mut rules = RuleSet::new((1280.0, 800.0));
                rules.add(&mut doc, UA_CSS, Origin::UserAgent);
                rules.add(&mut doc, ".c{color:red}p{margin:1em}", Origin::Author);
                black_box(rules.compute(&doc));
            })
        }),
    ]
}
