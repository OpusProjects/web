// The browsable API through a real router: the same route answers JSON to a
// client and a page to a browser, and leaves alone what is not its to draw.
use std::sync::OnceLock;

use auth_rs::http::BrowserIdentity;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::json;
use tower::ServiceExt;
use web_rs::pages::negotiate;
use web_rs::{Site, Spec};

fn site() -> Site {
    static SPEC: OnceLock<Spec> = OnceLock::new();
    let spec = SPEC.get_or_init(|| {
        let ok = json!({ "200": { "description": "ok" } });
        let doc = json!({
            "openapi": "3.1.0",
            "info": { "title": "test", "version": "0" },
            "paths": {
                "/api/v1/things": { "get": { "responses": ok } },
                "/api/v1/things/{id}": { "get": { "responses": ok } },
                "/api/v1/paged": { "get": { "parameters": [
                    { "name": "limit", "in": "query", "required": false, "schema": { "type": "integer" } }
                ], "responses": ok } },
                "/api/v1/whole": { "get": { "responses": ok } },
                "/api/v1/output/{id}": { "get": { "responses": ok } }
            }
        });
        Spec::new(serde_json::from_value(doc).unwrap())
    });
    Site {
        product: "test-service",
        spec,
        session_cookie: "test_session",
        login_cookie: "test_login",
        theme_cookie: "test_theme",
        raw_routes: &["/api/v1/output/{id}"],
    }
}

// A JSON body of about `bytes` bytes
fn big(bytes: usize) -> String {
    format!(r#"{{"blob":"{}"}}"#, "x".repeat(bytes))
}

fn app() -> Router {
    Router::new()
        .route(
            "/api/v1/things",
            get(|| async {
                let mut response = (
                    [(header::ETAG, "\"json-etag\"")],
                    axum::Json(json!({"name": "a"})),
                )
                    .into_response();
                response.extensions_mut().insert(BrowserIdentity {
                    key_name: "ops".to_string(),
                    csrf: "tok".to_string(),
                });
                response
            }),
        )
        .route(
            "/api/v1/output/{id}",
            get(|| async { axum::Json(json!({"all": {}})) }),
        )
        .route("/api/v1/text", get(|| async { "plain text" }))
        .route(
            "/api/v1/broken",
            get(|| async { ([(header::CONTENT_TYPE, "application/json")], "{not json") }),
        )
        .route(
            "/api/v1/paged",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "application/json")],
                    big(2 * 1024 * 1024),
                )
            }),
        )
        .route(
            "/api/v1/whole",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "application/json")],
                    big(2 * 1024 * 1024),
                )
            }),
        )
        .route(
            "/api/v1/huge",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "application/json")],
                    big(65 * 1024 * 1024),
                )
            }),
        )
        .layer(axum::middleware::from_fn_with_state(site(), negotiate))
}

async fn ask(path: &str, headers: &[(&str, &str)]) -> Response {
    let mut req = Request::builder().uri(path);
    for (name, value) in headers {
        req = req.header(*name, *value);
    }
    app()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn text(response: Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).to_string()
}

const BROWSER: (&str, &str) = ("accept", "text/html,application/xhtml+xml");

#[tokio::test]
async fn a_client_gets_json_and_a_browser_gets_a_page() {
    let resp = ask("/api/v1/things", &[]).await;
    assert_eq!(resp.headers()[header::VARY], "accept");
    assert_eq!(text(resp).await, r#"{"name":"a"}"#);

    let resp = ask("/api/v1/things", &[BROWSER]).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let headers = resp.headers().clone();
    assert_eq!(headers[header::CONTENT_TYPE], "text/html; charset=utf-8");
    assert!(
        headers[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .contains("frame-ancestors")
    );
    assert_eq!(headers[header::CACHE_CONTROL], "no-store, private");
    assert!(
        headers.get(header::ETAG).is_none(),
        "the JSON's ETag does not fit the page"
    );
    let html = text(resp).await;
    assert!(html.contains("test-service"));
    // The session the auth layer published: who, and the token forms carry
    assert!(html.contains("ops"));

    // ?format=json is the way back to the bytes from a browser
    let resp = ask("/api/v1/things?format=json", &[BROWSER]).await;
    assert_eq!(text(resp).await, r#"{"name":"a"}"#);
}

#[tokio::test]
async fn a_theme_is_picked_by_link_and_remembered_by_cookie() {
    let resp = ask("/api/v1/things?theme=nord", &[BROWSER]).await;
    let cookie = resp.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .to_string();
    assert!(cookie.starts_with("test_theme=nord;"), "{cookie}");

    let resp = ask(
        "/api/v1/things",
        &[BROWSER, ("cookie", "test_theme=dracula")],
    )
    .await;
    assert!(resp.headers().get(header::SET_COOKIE).is_none());
    assert!(text(resp).await.contains("dracula"));

    // A name that is not a theme is ignored
    let resp = ask(
        "/api/v1/things?theme=evil",
        &[BROWSER, ("cookie", "test_theme=evil")],
    )
    .await;
    assert!(resp.headers().get(header::SET_COOKIE).is_none());
}

#[tokio::test]
async fn what_is_not_ours_to_draw_is_left_alone() {
    // The product's own output, even though it is JSON
    let resp = ask("/api/v1/output/ep-a", &[BROWSER]).await;
    assert_eq!(text(resp).await, r#"{"all":{}}"#);
    // A body that is not JSON
    let resp = ask("/api/v1/text", &[BROWSER]).await;
    assert_eq!(text(resp).await, "plain text");
    // JSON that does not parse: the bytes are more useful than a page about them
    let resp = ask("/api/v1/broken", &[BROWSER]).await;
    assert_eq!(text(resp).await, "{not json");
}

#[tokio::test]
async fn a_body_too_big_to_draw_is_paged_or_explained() {
    // A route that pages: the browser is sent to the first page
    let resp = ask("/api/v1/paged", &[BROWSER]).await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(resp.headers()[header::LOCATION], "/api/v1/paged?limit=200");

    // A route that does not: the size, and the way to the bytes
    let resp = ask("/api/v1/whole", &[BROWSER]).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(text(resp).await.contains("too large to render"));

    // A window the caller named is rendered, however large
    let resp = ask("/api/v1/paged?limit=5000", &[BROWSER]).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(!text(resp).await.contains("too large to render"));

    // Past what the renderer will even read: an error, not a truncated page
    let resp = ask("/api/v1/huge", &[BROWSER]).await;
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}
