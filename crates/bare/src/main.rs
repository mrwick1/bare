//! bare: a browser written from scratch, to learn how browsers work.
//!
//! Usage: `cargo run -- http://example.com`
//!
//! It prints the DOM tree of the page. Add `--source` to print the source of the page instead,
//! `--styles` to print the computed style of each element, or `--layout` to print the place and size of each box.
//! `--resources` downloads the external scripts and style sheets of the page and lists them (see `resources.rs`).
//! `--run` loads the page and runs its classic scripts, its timers and `fetch` calls (see `browser_loader.rs`), then prints a short
//! sanitized status. It prints no source, headers, bodies or query strings.
//! `--width=900` sets the width of the window (1280 if not given).
//!
//! Comments compare Rust with JavaScript, because you know JS.

use bare_css::cascade::{self, Origin, RuleSet, Source};
use bare_html::{encoding, tree};
// The HTML crate: `encoding` turns bytes into text, and `tree` turns text into a DOM tree.
use bare_net::http;
// JS: like `import`. `bare_net` is another crate (package) in this project.
// It is listed in this crate's `Cargo.toml`, like a dependency in `package.json`.
use bare_url::Url;
use std::io::{Read, Write};

use bare::resources;
// Now we can write `Url` instead of `bare_url::Url`.

fn main() {
    // The program starts here.

    let args: Vec<String> = std::env::args().skip(1).collect();
    // All the words after the program name. JS: `process.argv.slice(2)`.
    let source = args.iter().any(|a| a == "--source");
    // Did the user type `--source`?
    let styles = args.iter().any(|a| a == "--styles");
    let layout = args.iter().any(|a| a == "--layout");
    let resources = args.iter().any(|a| a == "--resources");
    let run = args.iter().any(|a| a == "--run");
    let width = args
        .iter()
        .find_map(|a| a.strip_prefix("--width="))
        .and_then(|w| w.parse::<f32>().ok())
        .filter(|w| *w > 0.0);
    // `find_map`: the first flag that starts with `--width=`, without that start. JS: `args.find(...)?.slice(8)`.
    let viewport = (width.unwrap_or(VIEWPORT.0), VIEWPORT.1);
    let arg = match args.iter().find(|a| !a.starts_with("--")) {
        // Get the first word that is not a flag: the URL.
        // It may be missing. Rust makes us handle that case.
        // `match` is like `switch`, but it must handle every case.
        Some(a) => a,
        // The URL is there. Call it `a` and keep it.
        None => {
            // The URL is missing.
            eprintln!("usage: bare <http://url>");
            // Print a help message. JS: `console.error(...)`.
            std::process::exit(2);
            // Stop the program. A number other than 0 means "something went wrong".
        }
    };
    // `let` is like `const`: `arg` cannot change.

    if run {
        // The session loads the first document itself (one verified path for every document).
        if Url::parse(arg).is_err() {
            eprintln!("error: --run needs a valid URL");
            std::process::exit(2);
        }
        std::process::exit(run_page(arg.to_string()));
    }
    let url = Url::parse(&arg);
    let result = url
        .as_ref()
        // Split the URL into its parts. The `&` means "read it, but do not take it".
        // The answer is a `Result`: either a good value (`Ok`) or an error (`Err`).
        .map_err(|e| e.clone().into())
        // `|e| e.into()` is an arrow function: `e => e.into()`.
        // It changes the error into the kind of error that `http::get` uses.
        .and_then(|u| http::Client::new().get_stream(u));
    // If the URL was good, send the request and read the headers. The body is still on the connection.

    let mut streamed = match result {
        Ok(streamed) => streamed,
        Err(e) => {
            eprintln!("error: {e}");
            // It failed: print the error.
            std::process::exit(1);
            // Stop with an error number.
        }
    };
    if streamed.response.status != 200 {
        eprintln!("status: {}", streamed.response.status);
        // A 404 page still has a body, so we print it. We also tell you the status.
    }
    let mut decoder = encoding::Decoder::new(streamed.response.header("content-type"));
    // Find the encoding of the page, and turn the bytes into text, piece by piece.
    let mut parser = tree::Parser::new();
    let mut out = std::io::stdout().lock();
    let mut piece = vec![0u8; 64 * 1024];
    // We read the body in pieces of 64 KB. Each piece goes on at once: to the screen (`--source`)
    // or to the parser. The whole page is never in memory as bytes, and never as text.
    // JS: like `for await (const chunk of response.body)`.
    loop {
        let n = match streamed.body.read(&mut piece) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        };
        let text = decoder.feed(&piece[..n]);
        if source {
            out.write_all(text.as_bytes()).ok();
        } else {
            parser.feed(&text);
        }
    }
    let text = decoder.finish();
    if source {
        out.write_all(text.as_bytes()).ok();
        out.write_all(b"\n").ok();
        // `--source`: print the page as it is. JS: `console.log(text)`.
    } else {
        parser.feed(&text);
        let mut doc = parser.finish();
        // Build the DOM tree.
        drop(out);
        if resources {
            let Ok(page) = url.as_ref() else {
                eprintln!("error: --resources needs a valid URL");
                std::process::exit(2);
            };
            let (text, ok) = resources::report(&doc, page);
            print!("{text}");
            if !ok {
                std::process::exit(1);
            }
        } else if styles {
            let computed = computed_styles(&mut doc, url.as_ref().ok(), viewport);
            print!("{}", computed.dump(&doc));
            // `--styles`: the style of each element.
        } else if layout {
            let computed = computed_styles(&mut doc, url.as_ref().ok(), viewport);
            let result = bare_layout::layout(&doc, &computed, viewport);
            print!("{}", bare_layout::dump(&doc, &result));
            // `--layout`: each box, with its place and size.
        } else {
            print!("{}", doc.dump());
            // Print the tree, one line for each node.
        }
    }
}

