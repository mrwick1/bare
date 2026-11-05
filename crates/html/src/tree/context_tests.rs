//! Tests of the tokenizer and tree builder handshake for foreign content: raw text and RCDATA by insertion context, CDATA, and the
//! special elements of SVG and MathML. Each tree is in the html5lib text format (`<svg g>` is an SVG element).

use crate::tree::{Parser, parse};

/// The tree inside `<body>`, with one `|` column and the body indent cut off.
fn body(page: &str) -> String {
    let full = parse(&format!("<body>{page}")).dump();
    let head: String = ["<html>", "  <head>", "  <body>"].iter().map(|l| format!("| {l}\n")).collect();
    full.strip_prefix(&head).expect("a body").lines().map(|l| format!("{}\n", l.strip_prefix("|     ").unwrap_or(l))).collect()
}

fn lines(lines: &[&str]) -> String {
    lines.iter().map(|l| format!("{l}\n")).collect()
}

/// The tree of the page when it arrives in these pieces.
fn streamed(pieces: &[&str]) -> String {
    let mut parser = Parser::new();
    for piece in pieces {
        parser.feed(piece);
    }
    parser.finish().dump()
}

#[test]
fn svg_title_holds_markup() {
    assert_eq!(body("<svg><title><b>x</b></title></svg>"), lines(&["<svg svg>", "  <svg title>", "    <b>", "      \"x\""]));
    assert_eq!(body("<svg><desc>a<i>b</i></desc></svg>"), lines(&["<svg svg>", "  <svg desc>", "    \"a\"", "    <i>", "      \"b\""]));
}

#[test]
fn svg_style_decodes_references_and_holds_markup() {
    assert_eq!(body("<svg><style>a &amp; b<g>c</g></style></svg>"), lines(&["<svg svg>", "  <svg style>", "    \"a & b\"", "    <svg g>", "      \"c\""]));
}

#[test]
fn raw_text_names_are_markup_in_svg_and_mathml() {
    for name in ["style", "script", "textarea", "xmp", "iframe", "noembed", "noframes", "plaintext"] {
        let want = lines(&["<svg svg>", &format!("  <svg {name}>"), "    \"<\"", "    <svg g>", "      \"c\"", "<p>", "  \"after\""]);
        assert_eq!(body(&format!("<svg><{name}>&lt;<g>c</g></{name}></svg><p>after")), want, "svg {name}");
        let want = lines(&["<math math>", &format!("  <math {name}>"), "    \"<\"", "    <math g>", "      \"c\"", "<p>", "  \"after\""]);
        assert_eq!(body(&format!("<math><{name}>&lt;<g>c</g></{name}></math><p>after")), want, "math {name}");
    }
    assert_eq!(body("<math><title>&amp;<g/></title></math>"), lines(&["<math math>", "  <math title>", "    \"&\"", "    <math g>"]));
    // A MathML annotation-xml without the encoding is not an integration point.
    assert_eq!(
        body("<math><annotation-xml><style>&amp;<g/></style></annotation-xml></math>"),
        lines(&["<math math>", "  <math annotation-xml>", "    <math style>", "      \"&\"", "      <math g>"])
    );
}

