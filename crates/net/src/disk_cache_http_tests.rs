use super::*;
use crate::http::{Client, Request};
use bare_url::Url;
use std::net::TcpListener;
use std::sync::Barrier;

const PUBLIC: &str = "Cache-Control: public, max-age=60\r\n";
const BODY: &[u8] = b"cached reply";

struct Temp(PathBuf);

impl Temp {
    fn new() -> Self {
        let mut suffix = [0; 16];
        bare_os::random::fill(&mut suffix).unwrap();
        let dir =
            std::env::temp_dir().join(format!("bare-http-{:032x}", u128::from_ne_bytes(suffix)));
        fs::create_dir(&dir).unwrap();
        Self(dir)
    }

    fn client(&self) -> Client {
        Client::new().with_disk_cache(Some(self.0.clone()))
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn server(replies: Vec<String>) -> (Url, JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = Url::parse(&format!(
        "http://{}/bundle.js",
        listener.local_addr().unwrap()
    ))
    .unwrap();
    let thread = thread::spawn(move || {
        replies
            .into_iter()
            .map(|reply| {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                use std::io::BufRead;
                let mut input = io::BufReader::new(&mut socket);
                while !request.ends_with(b"\r\n\r\n") {
                    assert_ne!(input.read_until(b'\n', &mut request).unwrap(), 0);
                }
                socket.write_all(reply.as_bytes()).unwrap();
                String::from_utf8(request).unwrap().to_ascii_lowercase()
            })
            .collect()
    });
    (url, thread)
}

fn reply(headers: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nConnection: close\r\n{headers}Content-Length: {}\r\n\r\n{}",
        BODY.len(),
        str::from_utf8(BODY).unwrap()
    )
}

fn stream(client: &mut Client, request: &Request) -> Vec<u8> {
    let mut streamed = client.send_stream_cached(request).unwrap();
    let mut body = Vec::new();
    streamed.body.read_to_end(&mut body).unwrap();
    client.keep(&streamed, &body);
    body
}

#[test]
fn streams_a_reply_from_disk_into_a_second_client() {
    let dir = Temp::new();
    let (url, origin) = server(vec![reply(PUBLIC)]);
    assert_eq!(stream(&mut dir.client(), &Request::get(&url)), BODY);
    assert_eq!(origin.join().unwrap().len(), 1);
    let mut second = dir.client();
    assert_eq!(stream(&mut second, &Request::get(&url)), BODY);
    // Remove the disk copy. The second client now has its own memory hit.
    for (_, path, _) in scan(&dir.0).unwrap().1 {
        fs::remove_file(path).unwrap();
    }
    assert_eq!(second.get(&url).unwrap().body, BODY);
}

#[test]
fn revalidates_stale_disk_entries_and_persists_the_304() {
    let dir = Temp::new();
    let (url, origin) = server(vec![
        reply(
            "Cache-Control: public, max-age=0\r\nETag: \"old\"\r\nLast-Modified: Sat, 21 Nov 2026 00:00:00 GMT\r\n",
        ),
        format!("HTTP/1.1 304 Not Modified\r\nConnection: close\r\n{PUBLIC}ETag: \"new\"\r\n\r\n"),
    ]);
    assert_eq!(stream(&mut dir.client(), &Request::get(&url)), BODY);
    assert_eq!(stream(&mut dir.client(), &Request::get(&url)), BODY);
    let seen = origin.join().unwrap();
    assert!(seen[1].contains("if-none-match: \"old\"\r\n"));
    assert!(seen[1].contains("if-modified-since: sat, 21 nov 2026 00:00:00 gmt\r\n"));
    let stored = dir.client().get(&url).unwrap();
    assert_eq!(stored.header("etag"), Some("\"new\""));
    assert_eq!(stored.body, BODY);
}

#[test]
fn a_304_that_makes_the_reply_private_removes_the_disk_entry() {
    let dir = Temp::new();
    let (url, origin) = server(vec![
        reply("Cache-Control: public, max-age=0\r\nETag: \"old\"\r\n"),
        "HTTP/1.1 304 Not Modified\r\nConnection: close\r\nCache-Control: private, max-age=60\r\n\r\n".into(),
    ]);
    assert_eq!(dir.client().get(&url).unwrap().body, BODY);
    assert_eq!(scan(&dir.0).unwrap().1.len(), 1);
    assert_eq!(dir.client().get(&url).unwrap().body, BODY);
    assert!(origin.join().unwrap()[1].contains("if-none-match: \"old\"\r\n"));
    assert!(scan(&dir.0).unwrap().1.is_empty());
}

#[test]
fn excludes_private_per_jar_and_credentialed_responses() {
    for headers in [
        "Cache-Control: public, max-age=60\r\nCache-Control: no-store\r\n",
        "Cache-Control: public, private=\"field\", max-age=60\r\n",
        "Cache-Control: max-age=60\r\n",
        "Cache-Control: max-age=60, extension=\"one, public, two\"\r\n",
        "Cache-Control: immutable, max-age=60\r\nSet-Cookie: key=value\r\n",
    ] {
        let dir = Temp::new();
        let (url, origin) = server(vec![reply(headers)]);
        assert_eq!(stream(&mut dir.client(), &Request::get(&url)), BODY);
        origin.join().unwrap();
        assert!(scan(&dir.0).unwrap().1.is_empty(), "{headers}");
    }
    let dir = Temp::new();
    let (url, origin) = server(vec![
        reply(PUBLIC),
        reply("Set-Cookie: key=value; Path=/\r\n"),
        reply(PUBLIC),
    ]);
    let mut client = dir.client();
    let mut request = Request::get(&url);
    request.headers = &[("Authorization", "Bearer example")];
    assert_eq!(stream(&mut client, &request), BODY);
    client.send_once(&Request::get(&url), BODY.len()).unwrap();
    assert_eq!(stream(&mut client, &Request::get(&url)), BODY);
    drop(client);
    let seen = origin.join().unwrap();
    assert!(seen[0].contains("authorization: bearer example\r\n"));
    assert!(seen[2].contains("cookie: key=value\r\n"));
    assert!(scan(&dir.0).unwrap().1.is_empty());
}

fn entry(body: &[u8]) -> Arc<Response> {
    Arc::new(Response {
        status: 200,
        headers: vec![("cache-control".into(), "public, max-age=60".into())],
        body: body.to_vec(),
        ..Response::default()
    })
}

fn job(key: &str, body: &[u8]) -> Job {
    Job {
        id: sha256(key.as_bytes()),
        write: Some((
            key.to_owned(),
            entry(body),
            SystemTime::now(),
            Duration::from_secs(60),
        )),
        charge: 0,
    }
}

#[test]
fn deletes_corrupt_truncated_and_wrong_version_entries() {
    let dir = Temp::new();
    let disk = Disk::new(dir.0.clone());
    let key = "key";
    let job = job(key, BODY);
    let file = dir.0.join(name(job.id));
    for edit in [0, 1, 2, 3] {
        commit(&dir.0, CAP, &job).unwrap();
        let mut bytes = fs::read(&file).unwrap();
        match edit {
            0 => *bytes.last_mut().unwrap() ^= 1,
            1 => bytes.truncate(HEADER - 1),
            2 => bytes[8] ^= 1,
            _ => bytes[HEADER + 20] ^= 1,
        }
        fs::write(&file, bytes).unwrap();
        assert!(disk.get(key).is_none());
        assert!(!file.exists());
    }
}

#[test]
fn concurrent_writers_keep_one_complete_file_and_replace_an_existing_file() {
    let dir = Temp::new();
    commit(&dir.0, CAP, &job("key", b"old")).unwrap();
    let start = Barrier::new(2);
    let path = dir.0.as_path();
    thread::scope(|scope| {
        for value in [b"first".as_slice(), b"second".as_slice()] {
            let start = &start;
            scope.spawn(move || {
                start.wait();
                commit(path, CAP, &job("key", value)).unwrap();
            });
        }
    });
    let disk = Disk::new(dir.0.clone());
    let (response, _) = disk.get("key").unwrap();
    assert!([b"first".as_slice(), b"second".as_slice()].contains(&response.body.as_slice()));
    assert_eq!(scan(&dir.0).unwrap().1.len(), 1);
    assert!(!dir.0.join(".pending").exists());
}

#[test]
fn evicts_the_least_recently_used_file_before_crossing_the_cap() {
    let dir = Temp::new();
    let cap = 500;
    for key in ["a", "b"] {
        commit(&dir.0, cap, &job(key, &[0; 128])).unwrap();
    }
    for (key, time) in [("a", 1), ("b", 2)] {
        let path = dir.0.join(name(sha256(key.as_bytes())));
        OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(time))
            .unwrap();
    }
    let disk = Disk::new(dir.0.clone());
    assert!(disk.get("a").is_some());
    commit(&dir.0, cap, &job("c", &[0; 128])).unwrap();
    assert!(disk.get("a").is_some());
    assert!(disk.get("b").is_none());
    assert!(disk.get("c").is_some());
    assert!(scan(&dir.0).unwrap().0 <= cap);
}
