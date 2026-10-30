// web-rs — the web layer a JSON API service shares with its siblings.
//
// What a service's routes do is its own. How it answers on the web is not: the
// same error shape, the same audit events, the same web UI served from the same
// address behind the same sign-in, the same help on every browsable page. This
// crate holds that part:
//
// - `error`: ApiError and ErrorBody, the `{"error": ...}` every failure carries
// - `audit`: one structured event per operation that changed something
// - `config`: the configuration directory over HTTP, staged, validated whole
//   and committed atomically
// - `ui_assets`: the built web UI at "/", gated by sign-in, never mistaken for
//   an API answer
// - `spec`: what the OpenAPI document says about a route, for the pages that
//   explain themselves
// - `url`: escaping a value for a query string
//
// Who a caller is comes from auth-rs (API keys and browser sessions), so
// the gate and the audit events are generic over the service's permissions
// type, as auth-rs's own types are.

pub mod audit;
pub mod config;
pub mod error;
pub mod spec;
pub mod ui_assets;
pub mod url;

pub use error::{ApiError, ErrorBody};
pub use spec::Spec;
pub use ui_assets::UiAssets;