#[test]
fn integration_points_choose_html_raw_text() {
    let points: [(&str, &str, &str, &str, &[&str]); 9] = [
        ("svg", "<svg><foreignObject>", "</foreignObject></svg>", "svg foreignObject", &[]),
        ("svg", "<svg><desc>", "</desc></svg>", "svg desc", &[]),
        ("svg", "<svg><title>", "</title></svg>", "svg title", &[]),
        ("math", "<math><mi>", "</mi></math>", "math mi", &[]),
        ("math", "<math><mo>", "</mo></math>", "math mo", &[]),
        ("math", "<math><mn>", "</mn></math>", "math mn", &[]),
        ("math", "<math><ms>", "</ms></math>", "math ms", &[]),
        ("math", "<math><mtext>", "</mtext></math>", "math mtext", &[]),
        ("math", "<math><annotation-xml encoding=Text/HTML>", "</annotation-xml></math>", "math annotation-xml", &["    encoding=\"Text/HTML\""]),
    ];
    // RAWTEXT and script data keep every character. RCDATA decodes references.
    let kinds = [("style", "&amp;<b>"), ("script", "&amp;<b>"), ("xmp", "&amp;<b>"), ("iframe", "&amp;<b>"), ("noembed", "&amp;<b>"), ("noframes", "&amp;<b>"), ("title", "&<b>"), ("textarea", "&<b>")];
    for (root, open, close, point, attrs) in points {
        for (name, text) in kinds {
            let page = format!("{open}<{name}>&amp;<b></{name}>{close}");
            let mut want = vec![format!("<{root} {root}>"), format!("  <{point}>")];
            want.extend(attrs.iter().map(|a| a.to_string()));
            want.push(format!("    <{name}>"));
            want.push(format!("      \"{text}\""));
            let want: String = want.iter().map(|l| format!("{l}\n")).collect();
            assert_eq!(body(&page), want, "{page}");
        }
    }
    // A MathML annotation-xml with `application/xhtml+xml` too.
    assert_eq!(
        body("<math><annotation-xml encoding=application/xhtml+xml><title>&amp;<b></title></annotation-xml></math>"),
        lines(&["<math math>", "  <math annotation-xml>", "    encoding=\"application/xhtml+xml\"", "    <title>", "      \"&<b>\""])
    );
}

#[test]
fn plaintext_in_an_html_integration_point_takes_the_rest() {
    assert_eq!(body("<svg><foreignObject><plaintext></foreignObject>x"), lines(&["<svg svg>", "  <svg foreignObject>", "    <plaintext>", "      \"</foreignObject>x\""]));
}

#[test]
fn namespaces_switch_back_and_forth() {
    let page = "<svg><foreignObject><div>a<svg><title><b>x</b></title></svg><style>&amp;</style></div></foreignObject></svg><style>&amp;</style><title>&amp;</title>";
    assert_eq!(
        body(page),
        lines(&[
            "<svg svg>",
            "  <svg foreignObject>",
            "    <div>",
            "      \"a\"",
            "      <svg svg>",
            "        <svg title>",
            "          <b>",
            "            \"x\"",
            "      <style>",
            "        \"&amp;\"",
            "<style>",
            "  \"&amp;\"",
            "<title>",
            "  \"&\"",
        ])
    );
    // A breakout tag leaves SVG, and then HTML raw text applies.
    assert_eq!(body("<svg><g><p><style>&lt;</style>"), lines(&["<svg svg>", "  <svg g>", "<p>", "  <style>", "    \"&lt;\""]));
}

#[test]
fn a_foreign_element_in_a_table_is_moved_out_and_keeps_its_content() {
    assert_eq!(
        body("<table><svg><title><b>x</b></title><style>&amp;</style></svg><tr><td>1"),
        lines(&["<svg svg>", "  <svg title>", "    <b>", "      \"x\"", "  <svg style>", "    \"&\"", "<table>", "  <tbody>", "    <tr>", "      <td>", "        \"1\""])
    );
}

#[test]
fn cdata_is_text_in_foreign_content() {
    assert_eq!(body("<svg><![CDATA[a<b>&amp;]]></svg>"), lines(&["<svg svg>", "  \"a<b>&amp;\""]));
    assert_eq!(body("<math><![CDATA[x]]></math>"), lines(&["<math math>", "  \"x\""]));
    // It joins the text around it.
    assert_eq!(body("<svg>a<![CDATA[b]]>c&amp;</svg>"), lines(&["<svg svg>", "  \"abc&\""]));
    // The first `]]>` ends it, and the brackets before it stay.
    assert_eq!(body("<svg><![CDATA[a]]]]>z</svg>"), lines(&["<svg svg>", "  \"a]]z\""]));
    assert_eq!(body("<svg><![CDATA[]]]>z</svg>"), lines(&["<svg svg>", "  \"]z\""]));
    assert_eq!(body("<svg><![CDATA[]]>z</svg>"), lines(&["<svg svg>", "  \"z\""]));
    // A tag inside is text. A `>` and `<!--` too.
    assert_eq!(body("<svg><![CDATA[<g><!-- x -->]]><g/></svg>"), lines(&["<svg svg>", "  \"<g><!-- x -->\"", "  <svg g>"]));
    // The page ends inside. The text so far stays, with the brackets that were read.
    assert_eq!(body("<svg><![CDATA[x"), lines(&["<svg svg>", "  \"x\""]));
    assert_eq!(body("<svg><![CDATA[x]]"), lines(&["<svg svg>", "  \"x]]\""]));
    assert_eq!(body("<svg><![CDATA["), lines(&["<svg svg>"]));
    // Only the exact, upper case marker counts.
    assert_eq!(body("<svg><![cdata[x]]></svg>"), lines(&["<svg svg>", "  <!-- [cdata[x]] -->"]));
    assert_eq!(body("<svg><![CDATA x]]></svg>"), lines(&["<svg svg>", "  <!-- [CDATA x]] -->"]));
}

