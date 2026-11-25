//! The disk form of a clone record: a round trip of each platform object, and damaged input.

use crate::browser_subtle::Key;
use crate::browser_web::WebClone;
use crate::profile_record::{decode, encode};
use bare_js_api::clone::{Cloned, Moved};

fn record(items: Vec<WebClone>) -> Cloned {
    Cloned::new(vec![9, 8, 7].into(), items.into_iter().map(|w| Moved::Data(Box::new(w))).collect(), 123)
}

fn item(c: &Cloned, i: usize) -> &WebClone {
    let Moved::Data(d) = &c.moved()[i] else { panic!("data") };
    d.downcast_ref::<WebClone>().expect("a web clone")
}

#[test]
fn blob_file_and_key_round_trip() {
    let c = record(vec![
        WebClone::Blob { bytes: b"hi".to_vec().into(), ty: "text/plain".into(), file: None },
        WebClone::Blob { bytes: b"abc".to_vec().into(), ty: String::new(), file: Some(("n\u{e9}.txt".into(), 5.5)) },
        WebClone::Key(Key::from_parts(3, vec![1, 2, 3, 4], false, 0b1100).expect("hmac")),
    ]);
    let mut out = vec![0xAA];
    assert!(encode(&c, &mut out));
    out.extend([0xBB]);
    let (back, rest) = decode(&out[1..]).expect("decodes");
    assert_eq!(rest, &[0xBB]);
    assert_eq!((back.bytes(), back.cost(), back.moved().len()), (&[9u8, 8, 7][..], 123, 3));
    let WebClone::Blob { bytes, ty, file } = item(&back, 0) else { panic!("blob") };
    assert_eq!((&**bytes, ty.as_str(), file), (&b"hi"[..], "text/plain", &None));
    let WebClone::Blob { bytes, file, .. } = item(&back, 1) else { panic!("file") };
    assert_eq!((&**bytes, file), (&b"abc"[..], &Some(("n\u{e9}.txt".to_string(), 5.5))));
    let WebClone::Key(k) = item(&back, 2) else { panic!("key") };
    assert_eq!(k.parts(), (3, &[1u8, 2, 3, 4][..], false, 0b1100));
}

#[test]
fn damaged_input_and_items_without_a_disk_form() {
    let mut out = Vec::new();
    assert!(encode(&record(vec![WebClone::Blob { bytes: b"x".to_vec().into(), ty: "t".into(), file: None }]), &mut out));
    for n in 0..out.len() {
        assert!(decode(&out[..n]).is_none(), "cut at {n}");
    }
    assert!(Key::from_parts(200, vec![], true, 0).is_none());
    // A transferred buffer has no disk form, and the output stays as it was.
    let c = Cloned::new(vec![1].into(), vec![Moved::Buffer(vec![1, 2])], 1);
    let mut out = vec![7];
    assert!(!encode(&c, &mut out));
    assert_eq!(out, [7]);
}
