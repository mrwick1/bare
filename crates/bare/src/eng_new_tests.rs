use super::*;
use crate::browser::{Browser, BrowserLimits};

fn big(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap();
}

fn with_browser(f: impl FnOnce(&mut Browser<New>) + Send + 'static) {
    big(move || {
        let mut b = Browser::<New>::make(
            bare_html::tree::parse("<!doctype html><div id=a data-item=one><span></span></div>"),
            BrowserLimits::default(),
            Default::default(),
            Default::default(),
        ).unwrap();
        f(&mut b);
    });
}

fn js(b: &mut Browser<New>, source: &str) -> Value {
    b.run_script(source).unwrap_or_else(|e| panic!("{source}: {e:?}"))
}

#[test]
fn reads_symbol_descriptions() {
    with_browser(|b| {
        let Val::Symbol(s) = New::kind(js(b, "Symbol('test')")) else { panic!("symbol") };
        let d = b.it.h_symbol_description(s).unwrap().unwrap();
        assert_eq!(b.it.h_to_string_lossy(d).unwrap(), "test");
        let Val::Symbol(s) = New::kind(js(b, "Symbol()")) else { panic!("symbol") };
        assert_eq!(b.it.h_symbol_description(s).unwrap(), None);
        let Val::Symbol(s) = New::kind(js(b, "Symbol('')")) else { panic!("symbol") };
        let d = b.it.h_symbol_description(s).unwrap().unwrap();
        assert_eq!(b.it.h_to_string_lossy(d).unwrap(), "");
    });
}

#[test]
fn reads_the_shared_iterator_prototype() {
    with_browser(|b| {
        let proto = b.it.iterator_prototype();
        let v = js(b, "Object.getPrototypeOf(Object.getPrototypeOf([][Symbol.iterator]()))");
        assert_eq!(proto, NV(v));
        let v = js(b, "Object.getPrototypeOf(Object.getPrototypeOf(new FormData().entries()))");
        assert_eq!(proto, NV(v));
    });
}

#[test]
fn identifies_constructors() {
    with_browser(|b| {
        for (source, expected) in [
            ("(() => 1)", false),
            ("({method() {}}).method", false),
            ("(function* () {})", false),
            ("(function () {})", true),
            ("(class {})", true),
            ("HTMLElement", true),
            ("Math.max", false),
        ] {
            let v = js(b, source);
            assert_eq!(b.it.is_constructor_value(v).unwrap(), expected, "{source}");
        }
    });
}

