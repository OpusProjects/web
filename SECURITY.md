# Security Policy

## Supported versions

Only the latest release line receives security fixes.

| Version | Supported |
|---|---|
| 0.x (latest release) | Yes |
| older tags | No |

## Reporting a vulnerability

Please **do not open a public issue** for security problems.

Use GitHub's private vulnerability reporting instead:
[Report a vulnerability](https://github.com/OpusProjects/web/security/advisories/new)
— it opens a private thread with the maintainers.

Include what you can: affected module, the service and version you saw
it in, reproduction steps, and impact. You should hear back within a week. Once
a fix ships, the advisory is published and credited unless you prefer otherwise.

## Scope notes

- This repository is the web layer the services that pin it share: a vulnerability here is one in
  each of them. A fix is released here first, then each service moves to the new tag.
- **Every error has the same shape** (`{"error": "..."}`), and a refusal says what to fix without
  saying more than the caller may know: an unknown key and a wrong key read the same.
- **The web UI is gated** when API keys exist: a page load without a live session is sent to the
  sign-in form before the app loads. The app's own script and style files are not secrets and are
  served without one.
- **A miss under `/api`, `/swagger-ui` or `/api-docs` is always the API's JSON 404**, never the app's
  page, so a client cannot mistake an HTML page for an answer; the UI's files are read-only.
- **Audit events** name who acted (the API key's label), what they did and how it ended, with the
  request id that joins them to the access log.
