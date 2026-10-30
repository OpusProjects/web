use std::future::Future;
use std::time::Duration;

use auth_rs::http::{
    ApiKeyRegistry, Permission, SessionKind, SessionStore, cookie_value, csrf_matches, match_token,
    new_token,
};
use auth_rs::login::LoginError;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::pages::{CSP, Html, THEMES, palette_css};
use crate::site::Site;
use crate::url::{is_local_path, query_escape};

// The login form for the browsable API.
//
// Public routes, outside the key middleware — a page that asks for a credential
// cannot itself require one. They exist only when the UI is enabled.
//
// Two ways in, and neither is a credential store of its own. An API KEY is
// checked against the same registry and the same constant-time compare a
// header goes through. A directory account (when `server.ui.ldap` is set) is
// checked by the directory, and what it may touch comes from the configured
// access rules. There is no user table and no password stored here.

// A short-lived token planted when the form is served and required back when
// it is submitted, so only a form THIS server rendered can log anybody in.
//
// Without it, a page on the internet can auto-submit a login form carrying the
// ATTACKER's API key: the browser navigates top-level to our origin, so the
// response is first-party and the cookie is stored (SameSite=Strict governs
// sending an existing cookie, not setting a new one). The victim then works —
// and is audit-logged — under someone else's key without noticing.
//
// The cookie is the service's `Site::login_cookie`.

// What the sign-in routes need from the service: whether a directory account
// is a way in, where to land after signing in, and checking one. Checking is
// the service's because what an account may touch comes from its own access
// rules: it answers the account's canonical name and its permissions.
pub trait SignIn<P> {
    fn ldap_enabled(&self) -> bool;
    // Where to go after signing in when no page sent the browser here
    fn home(&self) -> String;
    fn ldap_login(
        &self,
        username: &str,
        password: &str,
    ) -> impl Future<Output = Result<(String, P), LoginError>> + Send;
}

// One form, two shapes: `key` for an API key, `username` + `password` for
// LDAP. Whichever is filled in decides the path.
#[derive(Deserialize)]
pub struct LoginForm {
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub next: Option<String>,
    #[serde(default)]
    pub _csrf: String,
}

#[derive(Deserialize)]
pub struct LoginQuery {
    #[serde(default)]
    pub next: Option<String>,
}

// Where to go after logging in: the page that sent the browser here, or
// `server.ui.home` when none did. Only a path on THIS origin:
// `next=https://evil.example` (or `//evil.example`, which a browser reads as
// another host) would turn the login into an open redirect that lends our
// address to a phishing link.
fn safe_next<'a>(next: Option<&'a str>, home: &'a str) -> &'a str {
    match next {
        Some(path) if is_local_path(path) => path,
        _ => home,
    }
}

fn esc(s: &str) -> String {
    Html::text(s).as_str().to_string()
}

// Rendered by hand rather than through the JSON page: there is no resource here
// and nothing to negotiate. It borrows the same palette so the login does not
// look like a different product.
struct Page<'a> {
    site: &'a Site,
    theme: &'a str,
    message: Option<&'a str>,
    status: StatusCode,
    challenge: Option<&'a str>,
    ldap: bool,
    // Which way in the card opens on: the one the person just tried, so a
    // mistyped key does not drop them back on the directory form.
    key_first: bool,
    next: &'a str,
}

