//! `BroadcastChannel` (HTML Standard, "Broadcasting to other browsing contexts") inside one page (`browser_broadcast.rs`).
//! Expectations come from the HTML Standard and Chrome 155 on Linux. Microsoft Teams makes one in a class field of its telemetry service.

use crate::browser::*;
use crate::eng::Ops;
use crate::eng_new::New;
use bare_html::tree;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn page() -> Browser<New> {
    let mut b = Browser::<New>::make(tree::parse("<!doctype html><html><body></body></html>"), BrowserLimits::default(), Default::default(), Default::default()).expect("browser");
    b.set_document_url("https://a.test/x").expect("url");
    b.use_virtual_clock_at(1_000_000.0).unwrap();
    b
}

fn js(b: &mut Browser<New>, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display_o(v),
        Err(e) => format!("fault:{e:?}"),
    }
}

/// Run `src`, then the tasks it queued, then return the value of `out`.
fn run(src: &str, out: &str) -> String {
    big({
        let (src, out) = (src.to_string(), out.to_string());
        move || {
            let mut b = page();
            let r = js(&mut b, &src);
            assert!(!r.starts_with("fault"), "{r}");
            b.advance_time(0.0, 1_000_000).expect("advance");
            js(&mut b, &out)
        }
    })
}

#[test]
fn the_interface_and_its_members() {
    let src = "var c = new BroadcastChannel('n'); var t = [typeof BroadcastChannel, c.name, String(c), c instanceof EventTarget,
        typeof c.postMessage, typeof c.close, c.onmessage, c.onmessageerror, BroadcastChannel.length].join(); 1";
    assert_eq!(run(src, "t"), "function,n,[object BroadcastChannel],true,function,function,,,1");
    assert_eq!(run("var t = String(new BroadcastChannel(42).name); 1", "t"), "42");
    let errs = "var t = []; try { BroadcastChannel('n') } catch (e) { t.push(e.name) } try { new BroadcastChannel() } catch (e) { t.push(e.name) } 1";
    assert_eq!(run(errs, "t.join()"), "TypeError,TypeError");
}

#[test]
fn a_message_goes_to_the_other_channels_of_the_name_and_not_to_the_sender() {
    let src = "var log = []; var a = new BroadcastChannel('n'), b = new BroadcastChannel('n'), c = new BroadcastChannel('n'), o = new BroadcastChannel('other');
        a.onmessage = function () { log.push('a') };
        b.onmessage = function (e) { log.push('b:' + e.data.k + ':' + e.origin + ':' + (e.source === null) + ':' + (e.target === b)) };
        c.addEventListener('message', function (e) { log.push('c:' + e.data.k) });
        o.onmessage = function () { log.push('o') };
        var sent = { k: 7 }; a.postMessage(sent); log.push('sync'); 1";
    assert_eq!(run(src, "log.join()"), "sync,b:7:https://a.test:true:true,c:7");
}

#[test]
fn each_receiver_gets_its_own_clone_and_a_function_cannot_be_sent() {
    let src = "var got = []; var a = new BroadcastChannel('n'), b = new BroadcastChannel('n'), c = new BroadcastChannel('n');
        b.onmessage = function (e) { got.push(e.data) }; c.onmessage = function (e) { got.push(e.data) };
        var sent = { k: [1, 2] }; a.postMessage(sent); var err = ''; try { a.postMessage(function () {}) } catch (e) { err = e.name } 1";
    assert_eq!(run(src, "[got.length, got[0] !== got[1], got[0] !== sent, got[0].k.join('-'), err].join()"), "2,true,true,1-2,DataCloneError");
}

#[test]
fn close_stops_sending_and_receiving() {
    let src = "var log = []; var a = new BroadcastChannel('n'), b = new BroadcastChannel('n');
        b.onmessage = function (e) { log.push(e.data) };
        a.postMessage('queued before close'); b.close(); b.close(); a.postMessage('after');
        var err = ''; try { b.postMessage('x') } catch (e) { err = e.name }
        var c = new BroadcastChannel('n'); c.onmessage = function (e) { log.push('c:' + e.data) }; a.postMessage('to c'); 1";
    assert_eq!(run(src, "log.join() + '|' + err"), "c:to c|InvalidStateError");
}

#[test]
fn an_open_channel_with_a_listener_lives_without_a_reference() {
    let src = "var log = []; (function () { var b = new BroadcastChannel('n'); b.onmessage = function (e) { log.push(e.data) } })(); 1";
    let out = big(move || {
        let mut b = page();
        js(&mut b, src);
        b.collect(&[]).expect("collect");
        js(&mut b, "new BroadcastChannel('n').postMessage('hi')");
        b.advance_time(0.0, 1_000_000).expect("advance");
        js(&mut b, "log.join()")
    });
    assert_eq!(out, "hi");
}

#[test]
fn the_old_engine_has_it_too() {
    big(|| {
        let mut b = Browser::new(tree::parse("<!doctype html><html><body></body></html>"), BrowserLimits::default(), Default::default(), Default::default()).expect("browser");
        b.use_virtual_clock_at(1_000_000.0).unwrap();
        b.run_script("var log = []; var a = new BroadcastChannel('n'), b = new BroadcastChannel('n'); b.onmessage = function (e) { log.push(e.data) }; a.postMessage('x')").expect("script");
        b.advance_time(0.0, 1_000_000).expect("advance");
        let v = b.run_script("log.join()").expect("script");
        assert_eq!(b.it.display(v), "x");
    });
}
