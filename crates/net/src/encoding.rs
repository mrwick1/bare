//! `Content-Encoding`: the body of a reply is undone as it is read. gzip, deflate and br.
//!
//! The client asks for them (`Accept-Encoding`, in `http.rs`; not of a loopback host, where they save no byte) and a `Body` gives the decoded bytes, as `fetch` and XHR show them. The
//! headers of the reply stay as the server sent them: `content-encoding` and `content-length` (the length on the wire).
//! - Codings are undone from the last in the header to the first. At most `MAX_CODINGS` (Fetch standard). `identity` and
//!   `x-gzip` follow the standards (`identity` does nothing, `x-gzip` is gzip). A coding that we do not know, or too many
//!   codings, leaves the bytes as they are, and `Response::encoded` says so: a caller must not read them as text.
//! - The decoder starts at the first read, not when the head comes, so a reply with a stream body keeps its first-byte time.
//! - `deflate` is a zlib stream by the standard. Some servers send a raw deflate stream. A zlib header (the first two bytes)
//!   decides.
//! - An empty body is an empty body, with any coding. A bad or cut stream is an error: nothing half decoded is given as complete.
//! - The decoded size is capped at `MAX_REPLY`, the cap of the wire size. A bomb of a few KB that grows to GB stops there, with
//!   an error, after `MAX_REPLY` bytes of work. A caller with a smaller cap reads less and stops sooner.
//! - Bytes after the end of the compressed stream are read and dropped. The body ends where its framing ends, so the connection
//!   is given back to the pool.
//!
//! Decoders: `flate2` (pure Rust `miniz_oxide`, which `png` already builds) for gzip and deflate, `brotli-decompressor` for br.
//! `zstd` is not offered: its decoder is C (`zstd`) or slower pure Rust (`ruzstd`), and servers have gzip and br.

use crate::http::{MAX_REPLY, Response};
use flate2::bufread::{DeflateDecoder, GzDecoder, ZlibDecoder};
use std::io::{self, BufReader, Read};
use std::sync::{Arc, Mutex};

/// The most codings that we undo. The Fetch standard allows an implementation to stop at 5.
const MAX_CODINGS: usize = 5;

#[derive(Clone, Copy)]
pub(crate) enum Coding {
    Gzip,
    Deflate,
    Brotli,
}

/// The codings that a `Content-Encoding` value names, in header order, without `identity`. `None` when one is unknown or there are too many.
pub(crate) fn codings(value: &str) -> Option<Vec<Coding>> {
    let mut out = Vec::new();
    for token in value.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        out.push(match token.to_ascii_lowercase().as_str() {
            "identity" => continue,
            "gzip" | "x-gzip" => Coding::Gzip,
            "deflate" => Coding::Deflate,
            "br" => Coding::Brotli,
            _ => return None,
        });
    }
    (out.len() <= MAX_CODINGS).then_some(out)
}

impl Response {
    /// The body is still encoded: the reply names a coding that the client did not undo. Read it as bytes of that coding, not as text.
    pub fn encoded(&self) -> bool {
        self.header("content-encoding").is_some_and(|v| codings(v).is_none())
    }
}

/// One end of the connection, shared with the decoders: after the last decoded byte, the rest of the body is read from it.
struct Tap(Arc<Mutex<dyn Read + Send>>);

impl Read for Tap {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.0.lock().unwrap().read(out)
    }
}

/// The decoded body of a reply. It reads from `raw`, the body as it is on the wire.
pub(crate) struct Decoded {
    raw: Arc<Mutex<dyn Read + Send>>,
    /// The codings to undo, until the first read builds the decoders.
    todo: Vec<Coding>,
    decoder: Option<Box<dyn Read + Send>>,
    /// Decoded bytes given out.
    given: usize,
    /// The end, or an error, was given out. Later reads give 0.
    done: bool,
}

impl Decoded {
    pub(crate) fn new(raw: impl Read + Send + 'static, todo: Vec<Coding>) -> Decoded {
        Decoded { raw: Arc::new(Mutex::new(raw)), todo, decoder: None, given: 0, done: false }
    }

