# Contributing to Web

Thanks for helping out! This is a small project with a few firm conventions —
following them keeps the history clean and the services that use it safe.

## Workflow: one logical change per PR

`main` moves only through squash-merged pull requests, one **logical change** per PR —
a fix, a dependency bump, a doc edit each get their own. This keeps every change
independently reviewable, bisectable, and revertable.

```bash
git checkout main && git pull
git checkout -b <type>/<short-name>       # e.g. fix/ui-cache-headers
# ...make exactly one change, test it...
git commit -am "Short imperative title"
git push -u origin <type>/<short-name>
gh pr create --base main
```

### Commit / PR message style

Commit and PR titles follow [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/):
release-please reads them to pick the next version and to write the CHANGELOG.

- Title: `<type>: short imperative summary`, lowercase after the colon —
  `feat: answer a 405 with the methods a route takes`, `fix: send a
  signed-out page load to sign-in`. Types in use: `feat`, `fix`,
  `perf`, `refactor`, `docs`, `test`, `build`, `chore`.
- A breaking change adds `!` after the type (`feat!: ...`) or a
  `BREAKING CHANGE:` footer in the body.
- The squashed title is the CHANGELOG line and decides the next version, so
  write it for someone reading the release notes: what changes for them. See
  [docs/releasing.md](docs/releasing.md).
- Body (when the change needs explaining): plain prose, *what* and *why*, wrapped
  at ~72 columns. The problem first, then the change.

## CI gates

Every PR must pass, in this order (run them locally before pushing):

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## Design rules

This crate is a library services build on; three rules keep it that way:

1. **Nothing service-specific.** A route, a resource or a configuration key never appears here.
   What a caller may do is the service's permissions type; the wording of its own refusals is an
   extension trait in the service.
2. **The web layer, not the API.** What a service's routes do stays in the service. What belongs
   here is how any route answers on the web: its errors, its events, its UI, its help.
3. **A change here is a change in every service that pins it.** Before merging, build them
   against your branch (see [docs/development.md](docs/development.md#working-on-a-service-against-a-local-copy)).

## Code comments

The codebase carries **teaching comments** that explain Rust concepts to the
maintainers. They are intentional — do not strip them when refactoring; move them
with the code they explain. When you add a non-obvious Rust construct, a comment in
the same spirit is welcome. Beyond those, comment only when the *why* is non-obvious.

## Releases

Releases are cut by release-please; no one bumps a version, edits the CHANGELOG
or pushes a tag by hand. Every merge into `main` updates one open
`chore: release vX.Y.Z` pull request, and merging it creates the tag the
services pin and the GitHub Release. See [docs/releasing.md](docs/releasing.md).

## Running the test suite

```bash
cargo test    # no network, no services
```

The tests drive a real axum router over a temporary UI build, an OpenAPI document written as JSON,
and a log subscriber. Every line of the crate runs in a test, and CI keeps it that way: see
[docs/development.md](docs/development.md#tests-and-coverage).
