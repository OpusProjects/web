# Usage

How a service uses the `web-rs` crate: the error type its handlers return, the audit events, serving
its web UI, and the route help its browsable pages read from the OpenAPI document.

- [Depending on the crate](#depending-on-the-crate)
- [Errors](#errors)
- [Audit events](#audit-events)
- [Serving the web UI](#serving-the-web-ui)
- [Route help from the OpenAPI document](#route-help-from-the-openapi-document)
- [The configuration API](#the-configuration-api)

---

## Depending on the crate

A service names a release tag of this repository in its `Cargo.toml`, next to the auth crate it already uses.

```toml
web-rs = { git = "https://github.com/OpusProjects/web", tag = "v0.1.0" }
auth-rs = { git = "https://github.com/OpusProjects/auth", tag = "v0.3.0" }
```

Use the `auth-rs` release this crate is built against (its `Cargo.toml` names it): the types the UI
gate and the audit events take are `auth-rs` types, and two releases of a crate are two sets of types.

---

## Errors

A handler returns `Result<T, ApiError>`, and every failure reaches the client as a status and `{"error": "..."}`.

| Constructor | Status |
|---|---|
| `ApiError::new(status, message)` | Any |
| `not_found(message)` | 404 |
| `forbidden(message)`, `admin_only()` | 403 |
| `bad_request(message)` | 400 |
| `internal(message)` | 500 |
| `missing_api_key()`, `invalid_api_key()` | 401 |
| `csrf_required()` | 403 |

A refusal from `auth-rs`'s middleware converts with `ApiError::from(rejection)`, so the wrapper a
service mounts it with is one `map_err`. `ErrorBody` is the body's schema: name it in each
`#[utoipa::path]` (`body = ErrorBody`) and the OpenAPI document shows the shape every error has.

Refusals a service's own routes share (an id that is not configured, a resource a key may not see)
go in an extension trait in the service, so the wording stays identical across its handlers:

```rust
pub trait SourceErrors {
    fn source_not_configured(id: &str) -> Self;
}

impl SourceErrors for ApiError {
    fn source_not_configured(id: &str) -> Self {
        ApiError::not_found(format!("source '{id}' is not configured"))
    }
}
```

---

## Audit events

`audit::record` logs one event per operation that changed something, where who and what meet: the handler.

```rust
web_rs::audit::record(&auth, &headers, "sync", &id, "ok");
```

The event is logged at INFO under the `audit` target with five fields: `actor` (the API key's label,
or `open` when no keys are configured), `action`, `resource`, `outcome`, and `request_id` (the
`x-request-id` header, the key that joins it to the access log). `RUST_LOG=audit=info` keeps these
and nothing else. Record only what ran: a refused request is already in the access log.

---

## Serving the web UI

`UiAssets` serves a built single-page app at `/`, on the API's own address.

```rust
let assets = UiAssets::new(&config.ui_dir, COOKIE_NAME).with_gate(keys, sessions);
let router = router.fallback(serve).with_state(assets);
```

`serve` and `page_for_browsers` are generic over the permissions type, so a service mounts them
through two small wrappers, or names the type (`serve::<Permissions>`). For a request nothing else
in the router matched:

| The request | The answer |
|---|---|
| A path under `/api`, `/swagger-ui` or `/api-docs` | The API's JSON 404, never the app |
| Anything but `GET` or `HEAD` | 405: the UI's files are read-only |
| A page load (`Accept: text/html`) with keys configured and no live session | A redirect to `/api/v1/login?next=<the page>` |
| A file of the build | The file; under `/assets/`, cached for a year (the names change with the content) |
| Any other path | The app's `index.html`, `Cache-Control: no-cache`, whose own router draws the page |

`with_gate` takes the service's API key registry and session store; without it, or with no keys
configured, the app is open as the API is. `page_for_browsers` is a middleware for a path that is
both an API route and an app page, such as `/metrics`: a browser gets the app, a scraper the route.

---

## Route help from the OpenAPI document

`Spec` answers questions about the service's routes from its OpenAPI document, so the help a page shows and the published contract cannot drift.

```rust
static SPEC: OnceLock<Spec> = OnceLock::new();
let spec = SPEC.get_or_init(|| Spec::new(ApiDoc::openapi()));
```

| Method | Answers |
|---|---|
| `lookup(path, method)` | The route a concrete path matched: template, methods, description, parameters, responses |
| `is_gettable(path)` | Whether the path is a GET route, so a breadcrumb can link it |
| `accepts_limit(path)` | Whether the GET route takes `limit`, so a page can offer paging |
| `actions(template)` | What a page can offer to do: the route's own writes, and a POST one segment below |

A template matches segment by segment, the way axum's router does, and the most specific template
wins (`/config/validate` over `/config/{file}`). `/api/v1/config/{file}` takes the rest of the path,
slashes included. An action that needs a path value the page does not have is not offered.

---

## The configuration API

The `config` module makes a service's configuration directory readable and writable over HTTP, admin-only.

| Route | What the function behind it does |
|---|---|
| `GET /api/v1/config` | `get_config`: every file with its hash, the directory ETag, whether it loads and whether it is running |
| `GET /api/v1/config/{file}` | `get_config_file`: the file verbatim, its sha256 as the ETag, `304` on `If-None-Match` |
| `PUT /api/v1/config/{file}` | `put_config_file`: one file, written whole |
| `DELETE /api/v1/config/{file}` | `delete_config_file`: one file removed, never `config.yaml` |
| `PUT /api/v1/config` | `put_config`: the whole directory as `{"files": {...}, "prune": bool}` |
| `POST /api/v1/config/validate` | `validate_config`: the verdict on a directory, nothing written |
| `POST /api/v1/config/reload` | `reload_config`: what is on disk becomes the running configuration |

Every write runs the same steps in the same order:
1. Load the directory as it would be after the change, in a staged copy beside it.
2. If the service is asked to reload, check what the reload needs.
3. Commit, file by file, each through a temporary file and a rename.
4. Reload, when asked.

A change that does not load is a `400` with every error at once. A write against an ETag that moved
since it was read is a `412`. A file whose bytes are unchanged is not rewritten, so its modification
time stays meaningful.

A service gives the module three things:

- **Its loader**, the function that turns a directory into its configuration, wrapped in
  `FsConfigStore::new(dir, load)`. It is the same function startup and `--check-config` use, so
  the API validates exactly what a restart would.
- **A `ConfigHost`**, saying:
  - who is an admin;
  - what the configuration counts (`summarize`);
  - whether a configuration differs from the running one (`pending`);
  - what a reload needs and does: `prepare` runs before the commit and may refuse with a `409`,
    `apply` runs after it;
  - how to count writes and reloads in its own metrics.
- **Its handlers**, each calling one function above with its own `#[utoipa::path]`, so the OpenAPI
  document names its own types. `WriteResult<ConfigSummary, ReloadInfo>` is documented as a schema
  of its own.

`files::config_files` lists a directory the way the loader should read it: every `.yaml` and `.yml`
file at any depth, hidden entries skipped, symlinks followed up to a depth cap. `is_config_path` is
the rule a file name must meet to be written at all.
