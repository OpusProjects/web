use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use sha2::{Digest, Sha256};

use super::files::{REQUIRED_CONFIG_FILE, config_files};

// Loading fails in two shapes — a file that does not parse (one error, and
// nothing after it is knowable) and a directory that parses but does not hang
// together (as many errors as there are broken references). Both arrive here,
// so a caller that wants the list gets the list and a caller that wants a
// message gets a message.
#[derive(Debug)]
pub struct ConfigErrors {
    pub errors: Vec<String>,
}

impl ConfigErrors {
    pub fn new(errors: Vec<String>) -> Self {
        Self { errors }
    }

    pub fn single(message: String) -> Self {
        Self {
            errors: vec![message],
        }
    }
}

impl std::fmt::Display for ConfigErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.errors.as_slice() {
            [one] => write!(f, "{}", one),
            many => write!(f, "Configuration errors:\n  - {}", many.join("\n  - ")),
        }
    }
}

impl std::error::Error for ConfigErrors {}

// The configuration directory as something the application can read, propose
// changes to, and commit — rather than as a directory the process happens to
// have read once at startup.
//
// A port, so the use case is written against "a place configuration lives"
// and the filesystem details (staging, atomic renames, which temp file) stay
// in the adapter. The types are deliberately about WHOLE FILES: this mirrors
// the deployment unit a configuration-as-code pipeline already works in — it
// renders `sources.yaml`, not a patch to a source — and it keeps YAML
// comments and key order intact, which any parse-and-reserialize scheme
// would quietly destroy.

// One file's identity without its contents: enough to list a directory, and
// enough to answer "did this change" without shipping every byte.
#[derive(Debug, Clone)]
pub struct ConfigFileStat {
    pub name: String,
    pub size: u64,
    // Hex sha256 of the contents. Also the file's ETag — see the HTTP adapter.
    pub sha256: String,
    pub modified: Option<SystemTime>,
}

// A proposed change to the directory, as one unit.
//
// Everything is applied together or not at all: a pipeline that renders five
// files has no interest in a state where three landed, and a cross-file
// reference (a source naming a project) can only be validated against the
// whole set anyway.
#[derive(Debug, Clone, Default)]
pub struct ConfigChange {
    // File name -> contents. Files not named here keep whatever is on disk,
    // unless `prune` says otherwise.
    pub write: BTreeMap<String, String>,
    pub delete: BTreeSet<String>,
    // Delete every known file that `write` does not name: the directory ends
    // up being exactly the payload. This is what makes a push idempotent —
    // the same semantics as the configuration image it replaces, where what
    // is not in the image is not in /config.
    pub prune: bool,
}

impl ConfigChange {
    pub fn writing(write: BTreeMap<String, String>) -> Self {
        Self {
            write,
            ..Self::default()
        }
    }

    pub fn deleting(name: &str) -> Self {
        Self {
            delete: [name.to_string()].into_iter().collect(),
            ..Self::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.write.is_empty() && self.delete.is_empty() && !self.prune
    }
}

// Generic over what a directory loads into (`Config`): the service's own
// configuration type, which this crate never looks inside.
pub trait ConfigStore: Send + Sync {
    type Config;

    // Every known configuration file that is present, in load order.
    fn stat_all(&self) -> Result<Vec<ConfigFileStat>, String>;

    // One file with its contents. Ok(None) = the file is not there, which for
    // every file but config.yaml is a legitimate configuration.
    fn read(&self, name: &str) -> Result<Option<(ConfigFileStat, String)>, String>;

    // Load the directory as it WOULD be with `change` applied, without
    // touching it. An empty change validates what is on disk right now —
    // exactly what `--check-config` does, from inside the running process.
    fn load(&self, change: &ConfigChange) -> Result<Self::Config, ConfigErrors>;

    // Apply the change. Callers validate first (`load`) — commit does not,
    // because "write this even though it does not load" is a thing an
    // operator can legitimately want and a thing the API refuses on its own.
    fn commit(&self, change: &ConfigChange) -> Result<(), String>;