#[test]
fn cdata_follows_the_adjusted_current_node() {
    // The current node is an SVG element, so it is allowed, even inside a foreignObject.
    assert_eq!(body("<svg><foreignObject><![CDATA[x]]></foreignObject></svg>"), lines(&["<svg svg>", "  <svg foreignObject>", "    \"x\""]));
    assert_eq!(body("<math><mi><![CDATA[x]]></mi></math>"), lines(&["<math math>", "  <math mi>", "    \"x\""]));
    // An HTML element inside it is the current node, so it is a comment again.
    assert_eq!(body("<svg><foreignObject><p><![CDATA[x]]></p></foreignObject></svg>"), lines(&["<svg svg>", "  <svg foreignObject>", "    <p>", "      <!-- [CDATA[x]] -->"]));
    // HTML content.
    assert_eq!(body("<p><![CDATA[x]]>y"), lines(&["<p>", "  <!-- [CDATA[x]] -->", "  \"y\""]));
    assert_eq!(body("<svg></svg><![CDATA[x]]>"), lines(&["<svg svg>", "<!-- [CDATA[x]] -->"]));
    assert_eq!(body("<svg><g></g><![CDATA[x]]></svg>"), lines(&["<svg svg>", "  <svg g>", "  \"x\""]));
    // After a breakout tag the current node is HTML.
    assert_eq!(body("<svg><p><![CDATA[x]]>"), lines(&["<svg svg>", "<p>", "  <!-- [CDATA[x]] -->"]));
}

#[test]
fn an_end_tag_inside_a_special_foreign_element_does_not_close_past_it() {
    for (open, close, point) in [
        ("<svg><foreignObject>", "</foreignObject></svg>", ("svg", "svg foreignObject")),
        ("<svg><desc>", "</desc></svg>", ("svg", "svg desc")),
        ("<svg><title>", "</title></svg>", ("svg", "svg title")),
        ("<math><mi>", "</mi></math>", ("math", "math mi")),
        ("<math><mtext>", "</mtext></math>", ("math", "math mtext")),
    ] {
        let (root, el) = point;
        let want = lines(&["<span>", &format!("  <{root} {root}>"), &format!("    <{el}>"), "      \"xy\""]);
        assert_eq!(body(&format!("<span>{open}x</span>y{close}")), want, "{open}");
    }
    // The same for annotation-xml, an integration point or not.
    let want = lines(&["<span>", "  <math math>", "    <math annotation-xml>", "      \"xy\""]);
    assert_eq!(body("<span><math><annotation-xml>x</span>y</annotation-xml></math>"), want);
}

#[test]
fn a_new_list_item_in_a_special_foreign_element_does_not_close_the_outer_one() {
    assert_eq!(
        body("<li><svg><foreignObject><li>x"),
        lines(&["<li>", "  <svg svg>", "    <svg foreignObject>", "      <li>", "        \"x\""])
    );
}

