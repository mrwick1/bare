//! Input events as the app sees them, and the plain logic that turns Wayland key data into DOM key names. No Wayland
//! here, so a test can check it with no compositor.
//!
//! The Wayland side gives an evdev scancode (the physical key), a keysym (what the key means in the layout, from
//! xkbcommon) and the text that the key makes. The DOM wants `code` (the physical key, named for a US keyboard) and
//! `key` (the meaning). `code_of` and `key_of` map one to the other. They follow the UI Events key tables, for the keys
//! that a page can meet: letters, digits, punctuation, F1 to F12, the editing and arrow keys and the modifiers.

use std::time::{Duration, Instant};

/// Modifier keys held at the time of an event.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    /// The logo (super, windows) key.
    pub meta: bool,
}

/// One input event, in window pixels (CSS pixels: the buffer scale is 1).
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    PointerMove { x: f64, y: f64, buttons: u16, mods: Mods },
    /// `button` is the DOM `MouseEvent.button` (0 left, 1 middle, 2 right, 3 back, 4 forward). `buttons` is the DOM
    /// bit mask after this event (1 left, 2 right, 4 middle, 8 back, 16 forward).
    PointerButton { x: f64, y: f64, button: u8, buttons: u16, pressed: bool, mods: Mods },
    /// The pointer left the window.
    PointerLeave,
    /// The wheel or a touchpad scrolled. `dx` and `dy` are CSS pixels, positive to the right and down (as DOM `deltaX` and
    /// `deltaY`). A wheel notch is `WHEEL_TICK_PX`; a touchpad gives its own pixels. One event for each pointer frame.
    Wheel { x: f64, y: f64, dx: f64, dy: f64, mods: Mods },
    /// A key went down or up. `key` and `code` are the DOM `KeyboardEvent` values. `text` is what the key types (only
    /// for a press), `None` for a key with no text and when ctrl or alt or logo is held.
    Key { key: String, code: &'static str, pressed: bool, repeat: bool, text: Option<String>, mods: Mods },
}

/// The shape of the mouse cursor that the app wants over the window.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Cursor {
    #[default]
    Default,
    /// A hand, over a link or a button.
    Pointer,
    /// An I-beam, over a text field.
    Text,
}

/// CSS pixels for one notch of a mouse wheel (`value120` of 120). To verify against Chrome on a real wheel: see `docs/resources.md`,
/// "Scrolling".
pub const WHEEL_TICK_PX: f64 = 53.0;

/// The pixels of one pointer frame of axis events. `value120`: the high-resolution wheel steps (120 is one notch), `discrete`:
/// the old whole notches, `absolute`: the pixel value that the compositor gives (a touchpad, or a wheel with no steps).
pub fn wheel_px(value120: i32, discrete: i32, absolute: f64) -> f64 {
    if value120 != 0 {
        value120 as f64 / 120.0 * WHEEL_TICK_PX
    } else if discrete != 0 {
        discrete as f64 * WHEEL_TICK_PX
    } else {
        absolute
    }
}

/// The DOM `MouseEvent.button` for a Linux button code (`BTN_LEFT` is 0x110), or `None` for a button we do not map.
pub fn button_of(linux: u32) -> Option<u8> {
    match linux {
        0x110 => Some(0),
        0x112 => Some(1),
        0x111 => Some(2),
        // `BTN_SIDE` and `BTN_BACK` go back, `BTN_EXTRA` and `BTN_FORWARD` go forward (X buttons 8 and 9).
        0x113 | 0x116 => Some(3),
        0x114 | 0x115 => Some(4),
        _ => None,
    }
}

/// The DOM `buttons` bit of a `button` value.
pub fn button_bit(button: u8) -> u16 {
    match button {
        0 => 1,
        1 => 4,
        2 => 2,
        b => 1 << b,
    }
}

