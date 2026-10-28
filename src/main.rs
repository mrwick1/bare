//! bare: a browser written from scratch, to learn how browsers work.
//!
//! Usage: `cargo run -- http://example.com`
//!
//! Comments compare Rust with JavaScript, because you know JS.

mod http;
// JS: like `import "./http.js"`. Rust only compiles files you list with `mod`.
mod idna;
// Internationalized domain names: "bücher.de" to "xn--bcher-kva.de".
mod unicode;
mod unicode_tables;
// Unicode data and normalization, used by `idna`.
mod url;
// Same for `src/url.rs`.

use url::Url;
// JS: like `import { Url } from "./url"`. Now we can write `Url`.

fn main() {
    // JS: like the top of your script. Rust runs `main` first.

    let arg = match std::env::args().nth(1) {
        // JS: like `process.argv[2]`. In Rust, item 0 is the program and item 1 is your first argument.
        // `.nth(1)` returns an `Option`. JS would give `string | undefined`.
        // The difference: Rust forces you to handle the "missing" case. You cannot forget it.
        // `match` is like `switch`, but it must cover every case, and it gives back a value.
        Some(a) => a,
        // The argument exists. Call it `a` and use it as the value of the `match`.
        None => {
            // The argument is missing.
            eprintln!("usage: bare <http://url>");
            // JS: `console.error(...)`. The `!` means it is a macro. Treat it like a function.
            std::process::exit(2);
            // JS: `process.exit(2)`. Code 0 means OK. Anything else means failure.
        }
    };
    // `arg` is now a plain string. `let` is like `const`: you cannot change it.

    let result = Url::parse(&arg)
        // JS: like a static method call, `Url.parse(arg)`.
        // `&arg` means "lend the value, do not give it away". We still own `arg`.
        // The result is a `Result`: it holds either a good value or an error.
        // JS has no direct match. It is like a function that returns `{ ok }` or `{ err }` instead of `throw`.
        .map_err(|e| e.into())
        // `|e| e.into()` is an arrow function: `e => e.into()`.
        // `map_err` runs it on the error only. A good value passes through.
        // It converts our error text to the error type that `http::get` uses.
        .and_then(|u| http::get(&u));
    // JS: like `.then(u => get(u))`. It runs only if the earlier step succeeded.
    // If `parse` failed, the error skips this step, like a rejected Promise skips `.then`.

    match result {
        // Check what happened.
        Ok(body) => println!("{body}"),
        // Success: print the page. JS: `console.log(body)`. `{body}` is like `${body}` in a template string.
        Err(e) => {
            // Failure.
            eprintln!("error: {e}");
            // Print the error to stderr.
            std::process::exit(1);
            // Exit with code 1, so scripts can see that it failed.
        }
    }
}
