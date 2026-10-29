//! The fuzzer. It makes random input from a few good seeds, and runs it through a parser.
//! It looks for panics, for hangs (more than 5 seconds for one input), for memory blowups, and for
//! trees or URLs that break a rule that must always hold.
//!
//! The random numbers come from a seed, so a run can be repeated: the same seed gives the same inputs.

use bare_css::cascade::{self, Origin, RuleSet, UA_CSS};
use bare_html::dom::{Document, NodeId};
use bare_html::tree;
use bare_url::Url;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

/// The input of the run that is going now. The watchdog and the allocator save it when something goes wrong.
static CURRENT: Mutex<(String, Vec<u8>)> = Mutex::new((String::new(), Vec::new()));
/// When the current input started, in milliseconds since the program started. 0 means none runs.
static STARTED: AtomicU64 = AtomicU64::new(0);
static SAVING: AtomicBool = AtomicBool::new(false);

/// Write the current input to a file. Called when the process is about to stop.
pub fn save_current_input() {
    if SAVING.swap(true, Relaxed) {
        return;
    }
    // The program is over its memory limit, and the file write needs memory too. Reset the count.
    crate::LIVE.store(0, Relaxed);
    if let Ok(cur) = CURRENT.try_lock() {
        if !cur.1.is_empty() {
            let path = format!("crash-{}-last.txt", cur.0);
            std::fs::write(&path, &cur.1).ok();
            eprintln!("input saved to {path}");
        }
    }
}

/// A small, fast random number generator (xorshift64*). JS: `Math.random()`, but with a seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545F4914F6CDD1D)
    }
    /// A number from 0 to `n - 1`.
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

const HTML_SEEDS: &[&str] = &[
    "<!doctype html><html><head><title>t</title><style>p{color:red}</style></head><body><p class=a id=b>x<b>y<i>z</b>w</i></p></body></html>",
    "<table><tr><td>a<td>b<tr><th>c</table><b>x<table><td><i>y</table>",
    "<ul><li>a<li>b<ul><li>c</ul></ul><p>one<p>two<div>three</div>",
    "<a href=x>1<a href=y>2</a></a><nobr>a<nobr>b<b><p>c</b>d</p>",
    "<select><option>a<option>b</select><textarea><b>x</textarea><script>if(a<b){}</script>",
    "&amp;&lt;&#65;&#x41;&notin;&nbsp&copy;&#0;&#x110000;<!-- c --><!x><?y>",
    "<svg><circle/></svg><math><mi>x</mi></math><template><td>x</template><frameset></frameset>",
    "<form><input name=a><button>b</button></form><h1>a<h2>b</h1>c<br></br></p>",
    "<div style='color:red;width:calc(1px + 2em)'><span class='a b c'>x</span></div>",
];

const HTML_WORDS: &[&str] = &[
    "<b>", "</b>", "<i>", "</i>", "<p>", "</p>", "<a href=x>", "</a>", "<table>", "</table>", "<tr>", "<td>", "<th>", "<div>", "</div>", "<span>", "<li>", "<ul>", "<nobr>", "<font>", "<em>", "<strong>", "<h1>", "<h6>", "<select>",
    "<textarea>", "<script>", "</script>", "<style>", "<title>", "<body>", "<html>", "<head>", "<caption>", "<col>", "<tbody>", "<form>", "<button>", "<svg>", "<math>", "<template>", "<!--", "-->", "<!", "<?", "&", "&amp;", "&#", "&#x", ";",
    "=", "\"", "'", "<", ">", "/", " ", "\n", "\0", "\u{fffd}", "\u{10ffff}", "<br>", "<img src=x>", "<p/>", "</", "<!doctype html>", "<![CDATA[", "]]>", "style=\"", "class=",
];

const CSS_SEEDS: &[&str] = &[
    "p { color: red; margin: 1px 2em 3% auto } .a > b + c ~ d { width: calc(100% - 2*3px) !important }",
    "@media (min-width: 100px) and (max-width: 2000px) { p { color: blue } } @supports (color: red) { a { color: red } }",
    "@layer a, b.c; @layer a { p { color: red } } @layer { q { color: blue } } @import url(x.css) layer(z) supports(color: red) screen;",
    ":root { --a: 1px; --b: var(--a) var(--c, 2px) } p { width: var(--b); margin: var(--x, var(--y, 3px)) }",
    "li:nth-child(2n+1):not(.a, #b):is(a, b > c):where(d e)::before { color: #fff } a[href^='x' i] { top: 0 }",
    "p { font: italic bold 12px/1.5 serif; border: 1px solid rgb(1 2 3 / 50%); background: hsl(120deg 50% 50%) } a { color: currentcolor }",
    "@font-face { font-family: x; src: url(y) } @keyframes k { from { top: 0 } to { top: 1px } } @page { margin: 1in } @container (min-width: 1px) { p { color: red } }",
    "p { width: calc(calc(1px + 2px) * 3) ; height: calc(1em + 50% - 2vw); margin: -.5e1px +3px } /* c */ a { content: \"\\41 \" }",
];