    fn read_some(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let decoder = match &mut self.decoder {
            Some(d) => d,
            none => {
                let mut input: Box<dyn Read + Send> = Box::new(Tap(self.raw.clone()));
                for coding in self.todo.drain(..).rev() {
                    input = stage(coding, input)?;
                }
                none.insert(input)
            }
        };
        let n = decoder.read(out)?;
        if n == 0 {
            // The stream ended. Read what is left of the body, so that its framing is checked and the connection goes back to the pool.
            io::copy(&mut Tap(self.raw.clone()), &mut io::sink())?;
        }
        self.given += n;
        if self.given > MAX_REPLY {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "the reply is too big"));
        }
        Ok(n)
    }
}

impl Read for Decoded {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.done || out.is_empty() {
            return Ok(0);
        }
        let result = self.read_some(out);
        self.done = !matches!(result, Ok(1..));
        result
    }
}

/// Undo one coding of `input`. The first two bytes are read here: no byte means an empty body, and two bytes tell zlib from raw deflate.
fn stage(coding: Coding, mut input: Box<dyn Read + Send>) -> io::Result<Box<dyn Read + Send>> {
    let (mut first, mut n) = ([0u8; 2], 0);
    while n < 2 {
        match input.read(&mut first[n..])? {
            0 => break,
            k => n += k,
        }
    }
    if n == 0 {
        return Ok(Box::new(io::empty()));
    }
    // `Body` already buffers 8 KiB, so the decoders get a small buffer. `flate2::read` would use 32 KiB for each of up to 6 downloads at once.
    let input = BufReader::with_capacity(8192, io::Cursor::new(first).take(n as u64).chain(input));
    Ok(match coding {
        Coding::Gzip => Box::new(GzDecoder::new(input)),
        Coding::Deflate if n == 2 && first[0] & 0x0f == 8 && (u16::from(first[0]) << 8 | u16::from(first[1])) % 31 == 0 => Box::new(ZlibDecoder::new(input)),
        Coding::Deflate => Box::new(DeflateDecoder::new(input)),
        Coding::Brotli => Box::new(brotli_decompressor::Decompressor::new(input, 8192)),
    })
}

#[cfg(test)]
mod tests {
    use crate::http::{Client, Request};
    use bare_url::Url;
    use flate2::Compression;
    use flate2::write::{DeflateEncoder, GzEncoder, ZlibEncoder};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc::{Receiver, channel};

