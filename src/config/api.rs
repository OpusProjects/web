use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Json;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use auth_rs::http::AuthContext;

use super::files::{REQUIRED_CONFIG_FILE, is_config_path};
use super::store::{ConfigChange, ConfigErrors, ConfigFileStat, ConfigStore, directory_etag};
use crate::audit;
use crate::error::ApiError;

// The configuration directory, over HTTP.
//
// This exists so a configuration-as-code pipeline can PUSH to an instance
// instead of building an image the instance has to PULL. The unit of work is
// the same one the pipeline already has — whole YAML files — and the contract
// is the one it already relies on: a change is validated exactly as
// `--check-config` validates it, as a DIRECTORY, and is refused whole if it
// does not load. Nothing lands half-applied, and an instance that refuses a
// push keeps serving what it had. Stale beats broken, in both directions.
//
// Admin-only, every route, reads included: the configuration describes the
// estate — which systems exist, which variable holds which credential — which
// is exactly what a restricted consumer key has no business enumerating.
//
// What is the service's own reaches this module through `ConfigHost`: what
// its configuration counts (the summary), what a reload does to the running
// process, and which API keys a configuration would leave in force. The
// functions below are the logic of each route; a service mounts them from its
// own handlers, which carry the OpenAPI annotations naming its own types.

// ---------------------------------------------------------------- wire types

#[derive(Serialize, ToSchema)]
pub struct ConfigFileInfo {
    pub name: String,
    pub size: u64,
    /// Hex sha256 of the contents — the value served as this file's ETag
    pub sha256: String,
    /// RFC 3339, absent if the filesystem does not report one
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct ConfigInventory {
    /// Where the files live on the instance
    pub directory: String,
    /// One hash over the whole directory — send it back as If-Match on a
    /// whole-directory push and a write that would clobber someone else's
    /// change is refused instead of silently winning
    pub etag: String,
    pub files: Vec<ConfigFileInfo>,
    /// Known files that are not present. Every one of them is optional except
    /// config.yaml
    pub missing: Vec<String>,
    /// How many times this process has reloaded. 0 = still running exactly
    /// what it booted with
    pub generation: u64,
    /// Whether what is on disk right now would load
    pub valid: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
    /// True when the directory differs from what the process is serving —
    /// somebody wrote files and nobody reloaded
    pub reload_pending: bool,
    /// config.yaml keys that differ from the ones this process was built with
    /// and that no reload can adopt
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub restart_required: Vec<String>,
}

// What changed in one section of the configuration, by item name. A service's
// reload report has one of these per section it reloads.
#[derive(Serialize, ToSchema)]
pub struct DeltaInfo {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub added: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changed: Vec<String>,
}

// Generic over the service's summary (`S`, what its configuration counts) and
// reload report (`R`): utoipa documents an instance such as
// `WriteResult<ConfigSummary, ReloadInfo>` as a schema of its own.
#[derive(Serialize, ToSchema)]
pub struct WriteResult<S, R> {
    pub written: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub deleted: Vec<String>,
    /// The directory ETag AFTER the write — feed it to the next If-Match
    pub etag: String,
    pub summary: S,
    /// Present when the write asked for ?reload=true
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reloaded: Option<R>,
    /// True when the files are on disk but the process is still serving the
    /// previous configuration (no ?reload=true, and something differs)
    pub reload_pending: bool,
}

#[derive(Serialize, ToSchema)]
pub struct ValidationResult<S> {
    pub valid: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<S>,
}

// A rejected configuration, as a SUPERSET of the ordinary error body: `error`
// is the one-line summary every other route answers with, `errors` is the
// whole list. A pipeline renders the list; a generic client that only knows
// `error` keeps working.
#[derive(Serialize, ToSchema)]
pub struct ConfigRejected {
    pub error: String,
    pub errors: Vec<String>,
}

// The whole directory in one request — the verb a pipeline actually wants.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfigBundle {
    /// File name -> YAML contents. Unnamed files keep what is on disk unless
    /// `prune` says otherwise
    pub files: BTreeMap<String, String>,
    /// Delete every known file this payload does not name, so the directory
    /// ends up being exactly the push. The same semantics as the
    /// configuration image it replaces: what is not in the image is not in
    /// /config
    #[serde(default)]
    pub prune: bool,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReloadQuery {
    /// Apply the new configuration to the running process as part of the
    /// write, so the push is one request instead of two
    #[serde(default)]
    pub reload: bool,
}

// ------------------------------------------------------------- the service

// Whether a configuration differs from what the process is running.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Pending {
    // Anything at all would change with a reload
    pub changed: bool,
    // config.yaml keys that differ and that no reload can adopt
    pub restart_required: Vec<String>,
}