const CSS_WORDS: &[&str] = &[
    "{", "}", "(", ")", "[", "]", ";", ":", ",", ">", "+", "~", "*", ".", "#", "@", "!", "\"", "'", "\\", "/*", "*/", "<!--", "-->", "url(", "calc(", "var(", "--a", "var(--a)", ":is(", ":not(", ":where(", ":nth-child(", "2n+1", "@media",
    "@supports", "@layer", "@import", "@container", "screen", "and", "not", "or", "!important", "px", "em", "%", "vw", "1e999", "-1", "0", " ", "\n", "p", "a", "color", "width", "red", "inherit", "initial", "unset", "\0", "\u{10ffff}",
];

const URL_SEEDS: &[&str] = &[
    "http://example.com/a/b?c=d#e",
    "http://user:pw@example.com:8080/%41/../x",
    "http://[::1]:80/",
    "HTTP://EXAMPLE.com./",
    "http://\u{fc}ber.example/",
    "http://xn--bcher-kva.example/",
    "//host/path",
    "../a/./b",
    "?q",
    "#f",
    "http://1.2.3.4/",
    "http://0x7f.1/",
    "http://a b/",
    "/a\\b",
];

const URL_WORDS: &[&str] = &[
    "http://", "https://", "//", "/", "..", ".", "%", "%2e", "%00", "%zz", "?", "#", "@", ":", "[", "]", "::", "\\", " ", "\t", "\n", "xn--", "\u{fc}", "\u{200b}", "\u{3002}", "\u{ff0e}", "0", "65536", "99999999999", ".", "-", "_", "\0",
];

/// Make one input: pick a seed, and break it in a few ways.
fn mutate(rng: &mut Rng, seeds: &[&str], words: &[&str]) -> Vec<u8> {
    let mut data = seeds[rng.below(seeds.len())].as_bytes().to_vec();
    for _ in 0..1 + rng.below(8) {
        let at = rng.below(data.len() + 1);
        match rng.below(8) {
            0 if !data.is_empty() => {
                let i = at.min(data.len() - 1);
                data[i] ^= 1 << rng.below(8);
            }
            1 if !data.is_empty() => {
                let i = at.min(data.len() - 1);
                data[i] = rng.next() as u8;
            }
            2 | 3 => {
                let w = words[rng.below(words.len())].as_bytes();
                data.splice(at..at, w.iter().copied());
            }
            4 => {
                // Delete a range.
                let end = (at + rng.below(20)).min(data.len());
                data.drain(at..end);
            }
            5 => {
                // Repeat one word many times: deep nesting and long runs.
                let w = words[rng.below(words.len())].as_bytes().repeat(1 + rng.below(600));
                data.splice(at..at, w);
            }
            6 => {
                // Copy a piece to another place.
                let end = (at + rng.below(60)).min(data.len());
                let piece = data[at..end].to_vec();
                let to = rng.below(data.len() + 1);
                data.splice(to..to, piece);
            }
            7 => {
                // Mix in another seed.
                let other = seeds[rng.below(seeds.len())].as_bytes();
                data.splice(at..at, other.iter().copied());
            }
            _ => data.truncate(at),
        }
        if data.len() > 200_000 {
            data.truncate(200_000);
        }
    }
    data
}

