// The config API's routes, against a test service: a real directory store with
// a stand-in loader (see config_store_test.rs), a "running" configuration a
// reload replaces, and counters for what the service would put in metrics.
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use auth_rs::http::{AuthContext, AuthMethod};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use web_rs::ApiError;
use web_rs::config::api::{
    delete_config_file, get_config, get_config_file, put_config, put_config_file, reload_config,
    validate_config,
};
use web_rs::config::{
    ConfigBundle, ConfigChange, ConfigErrors, ConfigFileStat, ConfigHost, ConfigMetric,
    ConfigStore, FsConfigStore, Pending, config_files,
};

type Files = BTreeMap<String, String>;

fn load(dir: &Path) -> Result<Files, ConfigErrors> {
    let mut files = Files::new();
    for name in config_files(dir).map_err(ConfigErrors::single)? {
        files.insert(
            name.clone(),
            std::fs::read_to_string(dir.join(name)).unwrap(),
        );
    }
    let errors: Vec<String> = files
        .iter()
        .filter(|(_, contents)| contents.contains("invalid"))
        .map(|(name, _)| format!("{name}: invalid"))
        .collect();
    if errors.is_empty() {
        Ok(files)
    } else {
        Err(ConfigErrors::new(errors))
    }
}

#[derive(Serialize)]
struct Summary {
    files: usize,
}

#[derive(Serialize)]
struct Report {
    generation: u64,
}

struct Host {
    store: Option<Arc<dyn ConfigStore<Config = Files>>>,
    running: Mutex<Files>,
    generation: Mutex<u64>,
    counted: Mutex<Vec<(ConfigMetric, &'static str)>>,
}

impl Host {
    fn new(store: Option<Arc<dyn ConfigStore<Config = Files>>>) -> Self {
        let running = store
            .as_ref()
            .and_then(|s| s.load(&ConfigChange::default()).ok())
            .unwrap_or_default();
        Self {
            store,
            running: Mutex::new(running),
            generation: Mutex::new(0),
            counted: Mutex::new(Vec::new()),
        }
    }
}

// Permissions: whether the caller is an admin
impl ConfigHost<bool> for Host {
    type Config = Files;
    type Summary = Summary;
    type Prepared = usize;
    type Report = Report;

    fn store(&self) -> Option<&Arc<dyn ConfigStore<Config = Files>>> {
        self.store.as_ref()
    }
    fn is_admin(&self, permissions: &bool) -> bool {
        *permissions
    }
    fn generation(&self) -> u64 {
        *self.generation.lock().unwrap()
    }
    fn summarize(&self, config: &Files) -> Summary {
        Summary {
            files: config.len(),
        }
    }
    fn pending(&self, config: &Files) -> Pending {
        let running = self.running.lock().unwrap();
        Pending {
            changed: *running != *config,
            restart_required: if running.get("config.yaml") != config.get("config.yaml") {
                vec!["server.port".to_string()]
            } else {
                Vec::new()
            },
        }
    }
    // A configuration with a file named nokeys.yaml would leave no API keys
    fn prepare(&self, config: &Files) -> Result<usize, ApiError> {
        if config.contains_key("nokeys.yaml") {
            Err(ApiError::new(StatusCode::CONFLICT, "refusing to reload"))
        } else {
            Ok(config.len())
        }
    }
    fn apply(&self, config: &Files, _prepared: usize) -> Report {
        *self.running.lock().unwrap() = config.clone();
        let mut generation = self.generation.lock().unwrap();
        *generation += 1;
        Report {
            generation: *generation,
        }
    }
    fn count(&self, metric: ConfigMetric, outcome: &'static str) {
        self.counted.lock().unwrap().push((metric, outcome));
    }
}

fn auth(admin: bool) -> AuthContext<bool> {
    AuthContext {
        key_name: Some("ops".to_string()),
        key_id: None,
        permissions: admin,
        method: AuthMethod::ApiKey,
    }
}

fn setup() -> (tempfile::TempDir, Host) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.yaml"), "port: 1\n").unwrap();
    std::fs::write(dir.path().join("a.yaml"), "a\n").unwrap();
    let store: Arc<dyn ConfigStore<Config = Files>> =
        Arc::new(FsConfigStore::new(dir.path(), load));
    let host = Host::new(Some(store));
    (dir, host)
}

