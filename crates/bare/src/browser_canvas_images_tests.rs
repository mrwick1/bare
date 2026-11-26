//! Tests of the pixels of the 2D context: `getImageData`, `putImageData`, `drawImage`, `toDataURL`, `toBlob` and `createImageBitmap`
//! (`browser_canvas.rs`). The expected values come from Chrome (headless shell 1254) for the same script.

use crate::browser::*;
use crate::eng_new::NewFault as Error;
use crate::eng_new::Peek;
use crate::js_limits::{EvalLimits, HeapLimits};
use bare_html::tree;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn browser() -> Browser {
    let mut b = Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock();
    b
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if crate::eng_new::is_object(v) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(e) => format!("other:{e:?}"),
    }
}

/// Run `body` (a function body that gives a string or a promise of one), then the tasks, and give the result.
fn run(body: &str) -> String {
    let body = body.to_string();
    big(move || {
        let mut b = browser();
        js(&mut b, &format!("function cv(w, h) {{ var c = document.createElement('canvas'); c.width = w; c.height = h; return c }}
            var out = 'pending'; Promise.resolve((function () {{ {body} }})()).then(function (v) {{ out = String(v) }}, function (e) {{ out = 'throw:' + e.name + ':' + e.message }})"));
        b.run_scheduled_tasks(10).unwrap();
        js(&mut b, "out")
    })
}

#[test]
fn get_image_data_reads_the_drawn_pixels() {
    let got = run("var c = cv(4, 2), x = c.getContext('2d'); x.fillStyle = '#ff0000'; x.fillRect(0, 0, 2, 2); x.fillStyle = 'rgba(0,0,255,0.5)';
        x.fillRect(1, 0, 2, 1); x.clearRect(3, 1, 1, 1); return Array.from(x.getImageData(-1, 0, 6, 1).data).join()");
    assert_eq!(got, "0,0,0,0,255,0,0,255,127,0,128,255,0,0,255,128,0,0,0,0,0,0,0,0");
}

#[test]
fn put_image_data_replaces_pixels_in_the_dirty_rectangle() {
    let got = run("var c = cv(4, 2), x = c.getContext('2d'); x.fillRect(0, 0, 4, 2); var d = x.createImageData(2, 2);
        for (var i = 0; i < 16; i++) d.data[i] = i * 16 + 15; x.putImageData(d, 1, 0, 1, 0, 1, 2); x.putImageData(d, -1, 1, 0, 0, 2, 1);
        return Array.from(x.getImageData(0, 0, 4, 2).data).join()");
    assert_eq!(got, "0,0,0,255,0,0,0,255,78,94,110,127,0,0,0,255,78,94,110,127,0,0,0,255,207,223,239,255,0,0,0,255");
}

#[test]
fn draw_image_scales_a_canvas_with_and_without_smoothing() {
    let got = run("var s = cv(2, 2), y = s.getContext('2d'); y.fillStyle = '#f00'; y.fillRect(0, 0, 1, 1); y.fillStyle = '#0f0'; y.fillRect(1, 0, 1, 1);
        y.fillStyle = '#00f'; y.fillRect(0, 1, 1, 1); y.fillStyle = '#fff'; y.fillRect(1, 1, 1, 1);
        var c = cv(4, 4), x = c.getContext('2d'); x.imageSmoothingEnabled = false; x.drawImage(s, 0, 0, 4, 4); x.drawImage(s, 1, 0, 1, 2, 0, 0, 1, 1);
        var r = Array.from(x.getImageData(0, 0, 4, 1).data).join(); x.imageSmoothingEnabled = true; x.clearRect(0, 0, 4, 4); x.drawImage(s, 0, 0, 4, 4);
        x.drawImage(s, 3, 3); return r + ' | ' + Array.from(x.getImageData(0, 1, 4, 1).data).join() + ' | ' + Array.from(x.getImageData(3, 3, 1, 1).data).join()");
    assert_eq!(got, "0,255,0,255,255,0,0,255,0,255,0,255,0,255,0,255 | 191,0,63,255,159,63,63,255,95,191,63,255,63,255,63,255 | 255,0,0,255");
}

#[test]
fn draw_image_of_a_canvas_on_itself_uses_the_old_pixels() {
    let got = run("var c = cv(2, 1), x = c.getContext('2d'); x.fillStyle = '#f00'; x.fillRect(0, 0, 1, 1); x.drawImage(c, 1, 0);
        return Array.from(x.getImageData(0, 0, 2, 1).data).join()");
    assert_eq!(got, "255,0,0,255,255,0,0,255");
}

#[test]
fn draw_image_checks_its_source_and_arity() {
    let got = run("var x = cv(1, 1).getContext('2d'), r = [];
        try { x.drawImage({}, 0, 0) } catch (e) { r.push(e.name) }
        try { x.drawImage(cv(0, 1), 0, 0) } catch (e) { r.push(e.name) }
        try { x.drawImage(cv(1, 1), 0) } catch (e) { r.push(e.name) }
        x.drawImage(new Image(), 0, 0); return r.join()");
    assert_eq!(got, "TypeError,InvalidStateError,TypeError");
}

fn unbase64(s: &str) -> Vec<u8> {
    let val = |c: u8| "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/".bytes().position(|x| x == c).expect("base64") as u32;
    let mut out = Vec::new();
    for q in s.trim_end_matches('=').as_bytes().chunks(4) {
        let n = q.iter().enumerate().fold(0, |n, (i, &c)| n | val(c) << (18 - 6 * i));
        out.extend_from_slice(&n.to_be_bytes()[1..q.len()]);
    }
    out
}

#[test]
fn to_data_url_gives_a_png_that_decodes_to_the_pixels() {
    big(|| {
        let mut b = browser();
        let url = js(&mut b, "var c = document.createElement('canvas'); c.width = 3; c.height = 2; var x = c.getContext('2d'); x.fillStyle = 'rgba(0,128,255,0.5)';
            x.fillRect(1, 0, 2, 2); c.toDataURL('image/webp', 0.8)");
        // Bare has no WebP encoder: the standard falls back to PNG.
        let b64 = url.strip_prefix("data:image/png;base64,").expect("a PNG data URL");
        let bytes = unbase64(b64);
        let img = bare_image::decode(&bytes).expect("a PNG");
        assert_eq!((img.width, img.height), (3, 2));
        assert_eq!((img.pixels[0], img.pixels[1]), (0, 0x8000_80FF));
        assert_eq!(js(&mut b, "var z = document.createElement('canvas'); z.width = 0; z.toDataURL()"), "data:,");
    });
}

#[test]
fn to_blob_calls_back_in_a_task_with_a_jpeg() {
    let got = run("var c = cv(2, 2); c.getContext('2d').fillRect(0, 0, 1, 1); var order = [];
        return new Promise(function (res) { c.toBlob(function (b) { order.push('blob'); res(order.join() + ' ' + b.type + ' ' + (b instanceof Blob) + ' ' + (b.size > 50)) }, 'image/jpeg', 0.8); order.push('sync') })");
    assert_eq!(got, "sync,blob image/jpeg true true");
    assert_eq!(run("var c = cv(0, 2); return new Promise(function (res) { c.toBlob(res) })"), "null");
    assert_eq!(run("try { cv(1, 1).toBlob() } catch (e) { return e.name }"), "TypeError");
}

#[test]
fn create_image_bitmap_crops_resizes_and_closes() {
    let got = run("var s = cv(4, 2), y = s.getContext('2d'); y.fillStyle = '#f00'; y.fillRect(0, 0, 2, 2); y.fillStyle = '#00f'; y.fillRect(2, 0, 2, 2);
        return Promise.all([createImageBitmap(s), createImageBitmap(s, 1, 0, 2, 2), createImageBitmap(s, { resizeWidth: 2, resizeQuality: 'pixelated' }), createImageBitmap(s, 3, 0, 2, 1)]).then(function (b) {
          var c = cv(8, 1), x = c.getContext('2d'); x.drawImage(b[3], 0, 0); var r = b.map(function (i) { return i.width + 'x' + i.height }).join(' '); b[0].close();
          return r + ' ' + b[0].width + ' ' + Array.from(x.getImageData(0, 0, 2, 1).data).join() + ' ' + (b[0] instanceof ImageBitmap) + ' ' + Object.prototype.toString.call(b[1]) })");
    assert_eq!(got, "4x2 2x2 2x1 2x1 0 0,0,255,255,0,0,0,0 true [object ImageBitmap]");
}

#[test]
fn create_image_bitmap_rejects_bad_sources() {
    let got = run("return createImageBitmap(cv(0, 1)).then(String, function (e) { return e.name }).then(function (a) { return createImageBitmap({}).then(String, function (e) { return a + ' ' + e.name + ':' + e.message }) })
        .then(function (a) { return createImageBitmap(cv(1, 1), 0, 0, 0, 1).then(String, function (e) { return a + ' ' + e.name + ':' + e.message }) })");
    assert_eq!(got, "InvalidStateError TypeError:Failed to execute 'createImageBitmap' on 'Window': The provided value is not of type '(Blob or HTMLCanvasElement or HTMLImageElement or HTMLVideoElement or ImageBitmap or ImageData or OffscreenCanvas or SVGImageElement or VideoFrame)'. RangeError:Failed to execute 'createImageBitmap' on 'Window': The crop rect width is 0.");
}

/// The photo send of WhatsApp Web: a picture file becomes an `ImageBitmap`, is drawn scaled on a canvas, and is encoded again.
#[test]
fn a_picture_blob_is_drawn_scaled_and_encoded_again() {
    let got = run("var s = cv(8, 6), y = s.getContext('2d'); y.fillStyle = '#204060'; y.fillRect(0, 0, 8, 6); y.fillStyle = '#f0a000'; y.fillRect(0, 0, 4, 6);
        return new Promise(function (res) { s.toBlob(res) }).then(function (blob) { return createImageBitmap(blob) }).then(function (bm) {
          var c = cv(4, 3), x = c.getContext('2d'); x.drawImage(bm, 0, 0, 4, 3);
          return new Promise(function (res) { c.toBlob(res) }) }).then(function (out) { return createImageBitmap(out) }).then(function (bm) {
          var c = cv(4, 3), x = c.getContext('2d'); x.drawImage(bm, 0, 0); var d = x.getImageData(0, 0, 4, 1).data;
          return bm.width + 'x' + bm.height + ' ' + [d[0], d[1], d[2], d[12], d[13], d[14]].join() })");
    assert_eq!(got, "4x3 240,160,0,32,64,96");
}

#[test]
fn a_canvas_that_nobody_reads_never_rasters() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var c = document.createElement('canvas'), x = c.getContext('2d'); for (var i = 0; i < 100; i++) x.fillRect(i % 300, 0, 1, 1)");
        assert_eq!(b.dom.canvases.borrow().values().map(|r| r.rasters()).sum::<u64>(), 0);
        js(&mut b, "x.getImageData(0, 0, 1, 1)");
        assert_eq!(b.dom.canvases.borrow().values().map(|r| r.rasters()).sum::<u64>(), 1);
    });
}