/// The DOM `KeyboardEvent.code` of an evdev scancode (US names), or `""`.
pub fn code_of(evdev: u32) -> &'static str {
    const LETTERS: [&str; 26] = [
        "KeyA", "KeyB", "KeyC", "KeyD", "KeyE", "KeyF", "KeyG", "KeyH", "KeyI", "KeyJ", "KeyK", "KeyL", "KeyM", "KeyN", "KeyO", "KeyP", "KeyQ", "KeyR", "KeyS", "KeyT", "KeyU", "KeyV",
        "KeyW", "KeyX", "KeyY", "KeyZ",
    ];
    // evdev: the keys are laid out in rows (QWERTY...), so the letters are not in order.
    let letter = |c: char| LETTERS[(c as u8 - b'A') as usize];
    match evdev {
        1 => "Escape",
        2 => "Digit1",
        3 => "Digit2",
        4 => "Digit3",
        5 => "Digit4",
        6 => "Digit5",
        7 => "Digit6",
        8 => "Digit7",
        9 => "Digit8",
        10 => "Digit9",
        11 => "Digit0",
        12 => "Minus",
        13 => "Equal",
        14 => "Backspace",
        15 => "Tab",
        16..=25 => letter(b"QWERTYUIOP"[evdev as usize - 16] as char),
        26 => "BracketLeft",
        27 => "BracketRight",
        28 => "Enter",
        29 => "ControlLeft",
        30..=38 => letter(b"ASDFGHJKL"[evdev as usize - 30] as char),
        39 => "Semicolon",
        40 => "Quote",
        41 => "Backquote",
        42 => "ShiftLeft",
        43 => "Backslash",
        44..=50 => letter(b"ZXCVBNM"[evdev as usize - 44] as char),
        51 => "Comma",
        52 => "Period",
        53 => "Slash",
        54 => "ShiftRight",
        55 => "NumpadMultiply",
        56 => "AltLeft",
        57 => "Space",
        58 => "CapsLock",
        59 => "F1",
        60 => "F2",
        61 => "F3",
        62 => "F4",
        63 => "F5",
        64 => "F6",
        65 => "F7",
        66 => "F8",
        67 => "F9",
        68 => "F10",
        69 => "NumLock",
        70 => "ScrollLock",
        71 => "Numpad7",
        72 => "Numpad8",
        73 => "Numpad9",
        74 => "NumpadSubtract",
        75 => "Numpad4",
        76 => "Numpad5",
        77 => "Numpad6",
        78 => "NumpadAdd",
        79 => "Numpad1",
        80 => "Numpad2",
        81 => "Numpad3",
        82 => "Numpad0",
        83 => "NumpadDecimal",
        86 => "IntlBackslash",
        87 => "F11",
        88 => "F12",
        96 => "NumpadEnter",
        97 => "ControlRight",
        98 => "NumpadDivide",
        100 => "AltRight",
        102 => "Home",
        103 => "ArrowUp",
        104 => "PageUp",
        105 => "ArrowLeft",
        106 => "ArrowRight",
        107 => "End",
        108 => "ArrowDown",
        109 => "PageDown",
        110 => "Insert",
        111 => "Delete",
        125 => "MetaLeft",
        126 => "MetaRight",
        127 => "ContextMenu",
        _ => "",
    }
}

/// The DOM `KeyboardEvent.key` of a keysym (X11 value) and the text that the key makes. A key with a name (Enter, Tab,
/// an arrow, a modifier...) gets its name. Another key gets its text. A key with no text (or only a control
/// character, for example ctrl+A) gets the character of its keysym, so `ctrl+a` has `key` `a`. Unknown: `"Unidentified"`.
pub fn key_of(keysym: u32, text: Option<&str>) -> String {
    let named = match keysym {
        0xff08 => "Backspace",
        0xff09 | 0xfe20 => "Tab",
        0xff0d | 0xff8d => "Enter",
        0xff1b => "Escape",
        0xffff => "Delete",
        0xff63 => "Insert",
        0xff50 => "Home",
        0xff57 => "End",
        0xff55 => "PageUp",
        0xff56 => "PageDown",
        0xff51 => "ArrowLeft",
        0xff52 => "ArrowUp",
        0xff53 => "ArrowRight",
        0xff54 => "ArrowDown",
        0xffbe..=0xffc9 => ["F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12"][(keysym - 0xffbe) as usize],
        0xffe1 | 0xffe2 => "Shift",
        0xffe3 | 0xffe4 => "Control",
        0xffe9 | 0xffea | 0xfe03 => "Alt",
        0xffeb | 0xffec | 0xffe7 | 0xffe8 => "Meta",
        0xffe5 => "CapsLock",
        0xff7f => "NumLock",
        0xff14 => "ScrollLock",
        0xff67 => "ContextMenu",
        _ => "",
    };
    if !named.is_empty() {
        return named.to_string();
    }
    if let Some(t) = text
        && !t.is_empty()
        && !t.chars().any(char::is_control)
    {
        return t.to_string();
    }
    // The keysym of a Latin-1 or Unicode character is the character (0x20..=0x7e, 0xa0..=0xff) or 0x0100_0000 + char.
    let c = match keysym {
        0x20..=0x7e | 0xa0..=0xff => char::from_u32(keysym),
        0x0100_0000..=0x0110_ffff => char::from_u32(keysym - 0x0100_0000),
        _ => None,
    };
    c.map_or_else(|| "Unidentified".to_string(), |c| c.to_string())
}

