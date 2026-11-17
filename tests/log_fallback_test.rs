// A service that logs through the `log` crate and never sets up a tracing
// subscriber still sees these events: tracing hands them to `log` (its "log"
// feature, which axum turns on). Its own test binary, because a `log` logger
// is set once per process and no tracing subscriber may ever be set here.
use std::sync::Mutex;

use auth_rs::http::{AuthContext, AuthMethod};
use axum::http::HeaderMap;
use web_rs::UiAssets;

struct Captured(Mutex<Vec<String>>);

impl log::Log for Captured {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, record: &log::Record) {
        self.0.lock().unwrap().push(record.args().to_string());
    }
    fn flush(&self) {}
}

static LOGGER: Captured = Captured(Mutex::new(Vec::new()));

#[derive(Clone)]
struct Perm;

impl auth_rs::http::Permission for Perm {
    fn admin() -> Self {
        Perm
    }
}

#[test]
fn events_reach_a_log_logger_when_there_is_no_tracing_subscriber() {
    log::set_logger(&LOGGER).unwrap();
    log::set_max_level(log::LevelFilter::Trace);

    let auth = AuthContext {
        key_name: Some("AWX".to_string()),
        key_id: None,
        permissions: (),
        method: AuthMethod::ApiKey,
    };
    web_rs::audit::record(&auth, &HeaderMap::new(), "sync", "src-a", "ok");
    let empty = tempfile::tempdir().unwrap();
    let _ = UiAssets::<Perm>::new(empty.path(), "test_session");

    let lines = LOGGER.0.lock().unwrap().join("\n");
    assert!(lines.contains("audit") && lines.contains("AWX"), "{lines}");
    assert!(lines.contains("no index.html"), "{lines}");
}