fn headers(pairs: &[(header::HeaderName, &str)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        map.insert(name.clone(), HeaderValue::from_str(value).unwrap());
    }
    map
}

async fn json(response: Response) -> (StatusCode, serde_json::Value) {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

async fn result(answer: Result<Response, ApiError>) -> (StatusCode, serde_json::Value) {
    json(answer.unwrap_or_else(|e| e.into_response())).await
}

fn bundle(files: &[(&str, &str)], prune: bool) -> ConfigBundle {
    ConfigBundle {
        files: files
            .iter()
            .map(|(n, c)| (n.to_string(), c.to_string()))
            .collect(),
        prune,
    }
}

#[tokio::test]
async fn only_an_admin_on_an_enabled_instance_gets_in() {
    let (_dir, host) = setup();
    let (status, _) =
        result(get_config(&host, &auth(false)).map(IntoResponse::into_response)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let disabled = Host::new(None);
    let (status, body) =
        result(get_config(&disabled, &auth(true)).map(IntoResponse::into_response)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body["error"].as_str().unwrap().contains("disabled"));
}

#[tokio::test]
async fn the_inventory_says_what_is_there_and_what_is_running() {
    let (dir, host) = setup();
    let inventory = get_config(&host, &auth(true)).unwrap().0;
    assert_eq!(inventory.files.len(), 2);
    assert!(inventory.missing.is_empty() && inventory.valid && !inventory.reload_pending);
    assert!(inventory.files[0].modified.is_some());

    // Edited by hand without a reload: pending, and needing a restart
    std::fs::write(dir.path().join("config.yaml"), "port: 2\n").unwrap();
    let inventory = get_config(&host, &auth(true)).unwrap().0;
    assert!(inventory.reload_pending);
    assert_eq!(inventory.restart_required, ["server.port"]);

    // Broken by hand: not valid, with the reasons
    std::fs::write(dir.path().join("a.yaml"), "invalid").unwrap();
    std::fs::remove_file(dir.path().join("config.yaml")).unwrap();
    let inventory = get_config(&host, &auth(true)).unwrap().0;
    assert!(!inventory.valid);
    assert_eq!(inventory.errors, ["a.yaml: invalid"]);
    assert_eq!(inventory.missing, ["config.yaml"]);
}

#[tokio::test]
async fn a_file_is_served_verbatim_with_its_etag() {
    let (_dir, host) = setup();
    let response = get_config_file(&host, &auth(true), "a.yaml", &HeaderMap::new()).unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let etag = response.headers()[header::ETAG]
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/yaml");

    let same = headers(&[(header::IF_NONE_MATCH, &etag)]);
    let response = get_config_file(&host, &auth(true), "a.yaml", &same).unwrap();
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    let other = headers(&[(header::IF_NONE_MATCH, "\"old\"")]);
    let response = get_config_file(&host, &auth(true), "a.yaml", &other).unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    for (file, status) in [
        ("missing.yaml", StatusCode::NOT_FOUND),
        ("../etc/passwd", StatusCode::NOT_FOUND),
    ] {
        let (got, _) = result(get_config_file(&host, &auth(true), file, &HeaderMap::new())).await;
        assert_eq!(got, status, "{file}");
    }
    let (got, _) = result(get_config_file(
        &host,
        &auth(false),
        "a.yaml",
        &HeaderMap::new(),
    ))
    .await;
    assert_eq!(got, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_write_lands_and_says_whether_it_is_running() {
    let (dir, host) = setup();
    let (status, body) = result(put_config_file(
        &host,
        &auth(true),
        "b.yaml",
        false,
        &HeaderMap::new(),
        "b\n".to_string(),
    ))
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["written"], serde_json::json!(["b.yaml"]));
    assert_eq!(body["summary"]["files"], 3);
    assert_eq!(body["reload_pending"], true);
    assert!(body.get("reloaded").is_none());
    assert!(dir.path().join("b.yaml").exists());

    // With ?reload=true it is applied in the same request
    let (_, body) = result(put_config_file(
        &host,
        &auth(true),
        "c.yaml",
        true,
        &HeaderMap::new(),
        "c\n".to_string(),
    ))
    .await;
    assert_eq!(body["reloaded"]["generation"], 1);
    assert_eq!(body["reload_pending"], false);
    assert_eq!(
        *host.counted.lock().unwrap(),
        [
            (ConfigMetric::Write, "success"),
            (ConfigMetric::Write, "success")
        ]
    );
}

#[tokio::test]
async fn a_write_that_would_not_load_lands_nowhere() {
    let (dir, host) = setup();
    let (status, body) = result(put_config_file(
        &host,
        &auth(true),
        "b.yaml",
        false,
        &HeaderMap::new(),
        "invalid".to_string(),
    ))
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "b.yaml: invalid");
    assert!(!dir.path().join("b.yaml").exists());

    // Several problems: the summary line counts them, the list names them
    let (_, body) = result(put_config(
        &host,
        &auth(true),
        false,
        &HeaderMap::new(),
        bundle(&[("x.yaml", "invalid"), ("y.yaml", "invalid")], false),
    ))
    .await;
    assert_eq!(body["error"], "2 configuration errors");
    assert_eq!(body["errors"].as_array().unwrap().len(), 2);
    assert_eq!(
        host.counted.lock().unwrap()[0],
        (ConfigMetric::Write, "rejected")
    );
}

#[tokio::test]
async fn a_reload_that_would_break_the_api_is_refused_before_anything_lands() {
    let (dir, host) = setup();
    let (status, _) = result(put_config_file(
        &host,
        &auth(true),
        "nokeys.yaml",
        true,
        &HeaderMap::new(),
        "x".to_string(),
    ))
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(!dir.path().join("nokeys.yaml").exists());
}

#[tokio::test]
async fn if_match_keeps_a_write_from_clobbering_another() {
    let (_dir, host) = setup();
    let stale = headers(&[(header::IF_MATCH, "\"not-it\"")]);
    let (status, body) = result(put_config_file(
        &host,
        &auth(true),
        "a.yaml",
        false,
        &stale,
        "a2".to_string(),
    ))
    .await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED);
    assert!(body["error"].as_str().unwrap().contains("current ETag"));

    // "*" means "must exist": fine for a.yaml, refused for a new file
    let any = headers(&[(header::IF_MATCH, "*")]);
    let (status, _) = result(put_config_file(
        &host,
        &auth(true),
        "a.yaml",
        false,
        &any,
        "a2".to_string(),
    ))
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = result(put_config_file(
        &host,
        &auth(true),
        "new.yaml",
        false,
        &any,
        "n".to_string(),
    ))
    .await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED);
    assert!(body["error"].as_str().unwrap().contains("does not exist"));

    // A whole-directory push checks the directory's ETag
    let etag = get_config(&host, &auth(true)).unwrap().0.etag;
    let current = headers(&[(header::IF_MATCH, &format!("\"{etag}\", \"other\""))]);
    let (status, _) = result(put_config(
        &host,
        &auth(true),
        false,
        &current,
        bundle(&[("d.yaml", "d")], false),
    ))
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = result(put_config(
        &host,
        &auth(true),
        false,
        &current,
        bundle(&[], false),
    ))
    .await;
    assert_eq!(
        status,
        StatusCode::PRECONDITION_FAILED,
        "the directory moved"
    );
}

