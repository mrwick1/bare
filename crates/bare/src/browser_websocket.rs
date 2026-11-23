//! The network side of `WebSocket` (`shims/websocket.js`). The shim calls one native, `ws(op, ...)`:
//! - `ws(0, url, protocols, cb)` opens a socket and gives its id. `url` is the `ws:` or `wss:` URL that the shim checked, `protocols`
//!   the subprotocols joined with `,`. A thread connects (`bare_net::ws::connect`), then reads. A second thread writes what the page
//!   sends, so a slow peer never holds the page.
//! - `ws(1, id, data)` sends a string as text, or bytes as binary. `ws(2, id, code, reason)` closes (`code` -1: none).
//!   `ws(3, id)` gives the bytes that are sent but not written yet (`bufferedAmount`).
//! - The reader thread sends each event to the page. `Browser::run_streams` takes them with the chunks of the event streams and calls
//!   `cb(kind, ...)` for each one, in its own task: 0 open (protocol, extensions), 1 text, 2 binary (an `ArrayBuffer`), 3 close (code, reason,
//!   clean), 4 failed (why). The shim fires the events.
//! - An open socket does not hold the page idle (`live_work_of(false)`), as an event stream does. `cb` is a persistent handle until the
//!   close, so the `WebSocket` object stays alive while it is open. When the page goes away, the channels drop: the writer sends a
//!   close with 1001, as Chrome does, and ends the connection. The reader stops.
//!
//! Gates: no `ws:` through an HTTP proxy; a message is at most `MAX_MESSAGE` bytes (close 1009).

use crate::browser::{Dom, R};
use crate::browser_scoped::*;
use crate::eng::{Eng, Ops};
use bare_js_api::{Args, Engine, Local, Persistent, Type};
use bare_net::http::Client;
use bare_net::ws::{self, Message, Stop};
use bare_url::Url;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

/// Open sockets on a page. One more fails at once. Each costs two threads.
const MAX_SOCKETS: usize = 16;
/// The largest message that a socket takes.
const MAX_MESSAGE: usize = 32 << 20;

/// What the reader thread sends to the page.
pub(crate) enum Event {
    Open(String, String),
    Text(String),
    Binary(Vec<u8>),
    Closed(u16, String, bool),
    Failed(String),
}

/// What the page sends to the writer thread.
enum Out {
    Data(u8, Vec<u8>),
    Close(Option<u16>, String),
}

pub(crate) struct Socket {
    id: u32,
    cb: Persistent,
    pub(crate) url: String,
    rx: Receiver<Event>,
    pub(crate) got: VecDeque<Event>,
    tx: Sender<Out>,
    buffered: Arc<AtomicUsize>,
    pub(crate) since: std::time::Instant,
}

impl Socket {
    /// Take what the thread sent. Gives whether a message waits for the page.
    pub(crate) fn pull(&mut self) -> bool {
        while let Ok(m) = self.rx.try_recv() {
            self.got.push_back(m);
        }
        !self.got.is_empty()
    }
}

/// `ws(op, ...)` of `shims/websocket.js`.
pub(crate) fn ws_host<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let num = |s: &mut Sc<'s, E>, i: usize| {
        let v = arg_s::<E>(s, a, i);
        if matches!(s.type_of(v), Type::Number) { s.to_number(v).unwrap_or(-1.0) } else { -1.0 }
    };
    let (op, id) = (num(s, 0), num(s, 1) as u32);
    match op as u8 {
        0 => open(d, s, a),
        1 => {
            let v = arg_s::<E>(s, a, 2);
            let (op, data) = if matches!(s.type_of(v), Type::String) {
                (ws::TEXT, d.string_arg_s(s, v)?.into_bytes())
            } else {
                (ws::BINARY, s.bytes(v).map(<[u8]>::to_vec).unwrap_or_default())
            };
            s.charge((data.len() / 64) as u64)?;
            if let Some(k) = d.fetch.sockets.borrow().iter().find(|k| k.id == id) {
                k.buffered.fetch_add(data.len(), Ordering::Relaxed);
                let _ = k.tx.send(Out::Data(op, data));
            }
            Ok(s.undefined())
        }
        2 => {
            let code = num(s, 2);
            let reason = d.arg_str_s(s, a, 3)?;
            let mut socks = d.fetch.sockets.borrow_mut();
            if let Some(i) = socks.iter().position(|k| k.id == id) {
                if code == -2.0 {
                    // A close before the open: drop the socket. The thread ends when it finds the page gone.
                    socks.remove(i);
                } else {
                    let _ = socks[i].tx.send(Out::Close((code >= 0.0).then_some(code as u16), reason));
                }
            }
            Ok(s.undefined())
        }
        _ => {
            let n = d.fetch.sockets.borrow().iter().find(|k| k.id == id).map_or(0, |k| k.buffered.load(Ordering::Relaxed));
            Ok(s.number(n as f64))
        }
    }
}

