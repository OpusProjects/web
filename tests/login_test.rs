// The sign-in form and its routes, with a test service: one API key, and a
// directory whose answer each test picks.
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use auth_rs::http::{ApiKeyRegistry, Permission, ResolvedApiKey, SessionKind, SessionStore};
use auth_rs::login::LoginError;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use serde_json::json;
use web_rs::login::{LoginForm, SessionTtl, SignIn, login_form, login_submit, logout};
use web_rs::{Site, Spec};

#[derive(Clone, Debug, PartialEq)]
enum Perm {
    Admin,
    Reader,
}

impl Permission for Perm {
    fn admin() -> Self {
        Perm::Admin
    }
}

fn site() -> &'static Site {
    static SITE: OnceLock<Site> = OnceLock::new();
    static SPEC: OnceLock<Spec> = OnceLock::new();
    SITE.get_or_init(|| Site {
        product: "test-service",
        spec: SPEC.get_or_init(|| {
            let doc = json!({ "openapi": "3.1.0", "info": { "title": "t", "version": "0" }, "paths": {} });
            Spec::new(serde_json::from_value(doc).unwrap())
        }),
        session_cookie: "test_session",
        login_cookie: "test_login",
        theme_cookie: "test_theme",
        raw_routes: &[],
    })
}

// What the directory answers next, or None when the service has no directory
type Answer = Result<(String, Perm), LoginError>;

struct Directory(Option<Mutex<Option<Answer>>>);

impl SignIn<Perm> for Directory {
    fn ldap_enabled(&self) -> bool {
        self.0.is_some()
    }
    fn home(&self) -> String {
        "/home".to_string()
    }
    async fn ldap_login(
        &self,
        username: &str,
        _password: &str,
    ) -> Result<(String, Perm), LoginError> {
        assert_eq!(username, "jdoe");
        self.0.as_ref().unwrap().lock().unwrap().take().unwrap()
    }
}

fn answering(answer: Answer) -> Directory {
    Directory(Some(Mutex::new(Some(answer))))
}

fn keys(present: bool) -> ApiKeyRegistry<Perm> {
    ApiKeyRegistry::new(if present {
        vec![ResolvedApiKey {
            id: "ops".to_string(),
            name: "Operations".to_string(),
            secret: "s3cret".to_string(),
            permissions: Perm::Admin,
        }]
    } else {
        Vec::new()
    })
}

// The challenge pair a rendered form carries: the cookie half and the field half
fn challenged(cookie: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::COOKIE,
        HeaderValue::from_str(&format!("test_theme=nord; test_login={cookie}")).unwrap(),
    );
    headers
}

fn form(key: Option<&str>, username: Option<&str>, next: Option<&str>, csrf: &str) -> LoginForm {
    LoginForm {
        key: key.map(str::to_string),
        username: username.map(str::to_string),
        password: username.map(|_| "pw".to_string()),
        next: next.map(str::to_string),
        _csrf: csrf.to_string(),
    }
}