#[tokio::test]
async fn a_pruning_push_names_what_it_removed() {
    let (dir, host) = setup();
    let (status, body) = result(put_config(
        &host,
        &auth(true),
        true,
        &HeaderMap::new(),
        bundle(&[("config.yaml", "port: 1\n"), ("z.yaml", "z")], true),
    ))
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["deleted"], serde_json::json!(["a.yaml"]));
    assert!(!dir.path().join("a.yaml").exists());

    let (status, _) = result(put_config(
        &host,
        &auth(true),
        false,
        &HeaderMap::new(),
        bundle(&[("../x.yaml", "x")], false),
    ))
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "not a configuration path");
    let (status, _) = result(put_config(
        &host,
        &auth(false),
        false,
        &HeaderMap::new(),
        bundle(&[], false),
    ))
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_file_is_deleted_but_never_config_yaml() {
    let (dir, host) = setup();
    let none = HeaderMap::new();
    let (status, body) = result(delete_config_file(
        &host,
        &auth(true),
        "a.yaml",
        false,
        &none,
    ))
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["deleted"], serde_json::json!(["a.yaml"]));
    assert!(!dir.path().join("a.yaml").exists());

    let (status, _) = result(delete_config_file(
        &host,
        &auth(true),
        "config.yaml",
        false,
        &none,
    ))
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = result(delete_config_file(
        &host,
        &auth(true),
        "a.yaml",
        false,
        &none,
    ))
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = result(delete_config_file(
        &host,
        &auth(true),
        "../a.yaml",
        false,
        &none,
    ))
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = result(delete_config_file(
        &host,
        &auth(false),
        "a.yaml",
        false,
        &none,
    ))
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn validating_writes_nothing() {
    let (_dir, host) = setup();
    let verdict = validate_config(&host, &auth(true), None).unwrap().0;
    assert!(verdict.valid && verdict.errors.is_empty());
    assert_eq!(verdict.summary.unwrap().files, 2);

    let verdict = validate_config(
        &host,
        &auth(true),
        Some(bundle(&[("b.yaml", "invalid")], false)),
    )
    .unwrap()
    .0;
    assert!(!verdict.valid && verdict.summary.is_none());
    assert_eq!(verdict.errors, ["b.yaml: invalid"]);

    assert!(
        validate_config(
            &host,
            &auth(true),
            Some(bundle(&[("../b.yaml", "b")], false))
        )
        .is_err()
    );
    assert!(validate_config(&host, &auth(false), None).is_err());
}

