//! Temp project fixture (replaces Go's `newCodeProject` / `newSkillProject` / `loadConfig` temp dirs).

use std::fs;

use iota::app::HostDirs;
use tempfile::TempDir;

/// Creates a temp project containing `files` (`(slash-relative path, contents)`, parents created) and returns it
/// with the `HostDirs` a test injects instead of touching the process environment: `cwd` = the temp root,
/// `home` = `<root>/home` (created), `cache` = `<root>/home/.cache` (created), `temp` = the process temp dir.
///
/// The `TempDir` must be kept alive for as long as the project is used.
pub fn temp_project(files: &[(&str, &str)]) -> (TempDir, HostDirs) {
    let dir = tempfile::tempdir().expect("create temp project");
    let root = dir.path().to_path_buf();
    for (rel, contents) in files {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create project subdirectory");
        }
        fs::write(&path, contents).expect("write project file");
    }
    let home = root.join("home");
    let cache = home.join(".cache");
    fs::create_dir_all(&cache).expect("create fixture home");
    let dirs = HostDirs {
        home: Some(home),
        cwd: Some(root),
        temp: std::env::temp_dir(),
        cache: Some(cache),
    };
    (dir, dirs)
}
