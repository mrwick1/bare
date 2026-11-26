//! The key logic of the shell (`shell_keys.rs`): modes, two-key commands, the command line, the URL rule and the hint labels.

use crate::shell_keys::{Act, Cmd, Key, Keys, Mode, hint_labels, parse, url_of, SEARCH};
use std::time::{Duration, Instant};

fn press(k: &mut Keys, key: &str, t: Instant) -> (bool, Vec<Act>) {
    let o = k.key(Key::plain(key), true, t);
    (o.eat, o.acts)
}

fn type_in(k: &mut Keys, text: &str, t: Instant) {
    for c in text.chars() {
        press(k, &c.to_string(), t);
    }
}

#[test]
fn normal_keys_are_eaten_and_give_one_action() {
    let (mut k, t) = (Keys::new(), Instant::now());
    for (key, act) in [("j", Act::Scroll(60)), ("k", Act::Scroll(-60)), ("d", Act::Half(1)), ("u", Act::Half(-1)), ("G", Act::Bottom), ("H", Act::Back), ("L", Act::Forward), ("r", Act::Reload), ("x", Act::ClosePage), ("y", Act::CopyUrl), ("p", Act::PasteOpen), ("i", Act::FocusField), ("n", Act::FindStep(1)), ("N", Act::FindStep(-1)), ("O", Act::OpenCurrent)] {
        assert_eq!(press(&mut k, key, t), (true, vec![act]), "{key}");
        assert_eq!(k.mode, Mode::Normal);
    }
}

#[test]
fn other_keys_and_keys_with_ctrl_or_alt_go_to_the_page() {
    let (mut k, t) = (Keys::new(), Instant::now());
    assert_eq!(press(&mut k, "z", t), (false, vec![]));
    assert_eq!(press(&mut k, "ArrowLeft", t), (false, vec![]));
    for key in [Key::ctrl("j"), Key { alt: true, ..Key::plain("ArrowLeft") }, Key { meta: true, ..Key::plain("r") }] {
        let o = k.key(key, true, t);
        assert!(!o.eat && o.acts.is_empty(), "{key:?}");
    }
}

#[test]
fn the_release_of_an_eaten_key_is_eaten_and_a_page_key_release_is_not() {
    let (mut k, t) = (Keys::new(), Instant::now());
    press(&mut k, "j", t);
    press(&mut k, "z", t);
    assert!(k.key(Key::plain("j"), false, t).eat);
    assert!(!k.key(Key::plain("z"), false, t).eat);
    // Once only.
    assert!(!k.key(Key::plain("j"), false, t).eat);
}

#[test]
fn two_key_commands_wait_one_second() {
    let (mut k, t) = (Keys::new(), Instant::now());
    assert_eq!(press(&mut k, "g", t), (true, vec![]));
    assert_eq!(press(&mut k, "g", t), (true, vec![Act::Top]));
    press(&mut k, "g", t);
    assert_eq!(press(&mut k, "t", t + Duration::from_millis(900)), (true, vec![Act::NextPage]));
    press(&mut k, "g", t);
    assert_eq!(press(&mut k, "T", t), (true, vec![Act::PrevPage]));
    // The Shift key of `T` is not a key of the command.
    press(&mut k, "g", t);
    assert_eq!(press(&mut k, "Shift", t), (false, vec![]));
    assert_eq!(press(&mut k, "T", t), (true, vec![Act::PrevPage]));
    // After the timeout the second key is a first key: `t` opens the command line.
    press(&mut k, "g", t);
    assert_eq!(press(&mut k, "t", t + Duration::from_millis(1100)), (true, vec![Act::Redraw]));
    assert_eq!((k.mode, k.line.as_str()), (Mode::Command, "tabopen "));
    // `g` then another key: the pair is dropped, and the key acts alone.
    let mut k = Keys::new();
    press(&mut k, "g", t);
    assert_eq!(press(&mut k, "j", t), (true, vec![Act::Scroll(60)]));
}

#[test]
fn focus_flips_normal_and_insert_only() {
    let (mut k, t) = (Keys::new(), Instant::now());
    k.focus(true);
    assert_eq!(k.mode, Mode::Insert);
    // In INSERT every key goes to the page, also Ctrl keys.
    assert_eq!(press(&mut k, "j", t), (false, vec![]));
    assert!(!k.key(Key::ctrl("v"), true, t).eat);
    k.focus(false);
    assert_eq!(k.mode, Mode::Normal);
    // The command line does not follow the focus.
    press(&mut k, ":", t);
    k.focus(true);
    assert_eq!(k.mode, Mode::Command);
    k.focus(false);
    assert_eq!(k.mode, Mode::Command);
}

#[test]
fn escape_in_insert_reaches_the_page_then_blurs() {
    let (mut k, t) = (Keys::new(), Instant::now());
    k.focus(true);
    assert_eq!(press(&mut k, "Escape", t), (false, vec![Act::Blur]));
    assert_eq!(k.mode, Mode::Normal);
}