fn open<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let href = d.arg_str_s(s, a, 1)?;
    let protocols: Vec<String> = d.arg_str_s(s, a, 2)?.split(',').filter(|p| !p.is_empty()).map(str::to_string).collect();
    let url = href.strip_prefix("ws").and_then(|rest| Url::parse(&format!("http{rest}")).ok()).ok_or_else(|| s.type_error("bad WebSocket URL"))?;
    let raw = raw_arg::<E>(s, a, 3).ok_or_else(|| s.type_error("no WebSocket callback"))?;
    let cb = old::<E, _>(s, |it| Ok(it.persist_value(raw)))?;
    let origin = d.fetch_base().map(|b| crate::browser_fetch::origin(&b));
    let (tx, rx) = channel();
    let (out_tx, out_rx) = channel();
    let buffered = Arc::new(AtomicUsize::new(0));
    let mut socks = d.fetch.sockets.borrow_mut();
    let id = d.fetch.next_socket.get();
    d.fetch.next_socket.set(id + 1);
    if socks.len() >= MAX_SOCKETS {
        let _ = tx.send(Event::Failed("too many WebSockets are open".into()));
    } else {
        let client = d.fetch.client.borrow().fork();
        let (fail, b) = (tx.clone(), buffered.clone());
        let spawned = std::thread::Builder::new().name("bare-websocket".into()).spawn(move || socket_thread(client, url, protocols, origin, tx, out_rx, b));
        if spawned.is_err() {
            let _ = fail.send(Event::Failed("no thread for the WebSocket".into()));
        }
    }
    socks.push(Socket { id, cb, url: href, rx, got: VecDeque::new(), tx: out_tx, buffered, since: std::time::Instant::now() });
    Ok(s.number(f64::from(id)))
}

/// The reader thread: connect, start the writer, then send each message to the page.
fn socket_thread(client: Client, url: Url, protocols: Vec<String>, origin: Option<String>, tx: Sender<Event>, out: Receiver<Out>, buffered: Arc<AtomicUsize>) {
    let (mut r, mut w, protocol, extensions) = match ws::connect(&client, &url, &protocols, origin.as_deref(), MAX_MESSAGE) {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(Event::Failed(e.to_string()));
            bare_net::parallel::wake();
            return;
        }
    };
    let gone = tx.send(Event::Open(protocol, extensions)).is_err();
    bare_net::parallel::wake();
    if gone {
        w.shutdown();
        return;
    }
    let stop = w.clone();
    let writer = std::thread::Builder::new().name("bare-websocket-write".into()).spawn(move || {
        let mut ok = true;
        for m in out {
            let r = match m {
                Out::Data(op, data) => {
                    let r = w.send(op, &data);
                    buffered.fetch_sub(data.len(), Ordering::Relaxed);
                    r
                }
                Out::Close(code, reason) => w.close(code, &reason),
            };
            if r.is_err() {
                ok = false;
                break;
            }
        }
        // The page went away (1001, as Chrome sends), or a write failed: end the connection, so the reader stops too. A socket that
        // sent its close already sends nothing more.
        if ok {
            let _ = w.close(Some(1001), "");
        }
        w.shutdown();
    });
    if writer.is_err() {
        stop.shutdown();
    }
    loop {
        let ev = match r.recv() {
            Ok(Message::Text(t)) => Event::Text(t),
            Ok(Message::Binary(b)) => Event::Binary(b),
            Ok(Message::Close(code, reason)) => Event::Closed(code, reason, true),
            Err(Stop::Protocol(_, why)) => Event::Failed(why.into()),
            Err(Stop::Io(_)) => Event::Closed(1006, String::new(), false),
        };
        let end = matches!(ev, Event::Closed(..) | Event::Failed(_));
        let gone = tx.send(ev).is_err();
        // A window sleeps until a wake (`Page::wake_on_done`): without one, the message waits for the next tick.
        bare_net::parallel::wake();
        if gone || end {
            stop.shutdown();
            return;
        }
    }
}

/// Give the page what the sockets received: one task for each event, with a microtask checkpoint. A socket that closed is dropped.
/// `Browser::run_streams` calls it with its engine.
pub(crate) fn run_sockets<E: Eng>(dom: &Dom<E>, it: &mut E::Vm) -> R<E, usize> {
    let evs: Vec<(E::Raw, String, Event)> = {
        let mut socks = dom.fetch.sockets.borrow_mut();
        let mut evs = Vec::new();
        socks.retain_mut(|k| {
            k.pull();
            let mut open = true;
            for e in k.got.drain(..) {
                open &= !matches!(e, Event::Closed(..) | Event::Failed(_));
                evs.push((it.persistent_value(&k.cb), k.url.clone(), e));
            }
            open
        });
        evs
    };
    let n = evs.len();
    for (cb, url, e) in evs {
        if let Event::Failed(why) = &e {
            let text = format!("WebSocket connection to '{url}' failed: {why}");
            crate::browser_console::push_text(dom, it, crate::browser_console::ConsoleLevel::Error, &text);
        }
        it.host_scope_o(|it| {
            run::<E, ()>(it, |s| {
                let f = s.engine().raw_local(&cb);
                let args = match e {
                    Event::Open(p, x) => vec![s.number(0.0), s.string(&p)?, s.string(&x)?],
                    Event::Text(t) => {
                        s.charge((t.len() / 64) as u64)?;
                        vec![s.number(1.0), s.string(&t)?]
                    }
                    Event::Binary(b) => {
                        s.charge((b.len() / 64) as u64)?;
                        vec![s.number(2.0), s.new_array_buffer(b)?]
                    }
                    Event::Closed(code, reason, clean) => vec![s.number(3.0), s.number(f64::from(code)), s.string(&reason)?, s.boolean(clean)],
                    Event::Failed(why) => vec![s.number(4.0), s.string(&why)?],
                };
                let undef = s.undefined();
                s.call(f, undef, &args)?;
                Ok(())
            })?;
            it.host_checkpoint().map(|_| ())
        })?;
    }
    Ok(n)
}
