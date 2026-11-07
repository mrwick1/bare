//! Why a request failed: a short kind and the text of the error, for diagnostics only (never for page code).
//!
//! The error of a request is a `Box<dyn Error>` and it stays one. `describe` reads it: `Phase` marks DNS and connect errors,
//! which no type tells apart. A TLS error, an HTTP/2 refusal and an `io::Error` are told apart by their type and kind.

use std::error::Error;
use std::{fmt, io};

/// An error of one phase of opening a connection. It shows the text of the error inside.
#[derive(Debug)]
pub(crate) struct Phase(pub &'static str, pub Box<dyn Error>);

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.1.fmt(f)
    }
}

impl Error for Phase {}

/// `kind: text`. Kinds: `dns`, `connect refused`, `connect timeout`, `connect failed`, `tls`, `h2 goaway`, `body cap`, `read timeout`,
/// `reset`, `closed early`, `protocol`, `io`.
pub fn describe(e: &(dyn Error + 'static)) -> String {
    let text = e.to_string();
    let io = |k: io::ErrorKind| match k {
        io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionAborted => "reset",
        io::ErrorKind::UnexpectedEof => "closed early",
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => "read timeout",
        io::ErrorKind::InvalidData => "protocol",
        _ => "io",
    };
    let kind = if text.contains("too big") {
        "body cap"
    } else if text.contains("GOAWAY") {
        "h2 goaway"
    } else if text.contains("too slow") || text.contains("stopped answering") {
        "read timeout"
    } else if let Some(p) = e.downcast_ref::<Phase>() {
        match (p.0, p.1.downcast_ref::<io::Error>().map(io::Error::kind)) {
            ("dns", _) => "dns",
            (_, Some(io::ErrorKind::ConnectionRefused)) => "connect refused",
            (_, Some(io::ErrorKind::TimedOut)) => "connect timeout",
            _ => "connect failed",
        }
    } else if e.is::<bare_tls::client::Error>() {
        "tls"
    } else if let Some(i) = e.downcast_ref::<io::Error>() {
        if i.get_ref().is_some_and(|s| s.is::<bare_tls::client::Error>()) { "tls" } else { io(i.kind()) }
    } else {
        "protocol"
    };
    format!("{kind}: {text}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use io::ErrorKind::*;

    fn phase(name: &'static str, e: impl Error + 'static) -> Box<dyn Error> {
        Box::new(Phase(name, Box::new(e)))
    }

    fn kind(e: &(dyn Error + 'static)) -> String {
        describe(e).split(':').next().unwrap().to_string()
    }

    #[test]
    fn each_failure_has_its_kind() {
        let dns: Box<dyn Error> = Box::new(Phase("dns", "no such host: a.test".into()));
        assert_eq!(describe(&*dns), "dns: no such host: a.test");
        assert_eq!(kind(&*phase("connect", io::Error::from(ConnectionRefused))), "connect refused");
        assert_eq!(kind(&*phase("connect", io::Error::from(TimedOut))), "connect timeout");
        assert_eq!(kind(&*phase("connect", io::Error::from(HostUnreachable))), "connect failed");
        assert_eq!(kind(&bare_tls::client::Error::Alpn), "tls");
        // A TLS error that came through a read is an `io::Error` with the TLS error inside.
        assert_eq!(kind(&io::Error::from(bare_tls::client::Error::BadFinished)), "tls");
        assert_eq!(kind(&io::Error::new(ConnectionReset, "x")), "reset");
        assert_eq!(kind(&io::Error::new(UnexpectedEof, "x")), "closed early");
        assert_eq!(kind(&io::Error::new(WouldBlock, "the server stopped answering")), "read timeout");
        assert_eq!(kind(&io::Error::new(TimedOut, "the server is too slow")), "read timeout");
        assert_eq!(kind(&io::Error::new(InvalidData, "the reply is too big")), "body cap");
        assert_eq!(kind(&io::Error::new(InvalidData, "a line is too long")), "protocol");
        assert_eq!(kind(&io::Error::new(ConnectionAborted, "the server said GOAWAY")), "h2 goaway");
    }
}