#[test]
fn the_command_line_edits_its_text() {
    let (mut k, t) = (Keys::new(), Instant::now());
    press(&mut k, ":", t);
    assert_eq!((k.mode, k.prompt), (Mode::Command, ':'));
    type_in(&mut k, "open exmple", t);
    // Left four times, then fix the word.
    for _ in 0..4 {
        press(&mut k, "ArrowLeft", t);
    }
    press(&mut k, "a", t);
    assert_eq!(k.line, "open example");
    press(&mut k, "End", t);
    press(&mut k, "Backspace", t);
    assert_eq!(k.line, "open exampl");
    press(&mut k, "Home", t);
    press(&mut k, "Delete", t);
    assert_eq!(k.line, "pen exampl");
    // Ctrl+W deletes a word, Ctrl+U clears.
    press(&mut k, "End", t);
    assert!(k.key(Key::ctrl("w"), true, t).eat);
    assert_eq!(k.line, "pen ");
    k.key(Key::ctrl("u"), true, t);
    assert_eq!((k.line.as_str(), k.caret), ("", 0));
    // Every key is eaten, also an unknown one and a Ctrl key.
    assert!(press(&mut k, "F5", t).0);
    assert!(k.key(Key::ctrl("q"), true, t).eat);
    // Ctrl+V asks for the clipboard; the text comes back as one insert.
    assert!(k.key(Key::ctrl("v"), true, t).acts.contains(&Act::PasteLine));
    k.insert_text("a b\nc");
    assert_eq!(k.line, "a bc");
}

#[test]
fn enter_runs_and_escape_cancels_and_up_walks_the_history() {
    let (mut k, t) = (Keys::new(), Instant::now());
    press(&mut k, ":", t);
    type_in(&mut k, "reload", t);
    assert_eq!(press(&mut k, "Enter", t), (true, vec![Act::Run("reload".into())]));
    assert_eq!(k.mode, Mode::Normal);
    press(&mut k, ":", t);
    type_in(&mut k, "back", t);
    press(&mut k, "Enter", t);
    press(&mut k, ":", t);
    type_in(&mut k, "qu", t);
    press(&mut k, "ArrowUp", t);
    assert_eq!(k.line, "back");
    press(&mut k, "ArrowUp", t);
    assert_eq!(k.line, "reload");
    press(&mut k, "ArrowUp", t);
    assert_eq!(k.line, "reload");
    press(&mut k, "ArrowDown", t);
    press(&mut k, "ArrowDown", t);
    assert_eq!(k.line, "qu");
    assert_eq!(press(&mut k, "Escape", t), (true, vec![Act::Cancel]));
    assert_eq!(k.mode, Mode::Normal);
}

#[test]
fn tab_completes_command_names_only() {
    let (mut k, t) = (Keys::new(), Instant::now());
    press(&mut k, ":", t);
    type_in(&mut k, "qui", t);
    press(&mut k, "Tab", t);
    assert_eq!(k.line, "quit ");
    k.key(Key::ctrl("u"), true, t);
    type_in(&mut k, "b", t);
    press(&mut k, "Tab", t);
    // `buffers`, `buffer` and `bdelete` share only `b`.
    assert_eq!(k.line, "b");
    k.key(Key::ctrl("u"), true, t);
    type_in(&mut k, "buf", t);
    press(&mut k, "Tab", t);
    assert_eq!(k.line, "buffer");
    // After a space Tab does nothing.
    type_in(&mut k, " 1", t);
    press(&mut k, "Tab", t);
    assert_eq!(k.line, "buffer 1");
}

#[test]
fn find_has_its_own_line_and_does_not_keep_history() {
    let (mut k, t) = (Keys::new(), Instant::now());
    press(&mut k, "/", t);
    assert_eq!((k.mode, k.prompt), (Mode::Find, '/'));
    type_in(&mut k, "needle", t);
    assert_eq!(press(&mut k, "Enter", t), (true, vec![Act::Find("needle".into())]));
    press(&mut k, ":", t);
    press(&mut k, "ArrowUp", t);
    assert_eq!(k.line, "");
}