#[test]
fn inspects_objects_as_compact_json() {
    with_browser(|b| {
        let v = js(b, "let tree = {name:'test', list:[1,,NaN], ignored:undefined, get bad(){throw 0}}; tree.self=tree; tree");
        assert_eq!(b.it.inspect_o(v, 1000, &|_| None), r#"{"name":"test","list":[1,null,null],"bad":"[threw]","self":"[Circular]"}"#);
        assert_eq!(b.it.inspect_o(v, 3, &|_| None), "{\"n… (cut: 3 of 72 chars)");
    });
}

#[test]
fn inspects_collections_dates_and_nodes() {
    with_browser(|b| {
        let v = js(b, r#"
            const map = new Map([['a', 1], ['remove', 2], ['b', { n: 3 }]]);
            map.delete('remove');
            Map.prototype.entries = () => { throw 0 };
            const set = new Set([2, 1, 3]);
            set.delete(1);
            Set.prototype.values = () => { throw 0 };
            const date = new Date(0);
            date.toJSON = () => { throw 0 };
            [map, set, date, document.getElementById('a')]
        "#);
        assert_eq!(b.inspect(v, 1000), r#"[[["a",1],["b",{"n":3}]],[2,3],"1970-01-01T00:00:00.000Z","<div#a>"]"#);
    });
}

#[test]
fn supplies_host_filled_properties() {
    with_browser(|b| {
        let v = js(b, "let el = document.getElementById('a'); el.dataset.item + ':' + el.querySelectorAll('span')[0].tagName");
        assert_eq!(b.it.display_o(v), "one:SPAN");
    });
}

#[test]
fn uses_the_same_element_tags_as_the_old_engine() {
    with_browser(|b| {
        use crate::eng_old::Old;
        let mut old = Browser::<Old>::make(
            bare_html::tree::parse("<!doctype html>"),
            BrowserLimits::default(),
            Default::default(),
            Default::default(),
        ).unwrap();
        for (source, expected) in [
            ("String(document.createElement('div'))", "[object HTMLElement]"),
            ("HTMLDivElement; String(document.createElement('div'))", "[object HTMLDivElement]"),
        ] {
            let old_value = old.run_script(source).unwrap();
            assert_eq!(old.it.display_o(old_value), expected);
            let new_value = js(b, source);
            assert_eq!(b.it.display_o(new_value), expected);
        }
    });
}

#[test]
fn counts_reclaimed_cells_and_honors_extra_roots() {
    with_browser(|b| {
        b.collect(&[]).unwrap();
        let object = b.it.h_new_object(None).unwrap();
        let text = b.it.h_shared_text(&"x".repeat(16_384)).unwrap();
        let symbol = js(b, "Symbol()");

        let freed = b.collect(&[object.0, text.0, symbol]).unwrap();
        assert_eq!(freed.symbols, 0);
        let freed = b.collect(&[]).unwrap();
        assert!(freed.objects >= 1);
        assert!(freed.strings >= 1);
        assert_eq!(freed.symbols, 1);
        assert_eq!(b.collect(&[]).unwrap(), Collected::default());
    });
}

#[test]
fn reads_console_call_arguments() {
    with_browser(|b| {
        js(b, "console.log('message', 3)");
        let records = b.take_console_records();
        assert_eq!(records.len(), 1);
        let args: Vec<_> = records[0].args.iter().map(|&v| b.it.display_o(v)).collect();
        assert_eq!(args, ["message", "3"]);
    });
}

#[test]
fn tracks_external_bytes() {
    with_browser(|b| {
        b.collect(&[]).unwrap();
        let live = b.it.external_live();
        assert!(live > 0);
        assert!(!b.it.gc_due());
        b.it.add_external((4 << 20).max(live / 4) + 1);
        assert!(b.collect_due());
        b.collect(&[]).unwrap();
        assert!(!b.it.gc_due());
    });
}

#[test]
fn a_virtual_session_uses_its_clock_for_date_without_replay() {
    use crate::browser_any::AnyBrowser;
    use crate::browser_loader::Stop;
    use crate::browser_session::{Session, SessionConfig, SessionStop};
    use crate::resources::test_server::{Reply, serve};

    big(|| {
        assert!(!bare_net::vclock::on());
        let server = serve(|_| Reply::new(200, Some("text/html"), r#"
            <script>
                var wall = Date.now(), mono = performance.now(), elapsed;
                setTimeout(() => {
                    elapsed = [Date.now() - wall, Math.abs(performance.now() - mono - 25) < 1e-6, Date.now() === +new Date()].join(',');
                }, 25);
            </script>
        "#));
        let mut cfg = SessionConfig::new(&server.url("/"));
        cfg.js2 = true;
        cfg.virtual_clock = true;
        cfg.page.quiet = std::time::Duration::from_millis(100);
        let mut session = Session::new(cfg);
        let result = session.run();
        assert_eq!(result.stop, SessionStop::Page(Stop::Settled), "{result:?}");
        let Some(AnyBrowser::New(b)) = session.browser() else { panic!("js2 session") };
        let v = js(b, "elapsed");
        assert_eq!(b.it.display_o(v), "25,true,true");
    });
}

#[test]
fn uses_a_virtual_wall_clock_with_scheduled_tasks() {
    with_browser(|b| {
        b.use_virtual_clock_at(1000.0).unwrap();
        b.use_virtual_wall_clock_at(5_000.0).unwrap();
        assert!(b.use_virtual_wall_clock_at(5_000.5).is_err());
        let v = js(b, "let result; setTimeout(() => result = [Date.now(), +new Date(), performance.now()].join(','), 12); Date.now()");
        assert_eq!(b.it.display_o(v), "5000");
        b.advance_time(11.0, 10).unwrap();
        let v = js(b, "result");
        assert_eq!(b.it.display_o(v), "undefined");
        b.use_virtual_wall_clock_at(5_012.0).unwrap();
        b.advance_time(1.0, 10).unwrap();
        let v = js(b, "result");
        assert_eq!(b.it.display_o(v), "5012,5012,1012");
    });
}
