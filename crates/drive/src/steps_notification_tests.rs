//! The step `expect notification` (`steps.rs`): a live notification that matches, or with `new` any event that an agent has not read.

use crate::locate::Text;
use crate::steps::{Action, Check, NoteCheck, parse};
use std::collections::HashMap;

fn p(line: &str) -> Result<Action, String> {
    parse(line, &HashMap::new()).map(|mut v| v.remove(0).action)
}

#[test]
fn expect_notification_parses_its_filters() {
    assert_eq!(p("expect notification"), Ok(Action::Expect(Check::Notification(NoteCheck::default()))));
    assert_eq!(p("wait notification title~\"Alice\" body=\"hi\""), Ok(Action::Expect(Check::Notification(NoteCheck { title: Some(Text::Has("Alice".into())), body: Some(Text::Is("hi".into())), new: false }))));
    assert_eq!(p("wait notification new"), Ok(Action::Expect(Check::Notification(NoteCheck { new: true, ..Default::default() }))));
    assert!(p("expect notification loud").is_err());
}
