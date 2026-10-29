// The audit event, read back from a log subscriber.
use std::io::Write;
use std::sync::{Arc, Mutex};

use auth_rs::http::{AuthContext, AuthMethod};
use axum::http::HeaderMap;

// Collects what the subscriber writes, so the event's fields can be read.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn context(key_name: Option<&str>) -> AuthContext<()> {
    AuthContext {
        key_name: key_name.map(str::to_string),
        key_id: None,
        permissions: (),
        method: AuthMethod::ApiKey,
    }
}

#[test]
fn an_event_names_who_did_what_and_how_it_ended() {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .finish();

    let mut headers = HeaderMap::new();
    headers.insert("x-request-id", "req-1".parse().unwrap());
    tracing::subscriber::with_default(subscriber, || {
        web_rs::audit::record(&context(Some("AWX")), &headers, "sync", "src-a", "ok");
        web_rs::audit::record(&context(None), &HeaderMap::new(), "evict", "src-b", "ok");
    });

    let log = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    assert!(log.contains("actor=\"AWX\""), "{log}");
    assert!(log.contains("request_id=\"req-1\""), "{log}");
    assert!(log.contains("action=\"sync\""), "{log}");
    assert!(log.contains("resource=\"src-a\""), "{log}");
    // No keys configured: the actor is "open", and no request id is ""
    assert!(log.contains("actor=\"open\""), "{log}");
    assert!(log.contains("request_id=\"\""), "{log}");
}