#[tokio::test]
async fn a_reload_runs_what_is_on_disk_or_says_why_not() {
    let (dir, host) = setup();
    let none = HeaderMap::new();
    let (status, body) = result(reload_config(&host, &auth(true), &none)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["generation"], 1);

    std::fs::write(dir.path().join("nokeys.yaml"), "x").unwrap();
    let (status, _) = result(reload_config(&host, &auth(true), &none)).await;
    assert_eq!(status, StatusCode::CONFLICT);

    std::fs::write(dir.path().join("a.yaml"), "invalid").unwrap();
    let (status, body) = result(reload_config(&host, &auth(true), &none)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["errors"], serde_json::json!(["a.yaml: invalid"]));
    assert_eq!(
        *host.counted.lock().unwrap(),
        [(ConfigMetric::Reload, "invalid")]
    );

    let (status, _) = result(reload_config(&host, &auth(false), &none)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

// A store that fails every way it can: each failure is a 500 naming it.
struct Broken {
    stats: bool,
}

impl ConfigStore for Broken {
    type Config = Files;
    fn stat_all(&self) -> Result<Vec<ConfigFileStat>, String> {
        if self.stats {
            Ok(Vec::new())
        } else {
            Err("cannot list".to_string())
        }
    }
    fn read(&self, _name: &str) -> Result<Option<(ConfigFileStat, String)>, String> {
        Err("cannot read".to_string())
    }
    fn load(&self, _change: &ConfigChange) -> Result<Files, ConfigErrors> {
        Ok(Files::from([("config.yaml".to_string(), String::new())]))
    }
    fn commit(&self, _change: &ConfigChange) -> Result<(), String> {
        Err("disk full".to_string())
    }
    fn location(&self) -> String {
        "nowhere".to_string()
    }
}

#[tokio::test]
async fn a_store_that_fails_is_a_server_error() {
    let none = HeaderMap::new();
    let failing = Host::new(Some(Arc::new(Broken { stats: false })));
    assert!(get_config(&failing, &auth(true)).is_err());
    assert!(get_config_file(&failing, &auth(true), "a.yaml", &none).is_err());
    assert!(put_config_file(&failing, &auth(true), "a.yaml", false, &none, String::new()).is_err());
    assert!(delete_config_file(&failing, &auth(true), "a.yaml", false, &none).is_err());
    assert!(put_config(&failing, &auth(true), false, &none, bundle(&[], false)).is_err());

    // Listing works but the commit does not: counted as an error
    let host = Host::new(Some(Arc::new(Broken { stats: true })));
    let (status, body) = result(put_config(
        &host,
        &auth(true),
        false,
        &none,
        bundle(&[], false),
    ))
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(body["error"].as_str().unwrap().contains("disk full"));
    assert_eq!(
        *host.counted.lock().unwrap(),
        [(ConfigMetric::Write, "error")]
    );
}
