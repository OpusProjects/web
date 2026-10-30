// FsConfigStore against real directories, with a stand-in loader: a directory
// loads when config.yaml is there, no file says `invalid`, and every
// `needs: <file>` names a file that is there too — a parse error and a broken
// reference between files, the two ways a real loader refuses.
use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use web_rs::config::{
    ConfigChange, ConfigErrors, ConfigStore, FsConfigStore, config_files, directory_etag,
    sha256_hex,
};

type Files = BTreeMap<String, String>;

fn load(dir: &Path) -> Result<Files, ConfigErrors> {
    let mut files = Files::new();
    for name in config_files(dir).map_err(ConfigErrors::single)? {
        let contents = std::fs::read_to_string(dir.join(&name)).unwrap();
        files.insert(name, contents);
    }
    if !files.contains_key("config.yaml") {
        return Err(ConfigErrors::single("config.yaml: missing".to_string()));
    }
    let mut errors = Vec::new();
    for (name, contents) in &files {
        if contents.contains("invalid") {
            errors.push(format!("{name}: invalid"));
        }
        if let Some(needed) = contents.strip_prefix("needs: ")
            && !files.contains_key(needed.trim())
        {
            errors.push(format!("{name}: needs {}", needed.trim()));
        }
    }
    if errors.is_empty() {
        Ok(files)
    } else {
        Err(ConfigErrors::new(errors))
    }
}

fn dir_with_config() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.yaml"), "server: {}\n").unwrap();
    dir
}

fn store(dir: &Path) -> FsConfigStore<Files> {
    FsConfigStore::new(dir, load)
}

fn writing(pairs: &[(&str, &str)]) -> ConfigChange {
    ConfigChange::writing(
        pairs
            .iter()
            .map(|(name, contents)| (name.to_string(), contents.to_string()))
            .collect(),
    )
}

fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}

fn lock(path: &Path) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o555)).unwrap();
}

fn unlock(path: &Path) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn a_rejected_change_leaves_the_directory_exactly_as_it_was() {
    let dir = dir_with_config();
    let store = store(dir.path());

    let errors = store
        .load(&writing(&[("a.yaml", "needs: ghost.yaml")]))
        .unwrap_err();
    assert_eq!(errors.errors, ["a.yaml: needs ghost.yaml"]);
    assert_eq!(
        names_in(dir.path()),
        ["config.yaml"],
        "nothing staged is left"
    );
}

#[test]
fn a_change_is_validated_with_the_files_it_does_not_touch() {
    let dir = dir_with_config();
    std::fs::write(dir.path().join("b.yaml"), "b").unwrap();
    let store = store(dir.path());

    // a.yaml needs b.yaml, which is on disk: the staged copy has both
    let files = store
        .load(&writing(&[("sub/a.yaml", "needs: b.yaml")]))
        .unwrap();
    assert_eq!(
        files.keys().collect::<Vec<_>>(),
        ["b.yaml", "config.yaml", "sub/a.yaml"]
    );

    // Deleting what another file needs is refused
    let mut change = writing(&[("a.yaml", "needs: b.yaml")]);
    change.delete = BTreeSet::from(["b.yaml".to_string()]);
    assert!(store.load(&change).is_err());

    // A file both written and deleted is deleted
    let mut change = writing(&[("x.yaml", "x")]);
    change.delete = BTreeSet::from(["x.yaml".to_string()]);
    assert!(!store.load(&change).unwrap().contains_key("x.yaml"));

    // An empty change validates what is on disk, in place
    assert_eq!(store.load(&ConfigChange::default()).unwrap().len(), 2);
}

#[test]
fn a_change_without_config_yaml_is_refused_before_staging() {
    let dir = dir_with_config();
    let store = store(dir.path());
    let mut change = writing(&[("a.yaml", "a")]);
    change.prune = true;
    let errors = store.load(&change).unwrap_err();
    assert!(errors.errors[0].contains("would be missing"), "{errors}");
    assert!(store.commit(&ConfigChange::deleting("config.yaml")).is_ok());
}

#[test]
fn a_commit_writes_whole_files_and_touches_nothing_else() {
    let dir = dir_with_config();
    let store = store(dir.path());
    let mtime = |name: &str| {
        std::fs::metadata(dir.path().join(name))
            .and_then(|m| m.modified())
            .unwrap()
    };
    let before = mtime("config.yaml");
    std::thread::sleep(std::time::Duration::from_millis(50));

    store.commit(&writing(&[("deep/er/a.yaml", "a")])).unwrap();
    assert_eq!(
        mtime("config.yaml"),
        before,
        "an unchanged file is not rewritten"
    );
    assert_eq!(
        names_in(dir.path()),
        ["config.yaml", "deep"],
        "no temp files left"
    );
    assert_eq!(store.read("deep/er/a.yaml").unwrap().unwrap().1, "a");
}

#[test]
fn pruning_removes_what_the_push_left_out_and_the_folders_it_empties() {
    let dir = dir_with_config();
    let store = store(dir.path());
    store
        .commit(&writing(&[
            ("sources/acme/a.yaml", "a"),
            ("keep/b.yaml", "b"),
            ("keep/c.yaml", "c"),
        ]))
        .unwrap();

    let mut change = writing(&[("config.yaml", "server: {}\n"), ("keep/b.yaml", "b")]);
    change.prune = true;
    store.load(&change).unwrap();
    store.commit(&change).unwrap();

    assert_eq!(
        config_files(dir.path()).unwrap(),
        ["config.yaml", "keep/b.yaml"]
    );
    assert!(
        !dir.path().join("sources").exists(),
        "emptied folders go too"
    );
}

