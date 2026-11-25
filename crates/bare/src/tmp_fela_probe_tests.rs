// TMP probe: never merge.
use crate::browser::*;
use crate::eng_new::Peek;
use crate::js_limits::{EvalLimits, HeapLimits};
use bare_html::tree;

#[test]
fn tmp_fela_insert_cost() {
    let mut b = Browser::new(tree::parse("<!doctype html><style id=s></style><div id=d></div>"), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000_000, ..Default::default() }, HeapLimits::default()).unwrap();
    b.use_virtual_clock_at(1000.0).unwrap();
    for n in [1000usize, 2000, 4000] {
        let src = format!("var st = document.getElementById('s'), dv = document.getElementById('d'); st.textContent = ''; var si = {{}}, order = [/:hover/, /:focus/, /:active/];
        function ins(sel, pseudo) {{ var m = 0===pseudo.length ? 0 : order.indexOf(order.find(function(e){{return null!==pseudo.match(e)}}))+1;
          var f = st.sheet.cssRules, h = f.length;
          if (0===m) h = void 0===si[''] ? 0 : si['']+1; else for (var g = si['']||0, p = f.length; g < p; ++g) if (f[g].score > m) {{ h = g; break }}
          st.sheet.insertRule(sel + '{{color:red}}', h); 0===m && (si[''] = h); f[h].score = m; }}
        var t0 = Date.now(); for (var i = 0; i < {n}; i++) {{ ins('.c' + i + ['', ':hover', ':focus'][i % 3], ['', ':hover', ':focus'][i % 3]); if (i % 10 == 0) dv.appendChild(document.createElement('i')); }}
        st.sheet.cssRules.length");
        let t = std::time::Instant::now();
        let r = b.run_script(&src).unwrap();
        eprintln!("FELA n={n} len={} ms={}", b.it.display(r), t.elapsed().as_millis());
    }
}