#[test]
fn the_adoption_agency_stops_at_a_foreign_scope_border() {
    // The formatting element is not in scope, so the end tag does nothing.
    assert_eq!(body("<b><svg><foreignObject>x</b>y"), lines(&["<b>", "  <svg svg>", "    <svg foreignObject>", "      \"xy\""]));
    assert_eq!(body("<b><svg><desc><p>x</b>y"), lines(&["<b>", "  <svg svg>", "    <svg desc>", "      <p>", "        \"xy\""]));
    assert_eq!(body("<b><svg><title>x</b>y"), lines(&["<b>", "  <svg svg>", "    <svg title>", "      \"xy\""]));
    assert_eq!(body("<b><math><mi>x</b>y"), lines(&["<b>", "  <math math>", "    <math mi>", "      \"xy\""]));
    assert_eq!(body("<b><math><annotation-xml>x</b>y"), lines(&["<b>", "  <math math>", "    <math annotation-xml>", "      \"xy\""]));
    // An SVG element with a special HTML name is not special, and not a border.
    assert_eq!(body("<b><svg><section>x</b>y"), lines(&["<b>", "  <svg svg>", "    <svg section>", "      \"x\"", "\"y\""]));
    // Inside the integration point the algorithm works as in HTML.
    assert_eq!(
        body("<svg><foreignObject><b>1<p>2</b>3"),
        lines(&["<svg svg>", "  <svg foreignObject>", "    <b>", "      \"1\"", "    <p>", "      <b>", "        \"2\"", "      \"3\""])
    );
}

/// Pages that use the handshake, cut in every way.
const PAGES: &[&str] = &[
    "<svg><title><b>x</b></title><style>a &amp; b<g>c</g></style><![CDATA[a<b>&amp;]]]]>z</svg><p><![CDATA[q]]>",
    "<svg><foreignObject><style>&amp;<b></style><title>&amp;</title><plaintext>x</foreignObject>",
    "<math><mtext><textarea>&lt;<b></textarea></mtext><annotation-xml encoding=TEXT/HTML><xmp>&amp;</xmp></annotation-xml><![CDATA[x]]></math>",
    "<svg><![CDATA[x]]",
    "<svg><![CDATA[",
    "<svg><![CDATA[]]>a<![CDAT",
    "<svg><![cdata[x]]>y<![CDATA[z]>]]]]>",
    "<svg><script>1 &lt; <g/></script><p><![CDATA[x]]></p><svg><![CDATA[y]]></svg>",
];

#[test]
fn foreign_pages_in_pieces_give_the_same_tree() {
    for page in PAGES {
        let whole = parse(page).dump();
        let cuts: Vec<usize> = (0..=page.len()).filter(|&i| page.is_char_boundary(i)).collect();
        for &i in &cuts {
            assert_eq!(streamed(&[&page[..i], &page[i..]]), whole, "cut at {i} in {page:?}");
        }
        for (n, &i) in cuts.iter().enumerate() {
            for &j in &cuts[n..] {
                assert_eq!(streamed(&[&page[..i], &page[i..j], &page[j..]]), whole, "cuts at {i} and {j} in {page:?}");
            }
        }
        let letters: Vec<String> = page.chars().map(String::from).collect();
        let letters: Vec<&str> = letters.iter().map(String::as_str).collect();
        assert_eq!(streamed(&letters), whole, "one letter at a time: {page:?}");
    }
}

#[test]
fn a_long_cdata_is_not_read_again_for_each_piece() {
    let mut parser = Parser::new();
    parser.feed("<svg><![CDATA[");
    let start = std::time::Instant::now();
    for _ in 0..20_000 {
        parser.feed("xxxxx");
    }
    parser.feed("]]><g/>done");
    let dump = parser.finish().dump();
    assert!(dump.contains("<svg g>") && dump.contains("done"));
    assert!(start.elapsed().as_secs() < 2);
}

#[test]
fn foreign_junk_ends_and_gives_the_same_tree_in_pieces() {
    let pieces = [
        "<svg>", "</svg>", "<math>", "</math>", "<title>", "</title>", "<style>", "</style>", "<foreignObject>", "</foreignObject>", "<desc>", "<mi>", "</mi>",
        "<annotation-xml encoding=text/html>", "<![CDATA[", "]]>", "]]", "<![CDAT", "<textarea>", "</textarea>", "<plaintext>", "<b>", "</b>", "<p>", "<li>",
        "<span>", "</span>", "<table>", "<td>", "&amp;", "<g/>", "x", "<", ">", "\0",
    ];
    let mut seed: u32 = 99;
    for _ in 0..3000 {
        let mut page = String::new();
        for _ in 0..(seed % 24) {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            page.push_str(pieces[(seed >> 16) as usize % pieces.len()]);
        }
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        let whole = parse(&page).dump();
        let cut = (seed >> 16) as usize % (page.len() + 1);
        assert_eq!(streamed(&[&page[..cut], &page[cut..]]), whole, "cut at {cut} in {page:?}");
    }
}