    // Where this store keeps the files, for the operator reading the response.
    fn location(&self) -> String;
}

// CONFIG_DIR, as a store the running process can write back to.
//
// Two things here are worth more than the code that does them.
//
// First, a change is VALIDATED IN A STAGING DIRECTORY, never in place. The
// loader's only entry point is "load this directory", and the question being
// asked is about a directory that does not exist yet — the current one with
// three files replaced. Staging a copy is what lets the answer arrive before
// anything on disk has moved, which is the whole difference between "your
// change was rejected" and "your change was rejected and the instance is now
// running on it".
//
// Second, the commit writes every file to a temp name and only then renames
// them into place. rename(2) within a filesystem is atomic, so no reader ever
// sees a half-written file. Renames of SEVERAL files are not atomic as a
// group — a crash between two of them leaves some new and some old — but each
// file is whole, the set was validated together moments earlier, and the
// alternative (swapping a whole directory by rename) would break every
// deployment that bind-mounts the directory itself, which is all of them.
//
// The store does not know the configuration's shape: the service hands it the
// function that loads a directory (the same one startup and --check-config
// use), and the store calls it on the real directory or on a staged copy.
pub struct FsConfigStore<C> {
    dir: PathBuf,
    load: Loader<C>,
}

// The service's loader: a directory in, its configuration or every error out.
pub type Loader<C> = fn(&Path) -> Result<C, ConfigErrors>;

// Staging directories are named per process and per attempt so two concurrent
// writes cannot land in the same one.
static STAGE_COUNTER: AtomicU64 = AtomicU64::new(0);

impl<C> FsConfigStore<C> {
    pub fn new(dir: impl Into<PathBuf>, load: Loader<C>) -> Self {
        Self {
            dir: dir.into(),
            load,
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    // The contents every file WOULD have with `change` applied: what is on
    // disk, plus the writes, minus the deletions (and minus everything the
    // payload does not name, when pruning). Paths are relative to the
    // directory, with `/`, at any depth.
    fn effective(&self, change: &ConfigChange) -> Result<BTreeMap<String, String>, String> {
        let mut files = BTreeMap::new();

        if !change.prune {
            for name in config_files(&self.dir)? {
                if change.delete.contains(&name) || change.write.contains_key(&name) {
                    continue;
                }
                match std::fs::read_to_string(self.dir.join(&name)) {
                    Ok(contents) => {
                        files.insert(name, contents);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(format!("read '{}': {}", name, e)),
                }
            }
        }
        for (name, contents) in &change.write {
            if !change.delete.contains(name) {
                files.insert(name.clone(), contents.clone());
            }
        }

        Ok(files)
    }

    // After files are removed, the folders they leave empty go too, up to
    // (never including) the configuration directory itself: an empty folder
    // is not configuration, and a pruned push should leave exactly its tree.
    fn remove_empty_parents(&self, name: &str) {
        let mut dir = self.dir.join(name);
        while dir.pop() && dir != self.dir && dir.starts_with(&self.dir) {
            if std::fs::remove_dir(&dir).is_err() {
                break;
            }
        }
    }

    fn stat_of(&self, name: &str, contents: &str) -> ConfigFileStat {
        let modified = std::fs::metadata(self.dir.join(name))
            .ok()
            .and_then(|m| m.modified().ok());
        ConfigFileStat {
            name: name.to_string(),
            size: contents.len() as u64,
            sha256: sha256_hex(contents),
            modified,
        }
    }
}

// Hex sha256 of a file's contents — the value the HTTP layer serves as an
// ETag. Content-addressed rather than mtime-based on purpose: a pipeline that
// rewrites a file with identical bytes has changed nothing, and should not be
// told it has.
pub fn sha256_hex(contents: &str) -> String {
    let digest = Sha256::digest(contents.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(hex, "{:02x}", byte);
    }
    hex
}

// A directory-wide ETag: one hash over every file's name and hash, in load
// order. This is what a pipeline sends back as If-Match when it pushes the
// whole directory, so a push that would overwrite someone else's change is
// refused instead of silently winning.
pub fn directory_etag(stats: &[ConfigFileStat]) -> String {
    let mut hasher = Sha256::new();
    for stat in stats {
        hasher.update(stat.name.as_bytes());
        hasher.update(b"\0");
        hasher.update(stat.sha256.as_bytes());
        hasher.update(b"\n");
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(hex, "{:02x}", byte);
    }
    hex
}

// Removes the staging directory when it goes out of scope, however it goes
// out of scope — a rejected configuration is the common case here, and it
// must not leave a directory behind on every failed attempt.
struct Staging {
    path: PathBuf,
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

impl<C> ConfigStore for FsConfigStore<C> {
    type Config = C;

    fn stat_all(&self) -> Result<Vec<ConfigFileStat>, String> {
        let mut stats = Vec::new();
        for name in config_files(&self.dir)? {
            match std::fs::read_to_string(self.dir.join(&name)) {
                Ok(contents) => stats.push(self.stat_of(&name, &contents)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("read '{}': {}", name, e)),
            }
        }
        Ok(stats)
    }

    fn read(&self, name: &str) -> Result<Option<(ConfigFileStat, String)>, String> {
        match std::fs::read_to_string(self.dir.join(name)) {
            Ok(contents) => Ok(Some((self.stat_of(name, &contents), contents))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("read '{}': {}", name, e)),
        }
    }

    fn load(&self, change: &ConfigChange) -> Result<C, ConfigErrors> {
        let files = self
            .effective(change)
            .map_err(|e| ConfigErrors::new(vec![e]))?;

        // Nothing to stage: the caller is validating what is already there.
        if change.is_empty() {
            return (self.load)(&self.dir);
        }

        if !files.contains_key(REQUIRED_CONFIG_FILE) {
            return Err(ConfigErrors::new(vec![format!(
                "{} would be missing — it is the one file a configuration cannot start without",
                REQUIRED_CONFIG_FILE
            )]));
        }

        let stage = Staging {
            path: self.dir.join(format!(
                ".config-api-stage-{}-{}",
                std::process::id(),
                STAGE_COUNTER.fetch_add(1, Ordering::Relaxed)
            )),
        };
        std::fs::create_dir_all(&stage.path).map_err(|e| {
            ConfigErrors::new(vec![format!(
                "create staging directory '{}': {}",
                stage.path.display(),
                e
            )])
        })?;
        for (name, contents) in &files {
            let path = stage.path.join(name);
            // A path joined onto the stage always has a parent: the stage itself.
            let parent = path.parent().unwrap_or(&stage.path);
            std::fs::create_dir_all(parent)
                .map_err(|e| ConfigErrors::new(vec![format!("stage '{}': {}", name, e)]))?;
            std::fs::write(&path, contents)
                .map_err(|e| ConfigErrors::new(vec![format!("stage '{}': {}", name, e)]))?;
        }

        (self.load)(&stage.path)
    }

    fn commit(&self, change: &ConfigChange) -> Result<(), String> {
        let files = self.effective(change)?;

        // Write every file first, rename every file second. The window in
        // which the directory holds a mix of old and new is the duration of
        // the renames rather than the duration of the writes.
        //
        // `files` is the whole directory, not just the payload — that is what
        // makes the change transactional. But a file whose bytes are already
        // what we would write is SKIPPED, because writing it would reset an
        // mtime that nothing changed, and mtime is exactly what
        // GET /api/v1/config reports as `modified`. A single-file PUT used to
        // stamp all eight files with the time of the request, so "when did
        // credentials.yaml last change" answered "just now" forever.
        let mut staged: Vec<(PathBuf, PathBuf)> = Vec::new();
        for (name, contents) in &files {
            let target = self.dir.join(name);
            if std::fs::read_to_string(&target).is_ok_and(|current| current == *contents) {
                continue;
            }
            // The temp file sits next to its target (a rename is only atomic
            // within one filesystem) and is hidden, so the loader skips it.
            let parent = target.parent().unwrap_or(&self.dir).to_path_buf();
            std::fs::create_dir_all(&parent)
                .map_err(|e| format!("create '{}': {}", parent.display(), e))?;
            let file_name = target
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let tmp = parent.join(format!(".{}.tmp", file_name));
            std::fs::write(&tmp, contents)
                .map_err(|e| format!("write '{}': {}", tmp.display(), e))?;
            staged.push((tmp, target));
        }
        for (tmp, target) in &staged {
            std::fs::rename(tmp, target).map_err(|e| {
                format!(
                    "rename '{}' to '{}': {}",
                    tmp.display(),
                    target.display(),
                    e
                )
            })?;
        }

        // Whatever `files` does not contain is gone — either explicitly
        // deleted or pruned away by a whole-directory push.
        for name in config_files(&self.dir)? {
            if files.contains_key(&name) {
                continue;
            }
            match std::fs::remove_file(self.dir.join(&name)) {
                Ok(()) => self.remove_empty_parents(&name),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("remove '{}': {}", name, e)),
            }
        }

        Ok(())
    }

    fn location(&self) -> String {
        self.dir.display().to_string()
    }
}