/// A key that does not repeat when held: the modifiers and the lock keys.
pub fn repeats(code: &str) -> bool {
    !matches!(code, "ShiftLeft" | "ShiftRight" | "ControlLeft" | "ControlRight" | "AltLeft" | "AltRight" | "MetaLeft" | "MetaRight" | "CapsLock" | "NumLock" | "ScrollLock" | "")
}

/// The auto-repeat of one held key. The compositor gives the `delay` before the first repeat and the `rate` after it. The
/// loop asks `wait` for its poll timeout and `due` for the repeats that are late. No timer thread and no wake-up when no
/// key is held.
#[derive(Debug)]
pub struct Repeat {
    delay: Duration,
    period: Duration,
    /// The key that repeats: its scancode, and when the next repeat is due.
    held: Option<(u32, Instant)>,
}

impl Repeat {
    /// A rate of 0 turns the repeat off.
    pub fn new(rate_per_s: u32, delay_ms: u32) -> Repeat {
        Repeat { delay: Duration::from_millis(delay_ms as u64), period: Duration::from_secs(1) / rate_per_s.max(1), held: None }.off_if(rate_per_s == 0)
    }

    fn off_if(mut self, off: bool) -> Repeat {
        if off {
            self.period = Duration::ZERO;
        }
        self
    }

    /// A key went down: it starts the repeat (and replaces the key that was repeating, as in X11 and Wayland toolkits).
    pub fn press(&mut self, scancode: u32, now: Instant) {
        self.held = (!self.period.is_zero()).then_some((scancode, now + self.delay));
    }

    /// A key went up: only the key that repeats stops it.
    pub fn release(&mut self, scancode: u32) {
        if self.held.is_some_and(|(k, _)| k == scancode) {
            self.held = None;
        }
    }

    pub fn clear(&mut self) {
        self.held = None;
    }

    /// How long the loop may sleep: `None` when no key is held (sleep until an event).
    pub fn wait(&self, now: Instant) -> Option<Duration> {
        self.held.map(|(_, at)| at.saturating_duration_since(now))
    }