fn login_page(page: Page<'_>) -> Response {
    let notice = match page.message {
        Some(text) => format!("<p class=\"bad\">{}</p>", esc(text)),
        None => String::new(),
    };
    let challenge = esc(page.challenge.unwrap_or(""));
    let next = esc(page.next);

    let hidden = format!(
        r#"<input type="hidden" name="_csrf" value="{challenge}">
    <input type="hidden" name="next" value="{next}">"#
    );
    let key_form = |autofocus: bool| {
        format!(
            r#"<form method="post" action="/api/v1/login" class="key">
    {hidden}
    <label for="key">API key</label>
    <input id="key" name="key" type="password" autocomplete="off"{autofocus}>
    <button type="submit">Sign in</button>
  </form>"#,
            autofocus = if autofocus { " autofocus" } else { "" },
        )
    };

    // With LDAP, one card and a switch between the two ways in. The switch is
    // a pair of radio buttons and `:has()` shows the form that matches: no
    // script, so the page keeps its strict CSP. The radios sit outside both
    // forms, so neither submits them.
    let content = if page.ldap {
        let (ldap_checked, key_checked) = if page.key_first {
            ("", " checked")
        } else {
            (" checked", "")
        };
        format!(
            r#"<div class="switch" role="radiogroup" aria-label="Sign in with">
    <input type="radio" name="mode" id="mode-ldap"{ldap_checked}>
    <label for="mode-ldap"><svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="8" r="5"/><path d="M20 21a8 8 0 0 0-16 0"/></svg>LDAP</label>
    <input type="radio" name="mode" id="mode-key"{key_checked}>
    <label for="mode-key"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="m15.5 7.5 2.3 2.3a1 1 0 0 0 1.4 0l2.1-2.1a1 1 0 0 0 0-1.4L19 4"/><path d="m21 2-9.6 9.6"/><circle cx="7.5" cy="15.5" r="5.5"/></svg>API key</label>
  </div>
  <form method="post" action="/api/v1/login" class="ldap">
    {hidden}
    <label for="username">Username</label>
    <input id="username" name="username" autocomplete="username"{ldap_focus}>
    <label for="password">Password</label>
    <input id="password" name="password" type="password" autocomplete="current-password">
    <button type="submit">Sign in</button>
  </form>
  {key}"#,
            ldap_focus = if page.key_first { "" } else { " autofocus" },
            key = key_form(page.key_first),
        )
    } else {
        key_form(true)
    };
    let lead = if page.ldap {
        "Sign in"
    } else {
        "Sign in with an API key"
    };
    let product = esc(page.site.product);
    let forms = format!(
        r#"<main class="card">
  <h1>{product}</h1>
  <p class="lead">{lead}</p>
  {notice}
  {content}
</main>"#
    );

    let body = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Sign in — {product}</title>
<style>
{palette}
* {{ box-sizing: border-box; }}
/* The system's own UI face: the page's CSP allows no font downloads, and the
   operating system's sans reads better here than the monospace the data
   pages use for JSON. */
body {{
  margin: 0; min-height: 100vh; display: grid; place-content: center;
  background: var(--backdrop); color: var(--fg);
  font: 15px/1.5 system-ui, -apple-system, "Segoe UI", Roboto, Cantarell, "Helvetica Neue", Arial, sans-serif;
  -webkit-font-smoothing: antialiased;
}}
.card {{
  background: var(--panel); border: 1px solid var(--rule); border-radius: 12px;
  padding: 28px 34px 32px; width: min(420px, 92vw);
  box-shadow: 0 1px 2px rgb(0 0 0 / 0.05), 0 12px 32px rgb(0 0 0 / 0.08);
}}
/* A tight line height: the inherited 1.5 adds space above the glyphs that
   the eye reads as extra padding at the top of the card. */
h1 {{ margin: 0 0 4px; font-size: 26px; font-weight: 600; line-height: 1.15; letter-spacing: -0.015em; }}
p.lead {{ margin: 0 0 20px; color: var(--dim); font-size: 14px; }}
label {{ display: block; font-size: 13px; font-weight: 500; color: var(--dim); margin: 14px 0 6px; }}
/* The switch between the two ways in: a pill that slides under the chosen
   option. The radios behind it are invisible but still focusable, so the
   keyboard (arrow keys) and screen readers get a real radio group. */
.switch {{
  position: relative; display: grid; grid-template-columns: 1fr 1fr;
  padding: 4px; margin: 4px 0 8px; border-radius: 10px;
  background: var(--bg); border: 1px solid var(--rule);
}}
.switch::before {{
  content: ""; position: absolute; top: 4px; bottom: 4px; left: 4px;
  width: calc(50% - 4px); border-radius: 7px; background: var(--panel);
  box-shadow: 0 0 0 1px var(--rule), 0 1px 3px rgb(0 0 0 / 0.10);
  transition: transform 260ms cubic-bezier(0.3, 0.7, 0.2, 1);
}}
.card:has(#mode-key:checked) .switch::before {{ transform: translateX(100%); }}
.switch input {{ position: absolute; opacity: 0; width: 1px; height: 1px; }}
.switch label {{
  position: relative; display: flex; align-items: center; justify-content: center;
  gap: 7px; margin: 0; padding: 8px 10px; font-size: 13.5px; font-weight: 500;
  color: var(--dim); cursor: pointer; transition: color 200ms;
}}
.switch svg {{
  width: 15px; height: 15px; fill: none; stroke: currentColor; stroke-width: 2;
  stroke-linecap: round; stroke-linejoin: round;
}}
.switch input:checked + label {{ color: var(--fg); }}
.switch input:focus-visible + label {{ outline: 2px solid var(--link); outline-offset: -2px; border-radius: 7px; }}
/* The form that appears settles in rather than snapping. */
.card form {{ animation: settle 220ms ease-out; }}
@keyframes settle {{ from {{ opacity: 0; transform: translateY(4px); }} }}
@media (prefers-reduced-motion: reduce) {{
  .switch::before, .switch label, .card form {{ transition: none; animation: none; }}
}}
.card:has(#mode-ldap) form.key {{ display: none; }}
.card:has(#mode-key:checked) form.key {{ display: block; }}
.card:has(#mode-key:checked) form.ldap {{ display: none; }}
input {{
  width: 100%; padding: 10px 12px; border-radius: 8px;
  border: 1px solid var(--rule); background: var(--bg); color: var(--fg);
  font: inherit;
}}
input:focus {{ outline: 2px solid var(--link); outline-offset: 1px; border-color: transparent; }}
button {{
  margin-top: 20px; width: 100%; padding: 11px 12px; border: 0; border-radius: 8px;
  background: var(--link); color: var(--bg); font: inherit;
  font-weight: 600; cursor: pointer;
}}
/* Red pulled toward the text colour: darker on light themes, lighter on dark. */
.bad {{ color: color-mix(in srgb, #e5534b 80%, var(--fg)); font-size: 14px; margin: 0 0 6px; }}
</style>
</head>
<body>
{forms}
</body>
</html>"#,
        palette = palette_css(page.theme),
        product = esc(page.site.product),
    );

    (
        page.status,
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            ),
            // The same policy the rendered pages get. This page is outside the
            // negotiate layer that sets it, and it is the one page that takes a
            // credential — the last place to leave unframeable.
            (
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static(CSP),
            ),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("no-store, private"),
            ),
        ],
        body,
    )
        .into_response()
}

fn theme_of(site: &Site, headers: &HeaderMap) -> String {
    headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|c| cookie_value(c, site.theme_cookie))
        .filter(|t| THEMES.contains(t))
        .unwrap_or("auto")
        .to_string()
}

// GET /api/v1/login: the form, with a fresh challenge.
pub fn login_form<P, S: SignIn<P>>(
    site: &Site,
    sign_in: &S,
    next: Option<&str>,
    headers: &HeaderMap,
) -> Response {
    let challenge = new_token();
    let home = sign_in.home();
    let response = login_page(Page {
        site,
        theme: &theme_of(site, headers),
        message: None,
        status: StatusCode::OK,
        challenge: Some(&challenge),
        ldap: sign_in.ldap_enabled(),
        key_first: false,
        next: safe_next(next, &home),
    });
    with_challenge(site, response, &challenge)
}

// Plants the cookie half of the login challenge; the form carries the other.
fn with_challenge(site: &Site, mut response: Response, challenge: &str) -> Response {
    if let Ok(value) = HeaderValue::from_str(&format!(
        "{}={challenge}; Path=/api/v1/login; HttpOnly; SameSite=Strict; Max-Age=600",
        site.login_cookie
    )) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    response
}

// POST /api/v1/login: an API key, or a directory account when the service has
// one configured, becomes a browser session.
#[allow(clippy::too_many_arguments)]
pub async fn login_submit<P: Permission, S: SignIn<P>>(
    site: &Site,
    sign_in: &S,
    keys: &ApiKeyRegistry<P>,
    sessions: &SessionStore<P>,
    ttl: Duration,
    headers: &HeaderMap,
    form: LoginForm,
) -> Response {
    let theme = theme_of(site, headers);
    let ldap = sign_in.ldap_enabled();
    let home = sign_in.home();
    let next = safe_next(form.next.as_deref(), &home).to_string();
    // A refused login re-renders the form with a FRESH challenge. Without
    // one, the page that says "wrong password" carried no token, and the
    // corrected attempt was rejected as an expired form.
    let key_first = form.username.is_none();
    let refuse = |message: &str, status: StatusCode| {
        let challenge = new_token();
        with_challenge(
            site,
            login_page(Page {
                site,
                theme: &theme,
                message: Some(message),
                status,
                challenge: Some(&challenge),
                ldap,
                key_first,
                next: &next,
            }),
            &challenge,
        )
    };
    // The form must be one this server handed out. Both halves of the pair
    // have to agree: the cookie planted when the form was rendered, and the
    // hidden field inside it. A cross-site form has neither.
    let challenge = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|c| cookie_value(c, site.login_cookie));

    let matched = challenge
        .is_some_and(|expected| !form._csrf.is_empty() && csrf_matches(expected, &form._csrf));

    if !matched {
        return refuse(
            "this login form has expired — reload the page and try again",
            StatusCode::FORBIDDEN,
        );
    }

    let (name, kind, key_id, permissions) = if let Some(username) = form.username.as_deref() {
        if !ldap {
            return refuse("LDAP login is not configured", StatusCode::BAD_REQUEST);
        }
        let password = form.password.as_deref().unwrap_or("");
        match sign_in.ldap_login(username, password).await {
            Ok((username, permissions)) => (
                format!("ldap:{username}"),
                SessionKind::Directory,
                None,
                permissions,
            ),
            Err(LoginError::InvalidCredentials) => {
                // Already slowed to a fixed total time by the throttle, so an
                // unknown user and a wrong password look alike.
                return refuse("invalid username or password", StatusCode::UNAUTHORIZED);
            }
            Err(LoginError::TooManyAttempts) => {
                return refuse(
                    "too many failed attempts for this account — try again in 15 minutes, \
                     or sign in with an API key",
                    StatusCode::TOO_MANY_REQUESTS,
                );
            }
            Err(LoginError::Busy) => {
                return refuse(
                    "too many sign-ins at once — try again in a moment",
                    StatusCode::SERVICE_UNAVAILABLE,
                );
            }
            Err(LoginError::NotAuthorized) => {
                // The password was right, so saying so leaks nothing a
                // guesser could use, and saves the person a ticket.
                return refuse(
                    "your account is valid but has no access to this instance — ask an \
                     administrator to add you or your group to server.ui.ldap.access",
                    StatusCode::FORBIDDEN,
                );
            }
            Err(LoginError::Unavailable(reason)) => {
                tracing::error!(%reason, "LDAP login failed: directory unavailable");
                return refuse(
                    "the directory could not be reached — try again shortly, or sign in \
                     with an API key",
                    StatusCode::SERVICE_UNAVAILABLE,
                );
            }
        }
    } else {
        let registry = keys.load();

        // An open API has nothing to log into: every caller is already admin, and a
        // form that accepts anything would suggest otherwise.
        if registry.is_empty() {
            // Deliberately the same answer a wrong key gets. Saying "this instance
            // has no authentication" to an unauthenticated caller is a banner for
            // anyone scanning; the real reason goes to the log, where an operator
            // can see it and a scanner cannot.
            tracing::warn!("login attempted on an instance with no API keys configured");
            return refuse("invalid API key", StatusCode::UNAUTHORIZED);
        }

        let Some(key) = match_token(form.key.as_deref().unwrap_or("").trim(), &registry) else {
            // No detail, deliberately: "no such key" and "wrong key" are the same
            // sentence to someone guessing.
            return refuse("invalid API key", StatusCode::UNAUTHORIZED);
        };
        (
            key.name.clone(),
            SessionKind::ApiKey,
            Some(key.id.clone()),
            key.permissions.clone(),
        )
    };

    let issued = sessions.create(name.clone(), kind, key_id, permissions, ttl);

    tracing::info!(key_name = %name, "browser session opened");

    // Secure is NOT set here: the console is reachable over plain HTTP in a
    // port-forward or a local run, and a Secure cookie would silently never be
    // stored there — a login that appears to work and does nothing. In a real
    // deployment this sits behind TLS at the ingress, and the attributes that
    // do the security work are HttpOnly (no script can read it; there are no
    // scripts anyway) and SameSite=Strict (the browser will not attach it to a
    // request another site caused), backed by the CSRF token on every write.
    let cookie = format!(
        "{}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}",
        site.session_cookie,
        issued.token,
        ttl.as_secs()
    );

    let mut response = axum::response::Redirect::to(&next).into_response();
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
}

// A sign-out form may say where the next sign-in should land (the UI posts
// next=/), so signing straight back in returns to the UI rather than to the
// service's home. Optional: a bare POST, with no form at all, still signs out.
pub fn logout<P: Permission>(
    site: &Site,
    sessions: &SessionStore<P>,
    headers: &HeaderMap,
    next: Option<String>,
) -> Response {
    if let Some(token) = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|c| cookie_value(c, site.session_cookie))
    {
        // Dropped server-side, not merely forgotten by the browser: a cookie
        // someone already copied must stop working too.
        sessions.remove(token);
    }

    let login = match next {
        Some(next) if is_local_path(&next) => {
            format!("/api/v1/login?next={}", query_escape(&next))
        }
        _ => "/api/v1/login".to_string(),
    };
    let mut response = axum::response::Redirect::to(&login).into_response();
    if let Ok(value) = HeaderValue::from_str(&format!(
        "{}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0",
        site.session_cookie
    )) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
}

// The session lifetime, injected so it can be read from configuration once and
// not threaded through every handler.
#[derive(Clone, Copy)]
pub struct SessionTtl(pub Duration);

impl Default for SessionTtl {
    fn default() -> Self {
        SessionTtl(Duration::from_secs(3600))
    }
}
