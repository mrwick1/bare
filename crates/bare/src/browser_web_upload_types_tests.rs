//! The `type` of an uploaded `File` comes from its extension (`mime_of` in `browser_web.rs`). Values are Chrome's on Linux. A
//! chat page sends a video, a song or an office file as a plain document when the type is `application/octet-stream`.

use crate::browser_web::mime_of;

#[test]
fn media_and_office_files_get_their_type() {
    for (name, ty) in [
        ("a.mp4", "video/mp4"),
        ("a.M4V", "video/mp4"),
        ("a.webm", "video/webm"),
        ("a.mov", "video/quicktime"),
        ("a.3gp", "video/3gpp"),
        ("a.mp3", "audio/mpeg"),
        ("a.m4a", "audio/x-m4a"),
        ("a.ogg", "audio/ogg"),
        ("a.opus", "audio/ogg"),
        ("a.wav", "audio/wav"),
        ("a.aac", "audio/aac"),
        ("a.flac", "audio/flac"),
        ("a.avif", "image/avif"),
        ("a.bmp", "image/bmp"),
        ("a.doc", "application/msword"),
        ("a.docx", "application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
        ("a.xls", "application/vnd.ms-excel"),
        ("a.xlsx", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
        ("a.ppt", "application/vnd.ms-powerpoint"),
        ("a.pptx", "application/vnd.openxmlformats-officedocument.presentationml.presentation"),
        ("a.unknownext", "application/octet-stream"),
    ] {
        assert_eq!(mime_of(name), ty, "{name}");
    }
}
