//! The MIME choice and the read state of the clipboard. No compositor needed.

use crate::clipboard::{IMAGE, ReadJob, TEXT, offer_for, pick};

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|m| m.to_string()).collect()
}

#[test]
fn text_prefers_utf8_over_plain_over_x11_names() {
    assert_eq!(pick(&s(&["UTF8_STRING", "text/plain", "text/plain;charset=utf-8"]), TEXT).as_deref(), Some("text/plain;charset=utf-8"));
    assert_eq!(pick(&s(&["UTF8_STRING", "text/plain"]), TEXT).as_deref(), Some("text/plain"));
    assert_eq!(pick(&s(&["STRING", "UTF8_STRING"]), TEXT).as_deref(), Some("UTF8_STRING"));
    assert_eq!(pick(&s(&["text/html"]), TEXT), None);
}

#[test]
fn image_takes_the_first_known_type_in_our_order() {
    assert_eq!(pick(&s(&["image/gif", "image/png"]), IMAGE).as_deref(), Some("image/png"));
    assert_eq!(pick(&s(&["image/webp", "image/jpeg"]), IMAGE).as_deref(), Some("image/jpeg"));
    assert_eq!(pick(&s(&["image/bmp"]), IMAGE), None);
}

#[test]
fn a_job_reads_one_mime_per_group_in_turn() {
    let mut job = ReadJob::new(&s(&["image/png", "text/plain", "text/html"]), &[TEXT, IMAGE]);
    assert_eq!(job.next().as_deref(), Some("text/plain"));
    job.push("text/plain", b"hi".to_vec());
    assert_eq!(job.next().as_deref(), Some("image/png"));
    job.push("image/png", vec![1, 2]);
    assert_eq!(job.next(), None);
    assert_eq!(job.items(), vec![("text/plain".to_string(), b"hi".to_vec()), ("image/png".to_string(), vec![1, 2])]);
}

#[test]
fn a_job_with_nothing_to_read_is_done_at_once() {
    let mut job = ReadJob::new(&s(&["text/html"]), &[TEXT, IMAGE]);
    assert_eq!(job.next(), None);
    assert!(job.items().is_empty());
}

#[test]
fn a_failed_read_leaves_the_item_out() {
    let mut job = ReadJob::new(&s(&["text/plain", "image/png"]), &[TEXT, IMAGE]);
    job.next();
    // The source closed the pipe with no data: an empty read is not an item.
    job.push("text/plain", Vec::new());
    job.next();
    job.push("image/png", vec![9]);
    assert_eq!(job.items(), vec![("image/png".to_string(), vec![9])]);
}

#[test]
fn our_offer_adds_the_text_aliases_and_keeps_images() {
    let o = offer_for(&[("text/plain".to_string(), vec![1].into()), ("image/png".to_string(), vec![2].into())]);
    let names: Vec<&str> = o.iter().map(|(m, _)| m.as_str()).collect();
    assert_eq!(names, ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING", "TEXT", "STRING", "image/png"]);
    assert_eq!(&*o[0].1, &[1]);
    assert_eq!(&*o[5].1, &[2]);
}