// Which counter a config route is counting, for the service's own metrics.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConfigMetric {
    // A write: outcome "success", "rejected" or "error"
    Write,
    // A reload of what is on disk that did not load: outcome "invalid"
    Reload,
}

// What the config API needs from the service it runs in. `P` is the service's
// permissions type, as in auth-rs.
//
// `prepare` and `apply` are a pair on purpose: everything a reload must be
// sure of (every API key's variable set, an API that keeps its
// authentication) is checked BEFORE anything is committed, and what it found
// is handed to `apply` afterwards, so a write that cannot be applied fails
// whole instead of landing on disk and then refusing to take effect.
pub trait ConfigHost<P> {
    type Config;
    // What a configuration counts, per section
    type Summary: Serialize;
    // What `prepare` hands to `apply`: the API keys the new configuration resolves to
    type Prepared;
    // What a reload reports
    type Report: Serialize;

    // None when the config API is disabled on this instance
    fn store(&self) -> Option<&Arc<dyn ConfigStore<Config = Self::Config>>>;
    fn is_admin(&self, permissions: &P) -> bool;
    // How many reloads this process has done
    fn generation(&self) -> u64;
    fn summarize(&self, config: &Self::Config) -> Self::Summary;
    fn pending(&self, config: &Self::Config) -> Pending;
    fn prepare(&self, config: &Self::Config) -> Result<Self::Prepared, ApiError>;
    fn apply(&self, config: &Self::Config, prepared: Self::Prepared) -> Self::Report;
    fn count(&self, metric: ConfigMetric, outcome: &'static str);
}

// ------------------------------------------------------------------- helpers

fn store<P, H: ConfigHost<P>>(
    host: &H,
) -> Result<&Arc<dyn ConfigStore<Config = H::Config>>, ApiError> {
    host.store().ok_or_else(|| {
        ApiError::forbidden(
            "the configuration API is disabled on this instance — set \
             config_api.enabled: true in config.yaml to allow writing the \
             configuration directory over HTTP",
        )
    })
}

fn admin<P, H: ConfigHost<P>>(host: &H, auth: &AuthContext<P>) -> Result<(), ApiError> {
    if host.is_admin(&auth.permissions) {
        Ok(())
    } else {
        Err(ApiError::admin_only())
    }
}

// A path the loader would read: relative, at any depth, safe segments, a
// YAML extension. Anything else could never be part of the configuration, so
// it is refused before it is read or written.
fn known_file(name: &str) -> Result<(), ApiError> {
    if is_config_path(name) {
        Ok(())
    } else {
        Err(ApiError::not_found(format!(
            "'{}' is not a configuration path — a relative path ending in .yaml or \
             .yml, whose segments use only letters, digits, '-', '_' and '.' and \
             do not start with a dot",
            name
        )))
    }
}

fn rfc3339(time: std::time::SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(time).to_rfc3339()
}

fn file_info(stat: &ConfigFileStat) -> ConfigFileInfo {
    ConfigFileInfo {
        name: stat.name.clone(),
        size: stat.size,
        sha256: stat.sha256.clone(),
        modified: stat.modified.map(rfc3339),
    }
}

// A rejected configuration is a 400 with the whole list, not a 400 with the
// first line of it: a pipeline that pushed eight files wants every problem in
// one round trip, which is the same reason --check-config prints them all.
fn rejected(errors: ConfigErrors) -> Response {
    let error = match errors.errors.as_slice() {
        [one] => one.clone(),
        many => format!("{} configuration errors", many.len()),
    };
    (
        StatusCode::BAD_REQUEST,
        Json(ConfigRejected {
            error,
            errors: errors.errors,
        }),
    )
        .into_response()
}

// If-Match, honored the way a config pipeline needs it: the client sends the
// ETag it read, and a write against a directory (or file) that has moved since
// is refused rather than applied on top of someone else's change. `*` means
// "must exist". No header at all means "I am the only writer", which is the
// common case and stays unceremonious.
fn precondition(headers: &HeaderMap, current: Option<&str>) -> Result<(), ApiError> {
    let Some(expected) = headers.get(header::IF_MATCH).and_then(|v| v.to_str().ok()) else {
        return Ok(());
    };

    let matches = expected
        .split(',')
        .map(|candidate| candidate.trim().trim_matches('"'))
        .any(|candidate| match current {
            Some(etag) => candidate == "*" || candidate == etag,
            None => false,
        });

    if matches {
        return Ok(());
    }

    Err(ApiError::new(
        StatusCode::PRECONDITION_FAILED,
        match current {
            Some(etag) => format!(
                "If-Match did not match: the current ETag is '{}'. Re-read the \
                 configuration and rebase the change onto it",
                etag
            ),
            None => "If-Match was sent but the file does not exist".to_string(),
        },
    ))
}

fn change_of(bundle: ConfigBundle) -> Result<ConfigChange, ApiError> {
    for name in bundle.files.keys() {
        known_file(name)?;
    }
    Ok(ConfigChange {
        write: bundle.files,
        prune: bundle.prune,
        ..ConfigChange::default()
    })
}

// ------------------------------------------------------------------- routes

// GET /api/v1/config: every file with its hash, whether it still loads, and
// whether the process is running it.
pub fn get_config<P, H: ConfigHost<P>>(
    host: &H,
    auth: &AuthContext<P>,
) -> Result<Json<ConfigInventory>, ApiError> {
    admin(host, auth)?;
    let store = store(host)?;

    let stats = store.stat_all().map_err(ApiError::internal)?;
    // Only one file is ever required; every other is an item file that is
    // there or not.
    let missing = if stats.iter().any(|s| s.name == REQUIRED_CONFIG_FILE) {
        Vec::new()
    } else {
        vec![REQUIRED_CONFIG_FILE.to_string()]
    };

    // What is on disk may differ from what is running (somebody wrote without
    // reloading) and may not even load (somebody edited the file by hand).
    // Both are worth an answer here rather than at the next restart.
    let (valid, errors, pending) = match store.load(&ConfigChange::default()) {
        Ok(config) => (true, Vec::new(), host.pending(&config)),
        Err(e) => (false, e.errors, Pending::default()),
    };

    Ok(Json(ConfigInventory {
        directory: store.location(),
        etag: directory_etag(&stats),
        files: stats.iter().map(file_info).collect(),
        missing,
        generation: host.generation(),
        valid,
        errors,
        reload_pending: pending.changed,
        restart_required: pending.restart_required,
    }))
}

// GET /api/v1/config/{file}: the file verbatim, its sha256 as the ETag.
pub fn get_config_file<P, H: ConfigHost<P>>(
    host: &H,
    auth: &AuthContext<P>,
    file: &str,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    admin(host, auth)?;
    let store = store(host)?;
    known_file(file)?;

    let Some((stat, contents)) = store.read(file).map_err(ApiError::internal)? else {
        return Err(ApiError::not_found(format!(
            "'{}' is not present in the configuration directory",
            file
        )));
    };

    let etag = format!("\"{}\"", stat.sha256);
    if let Some(sent) = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        && sent
            .split(',')
            .any(|candidate| candidate.trim() == etag || candidate.trim() == "*")
    {
        return Ok((StatusCode::NOT_MODIFIED, [(header::ETAG, etag)]).into_response());
    }

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/yaml".to_string()),
            (header::ETAG, etag),
        ],
        contents,
    )
        .into_response())
}