#[test]
fn reads_and_stats_follow_the_contents() {
    let dir = dir_with_config();
    let store = store(dir.path());
    assert!(store.read("absent.yaml").unwrap().is_none());

    let stats = store.stat_all().unwrap();
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].sha256, sha256_hex("server: {}\n"));
    assert_eq!(stats[0].size, 11);
    assert!(stats[0].modified.is_some());
    assert_eq!(store.location(), dir.path().display().to_string());
    assert_eq!(store.dir(), dir.path());

    // The directory ETag moves with any file's contents
    let before = directory_etag(&stats);
    store
        .commit(&writing(&[("config.yaml", "server: {x: 1}\n")]))
        .unwrap();
    assert_ne!(directory_etag(&store.stat_all().unwrap()), before);
}

#[test]
fn errors_read_as_one_line_or_a_list() {
    let one = ConfigErrors::single("a.yaml: bad".to_string());
    assert_eq!(one.to_string(), "a.yaml: bad");
    let many = ConfigErrors::new(vec!["a".to_string(), "b".to_string()]);
    assert_eq!(many.to_string(), "Configuration errors:\n  - a\n  - b");
    let as_error: Box<dyn std::error::Error> = Box::new(many);
    assert!(as_error.to_string().contains("- b"));
}

// What the filesystem refuses is reported by name, and leaves the directory
// as it was.
#[test]
fn unreadable_files_are_named() {
    let dir = dir_with_config();
    let store = store(dir.path());
    // A folder where a file is expected cannot be read as one
    std::fs::create_dir(dir.path().join("dir.yaml")).unwrap();
    assert!(store.read("dir.yaml").unwrap_err().contains("dir.yaml"));

    let locked = dir.path().join("locked.yaml");
    std::fs::write(&locked, "x").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    assert!(store.stat_all().unwrap_err().contains("locked.yaml"));
    assert!(store.load(&writing(&[("a.yaml", "a")])).is_err());
    assert!(store.commit(&writing(&[("a.yaml", "a")])).is_err());
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).unwrap();
}

#[test]
fn a_directory_that_cannot_be_listed_is_named() {
    let dir = dir_with_config();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o000)).unwrap();
    let store = store(dir.path());
    assert!(store.stat_all().unwrap_err().contains("sub"));
    assert!(store.load(&writing(&[("a.yaml", "a")])).is_err());
    assert!(store.commit(&writing(&[("a.yaml", "a")])).is_err());
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn a_read_only_directory_refuses_staging_and_commits() {
    let dir = dir_with_config();
    let store = store(dir.path());
    lock(dir.path());
    let err = store.load(&writing(&[("a.yaml", "a")])).unwrap_err();
    assert!(
        err.errors[0].starts_with("create staging directory"),
        "{err}"
    );
    let err = store.commit(&writing(&[("a.yaml", "a")])).unwrap_err();
    assert!(err.starts_with("write '"), "{err}");
    unlock(dir.path());
}

#[test]
fn a_commit_names_the_folder_it_cannot_create() {
    let dir = dir_with_config();
    std::fs::write(dir.path().join("blocker"), "a file").unwrap();
    let store = store(dir.path());
    let err = store
        .commit(&writing(&[("blocker/a.yaml", "a")]))
        .unwrap_err();
    assert!(err.starts_with("create '"), "{err}");
}

#[test]
fn a_stage_that_cannot_hold_a_file_is_named() {
    let dir = dir_with_config();
    let store = store(dir.path());
    // A file and a folder of the same name: the file is staged first, so the
    // folder cannot be created
    let err = store
        .load(&writing(&[("a.yaml/b.yaml", "b"), ("a.yaml", "a")]))
        .unwrap_err();
    assert!(err.errors[0].starts_with("stage 'a.yaml/b.yaml'"), "{err}");

    // A name longer than the filesystem allows cannot be written
    let long = format!("{}.yaml", "a".repeat(300));
    let err = store.load(&writing(&[(long.as_str(), "a")])).unwrap_err();
    assert!(err.errors[0].starts_with("stage '"), "{err}");
}

#[test]
fn a_rename_onto_a_folder_is_named() {
    let dir = dir_with_config();
    std::fs::create_dir_all(dir.path().join("x.yaml/inner")).unwrap();
    let store = store(dir.path());
    let err = store.commit(&writing(&[("x.yaml", "x")])).unwrap_err();
    assert!(err.starts_with("rename '"), "{err}");
}

#[test]
fn a_file_that_cannot_be_removed_is_named() {
    let dir = dir_with_config();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("a.yaml"), "a").unwrap();
    lock(&sub);
    let store = store(dir.path());
    let err = store
        .commit(&ConfigChange::deleting("sub/a.yaml"))
        .unwrap_err();
    assert!(err.starts_with("remove 'sub/a.yaml'"), "{err}");
    unlock(&sub);
}

// A folder that still holds something else stays when a file in it goes.
#[test]
fn a_folder_with_other_contents_stays() {
    let dir = dir_with_config();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("a.yaml"), "a").unwrap();
    std::fs::write(sub.join("notes.txt"), "kept").unwrap();
    store(dir.path())
        .commit(&ConfigChange::deleting("sub/a.yaml"))
        .unwrap();
    assert_eq!(names_in(&sub), ["notes.txt"]);
}
