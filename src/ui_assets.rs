// A built web UI (a single-page app), served at "/" from a directory:
// the UI and the API on one address, so the UI's calls are same-origin and the
// login's session cookie covers both, with no proxy in front.
//
// Three kinds of path reach this fallback (everything the router does not
// match):
//   - a file of the build (/assets/index-3f2a.js, /favicon.svg): served as is;
//   - a page of the app (/sources, /topology/...): no such file, so the app's
//     index.html, whose own router draws the page;
//   - an API path nothing matched (/api/v1/typo): the API's own JSON 404, never
//     the app, or a client would read a 200 HTML page as an answer.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use tower::ServiceExt;
use tower_http::services::{ServeDir, ServeFile};

use auth_rs::http::{ApiKeyRegistry, Permission, SessionStore, cookie_value};

use crate::error::ApiError;

// What the gate checks: the keys in force (none = an open API, and so an open
// app) and the browser sessions.
type Gate<P> = (Arc<ApiKeyRegistry<P>>, Arc<SessionStore<P>>);

// Generic over the service's permissions type, like the key registry and the
// session store it checks; `cookie` is the service's session cookie name.
pub struct UiAssets<P> {
    dir: PathBuf,
    cookie: &'static str,
    // Who may see the app's pages: a browser with no session is sent to the
    // login form by the server, before the app loads, instead of loading the
    // app only for it to find out it is signed out and redirect itself (a
    // flash of the overview, then the login page).
    gate: Option<Gate<P>>,
}

// By hand: `#[derive(Clone)]` would demand `P: Clone` for what is a path and
// two pointer copies whatever P is. A router clones its state per request.
impl<P> Clone for UiAssets<P> {
    fn clone(&self) -> Self {
        Self {
            dir: self.dir.clone(),
            cookie: self.cookie,
            gate: self.gate.clone(),
        }
    }
}

impl<P: Permission> UiAssets<P> {
    pub fn new(dir: impl Into<PathBuf>, cookie: &'static str) -> Self {
        let dir = dir.into();
        // Not fatal: the API works without its UI. Said at startup, where a
        // wrong path in a deployment is noticed, rather than as a 404 later.
        if !dir.join("index.html").is_file() {
            // Into a local first: tracing expands a field expression twice,
            // and one copy only runs without a subscriber.
            let shown = dir.display().to_string();
            tracing::warn!(
                dir = %shown,
                "the web UI directory has no index.html: the UI will not load"
            );
        }
        Self {
            dir,
            cookie,
            gate: None,
        }
    }

    pub fn with_gate(
        mut self,
        keys: Arc<ApiKeyRegistry<P>>,
        sessions: Arc<SessionStore<P>>,
    ) -> Self {
        self.gate = Some((keys, sessions));
        self
    }

    // Where a browser asking for `request` must go first: the login form,
    // when keys are configured (no keys = an open API, and so an open app)
    // and the request carries no live session. Only page loads are gated:
    // the app's own files (scripts, styles, icons) are not secrets, and the
    // login page needs some of them.
    fn sign_in_first(&self, request: &Request) -> Option<Response> {
        let (keys, sessions) = self.gate.as_ref()?;
        if keys.load().is_empty() || !is_page_load(request) {
            return None;
        }
        let signed_in = request
            .headers()
            .get(header::COOKIE)
            .and_then(|v| v.to_str().ok())
            .and_then(|c| cookie_value(c, self.cookie))
            .is_some_and(|token| sessions.get(token).is_some());
        if signed_in {
            return None;
        }
        let next = request
            .uri()
            .path_and_query()
            .map_or("/", |pq| pq.as_str())
            .to_string();
        let login = format!("/api/v1/login?next={}", crate::url::query_escape(&next));
        Some(axum::response::Redirect::to(&login).into_response())
    }

    fn index(&self) -> PathBuf {
        self.dir.join("index.html")
    }
}

// A browser navigating to a page, as opposed to fetching one of the app's
// files: it asks for HTML. The app's scripts and styles ask for something else.
fn is_page_load(request: &Request) -> bool {
    request
        .headers()
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|accept| accept.contains("text/html"))
}

// Paths that belong to the API: a miss there is the API's 404, not the app.
fn is_api_path(path: &str) -> bool {
    ["/api", "/swagger-ui", "/api-docs"]
        .iter()
        .any(|prefix| path == *prefix || path.starts_with(&format!("{prefix}/")))
}

// A service mounts these two through short wrappers naming its permissions
// type: a generic function is not a handler until its type is fixed.
pub async fn serve<P: Permission>(State(assets): State<UiAssets<P>>, request: Request) -> Response {
    let path = request.uri().path().to_string();
    if is_api_path(&path) {
        return ApiError::not_found(format!("no route for {path}")).into_response();
    }
    if request.method() != Method::GET && request.method() != Method::HEAD {
        return ApiError::new(
            axum::http::StatusCode::METHOD_NOT_ALLOWED,
            "the UI's files are read-only",
        )
        .into_response();
    }

    if let Some(redirect) = assets.sign_in_first(&request) {
        return redirect;
    }
    let service = ServeDir::new(&assets.dir).fallback(ServeFile::new(assets.index()));
    let response = match service.oneshot(request).await {
        Ok(response) => response.map(Body::new),
        Err(never) => match never {},
    };
    with_cache_policy(&path, response)
}

// Vite names every built file after its content (index-3f2a.js), so a file
// under /assets never changes and can be kept for good; everything else (the
// index.html that names those files) must be asked again on every load, or a
// browser would keep running the previous release's app.
fn with_cache_policy(path: &str, mut response: Response) -> Response {
    let policy = if path.starts_with("/assets/") && response.status().is_success() {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(policy));
    response
}

// /metrics is two things on one path: Prometheus's endpoint and the UI's
// Metrics page. A browser navigating there asks for HTML and gets the app;
// a scraper, or the page's own fetch, never does and gets the metrics.
pub async fn page_for_browsers<P: Permission>(
    State(assets): State<UiAssets<P>>,
    request: Request,
    next: Next,
) -> Response {
    let wants_html = request
        .headers()
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|accept| accept.contains("text/html"));
    if !wants_html {
        return next.run(request).await;
    }
    if let Some(redirect) = assets.sign_in_first(&request) {
        return redirect;
    }
    let response = match ServeFile::new(assets.index()).oneshot(request).await {
        Ok(response) => response.map(Body::new),
        Err(never) => match never {},
    };
    with_cache_policy("/metrics", response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_paths_are_never_the_app() {
        for path in [
            "/api",
            "/api/v1/nope",
            "/swagger-ui/",
            "/api-docs/openapi.json",
        ] {
            assert!(is_api_path(path), "{path}");
        }
        for path in ["/", "/sources", "/apis", "/assets/index.js", "/metrics"] {
            assert!(!is_api_path(path), "{path}");
        }
    }

    #[test]
    fn only_built_files_are_cached_for_good() {
        let ok = || Response::new(Body::empty());
        let cache = |r: Response| {
            r.headers()[header::CACHE_CONTROL]
                .to_str()
                .unwrap()
                .to_string()
        };
        assert!(cache(with_cache_policy("/assets/index-3f2a.js", ok())).contains("immutable"));
        assert_eq!(cache(with_cache_policy("/sources", ok())), "no-cache");
        assert_eq!(cache(with_cache_policy("/", ok())), "no-cache");
    }
}