// PUT /api/v1/config/{file}: one file, written whole.
pub fn put_config_file<P, H: ConfigHost<P>>(
    host: &H,
    auth: &AuthContext<P>,
    file: &str,
    reload: bool,
    headers: &HeaderMap,
    body: String,
) -> Result<Response, ApiError> {
    admin(host, auth)?;
    let store = store(host)?;
    known_file(file)?;

    let current = store.read(file).map_err(ApiError::internal)?;
    precondition(
        headers,
        current.as_ref().map(|(stat, _)| stat.sha256.as_str()),
    )?;

    let change = ConfigChange::writing([(file.to_string(), body)].into_iter().collect());
    write(host, store, auth, headers, change, reload)
}

// DELETE /api/v1/config/{file}: one file removed, never config.yaml.
pub fn delete_config_file<P, H: ConfigHost<P>>(
    host: &H,
    auth: &AuthContext<P>,
    file: &str,
    reload: bool,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    admin(host, auth)?;
    let store = store(host)?;
    known_file(file)?;

    if file == REQUIRED_CONFIG_FILE {
        return Err(ApiError::bad_request(format!(
            "'{}' cannot be deleted — it is the one file a configuration \
             cannot start without",
            REQUIRED_CONFIG_FILE
        )));
    }

    let Some((stat, _)) = store.read(file).map_err(ApiError::internal)? else {
        return Err(ApiError::not_found(format!(
            "'{}' is not present in the configuration directory",
            file
        )));
    };
    precondition(headers, Some(&stat.sha256))?;

    write(
        host,
        store,
        auth,
        headers,
        ConfigChange::deleting(file),
        reload,
    )
}

// PUT /api/v1/config: the whole directory in one request.
pub fn put_config<P, H: ConfigHost<P>>(
    host: &H,
    auth: &AuthContext<P>,
    reload: bool,
    headers: &HeaderMap,
    bundle: ConfigBundle,
) -> Result<Response, ApiError> {
    admin(host, auth)?;
    let store = store(host)?;
    let change = change_of(bundle)?;

    let stats = store.stat_all().map_err(ApiError::internal)?;
    precondition(headers, Some(&directory_etag(&stats)))?;

    write(host, store, auth, headers, change, reload)
}