    /// The scancode to repeat now, if one is due. Call again until it gives `None`: a slow loop catches up, one repeat
    /// for each period that passed (at most 3 at a time, then the schedule skips to now).
    pub fn due(&mut self, now: Instant) -> Option<u32> {
        let (key, at) = self.held?;
        if now < at {
            return None;
        }
        let next = at + self.period;
        // A loop that was away for long does not send a burst.
        self.held = Some((key, if next + self.period * 2 < now { now + self.period } else { next }));
        Some(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scancodes_name_the_physical_key() {
        assert_eq!(code_of(30), "KeyA");
        assert_eq!(code_of(44), "KeyZ");
        assert_eq!(code_of(16), "KeyQ");
        assert_eq!(code_of(25), "KeyP");
        assert_eq!(code_of(2), "Digit1");
        assert_eq!(code_of(11), "Digit0");
        assert_eq!(code_of(28), "Enter");
        assert_eq!(code_of(15), "Tab");
        assert_eq!(code_of(14), "Backspace");
        assert_eq!(code_of(105), "ArrowLeft");
        assert_eq!(code_of(111), "Delete");
        assert_eq!(code_of(42), "ShiftLeft");
        assert_eq!(code_of(999), "");
        // Every letter is named once.
        let mut letters: Vec<_> = (16..=50).map(code_of).filter(|c| c.starts_with("Key")).collect();
        letters.sort();
        letters.dedup();
        assert_eq!(letters.len(), 26);
    }

    #[test]
    fn keysyms_name_the_meaning() {
        assert_eq!(key_of(0xff0d, Some("\r")), "Enter");
        assert_eq!(key_of(0xff09, None), "Tab");
        assert_eq!(key_of(0xfe20, None), "Tab", "shift+tab is ISO_Left_Tab");
        assert_eq!(key_of(0xff08, Some("\u{8}")), "Backspace");
        assert_eq!(key_of(0x61, Some("a")), "a");
        assert_eq!(key_of(0x41, Some("A")), "A");
        assert_eq!(key_of(0xe9, Some("é")), "é");
        // ctrl+a: the text is a control character, the key is the letter.
        assert_eq!(key_of(0x61, Some("\u{1}")), "a");
        assert_eq!(key_of(0x61, None), "a");
        assert_eq!(key_of(0x0100_20ac, Some("€")), "€");
        assert_eq!(key_of(0xffe1, None), "Shift");
        assert_eq!(key_of(0xffbe, None), "F1");
        assert_eq!(key_of(0xffc9, None), "F12");
        assert_eq!(key_of(0xdead, None), "Unidentified");
    }

    #[test]
    fn buttons_map_to_dom_values() {
        assert_eq!(button_of(0x110), Some(0));
        assert_eq!(button_of(0x111), Some(2));
        assert_eq!(button_of(0x112), Some(1));
        assert_eq!(button_of(0x999), None);
        assert_eq!((button_of(0x113), button_of(0x116)), (Some(3), Some(3)), "the back buttons");
        assert_eq!((button_of(0x114), button_of(0x115)), (Some(4), Some(4)), "the forward buttons");
        assert_eq!((button_bit(0), button_bit(1), button_bit(2)), (1, 4, 2));
    }

    #[test]
    fn repeat_waits_for_the_delay_then_the_period_and_stops_on_release() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let mut r = Repeat::new(25, 600);
        assert_eq!(r.wait(t0), None, "no key held: sleep until an event");
        r.press(30, t0);
        assert_eq!(r.wait(t0), Some(ms(600)));
        assert_eq!(r.due(t0 + ms(599)), None);
        assert_eq!(r.due(t0 + ms(600)), Some(30));
        assert_eq!(r.due(t0 + ms(600)), None);
        assert_eq!(r.wait(t0 + ms(600)), Some(ms(40)));
        assert_eq!(r.due(t0 + ms(640)), Some(30));
        r.release(31);
        assert_eq!(r.wait(t0 + ms(640)), Some(ms(40)), "another key going up does not stop the repeat");
        r.release(30);
        assert_eq!(r.wait(t0 + ms(640)), None);
        assert_eq!(r.due(t0 + ms(10_000)), None);
    }

    #[test]
    fn repeat_does_not_burst_after_a_long_stall_and_can_be_off() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let mut r = Repeat::new(25, 600);
        r.press(30, t0);
        assert_eq!(r.due(t0 + ms(5_000)), Some(30));
        assert_eq!(r.due(t0 + ms(5_000)), None, "one repeat, then the schedule is in the future");
        assert!(r.wait(t0 + ms(5_000)).is_some_and(|w| w <= ms(40)));
        let mut off = Repeat::new(0, 600);
        off.press(30, t0);
        assert_eq!(off.wait(t0), None);
        let mut r = Repeat::new(25, 600);
        r.press(30, t0);
        r.press(31, t0 + ms(100));
        assert_eq!(r.due(t0 + ms(699)), None, "a new key replaces the old one and restarts the delay");
        assert_eq!(r.due(t0 + ms(700)), Some(31), "the old key does not repeat any more");
    }

    #[test]
    fn modifiers_do_not_repeat() {
        assert!(!repeats("ShiftLeft"));
        assert!(!repeats("ControlRight"));
        assert!(repeats("KeyA"));
        assert!(repeats("Backspace"));
    }

    #[test]
    fn a_wheel_notch_is_53_px_and_a_touchpad_gives_its_pixels() {
        assert_eq!(wheel_px(120, 0, 15.0), 53.0, "one notch in value120");
        assert_eq!(wheel_px(-240, 0, -30.0), -106.0);
        assert!((wheel_px(30, 0, 3.75) - 13.25).abs() < 1e-9, "a quarter of a notch from a high-resolution wheel");
        assert_eq!(wheel_px(0, 2, 20.0), 106.0, "an old compositor: whole notches");
        assert_eq!(wheel_px(0, 0, 7.5), 7.5, "a touchpad: pixels as given");
    }
}