#[test]
fn commands_parse() {
    assert_eq!(parse("open example.com"), Ok(Cmd::Open("https://example.com".into())));
    assert_eq!(parse("o localhost:3773"), Ok(Cmd::Open("http://localhost:3773".into())));
    assert_eq!(parse("t  rust lang"), Ok(Cmd::TabOpen(format!("{SEARCH}rust%20lang"))));
    assert_eq!(parse("ls"), Ok(Cmd::Buffers));
    assert_eq!(parse("buffers"), Ok(Cmd::Buffers));
    assert_eq!(parse("b 2"), Ok(Cmd::Buffer(2)));
    assert_eq!(parse("bd"), Ok(Cmd::Bdelete(None)));
    assert_eq!(parse("bdelete 3"), Ok(Cmd::Bdelete(Some(3))));
    assert_eq!((parse("r"), parse("reload"), parse("back"), parse("forward"), parse("q"), parse("quit")), (Ok(Cmd::Reload), Ok(Cmd::Reload), Ok(Cmd::Back), Ok(Cmd::Forward), Ok(Cmd::Quit), Ok(Cmd::Quit)));
    assert!(parse("open").is_err());
    assert!(parse("b").is_err());
    assert!(parse("b 0").is_err());
    assert!(parse("b x").is_err());
    assert!(parse("").is_err());
    assert!(parse("nope").unwrap_err().contains("nope"));
}

#[test]
fn the_url_rule() {
    for (text, url) in [
        ("https://a.test/x?y=1", "https://a.test/x?y=1"),
        ("http://a.test", "http://a.test"),
        ("file:///tmp/a.html", "file:///tmp/a.html"),
        ("about:blank", "about:blank"),
        ("localhost", "http://localhost"),
        ("localhost:3773", "http://localhost:3773"),
        ("localhost:3773/a/b?c#d", "http://localhost:3773/a/b?c#d"),
        ("127.0.0.1:8080/x", "http://127.0.0.1:8080/x"),
        ("192.168.1.5", "http://192.168.1.5"),
        ("[::1]:80/p", "http://[::1]:80/p"),
        ("example.com", "https://example.com"),
        ("example.com/a b", &format!("{SEARCH}example.com%2Fa%20b")),
        ("docs.rs/serde", "https://docs.rs/serde"),
        ("rust", &format!("{SEARCH}rust")),
        ("a&b c", &format!("{SEARCH}a%26b%20c")),
        ("é", &format!("{SEARCH}%C3%A9")),
    ] {
        assert_eq!(url_of(text), url, "{text}");
    }
}

#[test]
fn hint_labels_are_unique_and_prefix_free() {
    assert!(hint_labels(0).is_empty());
    assert_eq!(hint_labels(1), ["a"]);
    assert_eq!(hint_labels(9).len(), 9);
    for n in [2, 9, 10, 17, 81, 82, 200, 700] {
        let l = hint_labels(n);
        assert_eq!(l.len(), n);
        for (i, a) in l.iter().enumerate() {
            assert!(!a.is_empty() && a.chars().all(|c| "asdfghjkl".contains(c)), "{a}");
            for (j, b) in l.iter().enumerate() {
                assert!(i == j || !b.starts_with(a.as_str()), "{a} starts {b} (n = {n})");
            }
        }
    }
    // Few labels stay short.
    assert!(hint_labels(9).iter().all(|l| l.len() == 1));
    assert!(hint_labels(30).iter().all(|l| l.len() <= 2));
}

#[test]
fn hint_mode_filters_and_picks() {
    let (mut k, t) = (Keys::new(), Instant::now());
    assert_eq!(press(&mut k, "f", t), (true, vec![Act::Hints(false)]));
    assert_eq!(k.mode, Mode::Hint);
    k.set_labels(hint_labels(12));
    // 12 labels: `s d f g h j k l` and `aa as ad ...`.
    assert_eq!(press(&mut k, "a", t), (true, vec![Act::Redraw]));
    assert_eq!(k.live().count(), 4);
    // A letter that no label continues with is ignored, so is a key that is not a label letter.
    assert_eq!(press(&mut k, "q", t), (true, vec![]));
    assert_eq!(press(&mut k, "z", t), (true, vec![]));
    assert_eq!(k.typed, "a");
    press(&mut k, "Backspace", t);
    assert_eq!(k.typed, "");
    press(&mut k, "a", t);
    let want = hint_labels(12).iter().position(|l| l == "as").unwrap();
    assert_eq!(press(&mut k, "s", t), (true, vec![Act::Pick(want, false)]));
    assert_eq!(k.mode, Mode::Normal);
    // A one-letter label picks at once, and `F` asks for a new page.
    press(&mut k, "F", t);
    k.set_labels(hint_labels(3));
    assert_eq!(press(&mut k, "d", t), (true, vec![Act::Pick(2, true)]));
}

#[test]
fn hint_mode_escape_cancels_and_no_label_ends_the_mode() {
    let (mut k, t) = (Keys::new(), Instant::now());
    press(&mut k, "f", t);
    k.set_labels(hint_labels(3));
    assert_eq!(press(&mut k, "Escape", t), (true, vec![Act::Cancel]));
    assert_eq!(k.mode, Mode::Normal);
    press(&mut k, "f", t);
    k.set_labels(Vec::new());
    assert_eq!(k.mode, Mode::Normal);
    // Ctrl keys are eaten in HINT mode.
    press(&mut k, "f", t);
    k.set_labels(hint_labels(3));
    assert!(k.key(Key::ctrl("r"), true, t).eat);
}
