//! Analytics and tracker hosts. A request to one of them is refused before DNS and before any socket. `BARE_TRACKERS=allow` turns the list off.
//!
//! `LIST` is sorted. A host matches when it, or a suffix of it that starts after a dot, is in the list.

use std::error::Error;
use std::fmt;
use std::sync::OnceLock;

const LIST: &[&str] = &[
    "adsrvr.org",
    "amplitude.com",
    "analytics.google.com",
    "api.segment.io",
    "bat.bing.com",
    "browser-intake-datadoghq.com",
    "cdn.segment.com",
    "clarity.ms",
    "crazyegg.com",
    "datadoghq-browser-agent.com",
    "doubleclick.net",
    "fullstory.com",
    "google-analytics.com",
    "googleadservices.com",
    "googlesyndication.com",
    "googletagmanager.com",
    "heapanalytics.com",
    "hotjar.com",
    "hotjar.io",
    "mixpanel.com",
    "mouseflow.com",
    "nr-data.net",
    "outbrain.com",
    "quantserve.com",
    "scorecardresearch.com",
    "segment.io",
    "taboola.com",
];

/// The error of a refused request. `cause::kind` names it `blocked`.
#[derive(Debug)]
pub struct Blocked;

impl fmt::Display for Blocked {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("tracker host blocked")
    }
}

impl Error for Blocked {}

fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("BARE_TRACKERS").map_or(true, |v| v != "allow"))
}

/// Whether a request to `host` is refused.
pub fn blocked(host: &str) -> bool {
    if !on() {
        return false;
    }
    let mut s = host;
    loop {
        if LIST.binary_search(&s).is_ok() {
            return true;
        }
        match s.split_once('.') {
            Some((_, rest)) => s = rest,
            None => return false,
        }
    }
}
