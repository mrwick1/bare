//! Tests of the scroll and the line keys of a `<textarea>` in the window (`page.rs`): the caret stays in view, the wheel scrolls the box,
//! and Up, Down, Home and End move by visual line. The font is 20px monospace: a letter is 12px, so a box of 72px holds 6 letters.

use crate::page::Page;
use crate::page_textarea_wrap_tests::{click, eval, mid, page};
use bare_window::{Event, Mods};

const NO: Mods = Mods { shift: false, ctrl: false, alt: false, meta: false };
const STYLE: &str = "position:absolute;left:10px;top:10px;width:72px;height:100px;padding:0;border:0;overflow:hidden;font:20px monospace";

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn press(p: &mut Page, key: &str, text: Option<&str>, mods: Mods) {
    for pressed in [true, false] {
        p.handle(Event::Key { key: key.into(), code: "", pressed, repeat: false, text: text.filter(|_| pressed).map(str::to_string), mods });
    }
}

fn sel(p: &mut Page) -> String {
    eval(p, "t.selectionStart + ',' + t.selectionEnd")
}

#[test]
fn up_down_home_and_end_move_by_visual_line() {
    big(|| {
        // Three visual lines: "aaaa ", "bbbb ", "cccc" at 0, 5 and 10.
        let mut p = page("aaaa bbbb cccc", STYLE);
        click(&mut p, 34.0, mid(1));
        assert_eq!(sel(&mut p), "7,7");
        press(&mut p, "ArrowUp", None, NO);
        assert_eq!(sel(&mut p), "2,2", "up keeps the x of the caret");
        press(&mut p, "ArrowDown", None, NO);
        press(&mut p, "ArrowDown", None, NO);
        assert_eq!(sel(&mut p), "12,12");
        press(&mut p, "Home", None, NO);
        assert_eq!(sel(&mut p), "10,10", "home goes to the start of the visual line");
        press(&mut p, "ArrowUp", None, NO);
        press(&mut p, "End", None, NO);
        assert_eq!(sel(&mut p), "9,9", "end stops before the space that wraps");
        press(&mut p, "ArrowDown", None, NO);
        press(&mut p, "ArrowDown", None, NO);
        assert_eq!(sel(&mut p), "14,14", "down on the last line goes to the end");
        press(&mut p, "ArrowUp", None, NO);
        press(&mut p, "ArrowUp", None, NO);
        press(&mut p, "ArrowUp", None, NO);
        assert_eq!(sel(&mut p), "0,0", "up on the first line goes to the start");
        press(&mut p, "End", None, Mods { ctrl: true, ..NO });
        assert_eq!(sel(&mut p), "14,14", "control+end goes to the end of the value");
        press(&mut p, "Home", None, Mods { ctrl: true, ..NO });
        assert_eq!(sel(&mut p), "0,0");
    });
}

#[test]
fn shift_extends_the_selection_by_visual_line() {
    big(|| {
        let mut p = page("aaaa bbbb cccc", STYLE);
        click(&mut p, 34.0, mid(2));
        press(&mut p, "ArrowUp", None, Mods { shift: true, ..NO });
        assert_eq!(sel(&mut p), "7,12");
        press(&mut p, "Home", None, Mods { shift: true, ..NO });
        assert_eq!(sel(&mut p), "5,12");
    });
}

#[test]
fn the_caret_stays_in_view_and_the_wheel_scrolls_the_box() {
    big(|| {
        let mut p = page("", STYLE);
        click(&mut p, 20.0, mid(0));
        for _ in 0..30 {
            press(&mut p, "a", Some("a"), NO);
            press(&mut p, "Enter", None, NO);
        }
        p.paint();
        // The box shows the last lines: a press near its bottom lands near the end of the value (60 characters).
        click(&mut p, 20.0, 105.0);
        let at: u32 = eval(&mut p, "t.selectionStart").parse().unwrap();
        assert!(at >= 50, "caret offset {at}");
        // The wheel scrolls up; a press at the top of the box lands on a line above.
        p.handle(Event::Wheel { x: 40.0, y: 50.0, dx: 0.0, dy: -10_000.0, mods: NO });
        click(&mut p, 11.0, 11.0);
        assert_eq!(sel(&mut p), "0,0");
        p.handle(Event::Wheel { x: 40.0, y: 50.0, dx: 0.0, dy: 60.0, mods: NO });
        click(&mut p, 11.0, 11.0);
        let at: u32 = eval(&mut p, "t.selectionStart").parse().unwrap();
        assert!(at > 0 && at < 10, "caret offset {at}");
    });
}
