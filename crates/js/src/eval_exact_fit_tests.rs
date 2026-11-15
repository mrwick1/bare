//! Exact-fit element buffers: an array whose length is known when it is made gets a buffer of that length, and it grows by the normal
//! rule when a push follows. The behaviour tests hold before and after the change; the capacity tests (`slack`) fail without it.

use super::*;
use crate::heap::Heap;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn run(it: &mut Interp, src: &str) -> String {
    match it.run_script(src) {
        Ok(v) => it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", it.get_str(v, "name"), it.get_str(v, "message")),
        Err(e) => format!("error:{e:?}"),
    }
}

fn eval(src: &str) -> String {
    let src = src.to_string();
    big(move || run(&mut Interp::realm(Heap::default(), EvalLimits::default()).unwrap(), &src))
}

/// Unused element bytes (capacity minus length) that 1000 results of `expr` add. `expr` may read `i` and `src` (a 3-element array).
/// The array that holds them grows by push, so it adds at most about 400 bytes itself.
fn slack(expr: &str) -> usize {
    let expr = expr.to_string();
    big(move || {
        let mut it = Interp::realm(Heap::default(), EvalLimits::default()).unwrap();
        run(&mut it, "var src = [1, 2, 3], keep = [];");
        let f = it.heap.footprint();
        let before = f.elems_cap - f.elems_used;
        let s = format!("for (var i = 0; i < 1000; i++) keep.push({expr}); keep.length");
        assert_eq!(run(&mut it, &s), "1000", "{s}");
        let f = it.heap.footprint();
        f.elems_cap - f.elems_used - before
    })
}

#[test]
fn a_known_length_array_has_no_spare_room() {
    for expr in [
        "[i]", "[i, i, i]", "[i, i, i, i, i]", "[i, , i]", "'a-b-c'.split('-')", "'a'.split('-')", "src.slice(0, 1)", "src.slice(1)",
        "src.map(function (x) { return x })", "[0].concat([1, 2])", "src.concat(src)", "Array.from(src)", "Array.from({ length: 3 })",
        "Array.from(src, function (x) { return x })", "(function () { return Array.prototype.slice.call(arguments) })(i, i)", "/(a)(b)/.exec('ab')",
        "(function (...r) { return r })(i, i)", "[...src]", "[i, ...src]", "[...src, ...src]",
    ] {
        let s = slack(expr);
        assert!(s <= 600, "{expr}: {s} spare bytes for 1000 arrays");
    }
}

#[test]
fn an_arguments_object_has_no_spare_room() {
    let s = slack("(function () { return arguments })(i)");
    assert!(s <= 600, "{s} spare bytes for 1000 arguments objects");
}

#[test]
fn an_array_of_unknown_length_may_keep_room() {
    // `filter` does not know its size: the result grows by the normal rule, so a push after it works and reads back.
    assert_eq!(eval("var r = [1, 2, 3].filter(function (x) { return x > 2 }); r.push(9, 8, 7, 6, 5); r.join()"), "3,9,8,7,6,5");
}

#[test]
fn a_push_after_an_exact_fit_array_grows_it() {
    assert_eq!(eval("var a = [7]; for (var i = 0; i < 20; i++) a.push(i); a.length + ':' + a[0] + ':' + a[20] + ':' + a.join()"), "21:7:19:7,0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19");
    assert_eq!(eval("var a = 'a-b'.split('-'); a.push('c'); a.push('d'); a.push('e'); a.join('')"), "abcde");
    assert_eq!(eval("var a = [1, 2].slice(0, 1); a[1] = 5; a[2] = 6; a.length + ':' + a.join()"), "3:1,5,6");
    assert_eq!(eval("var a = [1, 2].concat([3]); a.unshift(0); a.push(4); a.join()"), "0,1,2,3,4");
    assert_eq!(eval("(function () { arguments[3] = 'x'; return arguments.length + ':' + arguments[3] + ':' + arguments[0] })(1, 2)"), "2:x:1");
}

#[test]
fn length_holes_sort_and_splice_are_unchanged() {
    assert_eq!(eval("var a = [3, 1, 2]; a.sort(); a.join()"), "1,2,3");
    assert_eq!(eval("var a = [3, 1, 2].slice(0); a.sort(function (x, y) { return y - x }); a.join()"), "3,2,1");
    assert_eq!(eval("var a = [1, 2, 3, 4].map(function (x) { return x }); a.splice(1, 2, 'a', 'b', 'c'); a.length + ':' + a.join()"), "5:1,a,b,c,4");
    assert_eq!(eval("var a = [1, 2, 3]; a.splice(1, 1); a.length + ':' + a.join()"), "2:1,3");
    assert_eq!(eval("var a = [1, , 3]; (1 in a) + ':' + a.length + ':' + a.join()"), "false:3:1,,3");
    assert_eq!(eval("var a = [, ]; a.length + ':' + (0 in a)"), "1:false");
    assert_eq!(eval("var a = [1, 2, 3].slice(); a.length = 10; a.length + ':' + (5 in a) + ':' + a[2]"), "10:false:3");
    assert_eq!(eval("var a = [1, 2, 3].slice(); a.length = 1; a.length + ':' + a.join() + ':' + a[2]"), "1:1:undefined");
    assert_eq!(eval("var a = [1, 2, 3].slice(); a.length = 0; a.push(4); a.length + ':' + a.join()"), "1:4");
    assert_eq!(eval("var a = [1]; a[5] = 6; a.length + ':' + a.join()"), "6:1,,,,,6");
    assert_eq!(eval("var a = 'x'.split(''); a.length = 3; a.fill(0); a.join()"), "0,0,0");
    assert_eq!(eval("var a = [1, 2, 3].map(String); a.reverse(); a.shift(); a.pop(); a.join() + ':' + a.length"), "2:1");
    assert_eq!(eval("var m = /(a)(b)/.exec('ab'); m.length + ':' + m.index + ':' + m[2] + ':' + m.input"), "3:0:b:ab");
    assert_eq!(eval("(function () { return arguments.length + ':' + Array.prototype.join.call(arguments) + ':' + [...arguments].join('|') })(1, 2, 3)"), "3:1,2,3:1|2|3");
    assert_eq!(eval("Array.from({ length: 3 }, function (_, i) { return i * 2 }).join()"), "0,2,4");
    assert_eq!(eval("[1, 2].concat(3, [4, [5]], 'x').length"), "6");
    assert_eq!(eval("var a = []; a.concat([]).length + ':' + [].slice().length + ':' + [].map(String).length + ':' + 'a'.split('').length"), "0:0:0:1");
}

#[test]
fn a_big_length_does_not_reserve_big_buffers() {
    // `new Array(n)` and `Array.from({ length: n })` for a big `n` keep the normal growth: no buffer of n slots before the elements exist.
    assert_eq!(eval("var a = Array.from({ length: 100000 }, function (_, i) { return i }); a.length + ':' + a[99999]"), "100000:99999");
    assert_eq!(eval("var a = new Array(100000); a[3] = 1; a.length + ':' + a[3]"), "100000:1");
    assert_eq!(eval("var a = [1].slice(0, 4294967295); a.length"), "1");
}