// POST /api/v1/config/validate: the verdict on a directory, or on what is on
// disk when no body is sent. Nothing is ever written.
pub fn validate_config<P, H: ConfigHost<P>>(
    host: &H,
    auth: &AuthContext<P>,
    bundle: Option<ConfigBundle>,
) -> Result<Json<ValidationResult<H::Summary>>, ApiError> {
    admin(host, auth)?;
    let store = store(host)?;

    let change = match bundle {
        Some(bundle) => change_of(bundle)?,
        None => ConfigChange::default(),
    };

    Ok(Json(match store.load(&change) {
        Ok(config) => ValidationResult {
            valid: true,
            errors: Vec::new(),
            summary: Some(host.summarize(&config)),
        },
        Err(e) => ValidationResult {
            valid: false,
            errors: e.errors,
            summary: None,
        },
    }))
}

// POST /api/v1/config/reload: what is on disk becomes the running configuration.
pub fn reload_config<P, H: ConfigHost<P>>(
    host: &H,
    auth: &AuthContext<P>,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    admin(host, auth)?;
    let store = store(host)?;

    let config = match store.load(&ConfigChange::default()) {
        Ok(config) => config,
        Err(e) => {
            host.count(ConfigMetric::Reload, "invalid");
            audit::record(auth, headers, "config_reload", "config", "invalid");
            return Ok(rejected(e));
        }
    };

    let prepared = host.prepare(&config)?;
    let report = host.apply(&config, prepared);

    audit::record(auth, headers, "config_reload", "config", "success");

    Ok(Json(report).into_response())
}

// Validate, then commit, then — only if asked — apply. Shared by all three
// write routes so the ORDER cannot differ between them: nothing is written
// until the whole directory loads, and nothing is applied until everything a
// reload needs is known to be there.
fn write<P, H: ConfigHost<P>>(
    host: &H,
    store: &Arc<dyn ConfigStore<Config = H::Config>>,
    auth: &AuthContext<P>,
    headers: &HeaderMap,
    change: ConfigChange,
    reload: bool,
) -> Result<Response, ApiError> {
    let names = changed_names(&change).join(",");
    let config = match store.load(&change) {
        Ok(config) => config,
        Err(e) => {
            host.count(ConfigMetric::Write, "rejected");
            audit::record(auth, headers, "config_write", &names, "rejected");
            return Ok(rejected(e));
        }
    };

    // Before the commit, deliberately: a write that cannot be applied should
    // fail as a whole rather than land on disk and then refuse to take effect.
    let prepared = if reload {
        Some(host.prepare(&config)?)
    } else {
        None
    };

    // Named before the commit: afterwards, what a prune removed is no longer
    // there to be listed.
    let deleted = deleted_names(store.as_ref(), &change)?;
    store.commit(&change).map_err(|e| {
        host.count(ConfigMetric::Write, "error");
        ApiError::internal(format!("configuration written to nowhere: {}", e))
    })?;
    host.count(ConfigMetric::Write, "success");

    let written: Vec<String> = change.write.keys().cloned().collect();
    let reloaded = prepared.map(|prepared| host.apply(&config, prepared));
    // Without a reload the files are on disk and the process is still serving
    // the old configuration. Saying so is the difference between a pipeline
    // that knows it has one more step and one that reports a deploy it did
    // not do.
    let reload_pending = reloaded.is_none() && host.pending(&config).changed;

    let action = if reload {
        "config_write_reload"
    } else {
        "config_write"
    };
    audit::record(auth, headers, action, &names, "success");

    let stats = store.stat_all().map_err(ApiError::internal)?;

    Ok(Json(WriteResult {
        written,
        deleted,
        etag: directory_etag(&stats),
        summary: host.summarize(&config),
        reloaded,
        reload_pending,
    })
    .into_response())
}

fn changed_names(change: &ConfigChange) -> Vec<String> {
    let mut names: Vec<String> = change.write.keys().cloned().collect();
    names.extend(change.delete.iter().cloned());
    if change.prune {
        names.push("(prune)".to_string());
    }
    names
}

// Which files the commit will remove: the explicit deletions, plus whatever
// a pruning push leaves out.
fn deleted_names<C>(
    store: &dyn ConfigStore<Config = C>,
    change: &ConfigChange,
) -> Result<Vec<String>, ApiError> {
    let present = store.stat_all().map_err(ApiError::internal)?;
    let mut deleted: Vec<String> = change.delete.iter().cloned().collect();
    if change.prune {
        for stat in &present {
            if !change.write.contains_key(&stat.name) {
                deleted.push(stat.name.clone());
            }
        }
    }
    deleted.sort();
    deleted.dedup();
    Ok(deleted)
}
