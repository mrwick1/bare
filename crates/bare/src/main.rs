//! bare: a browser written from scratch, to learn how browsers work.
//!
//! Usage: `cargo run -- http://example.com`
//!
//! It prints the DOM tree of the page. Add `--source` to print the source of the page instead,
//! or `--styles` to print the computed style of each element.
//!
//! Comments compare Rust with JavaScript, because you know JS.

use bare_css::cascade::{self, Origin, RuleSet, Source};
use bare_html::{encoding, tree};
// The HTML crate: `encoding` turns bytes into text, and `tree` turns text into a DOM tree.
use bare_net::http;
// JS: like `import`. `bare_net` is another crate (package) in this project.
// It is listed in this crate's `Cargo.toml`, like a dependency in `package.json`.
use bare_url::Url;
// Now we can write `Url` instead of `bare_url::Url`.

fn main() {
    // The program starts here.

    let args: Vec<String> = std::env::args().skip(1).collect();
    // All the words after the program name. JS: `process.argv.slice(2)`.
    let source = args.iter().any(|a| a == "--source");
    // Did the user type `--source`?
    let styles = args.iter().any(|a| a == "--styles");
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

    let url = Url::parse(&arg);
    let result = url
        .as_ref()
        // Split the URL into its parts. The `&` means "read it, but do not take it".
        // The answer is a `Result`: either a good value (`Ok`) or an error (`Err`).
        .map_err(|e| e.clone().into())
        // `|e| e.into()` is an arrow function: `e => e.into()`.
        // It changes the error into the kind of error that `http::get` uses.
        .and_then(|u| http::Client::new().get(u));
    // If the URL was good, fetch the page. If not, skip this step and keep the error.

    match result {
        Ok(response) => {
            if response.status != 200 {
                eprintln!("status: {}", response.status);
                // A 404 page still has a body, so we print it. We also tell you the status.
            }
            let text = encoding::decode(&response.body, response.header("content-type"));
            // Find the encoding of the page and turn the bytes into text.
            if source {
                println!("{text}");
                // `--source`: print the page as it is. JS: `console.log(text)`.
            } else {
                let mut doc = tree::parse(&text);
                // Build the DOM tree.
                if styles {
                    print!("{}", computed_styles(&mut doc, url.as_ref().ok()));
                    // `--styles`: the style of each element.
                } else {
                    print!("{}", doc.dump());
                    // Print the tree, one line for each node.
                }
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            // It failed: print the error.
            std::process::exit(1);
            // Stop with an error number.
        }
    }
}

/// The window is 1280 by 800 until module 5 has a window. `@media`, `vw` and `vh` use it.
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
        let imports = cascade::imports(doc, text, VIEWPORT);
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
/// find the style of every element, and give it as text.
fn computed_styles(doc: &mut bare_html::dom::Document, page: Option<&Url>) -> String {
    let mut rules = RuleSet::new(VIEWPORT);
    rules.add(doc, cascade::UA_CSS, Origin::UserAgent);
    let mut client = http::Client::new();
    for source in cascade::sources(doc, VIEWPORT) {
        match source {
            Source::Inline(text) => add_sheet(&mut rules, doc, &mut client, page, &text, None, 0),
            Source::Link(href) => {
                if let Some((url, text)) = fetch_sheet(&mut client, page, &href) {
                    add_sheet(&mut rules, doc, &mut client, Some(&url), &text, None, 0);
                }
            }
        }
    }
    rules.compute(doc).dump(doc)
}