/// The window is 1280 by 800 until module 5 has a window. `@media`, `vw`, `vh` and layout use it.
const VIEWPORT: (f32, f32) = (1280.0, 800.0);

/// `@import` goes this deep. A sheet that imports itself stops here.
const MAX_IMPORT_DEPTH: usize = 8;

/// Fetch a style sheet. One that fails gives `None`: the page still shows, like in a browser.
fn fetch_sheet(client: &mut http::Client, base: Option<&Url>, href: &str) -> Option<(Url, String)> {
    let fetched = base
        .ok_or_else(|| "no address for the page".to_string())
        .and_then(|p| p.join(href))
        .and_then(|u| client.get(&u).map(|r| (u, r)).map_err(|e| e.to_string()));
    match fetched {
        Ok((u, r)) if r.status == 200 => {
            Some((u, encoding::decode(&r.body, r.header("content-type"))))
        }
        Ok((_, r)) => {
            eprintln!("style sheet {href}: status {}", r.status);
            None
        }
        Err(e) => {
            eprintln!("style sheet {href}: {e}");
            None
        }
    }
}

/// Add a style sheet. The sheets that it imports go first, because an `@import` comes before the rules of the sheet.
fn add_sheet(
    rules: &mut RuleSet,
    doc: &mut bare_html::dom::Document,
    client: &mut http::Client,
    base: Option<&Url>,
    text: &str,
    layer: Option<&str>,
    depth: usize,
) {
    if depth < MAX_IMPORT_DEPTH {
        let imports = cascade::imports(doc, text, rules.viewport());
        for import in imports {
            if let Some((url, text)) = fetch_sheet(client, base, &import.href) {
                // A sheet that is imported in a layer, and imports another one in a layer, is in `outer.inner`.
                // (A layer with no name inside a layer is not kept apart: it is in the outer one.)
                let inner = match (layer, import.layer.as_deref()) {
                    (Some(outer), Some(name)) if !name.is_empty() => Some(format!("{outer}.{name}")),
                    (outer, None | Some("")) if outer.is_some() => outer.map(str::to_string),
                    (_, name) => name.map(str::to_string),
                };
                add_sheet(rules, doc, client, Some(&url), &text, inner.as_deref(), depth + 1);
            }
        }
    }
    rules.add_in_layer(doc, text, Origin::Author, layer);
}

/// Read the style sheets of the page (the `<style>` elements and the `<link rel=stylesheet>` that we can fetch),
/// and find the style of every element.
fn computed_styles(doc: &mut bare_html::dom::Document, page: Option<&Url>, viewport: (f32, f32)) -> cascade::Styles {
    let mut rules = RuleSet::new(viewport);
    rules.add(doc, cascade::UA_CSS, Origin::UserAgent);
    let mut client = http::Client::new();
    for source in cascade::sources(doc, viewport) {
        match source {
            Source::Inline(text) => add_sheet(&mut rules, doc, &mut client, page, &text, None, 0),
            Source::Link(href) => {
                if let Some((url, text)) = fetch_sheet(&mut client, page, &href) {
                    add_sheet(&mut rules, doc, &mut client, Some(&url), &text, None, 0);
                }
            }
        }
    }
    rules.compute(doc)
}

/// `--run`: a session over the real URL (document load, scripts, timers, `fetch`, navigations: `browser_session.rs`).
/// The VM needs a big stack: 8 MiB is the floor in release, a debug build uses more stack per frame.
/// Exit code: 0 settled or a budget ended it, 1 fatal or a failed load.
fn run_page(page: String) -> i32 {
    use bare::browser_session::{Session, SessionConfig, SessionStop};
    use bare::Stop;
    let stack = if cfg!(debug_assertions) { 64 << 20 } else { 8 << 20 };
    let worker = std::thread::Builder::new().stack_size(stack).spawn(move || {
        let mut s = Session::new(SessionConfig::new(&page));
        let r = s.run();
        println!("stop: {:?}", r.stop);
        println!("documents {}, tasks {}, rounds {}", r.pages.len(), r.tasks, r.rounds);
        for (u, p) in r.urls.iter().zip(&r.pages) {
            println!("document {u}: stop {:?}, ready {}", p.stop, p.ready_state);
            println!("  scripts run {}, threw {}, load errors {}, fetches {}, script bytes {}", p.scripts_run, p.script_throws, p.load_errors, p.fetches, p.bytes);
            for d in &p.diagnostics {
                println!("  note: {d}");
            }
            if p.dropped_diagnostics > 0 {
                println!("  note: {} more not shown", p.dropped_diagnostics);
            }
        }
        if let Some(b) = s.browser() {
            for d in b.take_fetch_diagnostics() {
                println!("fetch: {d}");
            }
        }
        if matches!(r.stop, SessionStop::Page(Stop::Fatal(_)) | SessionStop::LoadFailed(_)) { 1 } else { 0 }
    });
    worker.map_or(1, |w| w.join().unwrap_or(1))
}
