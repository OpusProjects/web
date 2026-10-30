# Web

[![web CI](https://github.com/OpusProjects/web/actions/workflows/build.yaml/badge.svg)](https://github.com/OpusProjects/web/actions/workflows/build.yaml)
[![License: Apache 2.0](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-edition%202024-orange.svg?logo=rust)](https://www.rust-lang.org/)

The web layer of a JSON API service, written in Rust.

What a service's routes do is its own; how it answers on the web does not have to be. Web holds
that part: one error shape for every failure, an audit event for every change, the web UI served
from the API's own address behind sign-in, help for each route read from the OpenAPI document, the
configuration directory readable and writable over HTTP, and the same routes as browsable pages.

One crate, `web-rs`, built on axum and utoipa. Who a caller is comes from the `auth-rs` crate of
the [auth](https://github.com/OpusProjects/auth) repository, so the parts that check it are generic
over the service's own permissions type.

---

## ✨ Features

- **One error shape**: every failure is a status and `{"error": "..."}`, never an empty body
- **Refusals that name their fix**: a missing key, a wrong key and a missing CSRF token differ
- **Audit events**: who did what to which resource and how it ended, under the `audit` target
- **The web UI on the API's address**: same origin, so one session cookie covers both
- **Signed out goes to sign-in**: a page load without a session is redirected before the app loads
- **An API miss is never the app**: `/api`, `/swagger-ui` and `/api-docs` answer JSON 404s
- **Cached the right way**: content-named files kept for good, `index.html` asked every time
- **Route help from the spec**: description, parameters, responses and actions of any route
- **Config over HTTP**: read, write, validate and reload the configuration directory, admin-only
- **Validated before it lands**: a change is loaded in a staged copy, refused whole if it fails
- **Atomic, careful writes**: temp file and rename per file, unchanged files never rewritten
- **ETags and If-Match**: a push against a directory someone else changed is refused, not merged
- **Browsable API**: a browser gets a page for any JSON route, with help, links and action buttons
- **Safe pages**: every value escaped, a strict CSP, never cached, never framed
- **Sign-in form**: an API key or a directory account becomes a session; no password stored here
- **Pinned by tag**: a service depends on a `vX.Y.Z` tag of this repository, never on `main`

---

## 📚 Documentation

| Document | What it covers |
|---|---|
| [Usage](docs/usage.md) | Errors, audit events, the web UI, route help, the configuration API, pages and sign-in |
| [Development](docs/development.md) | The layout, the CI gates, the tests, and working on a service against a local copy |
| [Releasing](docs/releasing.md) | How release-please cuts a version, and how a service moves to it |

---

## 🤝 Contributing

Contributions are welcome: [CONTRIBUTING.md](CONTRIBUTING.md) covers the PR workflow, commit style, CI gates and design rules.

Security issues: see [SECURITY.md](SECURITY.md) for private reporting.

---

## 👥 Authors

- [Fernando Roca](https://github.com/fernandorocagonzalez)
- [Blai Peidro](https://github.com/blaipr)

---

## ⚖️ License

[Apache 2.0](LICENSE)
