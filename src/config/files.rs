// Which files make up a configuration directory: the rule both the loader and
// the config API apply, so a file the API cannot name cannot be part of the
// configuration either.

use std::fs;
use std::path::Path;

// The one file a configuration cannot start without; every other file is an
// item file that is there or not.
pub const REQUIRED_CONFIG_FILE: &str = "config.yaml";

// Whether a path, relative to the configuration directory, can be a
// configuration file: segments of letters, digits, `-`, `_` and `.`, none
// hidden, ending in .yaml or .yml. The same rule the loader applies to what
// it finds and the config API applies to what it is asked to write, so a file
// the API cannot name cannot be part of the configuration either.
pub fn is_config_path(path: &str) -> bool {
    let segments: Vec<&str> = path.split('/').collect();
    path.len() <= 512
        && segments.len() <= 16
        && segments.iter().all(|s| is_safe_id(s))
        && (path.ends_with(".yaml") || path.ends_with(".yml"))
}

// Every configuration file under `dir`, as relative paths with `/`, sorted:
// the same directory always loads in the same order. Hidden entries are
// skipped (the config API's staging directories, the migration's backup,
// editor swap files, .git).
//
// Symlinks ARE followed: a Kubernetes ConfigMap mount is nothing but links
// into a hidden `..data` folder, and skipping them would load an empty
// configuration. The directory is the admins' to write either way; the depth
// cap is what keeps a link loop from walking forever.
pub fn config_files(dir: &Path) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    walk(dir, "", 0, &mut out)?;
    out.sort();
    Ok(out)
}

const MAX_CONFIG_DEPTH: usize = 16;

fn walk(dir: &Path, prefix: &str, depth: usize, out: &mut Vec<String>) -> Result<(), String> {
    if depth > MAX_CONFIG_DEPTH {
        return Err(format!(
            "{}: folders nested more than {MAX_CONFIG_DEPTH} deep (a symlink loop?)",
            dir.display()
        ));
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && prefix.is_empty() => {
            return Ok(());
        }
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let rel = format!("{prefix}{name}");
        // metadata, not file_type: it follows a link to what it points at.
        let Ok(meta) = fs::metadata(entry.path()) else {
            continue;
        };
        if meta.is_dir() {
            walk(&entry.path(), &format!("{rel}/"), depth + 1, out)?;
        } else if meta.is_file() && (name.ends_with(".yaml") || name.ends_with(".yml")) {
            out.push(rel);
        }
    }
    Ok(())
}

// An identity that is also safe as a path segment and in a URL: letters,
// digits, `-`, `_` and `.`, not starting with a dot. Item names are held to
// it because they appear in both (`/api/v1/sources/{id}`, the migration's
// `<section>/<name>.yaml`), and configuration paths are made of it.
pub fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('.')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_config_path_is_relative_safe_and_yaml() {
        assert!(is_config_path("config.yaml"));
        assert!(is_config_path("sources/acme/a-1_b.yml"));
        for bad in [
            "../x.yaml",
            "a/.hidden.yaml",
            "a//b.yaml",
            "x.json",
            "",
            "a b.yaml",
        ] {
            assert!(!is_config_path(bad), "{bad}");
        }
        assert!(!is_config_path(&format!("{}.yaml", "a".repeat(600))));
        assert!(!is_config_path(&format!("{}x.yaml", "d/".repeat(16))));
    }

    #[test]
    fn a_safe_id_is_a_url_and_path_segment() {
        assert!(is_safe_id("src-d42.prod_1"));
        assert!(!is_safe_id(""));
        assert!(!is_safe_id(".hidden"));
        assert!(!is_safe_id("a/b"));
    }

    #[test]
    fn every_yaml_file_at_any_depth_is_found_hidden_ones_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("config.yaml"), "").unwrap();
        fs::create_dir_all(root.join("sources/acme")).unwrap();
        fs::write(root.join("sources/acme/a.yml"), "").unwrap();
        fs::write(root.join("notes.txt"), "").unwrap();
        fs::write(root.join(".swap.yaml"), "").unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git/x.yaml"), "").unwrap();
        // A dangling link is skipped, not an error
        std::os::unix::fs::symlink(root.join("nowhere"), root.join("gone.yaml")).unwrap();

        assert_eq!(
            config_files(root).unwrap(),
            ["config.yaml", "sources/acme/a.yml"]
        );
    }

    #[test]
    fn a_missing_directory_is_empty_and_an_unreadable_one_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(config_files(&dir.path().join("absent")).unwrap().is_empty());

        // A file where a folder is expected, below the top: an error naming it
        let err = walk(&dir.path().join("absent"), "x/", 0, &mut Vec::new()).unwrap_err();
        assert!(err.contains("absent"), "{err}");
    }

    #[test]
    fn a_link_loop_stops_at_the_depth_cap() {
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(dir.path(), dir.path().join("loop")).unwrap();
        let err = config_files(dir.path()).unwrap_err();
        assert!(err.contains("nested more than"), "{err}");
    }
}
