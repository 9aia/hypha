#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::pedantic,
    clippy::missing_panics_doc
)]

//! `NodeClient` wire tests against canned-HTTP stubs — raw TCP
//! responders, no node harness. The full-cycle engine tests against a
//! real node live in peregrin until the P4 re-home.

use hypha_sync::NodeClient;

/// Canned-HTTP stub: answers every accepted connection with one fixed
/// response until the port is dropped. Returns the bound port.
fn canned_server(status: &str, body: &str) -> u16 {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let res = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    std::thread::spawn(move || {
        loop {
            let Ok((mut conn, _)) = listener.accept() else {
                return;
            };
            use std::io::Write;
            let _ = conn.write_all(res.as_bytes());
        }
    });
    port
}

#[test]
fn client_rejects_bad_base_url_and_classifies_errors() {
    let err = NodeClient::new("ftp://node", None).err().unwrap();
    assert!(err.to_string().contains("http(s)://"));
    assert!(!err.is_transport());
    let http = hypha_sync::client::NodeError::Http {
        status: 500,
        body: "x".into(),
    };
    assert!(!http.is_transport());
    let io: hypha_sync::client::NodeError = std::io::Error::other("reset").into();
    assert!(io.is_transport());
    // ureq transport — a refused connect.
    let client = NodeClient::new("http://127.0.0.1:1/", None).unwrap();
    assert!(client.health().unwrap_err().is_transport());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_malformed_bodies_and_http_errors() {
    // `sessions` field not an array.
    let port = canned_server("200 OK", "{}");
    let client = NodeClient::new(&format!("http://127.0.0.1:{port}"), None).unwrap();
    let err = tokio::task::spawn_blocking(move || client.list_entities(None, false))
        .await
        .unwrap()
        .unwrap_err();
    assert!(err.to_string().contains("missing entities array"));

    // Non-2xx on `health` → the `expect_json` Http arm.
    let port = canned_server("503 Service Unavailable", "{\"error\":\"down\"}");
    let client = NodeClient::new(&format!("http://127.0.0.1:{port}"), None).unwrap();
    let err = tokio::task::spawn_blocking(move || client.health())
        .await
        .unwrap()
        .unwrap_err();
    assert!(err.to_string().contains("503"));

    // Non-2xx on the SSE open → Http with the error body.
    let port = canned_server("404 Not Found", "{\"error\":\"no\"}");
    let client = NodeClient::new(&format!("http://127.0.0.1:{port}"), None).unwrap();
    let err = tokio::task::spawn_blocking(move || client.events())
        .await
        .unwrap()
        .err()
        .unwrap();
    assert!(err.to_string().contains("404"));

    // Entity docs missing `id` degrade to an empty id — tolerant
    // decode; a non-2xx still surfaces the status.
    let port = canned_server(
        "200 OK",
        "{\"entities\":[{\"id\":\"s9\",\"kind\":\"sepia-session\"}]}",
    );
    let client = NodeClient::new(&format!("http://127.0.0.1:{port}"), None).unwrap();
    let docs = tokio::task::spawn_blocking(move || client.list_entities(None, false))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(docs[0]["id"], "s9");
    assert_eq!(docs[0]["kind"], "sepia-session");
    let port = canned_server("500 Internal Server Error", "boom");
    let client = NodeClient::new(&format!("http://127.0.0.1:{port}"), None).unwrap();
    let err = tokio::task::spawn_blocking(move || client.get_json("/api/node"))
        .await
        .unwrap()
        .unwrap_err();
    assert!(err.to_string().contains("500"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_sse_stream_ends_cleanly_at_eof() {
    // A 200 that closes after one frame — the pump sees `Ok(None)`,
    // the clean-EOF `Closed(None)` path (not a dropped connection).
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    // A plain thread, not `spawn_blocking` — an infinite accept loop on
    // the blocking pool keeps the runtime from shutting down at test
    // end (blocking tasks can't be aborted → 120s nextest kill).
    std::thread::spawn(move || {
        use std::io::Write;
        loop {
            let Ok((mut conn, _)) = listener.accept() else {
                return;
            };
            let _ = conn.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n\
                  event: heartbeat\ndata: {}\n\n",
            );
        }
    });
    let client = NodeClient::new(&format!("http://127.0.0.1:{port}"), None).unwrap();
    // std::thread + mpsc — a hung `spawn_blocking` can't be aborted and
    // would stall runtime shutdown past the watchdog.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let events = {
            let mut stream = client.events().unwrap();
            let first = stream.next_event().unwrap();
            let second = stream.next_event().unwrap();
            (first, second)
        };
        let _ = tx.send(events);
    });
    let events = rx
        .recv_timeout(std::time::Duration::from_secs(20))
        .expect("SSE EOF watchdog lapsed");
    assert_eq!(
        events,
        (Some(hypha_sync::client::FeedEvent::Heartbeat), None)
    );
}