pub fn run(args: &[String]) {
    let target = args.first().map(String::as_str).unwrap_or("");
    let iterations: usize = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(10_000);
    let seed: u64 = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(1);
    let (seeds, words, work): (&[&str], &[&str], fn(&[u8]) -> Result<(), String>) = match target {
        "html" => (HTML_SEEDS, HTML_WORDS, fuzz_html),
        "css" => (CSS_SEEDS, CSS_WORDS, fuzz_css),
        "url" => (URL_SEEDS, URL_WORDS, fuzz_url),
        _ => return eprintln!("targets: html css url"),
    };

    // The watchdog: a thread that stops the program when one input takes more than 5 seconds.
    let begin = Instant::now();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_millis(100));
            let started = STARTED.load(Relaxed);
            if started != 0 && begin.elapsed().as_millis() as u64 - started > 5000 {
                eprintln!("HANG: one input ran for more than 5 seconds");
                save_current_input();
                std::process::exit(4);
            }
        }
    });
    std::panic::set_hook(Box::new(|info| eprintln!("PANIC: {info}\n{}", std::backtrace::Backtrace::force_capture())));

    let mut rng = Rng(seed.wrapping_mul(0x9E3779B97F4A7C15) | 1);
    let (mut failures, mut slowest) = (0, Duration::ZERO);
    for i in 0..iterations {
        let input = mutate(&mut rng, seeds, words);
        {
            let mut cur = CURRENT.lock().unwrap();
            cur.0 = target.to_string();
            cur.1.clone_from(&input);
        }
        let t = Instant::now();
        STARTED.store((begin.elapsed().as_millis() as u64).max(1), Relaxed);
        let result = catch_unwind(AssertUnwindSafe(|| work(&input)));
        STARTED.store(0, Relaxed);
        slowest = slowest.max(t.elapsed());
        let problem = match result {
            Err(_) => Some("panic".to_string()),
            Ok(Err(why)) => Some(why),
            Ok(Ok(())) => None,
        };
        if let Some(why) = problem {
            failures += 1;
            let path = format!("crash-{target}-{seed}-{i}.txt");
            std::fs::write(&path, &input).ok();
            eprintln!("FAIL {why}: input saved to {path} ({} bytes)", input.len());
            if failures >= 10 {
                break;
            }
        }
    }
    println!("{target}: {iterations} inputs, seed {seed}, {failures} failures, slowest {:.0} ms", slowest.as_secs_f64() * 1000.0);
    if failures > 0 {
        std::process::exit(1);
    }
}

/// Parse a page, and check that the tree is sound: every link agrees with its twin.
fn fuzz_html(input: &[u8]) -> Result<(), String> {
    let text = bare_html::encoding::decode(input, None);
    let doc = tree::parse(&text);
    check_tree(&doc)
}

fn check_tree(doc: &Document) -> Result<(), String> {
    let mut seen = 0;
    let mut stack = vec![doc.root()];
    while let Some(node) = stack.pop() {
        seen += 1;
        if seen > doc.len() {
            return Err("tree has a loop: more nodes reached than exist".into());
        }
        let mut prev: Option<NodeId> = None;
        for child in doc.children(node) {
            if doc.parent(child) != Some(node) {
                return Err("a child does not point to its parent".into());
            }
            if doc.prev_sibling(child) != prev {
                return Err("prev_sibling does not match the order".into());
            }
            if let Some(p) = prev {
                if doc.next_sibling(p) != Some(child) {
                    return Err("next_sibling does not match the order".into());
                }
            }
            prev = Some(child);
            stack.push(child);
        }
        if doc.last_child(node) != prev {
            return Err("last_child is not the last child".into());
        }
    }
    Ok(())
}

/// A style sheet, as a sheet and as a `style` attribute, on a small page.
fn fuzz_css(input: &[u8]) -> Result<(), String> {
    let sheet = String::from_utf8_lossy(input);
    let inline = sheet.replace(['\'', '&', '<'], " ");
    let page = format!("<div id=b class='a c'><ul><li>1<li class=a>2<li>3</ul><p style='{inline}'>x<b>y</b><a href=x>z</a></div>");
    let mut doc = tree::parse(&page);
    let mut rules = RuleSet::new((1280.0, 800.0));
    rules.add(&mut doc, UA_CSS, Origin::UserAgent);
    rules.add(&mut doc, &sheet, Origin::Author);
    let styles = rules.compute(&doc);
    std::hint::black_box(styles.dump(&doc));
    std::hint::black_box(cascade::imports(&mut doc, &sheet, (1280.0, 800.0)));
    Ok(())
}

/// Parse a URL. A good URL must stay the same when we write it down and parse it again.
fn fuzz_url(input: &[u8]) -> Result<(), String> {
    let text = String::from_utf8_lossy(input);
    let mut mid = text.len() / 2;
    while !text.is_char_boundary(mid) {
        mid -= 1;
    }
    let (first, second) = text.split_at(mid);
    for (base, link) in [(&*text, second), (first, &*text)] {
        let Ok(url) = Url::parse(base) else { continue };
        if !url.path.starts_with('/') {
            return Err(format!("path does not start with a slash: {:?}", url.path));
        }
        if url.host.is_empty() {
            return Err("empty host".into());
        }
        if !url.host.contains(':') {
            let again = Url::parse(&format!("http://{}:{}{}", url.host, url.port, url.path));
            if again.as_ref() != Ok(&url) {
                return Err(format!("not stable: {url:?} then {again:?}"));
            }
        }
        if let Ok(joined) = url.join(link) {
            if !joined.path.starts_with('/') || joined.host.is_empty() {
                return Err(format!("bad join: {joined:?}"));
            }
        }
    }
    Ok(())
}
