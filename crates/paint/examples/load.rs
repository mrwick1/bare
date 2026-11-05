//! Load test for paint: `cargo run --release -p bare-paint --example load -- <case> <n> [canvas height]`.
//!
//! It builds a page of `n` boxes or text lines (not timed), lays it out (timed, for scale), then paints it into a canvas
//! of 1280 by `height` pixels (800 if not given) and prints one line: the time of each step, the extra peak memory of
//! painting, and the size of the glyph cache. Peak memory is `VmHWM` of `/proc/self/status`, reset just before the paint
//! (`echo 5 > /proc/self/clear_refs`), so it counts only what the paint needs on top of the page. Each run is one process.
//!
//! Cases: `dense` (long text lines that fill the canvas), `boxes` (a div with a background and a border), `lines` (a paragraph of one text line), `mixed` (a card with
//! a heading, a row of inline boxes with a background, and text), `nested` (boxes 20 levels deep), `rounded` (a box with a
//! background, a 2 px border and `border-radius`), `clipped` (a box with `overflow: hidden` and a wide child), `stacked`
//! (positioned boxes with `z-index` from -3 to 3, and every third with `opacity`), `dashed` (a box with a dashed border),
//! `controls` (a row of an input with a value, a checkbox, a select and a button).

use bare_css::cascade::{Origin, RuleSet, UA_CSS};
use bare_paint::canvas::Canvas;
use bare_paint::{Painter, png};
use std::time::Instant;

fn status(key: &str) -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines().find_map(|l| l.strip_prefix(key)).and_then(|v| v.split_whitespace().next()).and_then(|v| v.parse().ok()).unwrap_or(0)
}

fn page(case: &str, n: usize) -> String {
    let body = match case {
        "boxes" => "<div class=b></div>".repeat(n),
        "lines" => "<p>The quick brown fox jumps over the lazy dog 0123456789</p>".repeat(n),
        "dense" => "<p>Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud 1234567890</p>".repeat(n),
        "mixed" => "<div class=card><h2>Title</h2><div><span class=chip>one</span> <span class=chip>two</span> some text here</div></div>".repeat(n),
        "nested" => {
            let one = format!("{}x{}", "<div class=n>".repeat(20), "</div>".repeat(20));
            one.repeat(n / 20 + 1)
        }
        "rounded" => "<div class=r></div>".repeat(n),
        "clipped" => "<div class=c><div class=w></div></div>".repeat(n),
        "dashed" => "<div class=d></div>".repeat(n),
        "controls" => "<div class=k><input value='someone@example.com'> <input type=checkbox checked> <select><option>One</option></select> <button>Go</button></div>".repeat(n),
        "stacked" => (0..n)
            .map(|i| {
                let (x, y) = ((i * 37) % 1200, (i * 91) % 760);
                let op = if i % 3 == 0 { "opacity:.8;" } else { "" };
                format!("<div class=s style='left:{x}px;top:{y}px;z-index:{};{op}background:#{:06x}'></div>", i as i32 % 7 - 3, (i * 2654435) & 0xffffff)
            })
            .collect(),
        _ => panic!("cases: dense, boxes, lines, mixed, nested, rounded, clipped, stacked, dashed, controls"),
    };
    format!(
        "<!doctype html><style>body{{margin:0}} .b{{height:10px;background:#abc;border:1px solid #333}} p{{margin:0}} \
         .card{{background:#fff;border:1px solid #ccc;padding:4px}} h2{{margin:0;font-size:18px}} .chip{{background:#eef;padding:0 3px}} \
         .n{{padding:1px;background:#f4f4f4;border-left:1px solid #999}} \
         .r{{height:20px;margin:2px;background:#cde;border:2px solid #357;border-radius:8px}} .c{{height:20px;overflow:hidden;background:#eee}} .w{{width:3000px;height:10px;background:#9ab}} \
         .d{{height:10px;border:2px dashed #333}} .k{{height:30px}} .s{{position:absolute;width:40px;height:30px}}</style>{body}"
    )
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (case, n) = (args[0].as_str(), args[1].parse::<usize>().unwrap());
    let height = args.get(2).map_or(800, |h| h.parse::<u32>().unwrap());
    let html = page(case, n);
    let viewport = (1280.0, 800.0);
    let t = Instant::now();
    let mut doc = bare_html::tree::parse(&html);
    let mut rules = RuleSet::new(viewport);
    rules.add(&mut doc, UA_CSS, Origin::UserAgent);
    for source in bare_css::cascade::sources(&doc, viewport) {
        if let bare_css::cascade::Source::Inline(text) = source {
            rules.add(&mut doc, &text, Origin::Author);
        }
    }
    let styles = rules.compute(&doc);
    let parse = ms(t);
    let t = Instant::now();
    let layout = bare_layout::layout(&doc, &styles, viewport);
    let lay = ms(t);

    let mut canvas = Canvas::new(1280, height);
    let mut painter = Painter::new();
    std::fs::write("/proc/self/clear_refs", "5").ok();
    let before = status("VmRSS:");
    let t = Instant::now();
    painter.list.build(&doc, &styles, &layout, (1280, height));
    let build = ms(t);
    let t = Instant::now();
    painter.draw(&mut canvas);
    let raster = ms(t);
    let peak = status("VmHWM:");
    // The second frame: the glyph cache and all buffers are warm.
    let t = Instant::now();
    painter.paint(&doc, &styles, &layout, &mut canvas);
    let again = ms(t);
    let t = Instant::now();
    let mut bytes = Vec::new();
    png::encode(&canvas, &mut bytes).unwrap();
    let encode = ms(t);
    let (glyphs, mask_bytes) = painter.glyphs.stats();
    println!(
        "| {case} | {n} | {height} | {} | {parse:.1} | {lay:.1} | {build:.2} | {raster:.2} | {again:.2} | {encode:.1} | {:.0} | {:.0} | {} | {} | {} |",
        layout.fragments.len(),
        peak.saturating_sub(before) as f64,
        (painter.list.items.capacity() * std::mem::size_of::<bare_paint::display::Item>() + painter.list.text.capacity()) as f64 / 1024.0,
        glyphs,
        mask_bytes,
        painter.list.items.len(),
    );
    let _ = (bytes.len(), canvas.hash());
}
