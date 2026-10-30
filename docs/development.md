# Development

How the crate is laid out, what CI checks, how the tests work, and how to build a service against a
local copy before anything is released.

- [Layout](#layout)
- [CI gates](#ci-gates)
- [Tests and coverage](#tests-and-coverage)
- [Working on a service against a local copy](#working-on-a-service-against-a-local-copy)

---

## Layout

One crate, `web-rs`, at the root of the repository.

```
Cargo.toml                    # the crate: version, MSRV, dependencies
src/
├── lib.rs                    # module tree and re-exports
├── error.rs                  # ApiError, ErrorBody
├── audit.rs                  # audit::record
├── ui_assets.rs              # UiAssets, serve, page_for_browsers
├── spec.rs                   # Spec: route help from the OpenAPI document
├── config/                   # files (which files), store (staged, atomic), api (the routes' logic)
├── site.rs                   # Site: the service's name, document and cookies
├── pages.rs                  # the browsable API (negotiate, page)
├── login.rs                  # the sign-in form and its routes, SignIn
└── url.rs                    # query_escape
tests/                        # integration tests: a real router, a JSON document, a log subscriber
docs/                         # this documentation
release-please-config.json    # release-please: version file, changelog sections
.release-please-manifest.json # the last released version
```

`auth-rs` comes from the [auth](https://github.com/OpusProjects/auth) repository, pinned by tag;
`deny.toml` allows exactly that git source.

---

## CI gates

`.github/workflows/build.yaml` runs these on every pull request; `test`, `audit` and `deny` are required.

| Job | What it checks |
|---|---|
| `test` | `cargo fmt --check`, clippy with warnings as errors, a check under the MSRV, tests with 100% line coverage |
| `audit` | `cargo audit`: no RUSTSEC advisory in `Cargo.lock` (also nightly, on `main`) |
| `deny` | `cargo deny check`: licenses, sources and advisories, per `deny.toml` (also nightly) |

The MSRV is `rust-version` in `Cargo.toml`, the same as the services': a language feature newer
than their pinned toolchain must fail here, not in their builds.

---

## Tests and coverage

Every line of the crate runs in a test, and the `test` check fails a pull request that changes that.

| Where | What goes there |
|---|---|
| `#[cfg(test)] mod tests` at the bottom of a file in `src/` | Unit tests of that file: error bodies, escaping, cache headers |
| `tests/ui_assets_test.rs` | A real router over a temporary UI build: pages, files, API misses, the sign-in gate |
| `tests/spec_test.rs` | `Spec` asked about a small OpenAPI document written as JSON |
| `tests/audit_test.rs` | An audit event read back from a log subscriber |
| `tests/config_store_test.rs` | `FsConfigStore` on real directories, with a stand-in loader, including what the filesystem refuses |
| `tests/pages_test.rs` | `negotiate` in a real router: pages, themes, what is left alone, bodies too big to draw |
| `tests/login_test.rs` | The sign-in routes with a test directory: keys, accounts, every refusal, sign-out |
| `tests/config_api_test.rs` | Every config route against a test `ConfigHost` and a store that fails on purpose |

Test code under `tests/` is not counted by `cargo llvm-cov`, which is why helpers live there.

```bash
cargo llvm-cov --summary-only          # the number CI checks (lines: 100%)
cargo llvm-cov --html --open           # which lines a change left uncovered
```

A value logged as a field is computed into a local first, because `tracing` expands a field
expression twice and one copy only runs without a subscriber.

---

## Working on a service against a local copy

A change here is a change in every service that pins it, so build one against it before it is merged.

With this repository checked out next to the service, add a `[patch]` section to the service's
`Cargo.toml` **without committing it**:

```toml
[patch."https://github.com/OpusProjects/web"]
web-rs = { path = "../web" }
```

Then run the service's own gates (`cargo clippy --all-targets -- -D warnings` and `cargo test`):
its tests are what prove a change here did not alter what the service does. Remove the section
before committing anything in the service.
