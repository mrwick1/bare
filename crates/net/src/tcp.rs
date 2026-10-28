//! Open a TCP connection, with time limits and "Happy Eyeballs".
//!
//! A site often has more than one address: some IPv6, some IPv4.
//! If we try them one by one, one dead address can make us wait for minutes.
//! Happy Eyeballs: start with the first address. If it has not connected after 250 ms,
//! start the next one too, without stopping the first. The first to connect wins.

use std::io;
use std::net::{SocketAddr, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
// Give up on one address after 10 seconds. Without a limit, Linux waits about 2 minutes.
const READ_TIMEOUT: Duration = Duration::from_secs(30);
// Give up if the server sends nothing for 30 seconds.
const NEXT_TRY_DELAY: Duration = Duration::from_millis(250);
// How long one try may run alone before we start the next one.

/// Connect to the first address that answers. Set time limits on the connection.
pub fn connect(addrs: &[SocketAddr]) -> io::Result<TcpStream> {
    let stream = race(&order(addrs), NEXT_TRY_DELAY, |addr| TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT))?;
    // Each try is one real connection, with its own 10-second limit.
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    stream.set_write_timeout(Some(READ_TIMEOUT))?;
    // From now on, a read or write that waits longer than 30 seconds gives an error.
    Ok(stream)
}

/// Put IPv6 first, then switch between IPv6 and IPv4: [v6, v4, v6, v4, ...].
fn order(addrs: &[SocketAddr]) -> Vec<SocketAddr> {
    let (v6, v4): (Vec<SocketAddr>, Vec<SocketAddr>) = addrs.iter().partition(|a| a.is_ipv6());
    // `partition` splits one list into two. JS: two `filter` calls in one step.
    let mut out = Vec::new();
    for i in 0..v6.len().max(v4.len()) {
        out.extend(v6.get(i));
        out.extend(v4.get(i));
        // `get(i)` gives `None` past the end, and `extend` then adds nothing.
    }
    out
    // Switching means a broken IPv6 network costs us only one short wait.
}

/// Run `attempt` on each address, as described at the top of this file.
/// `attempt` is a parameter so that tests can use fake connections.
fn race<T: Send + 'static>(addrs: &[SocketAddr], delay: Duration, attempt: fn(SocketAddr) -> io::Result<T>) -> io::Result<T> {
    // `T` is whatever `attempt` gives back: a connection here, a number in the tests.
    let (send, results) = mpsc::channel();
    // A channel is a queue between threads. Each try puts its result in it, and we take
    // results out here, in the order they finish.
    let mut next = 0; // the next address to try
    let mut running = 0; // tries that have not finished yet
    let mut last_error = io::Error::new(io::ErrorKind::InvalidInput, "no addresses to connect to");

    loop {
        if next < addrs.len() {
            let send = send.clone();
            let addr = addrs[next];
            thread::spawn(move || {
                let _ = send.send(attempt(addr));
                // A thread runs at the same time as the rest of the program.
                // If another try already won, nobody reads this result. It is dropped,
                // and a connection in it is closed.
            });
            next += 1;
            running += 1;
        } else if running == 0 {
            return Err(last_error);
            // Every try failed. Report the last error.
        }

        let wait = if next < addrs.len() { delay } else { Duration::MAX };
        // While addresses are left, wait only `delay` for a result. Then start the next try.
        match results.recv_timeout(wait) {
            Ok(Ok(value)) => return Ok(value),
            // The first try to connect wins.
            Ok(Err(e)) => {
                running -= 1;
                last_error = e;
                // A try failed. The loop starts the next one at once, without waiting.
            }
            Err(_) => {}
            // No result in time. The loop starts the next try, and the slow one keeps going.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::time::Instant;

    fn addr(text: &str) -> SocketAddr {
        text.parse().unwrap()
    }

    #[test]
    fn orders_ipv6_first() {
        let addrs = [addr("1.1.1.1:80"), addr("2.2.2.2:80"), addr("[::1]:80")];
        assert_eq!(order(&addrs), vec![addr("[::1]:80"), addr("1.1.1.1:80"), addr("2.2.2.2:80")]);
        // IPv6 first, then IPv4, then whatever is left.
    }

    #[test]
    fn slow_address_does_not_block() {
        let addrs = [addr("[::1]:1"), addr("127.0.0.1:2")];
        let start = Instant::now();
        let winner = race(&addrs, Duration::from_millis(250), |a| {
            if a.port() == 1 {
                thread::sleep(Duration::from_secs(5));
                // A fake address that never answers in time.
            }
            Ok(a.port())
        });
        assert_eq!(winner.unwrap(), 2);
        let took = start.elapsed();
        assert!(took >= Duration::from_millis(250) && took < Duration::from_secs(1), "took {took:?}");
        // The second address starts after 250 ms. We do not wait 5 seconds for the first.
    }

    #[test]
    fn failure_starts_next_at_once() {
        let addrs = [addr("[::1]:1"), addr("127.0.0.1:2")];
        let start = Instant::now();
        let winner = race(&addrs, Duration::from_millis(250), |a| {
            if a.port() == 1 { Err(io::ErrorKind::ConnectionRefused.into()) } else { Ok(a.port()) }
        });
        assert_eq!(winner.unwrap(), 2);
        assert!(start.elapsed() < Duration::from_millis(200));
        // The first address failed at once, so we do not wait 250 ms for the next one.
    }

    #[test]
    fn all_fail() {
        let addrs = [addr("[::1]:1"), addr("127.0.0.1:2")];
        let result = race(&addrs, Duration::from_millis(50), |_| Err::<(), _>(io::ErrorKind::ConnectionRefused.into()));
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::ConnectionRefused);
        assert!(race(&[], Duration::from_millis(50), |_| Ok(())).is_err());
        // No addresses at all is an error too.
    }

    #[test]
    fn real_connection_has_time_limits() {
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        // A real server on this machine. Port 0 means "any free port".
        let stream = connect(&[server.local_addr().unwrap()]).unwrap();
        assert_eq!(stream.read_timeout().unwrap(), Some(READ_TIMEOUT));
        assert_eq!(stream.write_timeout().unwrap(), Some(READ_TIMEOUT));
    }
}