async fn body(response: Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

async fn submit(
    directory: &Directory,
    keys: &ApiKeyRegistry<Perm>,
    sessions: &SessionStore<Perm>,
    form: LoginForm,
) -> Response {
    login_submit(
        site(),
        directory,
        keys,
        sessions,
        Duration::from_secs(60),
        &challenged("chal"),
        form,
    )
    .await
}

#[tokio::test]
async fn the_form_plants_its_challenge_and_offers_what_is_configured() {
    let response = login_form(
        site(),
        &Directory(None),
        Some("https://evil.example"),
        &HeaderMap::new(),
    );
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .to_string();
    assert!(cookie.starts_with("test_login="), "{cookie}");
    let html = body(response).await;
    assert!(html.contains("test-service"));
    assert!(html.contains("Sign in with an API key"));
    // A next on another origin is replaced by the service's home
    assert!(html.contains(r#"value="/home""#), "{html}");

    let response = login_form(
        site(),
        &answering(Err(LoginError::Busy)),
        Some("/sources"),
        &challenged("x"),
    );
    let html = body(response).await;
    assert!(html.contains("Username"), "a directory sign-in is offered");
    assert!(html.contains(r#"value="/sources""#));
}

#[tokio::test]
async fn a_form_this_server_did_not_serve_is_refused() {
    let sessions = SessionStore::new();
    for csrf in ["", "other"] {
        let response = submit(
            &Directory(None),
            &keys(true),
            &sessions,
            form(Some("s3cret"), None, None, csrf),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(body(response).await.contains("expired"));
    }
    // No challenge cookie at all
    let response = login_submit(
        site(),
        &Directory(None),
        &keys(true),
        &sessions,
        Duration::from_secs(60),
        &HeaderMap::new(),
        form(Some("s3cret"), None, None, "chal"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(sessions.is_empty());
}

#[tokio::test]
async fn an_api_key_opens_a_session_as_that_key() {
    let sessions = SessionStore::new();
    let response = submit(
        &Directory(None),
        &keys(true),
        &sessions,
        form(Some(" s3cret "), None, Some("/sources"), "chal"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers()[header::LOCATION], "/sources");
    let cookie = response.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        cookie.starts_with("test_session=") && cookie.contains("Max-Age=60"),
        "{cookie}"
    );
    let token = cookie
        .trim_start_matches("test_session=")
        .split(';')
        .next()
        .unwrap();
    let session = sessions.get(token).expect("a session");
    assert_eq!(session.key_name, "Operations");
    assert_eq!(session.kind, SessionKind::ApiKey);

    // A wrong key, and an instance with no keys at all, read the same
    for (registry, key) in [(keys(true), "wrong"), (keys(false), "s3cret")] {
        let response = submit(
            &Directory(None),
            &registry,
            &sessions,
            form(Some(key), None, None, "chal"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(body(response).await.contains("invalid API key"));
    }
    // No key field at all is an empty key
    let response = submit(
        &Directory(None),
        &keys(true),
        &sessions,
        form(None, None, None, "chal"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_directory_account_opens_a_session_with_what_the_rules_grant() {
    let sessions = SessionStore::new();
    let directory = answering(Ok(("jdoe".to_string(), Perm::Reader)));
    let response = submit(
        &directory,
        &keys(true),
        &sessions,
        form(None, Some("jdoe"), None, "chal"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers()[header::LOCATION], "/home");
    let cookie = response.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .to_string();
    let token = cookie
        .trim_start_matches("test_session=")
        .split(';')
        .next()
        .unwrap();
    let session = sessions.get(token).unwrap();
    assert_eq!(session.key_name, "ldap:jdoe");
    assert_eq!(session.kind, SessionKind::Directory);
    assert_eq!(session.permissions, Perm::Reader);
}

#[tokio::test]
async fn every_directory_refusal_has_its_status_and_its_words() {
    let cases = [
        (
            LoginError::InvalidCredentials,
            StatusCode::UNAUTHORIZED,
            "invalid username or password",
        ),
        (
            LoginError::TooManyAttempts,
            StatusCode::TOO_MANY_REQUESTS,
            "too many failed attempts",
        ),
        (
            LoginError::Busy,
            StatusCode::SERVICE_UNAVAILABLE,
            "too many sign-ins at once",
        ),
        (
            LoginError::NotAuthorized,
            StatusCode::FORBIDDEN,
            "no access to this instance",
        ),
        (
            LoginError::Unavailable("down".to_string()),
            StatusCode::SERVICE_UNAVAILABLE,
            "could not be reached",
        ),
    ];
    for (error, status, words) in cases {
        let sessions = SessionStore::new();
        let response = submit(
            &answering(Err(error)),
            &keys(true),
            &sessions,
            form(None, Some("jdoe"), None, "chal"),
        )
        .await;
        assert_eq!(response.status(), status);
        let html = body(response).await;
        assert!(html.contains(words), "{words}: {html}");
        assert!(sessions.is_empty());
    }

    // A username where no directory is configured
    let response = submit(
        &Directory(None),
        &keys(true),
        &SessionStore::new(),
        form(None, Some("jdoe"), None, "chal"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[test]
fn signing_out_ends_the_session_and_returns_to_the_form() {
    let sessions = SessionStore::new();
    let issued = sessions.create(
        "k".into(),
        SessionKind::ApiKey,
        Some("k".into()),
        Perm::Admin,
        Duration::from_secs(60),
    );
    let mut headers = HeaderMap::new();
    headers.insert(
        header::COOKIE,
        HeaderValue::from_str(&format!("test_session={}", issued.token)).unwrap(),
    );

    let response = logout(site(), &sessions, &headers, Some("/".to_string()));
    assert!(sessions.get(&issued.token).is_none(), "ended server-side");
    assert_eq!(response.headers()[header::LOCATION], "/api/v1/login?next=/");
    assert!(
        response.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );

    // No session, and a next on another origin: just the form
    let response = logout(
        site(),
        &sessions,
        &HeaderMap::new(),
        Some("//evil".to_string()),
    );
    assert_eq!(response.headers()[header::LOCATION], "/api/v1/login");
    assert_eq!(SessionTtl::default().0, Duration::from_secs(3600));
}

// A site whose cookie names cannot be header values: no cookie is set, and
// nothing else fails. (A service names its cookies in code; this is the
// guard against one that got it wrong.)
fn broken_site() -> &'static Site {
    static SITE: OnceLock<Site> = OnceLock::new();
    SITE.get_or_init(|| Site {
        session_cookie: "bad\ncookie",
        login_cookie: "bad\ncookie",
        ..*site()
    })
}

#[tokio::test]
async fn a_cookie_that_cannot_be_set_is_left_out() {
    let response = login_form(broken_site(), &Directory(None), None, &HeaderMap::new());
    assert!(response.headers().get(header::SET_COOKIE).is_none());

    let sessions = SessionStore::new();
    let mut headers = HeaderMap::new();
    headers.insert(header::COOKIE, HeaderValue::from_static("x=1"));
    let response = login_submit(
        broken_site(),
        &Directory(None),
        &keys(true),
        &sessions,
        Duration::from_secs(60),
        &headers,
        form(Some("s3cret"), None, None, "chal"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let response = logout(broken_site(), &sessions, &HeaderMap::new(), None);
    assert!(response.headers().get(header::SET_COOKIE).is_none());
}

// A wrong key on a form that offers both ways in reopens on the key form.
#[tokio::test]
async fn a_refused_key_reopens_on_the_key_form() {
    let directory = answering(Err(LoginError::Busy));
    let response = submit(
        &directory,
        &keys(true),
        &SessionStore::new(),
        form(Some("wrong"), None, None, "chal"),
    )
    .await;
    let html = body(response).await;
    assert!(html.contains(r#"id="mode-key" checked"#), "{html}");
}
