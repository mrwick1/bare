//! #973: `snapshot` showed the text of a `visibility:hidden` element, and a `<select>` showed only its selected option.

mod common;

use bare::page::Page;
use bare_drive::snap::{self, Mode, Refs};
use std::time::Duration;

fn route(_: &str, _: &str, _: &str, _: Option<&str>) -> (u16, &'static str, Vec<u8>) {
    (200, "text/html", br#"<!doctype html><title>t</title><body><p>Visible start</p><p style="visibility:hidden">HIDDENVISIBILITY marker</p><div style="visibility:hidden"><p style="visibility:visible">SHOWNAGAIN marker</p></div><label for=c>Country</label><select id=c><option>Norway<option>Portugal<optgroup label=More><option>Chile</optgroup></select><p>Visible end</p></body>"#.to_vec())
}

fn outline(mode: Mode) -> String {
    let origin = common::serve_route(route);
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || {
            let mut p = Page::open(&format!("{origin}/"), (800, 600)).unwrap();
            let _ = p.wait_load(Duration::from_secs(5));
            snap::snapshot(&mut p, &mut Refs::default(), mode, None, 0, 100_000, false).unwrap()
        })
        .unwrap()
        .join()
        .unwrap()
}

#[test]
fn visibility_hidden_text_stays_out_of_the_outline() {
    let s = outline(Mode::All);
    assert!(s.contains("Visible end"), "{s}");
    assert!(!s.contains("HIDDENVISIBILITY"), "{s}");
    assert!(s.contains("SHOWNAGAIN"), "{s}");
}

#[test]
fn a_select_shows_all_its_options() {
    for mode in [Mode::All, Mode::Interactive] {
        let s = outline(mode);
        let line = s.lines().find(|l| l.contains("combobox")).unwrap_or_else(|| panic!("{s}"));
        assert!(line.contains("= \"Norway\""), "{s}");
        assert!(line.contains("options: \"Norway\" | \"Portugal\" | \"Chile\""), "{s}");
    }
}
