//! bare: a browser written from scratch, to learn how browsers work.
//!
//! Usage: `cargo run -- http://example.com`
//!
//! Comments compare Rust with JavaScript, because you know JS.

use bare_net::http;
// JS: like `import`. `bare_net` is another crate (package) in this project.
// It is listed in this crate's `Cargo.toml`, like a dependency in `package.json`.
use bare_url::Url;
// Now we can write `Url` instead of `bare_url::Url`.

fn main() {
    // The program starts here.

    let arg = match std::env::args().nth(1) {
        // Get the first word typed after the program name: the URL.
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

    let result = Url::parse(&arg)
        // Split the URL into its parts. The `&` means "read it, but do not take it".
        // The answer is a `Result`: either a good value (`Ok`) or an error (`Err`).
        .map_err(|e| e.into())
        // `|e| e.into()` is an arrow function: `e => e.into()`.
        // It changes the error into the kind of error that `http::get` uses.
        .and_then(|u| http::Client::new().get(&u));
    // If the URL was good, fetch the page. If not, skip this step and keep the error.

    match result {
        Ok(response) => {
            if response.status != 200 {
                eprintln!("status: {}", response.status);
                // A 404 page still has a body, so we print it. We also tell you the status.
            }
            println!("{}", String::from_utf8_lossy(&response.body));
            // It worked: print the page. Bad bytes become "�". JS: `console.log(text)`.
        }
        Err(e) => {
            eprintln!("error: {e}");
            // It failed: print the error.
            std::process::exit(1);
            // Stop with an error number.
        }
    }
}
