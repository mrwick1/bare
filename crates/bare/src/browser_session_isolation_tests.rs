use super::*;
use crate::resources::test_server::{Reply, serve};

#[test]
fn isolation_request_comes_from_the_final_document_response() {
    let server = serve(|path| {
        let redirect = |target| Reply::new(302, None, "").header("Location", target);
        let document = || Reply::new(200, Some("text/html"), "<p>document</p>");
        match path {
            "/to-plain" => redirect("/plain")
                .header("Cross-Origin-Opener-Policy", "same-origin")
                .header("Cross-Origin-Embedder-Policy", "require-corp"),
            "/to-isolated" => redirect("/isolated"),
            "/isolated" => document()
                .header("Cross-Origin-Opener-Policy", "same-origin")
                .header("Cross-Origin-Embedder-Policy", "require-corp"),
            "/credentialless" => document()
                .header("Cross-Origin-Opener-Policy", "same-origin")
                .header("Cross-Origin-Embedder-Policy", "credentialless"),
            "/report-only" => document()
                .header("Cross-Origin-Opener-Policy", "same-origin")
                .header("Cross-Origin-Embedder-Policy-Report-Only", "require-corp"),
            "/duplicate" => document()
                .header("Cross-Origin-Opener-Policy", "same-origin")
                .header("Cross-Origin-Embedder-Policy", "require-corp")
                .header("Cross-Origin-Embedder-Policy", "unsafe-none"),
            _ => document(),
        }
    });
    let cases = [
        ("/to-plain", None),
        ("/to-isolated", Some(EmbedderPolicy::RequireCorp)),
        ("/credentialless", Some(EmbedderPolicy::Credentialless)),
        ("/report-only", None),
        ("/duplicate", None),
    ];
    for (path, requested) in cases {
        for streaming in [false, true] {
            let url = server.url(path);
            let mut client = Client::new();
            let feed = streaming.then(|| Arc::new(Feed::default()));
            let producer = feed.clone();
            let response = load_document(
                &mut client, &url, None, None, &SessionLimits::default(),
                Instant::now() + std::time::Duration::from_secs(5), None, false, producer.as_ref(), false,
            ).unwrap();
            assert_eq!(response.isolation_requested, requested, "{path}");
            if let Some(feed) = feed {
                let Some(Msg::Head(head)) = feed.pop() else { panic!("expected a streamed response head") };
                assert_eq!(head.isolation_requested, requested, "{path}");
            }
        }
    }
}