    /// A server on this machine. It reads one request head at a time and answers with the next reply (raw bytes).
    /// Gives the port and the request heads. It keeps the connection open after the last reply.
    fn serve(replies: Vec<Vec<u8>>) -> (u16, Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (heads, seen) = channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            for reply in replies {
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if stream.read(&mut byte).unwrap_or(0) == 0 {
                        return;
                    }
                    head.push(byte[0]);
                }
                let _ = heads.send(String::from_utf8_lossy(&head).into_owned());
                if stream.write_all(&reply).is_err() {
                    return;
                }
            }
            let _ = stream.read(&mut [0u8; 1]);
        });
        (port, seen)
    }

    fn url(port: u16) -> Url {
        Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap()
    }

    /// A reply with a `Content-Length` and the given extra headers.
    fn reply(headers: &str, body: &[u8]) -> Vec<u8> {
        let mut out = format!("HTTP/1.1 200 OK\r\n{headers}Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        out.extend_from_slice(body);
        out
    }

    /// A chunked reply: the body is cut into pieces of `piece` bytes.
    fn chunked(headers: &str, body: &[u8], piece: usize) -> Vec<u8> {
        let mut out = format!("HTTP/1.1 200 OK\r\n{headers}Transfer-Encoding: chunked\r\n\r\n").into_bytes();
        for part in body.chunks(piece) {
            out.extend_from_slice(format!("{:x}\r\n", part.len()).as_bytes());
            out.extend_from_slice(part);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"0\r\n\r\n");
        out
    }

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut e = GzEncoder::new(Vec::new(), Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut e = ZlibEncoder::new(Vec::new(), Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    fn raw_deflate(data: &[u8]) -> Vec<u8> {
        let mut e = DeflateEncoder::new(Vec::new(), Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    /// 223 bytes, compressed with `brotli -q 5`.
    const BR: [u8; 40] = [
        0x1f, 0xde, 0x00, 0x00, 0x44, 0x37, 0x97, 0xea, 0xe8, 0x62, 0xec, 0x0e, 0x0d, 0x82, 0x20, 0xf8, 0x95, 0xe7, 0x30, 0x13, 0x29, 0x96, 0x47, 0xe5, 0x55, 0xc6, 0x15, 0x78, 0xab, 0x69, 0xa8, 0x9e, 0xc0, 0xcd, 0x6f, 0x58, 0x1d, 0x56, 0x3d, 0x02,
    ];

    fn text() -> Vec<u8> {
        "<html>a page that repeats itself, a page that repeats itself. "
            .repeat(300)
            .into_bytes()
    }

    #[test]
    fn decodes_gzip_and_keeps_the_headers() {
        let body = text();
        let packed = gzip(&body);
        let (port, _) = serve(vec![reply("Content-Encoding: gzip\r\n", &packed)]);
        let response = Client::new().get(&url(port)).unwrap();
        assert_eq!(response.body, body);
        assert_eq!(response.header("content-encoding"), Some("gzip"));
        assert_eq!(response.header("content-length"), Some(packed.len().to_string().as_str()));
        assert!(!response.encoded());
    }

    #[test]
    fn decodes_deflate_with_and_without_the_zlib_wrapper() {
        let body = text();
        let (port, _) = serve(vec![reply("Content-Encoding: deflate\r\n", &zlib(&body)), reply("Content-Encoding: deflate\r\n", &raw_deflate(&body))]);
        let mut client = Client::new();
        assert_eq!(client.get(&url(port)).unwrap().body, body);
        assert_eq!(client.get(&url(port)).unwrap().body, body);
    }

    #[test]
    fn decodes_brotli() {
        let (port, _) = serve(vec![reply("Content-Encoding: br\r\n", &BR)]);
        let body = Client::new().get(&url(port)).unwrap().body.clone();
        assert_eq!(body.len(), 223);
        assert!(body.starts_with(b"bare brotli test body, hello hello"));
    }

    #[test]
    fn decodes_a_chunked_gzip_body_cut_at_any_place() {
        let body = text();
        let packed = gzip(&body);
        for piece in [1, 7, 4096] {
            let (port, _) = serve(vec![chunked("Content-Encoding: gzip\r\n", &packed, piece)]);
            assert_eq!(Client::new().get(&url(port)).unwrap().body, body, "piece {piece}");
        }
    }

    #[test]
    fn decodes_a_list_of_codings_from_the_last_one_back() {
        // `deflate, gzip`: deflate was applied first, gzip last. So gzip is undone first.
        let body = text();
        let (port, _) = serve(vec![reply("Content-Encoding: deflate, GZIP\r\n", &gzip(&zlib(&body))), reply("Content-Encoding: x-gzip, identity\r\n", &gzip(b"x"))]);
        let mut client = Client::new();
        assert_eq!(client.get(&url(port)).unwrap().body, body);
        assert_eq!(client.get(&url(port)).unwrap().body, b"x");
    }

    #[test]
    fn a_bad_or_cut_body_is_an_error() {
        let packed = gzip(&text());
        let cases: [(&str, Vec<u8>); 4] = [
            ("gzip", b"this is not gzip at all".to_vec()),
            ("gzip", packed[..packed.len() / 2].to_vec()),
            ("br", b"this is not brotli at all".to_vec()),
            ("deflate", vec![0x78, 0x9c, 0xff, 0xff, 0xff]),
        ];
        for (coding, body) in cases {
            let (port, _) = serve(vec![reply(&format!("Content-Encoding: {coding}\r\n"), &body)]);
            assert!(Client::new().get(&url(port)).is_err(), "{coding} {body:?}");
        }
    }

    #[test]
    fn an_empty_body_with_a_coding_is_an_empty_body() {
        let (port, _) = serve(vec![reply("Content-Encoding: gzip\r\n", b""), reply("Content-Encoding: br\r\n", b"")]);
        let mut client = Client::new();
        assert!(client.get(&url(port)).unwrap().body.is_empty());
        assert!(client.get(&url(port)).unwrap().body.is_empty());
    }

    #[test]
    fn a_coding_we_do_not_know_is_left_as_it_is_and_marked() {
        let (port, _) = serve(vec![reply("Content-Encoding: zstd\r\n", b"zzz"), reply("Content-Encoding: gzip, gzip, gzip, gzip, gzip, gzip\r\n", b"zzz"), reply("Content-Encoding: identity\r\n", b"ok")]);
        let mut client = Client::new();
        let first = client.get(&url(port)).unwrap();
        assert_eq!(first.body, b"zzz");
        assert!(first.encoded());
        let second = client.get(&url(port)).unwrap();
        assert!(second.encoded(), "more than 5 codings");
        let third = client.get(&url(port)).unwrap();
        assert_eq!(third.body, b"ok");
        assert!(!third.encoded());
    }

    #[test]
    fn bytes_after_the_end_of_the_stream_are_dropped_and_the_connection_is_reused() {
        let mut packed = gzip(b"first");
        packed.extend_from_slice(&[0u8; 9]);
        let (port, _) = serve(vec![reply("Content-Encoding: gzip\r\n", &packed), reply("", b"second")]);
        let mut client = Client::new();
        assert_eq!(client.get(&url(port)).unwrap().body, b"first");
        assert_eq!(client.get(&url(port)).unwrap().body, b"second");
    }

    #[test]
    fn a_bomb_stops_at_the_cap_of_the_caller_and_at_the_cap_of_the_client() {
        // 130 MiB of zeros is about 130 KB of gzip.
        let mut e = GzEncoder::new(Vec::new(), Compression::fast());
        let zeros = vec![0u8; 1 << 20];
        for _ in 0..130 {
            e.write_all(&zeros).unwrap();
        }
        let packed = e.finish().unwrap();
        assert!(packed.len() < 1 << 20);
        let (port, _) = serve(vec![reply("Content-Encoding: gzip\r\n", &packed), reply("Content-Encoding: gzip\r\n", &packed)]);
        let u = url(port);
        let mut client = Client::new();
        // The caller asks for at most 1 MiB: an error, after about 1 MiB of work.
        let err = client.send_once(&Request::get(&u), 1 << 20).err().unwrap();
        assert!(err.to_string().contains("too big"), "{err}");
        // The connection holds unread bytes, so the next request opens a new one: the server takes one only. Use a new client and server.
        let (port, _) = serve(vec![reply("Content-Encoding: gzip\r\n", &packed)]);
        let mut streamed = Client::new().get_stream(&url(port)).unwrap();
        let (mut total, mut buf) = (0usize, vec![0u8; 64 * 1024]);
        let err = loop {
            match streamed.body.read(&mut buf) {
                Ok(0) => panic!("the bomb ended"),
                Ok(n) => total += n,
                Err(e) => break e,
            }
        };
        assert!(err.to_string().contains("too big"), "{err}");
        assert!(total <= 128 * 1024 * 1024 + 64 * 1024 && total > 100 * 1024 * 1024, "{total}");
    }

    #[test]
    fn a_reply_that_says_vary_accept_encoding_is_still_cached() {
        let body = text();
        let packed = gzip(&body);
        let fresh = "Content-Encoding: gzip\r\nVary: accept-encoding\r\nCache-Control: max-age=60\r\n";
        let (port, heads) = serve(vec![reply(fresh, &packed)]);
        let mut client = Client::new();
        assert_eq!(client.get(&url(port)).unwrap().body, body);
        assert_eq!(client.get(&url(port)).unwrap().body, body);
        assert_eq!(heads.try_iter().count(), 1);
    }

    #[test]
    fn a_reply_that_varies_on_more_than_accept_encoding_is_not_cached() {
        let (port, heads) = serve(vec![reply("Vary: Accept-Encoding, Cookie\r\nCache-Control: max-age=60\r\n", b"a"), reply("Vary: Accept-Encoding, Cookie\r\nCache-Control: max-age=60\r\n", b"a")]);
        let mut client = Client::new();
        client.get(&url(port)).unwrap();
        client.get(&url(port)).unwrap();
        assert_eq!(heads.try_iter().count(), 2);
    }
}
