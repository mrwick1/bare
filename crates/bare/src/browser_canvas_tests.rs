//! `ImageData` and the zero-pixel parts of the 2D context (`browser_canvas.rs`). A canvas has no pixels, so every read is transparent black.

use crate::browser::*;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(e) => format!("other:{e:?}"),
    }
}

fn browser() -> Browser {
    let mut b = Browser::new(tree::parse("<body><canvas id=c></canvas><img id=i></body>"), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock();
    b
}

#[test]
fn the_constructor_makes_zero_bytes_and_checks_its_sizes() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "var d = new ImageData(3, 2); [d.width, d.height, d.colorSpace, d.data.length, d.data instanceof Uint8ClampedArray, d.data.every(function (x) { return x === 0 }), d instanceof ImageData].join()"), "3,2,srgb,24,true,true,true");
        assert_eq!(js(&mut b, "Object.prototype.toString.call(d)"), "[object ImageData]");
        assert_eq!(js(&mut b, "new ImageData(0, 1)"), "throw:IndexSizeError:Failed to construct 'ImageData': The source width or height is zero or not a number.");
        assert_eq!(js(&mut b, "new ImageData(1)"), "throw:TypeError:Failed to construct 'ImageData': 2 arguments required, but only 1 present.");
        assert_eq!(js(&mut b, "new ImageData(-1, 1)"), "throw:TypeError:Failed to construct 'ImageData': The source width is outside the range of an unsigned long.");
        assert_eq!(js(&mut b, "new ImageData(NaN, 1)"), "throw:TypeError:Failed to construct 'ImageData': The source width is outside the range of an unsigned long.");
    });
}

#[test]
fn the_constructor_with_data_keeps_the_array_and_checks_its_length() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "var a = new Uint8ClampedArray(8); a[0] = 7; var d = new ImageData(a, 2); [d.width, d.height, d.data === a, d.data[0]].join()"), "2,1,true,7");
        assert_eq!(js(&mut b, "var e = new ImageData(new Uint8ClampedArray(24), 3, 2); [e.width, e.height].join()"), "3,2");
        assert_eq!(js(&mut b, "new ImageData(new Uint8ClampedArray(24), 3, 3)"), "throw:IndexSizeError:Failed to construct 'ImageData': The input data length is not equal to (4 * width * height).");
        assert_eq!(js(&mut b, "new ImageData(new Uint8ClampedArray(12), 2)"), "throw:IndexSizeError:Failed to construct 'ImageData': The input data length is not a multiple of (4 * width).");
        assert_eq!(js(&mut b, "new ImageData(new Uint8ClampedArray(6), 1)"), "throw:InvalidStateError:Failed to construct 'ImageData': The input data length is not a multiple of 4.");
        assert_eq!(js(&mut b, "new ImageData(new Uint8ClampedArray(0), 1)"), "throw:IndexSizeError:Failed to construct 'ImageData': The source width is zero or not a number.");
        assert_eq!(js(&mut b, "new ImageData(new Uint8Array(4), 1)"), "throw:TypeError:Failed to construct 'ImageData': parameter 1 is not of type 'Uint8ClampedArray'.");
    });
}

#[test]
fn one_image_data_holds_at_most_64_mib() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "new ImageData(4096, 4096).data.length"), "67108864");
        assert_eq!(js(&mut b, "new ImageData(4096, 4097)"), "throw:RangeError:Out of memory at ImageData creation");
        assert_eq!(js(&mut b, "new ImageData(4294967295, 4294967295)"), "throw:RangeError:Out of memory at ImageData creation");
        assert_eq!(js(&mut b, "document.getElementById('c').getContext('2d').getImageData(0, 0, 70000, 70000)"), "throw:RangeError:Out of memory at ImageData creation");
    });
}

#[test]
fn the_context_reads_transparent_black_and_has_gradients_and_patterns() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "var x = document.getElementById('c').getContext('2d'); var g = x.getImageData(5, 5, 4, 3); [g.width, g.height, g.data.length, g.data.some(function (v) { return v })].join()"), "4,3,48,false");
        assert_eq!(js(&mut b, "var c = x.createImageData(2, 5); [c.width, c.height, c.data.length].join()"), "2,5,40");
        assert_eq!(js(&mut b, "var c2 = x.createImageData(c); [c2.width, c2.height, c2 !== c].join()"), "2,5,true");
        assert_eq!(js(&mut b, "var n = x.createImageData(-2, -3); [n.width, n.height].join()"), "2,3");
        assert_eq!(js(&mut b, "x.getImageData(0, 0, 0, 1)"), "throw:IndexSizeError:Failed to execute 'getImageData' on 'CanvasRenderingContext2D': The source width or height is zero or not a number.");
        assert_eq!(js(&mut b, "x.createImageData(1, 0)"), "throw:IndexSizeError:Failed to execute 'createImageData' on 'CanvasRenderingContext2D': The source width or height is zero or not a number.");
        assert_eq!(js(&mut b, "x.putImageData(c, 0, 0); c instanceof ImageData"), "true");
        assert_eq!(js(&mut b, "[x.createLinearGradient(0, 0, 1, 1), x.createRadialGradient(0, 0, 1, 2, 2, 3), x.createConicGradient(0, 1, 1)].map(function (g) { return typeof g.addColorStop === 'function' && g.addColorStop(0, 'red') === undefined }).join()"), "true,true,true");
        assert_eq!(js(&mut b, "[x.isPointInPath(1, 1), x.isPointInStroke(1, 1)].join()"), "false,false");
        assert_eq!(js(&mut b, "document.getElementById('i').complete"), "true");
        assert_eq!(js(&mut b, "document.getElementById('i').src = 'http://localhost:1/x.png'; document.getElementById('i').complete"), "false");
        assert_eq!(js(&mut b, "x.createPattern(document.getElementById('i'), 'repeat')"), "null");
        assert_eq!(js(&mut b, "typeof x.createPattern(document.getElementById('c'), 'repeat').setTransform"), "function");
        assert_eq!(js(&mut b, "x.createPattern(1, 'repeat')"), "throw:TypeError:Failed to execute 'createPattern' on 'CanvasRenderingContext2D': The provided value is not of type 'CanvasImageSource'.");
    });
}
