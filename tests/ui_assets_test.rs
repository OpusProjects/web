// The web UI served from a temporary build, through a real router: what a
// browser, the app's own fetches and an API client each get back.
use std::sync::Arc;
use std::time::Duration;

use auth_rs::http::{ApiKeyRegistry, Permission, ResolvedApiKey, SessionKind, SessionStore};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::response::Response;
use tower::ServiceExt;
use web_rs::UiAssets;
use web_rs::ui_assets::{page_for_browsers, serve};

#[derive(Clone)]
struct Perm;

impl Permission for Perm {
    fn admin() -> Self {
        Perm
    }
}

const COOKIE: &str = "test_session";

// A built app: its index.html, and one content-named file under /assets.
fn build() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("index.html"), "<!doctype html>app").unwrap();
    std::fs::create_dir(dir.path().join("assets")).unwrap();
    std::fs::write(dir.path().join("assets/index-3f2a.js"), "js").unwrap();
    dir
}

struct Gate {
    keys: Arc<ApiKeyRegistry<Perm>>,
    sessions: Arc<SessionStore<Perm>>,
}

fn gate(with_keys: bool) -> Gate {
    let keys = if with_keys {
        vec![ResolvedApiKey {
            id: "k".to_string(),
            name: "k".to_string(),
            secret: "s".to_string(),
            permissions: Perm,
        }]
    } else {
        Vec::new()
    };
    Gate {
        keys: Arc::new(ApiKeyRegistry::new(keys)),
        sessions: Arc::new(SessionStore::new()),
    }
}

fn app(assets: UiAssets<Perm>) -> Router {
    Router::new()
        .route(
            "/metrics",
            axum::routing::get(|| async { "metrics" }).layer(axum::middleware::from_fn_with_state(
                assets.clone(),
                page_for_browsers::<Perm>,
            )),
        )
        .fallback(serve::<Perm>)
        .with_state(assets)
}

async fn ask(app: &Router, method: &str, path: &str, headers: &[(&str, &str)]) -> Response {
    let mut req = Request::builder().method(method).uri(path);
    for (name, value) in headers {
        req = req.header(*name, *value);
    }
    app.clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn text(response: Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn cache(response: &Response) -> &str {
    response.headers()[header::CACHE_CONTROL].to_str().unwrap()
}

const PAGE: (&str, &str) = ("accept", "text/html,application/xhtml+xml");

#[tokio::test]
async fn an_open_instance_serves_its_app_to_anyone() {
    let dir = build();
    let g = gate(false);
    let app = app(UiAssets::new(dir.path(), COOKIE).with_gate(g.keys, g.sessions));

    // A page of the app: no such file, so the app's index.html
    let resp = ask(&app, "GET", "/sources/abc", &[PAGE]).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(cache(&resp), "no-cache");
    assert_eq!(text(resp).await, "<!doctype html>app");

    // A built file: as is, kept for good
    let resp = ask(&app, "GET", "/assets/index-3f2a.js", &[]).await;
    assert_eq!(cache(&resp), "public, max-age=31536000, immutable");
    assert_eq!(text(resp).await, "js");
}

#[tokio::test]
async fn an_api_path_is_never_the_app() {
    let dir = build();
    let app = app(UiAssets::new(dir.path(), COOKIE));

    let resp = ask(&app, "GET", "/api/v1/typo", &[PAGE]).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(text(resp).await, r#"{"error":"no route for /api/v1/typo"}"#);

    let resp = ask(&app, "POST", "/index.html", &[]).await;
    assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert!(text(resp).await.contains("read-only"));
}

// With keys configured, a page load without a session goes to the sign-in
// form first, carrying where it was going.
#[tokio::test]
async fn a_signed_out_browser_signs_in_first() {
    let dir = build();
    let g = gate(true);
    let sessions = Arc::clone(&g.sessions);
    let app = app(UiAssets::new(dir.path(), COOKIE).with_gate(g.keys, g.sessions));

    let resp = ask(&app, "GET", "/sources?x=1", &[PAGE]).await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        resp.headers()[header::LOCATION],
        "/api/v1/login?next=/sources%3Fx%3D1"
    );

    // The app's own files are not gated: the sign-in page needs some
    let resp = ask(&app, "GET", "/assets/index-3f2a.js", &[]).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // A stale cookie is no session
    let stale = format!("{COOKIE}=nope");
    let resp = ask(&app, "GET", "/", &[PAGE, ("cookie", &stale)]).await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);

    // A live one is
    let issued = sessions.create(
        "k".into(),
        SessionKind::ApiKey,
        Some("k".into()),
        Perm,
        Duration::from_secs(60),
    );
    let live = format!("{COOKIE}={}", issued.token);
    let resp = ask(&app, "GET", "/", &[PAGE, ("cookie", &live)]).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

// /metrics is Prometheus's for a scraper and the app's page for a browser.
#[tokio::test]
async fn metrics_is_a_page_only_for_a_browser() {
    let dir = build();
    let g = gate(true);
    let gated = app(UiAssets::new(dir.path(), COOKIE).with_gate(g.keys, g.sessions));

    let resp = ask(&gated, "GET", "/metrics", &[]).await;
    assert_eq!(text(resp).await, "metrics");
    let resp = ask(&gated, "GET", "/metrics", &[PAGE]).await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);

    let open = app(UiAssets::new(dir.path(), COOKIE));
    let resp = ask(&open, "GET", "/metrics", &[PAGE]).await;
    assert_eq!(cache(&resp), "no-cache");
    assert_eq!(text(resp).await, "<!doctype html>app");
}

// A directory with no index.html still serves (the API works without its UI);
// the warning at start-up is what tells an operator.
#[tokio::test]
async fn a_build_without_its_index_still_answers() {
    let empty = tempfile::tempdir().unwrap();
    let app = app(UiAssets::new(empty.path(), COOKIE));
    let resp = ask(&app, "GET", "/sources", &[PAGE]).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
