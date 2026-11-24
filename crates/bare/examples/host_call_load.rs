//! Scratch: the cost of one host call. `host_call_load MODE N`, MODE: nop, get, has, match, hcn, isc, dom (getAttribute on a fresh element's `parentNode`).
use bare::eng_new::Peek;
use bare::browser::*;
use bare_html::tree;
use bare::js_limits::*;
use bare::js_limits::HeapLimits;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (mode, n) = (args[1].clone(), args[2].parse::<usize>().unwrap());
    let t = std::thread::Builder::new().stack_size(8 << 20).spawn(move || exec(&mode, n)).unwrap();
    t.join().unwrap();
}

fn exec(mode: &str, n: usize) {
    let page = "<!doctype html><body><div id=a class=\"x y\" data-k=v><p class=z>t</p></div>";
    let mut b = Browser::new(tree::parse(page), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    let body = match mode {
        "nop" => "x = 1",
        "get" => "x = e.getAttribute('class')",
        "has" => "x = e.hasAttribute('data-k')",
        "match" => "x = e.matches('div.x')",
        "hcn" => "x = e.hasChildNodes()",
        "isc" => "x = e.isConnected",
        _ => panic!("mode"),
    };
    let src = format!("var e = document.getElementById('a'), x; for (var i = 0; i < {n}; i++) {{ {body} }} x");
    match b.run_script(&src) {
        Ok(v) => println!("{}", b.it.display(v)),
        Err(e) => panic!("{e:?}"),
    }
}
