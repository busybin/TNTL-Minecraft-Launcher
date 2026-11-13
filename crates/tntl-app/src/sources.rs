//! Game directories the user added, and the instances found inside them.
//!
//! The library used to be a flat `instances/` folder owned by the launcher, and
//! every instance recorded absolute paths to its game data and to the version
//! chain. That is what let an instance point at a disk that was not mounted and
//! report "does not exist" for a folder that was in fact fine.
//!
//! Here a game directory is registered once, the instances inside it are
//! discovered from the filesystem, and each instance keeps its settings beside
//! its own game files (`tntl-instance.json`). Nothing about where things live
//! is stored, so moving the folder or re-attaching the disk keeps working.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::Launcher;

/// Name of the file listing the game directories the user added.
pub const SOURCES_FILE: &str = "sources.json";

/// How deep below a game directory an instance may sit. `versions/<id>` is two
/// levels; packs and hand-made layouts add one or two more. Deeper than this is
/// a directory that happens to contain game folders, not a game directory.
const MAX_DEPTH: usize = 4;

/// Directory names that never hold an instance and can be slow or huge to walk.
const SKIP_DIRS: &[&str] = &[
    ".git",
    ".svn",
    ".hg",
    "node_modules",
    "target",
    "__pycache__",
    "assets",
    "libraries",
    "natives",
    "logs",
    "crash-reports",
    "screenshots",
    "saves",
    "mods",
    "config",
    "resourcepacks",
    "shaderpacks",
    "datapacks",
    "defaultconfigs",
    "downloads",
    "backups",
];

/// Persisted list of game directories.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GameSources {
    #[serde(default)]
    pub paths: Vec<PathBuf>,
}

impl GameSources {
    pub fn load(launcher_root: &Path) -> Self {
        let path = launcher_root.join(SOURCES_FILE);
        fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, launcher_root: &Path) -> Result<()> {
        let path = launcher_root.join(SOURCES_FILE);
        tntl_store::atomic_write_json(&path, self).with_context(|| format!("cannot write {}", path.display()))
    }

    /// Add a directory. Compared by resolved path so the same folder spelled
    /// with a trailing slash or a symlink is not added twice.
    pub fn add(&mut self, path: &Path) -> Result<bool> {
        let resolved = resolve(path);
        if !resolved.is_dir() {
            bail!("{} is not a directory", resolved.display());
        }
        if self.paths.iter().any(|known| resolve(known) == resolved) {
            return Ok(false);
        }
        self.paths.push(resolved);
        self.paths.sort();
        Ok(true)
    }

    pub fn remove(&mut self, path: &Path) -> bool {
        let resolved = resolve(path);
        let before = self.paths.len();
        self.paths.retain(|known| resolve(known) != resolved);
        self.paths.len() != before
    }
}

/// One discovered instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovered {
    /// Directory holding the instance: the game directory for a portable
    /// instance, the library entry for a legacy one.
    pub dir: PathBuf,
    /// Game directory the user added that this instance was found under.
    pub source: PathBuf,
}

/// Every instance under every registered game directory, plus the legacy
/// `instances/` folder so an upgrade does not hide the existing library.
///
/// Missing sources are skipped rather than reported: an external disk that is
/// not mounted right now must not turn into an error, and the caller decides
/// how to show that.
pub fn discover(launcher: &Launcher) -> Vec<Discovered> {
    let mut out = Vec::new();
    for source in GameSources::load(&launcher.root).paths {
        out.extend(scan_source(&source));
    }
    for dir in legacy_instance_dirs(&launcher.root) {
        out.push(Discovered {
            dir,
            source: launcher.root.clone(),
        });
    }
    out.sort_by(|left, right| left.dir.cmp(&right.dir));
    out.dedup_by(|left, right| left.dir == right.dir);
    out
}

/// Entries of the pre-portable `instances/` folder.
fn legacy_instance_dirs(launcher_root: &Path) -> Vec<PathBuf> {
    let root = launcher_root.join("instances");
    let Ok(entries) = fs::read_dir(&root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.join(tntl_instance::INSTANCE_FILE).is_file() || path.join(tntl_instance::PORTABLE_FILE).is_file()
        })
        .collect()
}

/// Instances inside one game directory.
pub fn scan_source(source: &Path) -> Vec<Discovered> {
    let mut out = Vec::new();
    if !source.is_dir() {
        return out;
    }
    walk(source, source, 0, &mut out);
    out.sort_by(|left, right| left.dir.cmp(&right.dir));
    out.dedup_by(|left, right| left.dir == right.dir);
    out
}

fn walk(dir: &Path, source: &Path, depth: usize, out: &mut Vec<Discovered>) {
    // A folder holding `versions/` is a Minecraft installation, not an
    // instance: the game directory there is `.minecraft` itself and it carries
    // `options.txt` and `logs/` just like an instance does.
    let versions = dir.join("versions");
    if versions.is_dir() {
        // Versions placed in place by another launcher keep their game data in
        // `versions/<id>/`; instances this launcher creates use
        // `instances/<name>/` so game data is never mixed with version
        // definitions. Both are discovered.
        for container in [versions, dir.join("instances")] {
            let Ok(entries) = fs::read_dir(&container) else {
                continue;
            };
            for entry in entries.flatten() {
                let candidate = entry.path();
                if candidate.is_dir() && holds_game_data(&candidate) {
                    out.push(Discovered {
                        dir: candidate,
                        source: source.to_path_buf(),
                    });
                }
            }
        }
        return;
    }
    // Instances created inside a game directory live in `instances/`, even when
    // the directory itself is a game directory: without this pass a source with
    // `options.txt` (a Prism instance, an isolated game folder) hid everything
    // the launcher created there, and the version guard could not see it either.
    let container = dir.join("instances");
    if let Ok(entries) = fs::read_dir(&container) {
        for entry in entries.flatten() {
            let candidate = entry.path();
            if candidate.is_dir() && holds_game_data(&candidate) {
                out.push(Discovered {
                    dir: candidate,
                    source: source.to_path_buf(),
                });
            }
        }
    }
    // A game-directory source is an instance itself too (an isolated game
    // folder, a Prism instance); its children are not walked further.
    if holds_game_data(dir) {
        out.push(Discovered {
            dir: dir.to_path_buf(),
            source: source.to_path_buf(),
        });
        return;
    }
    if depth >= MAX_DEPTH {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut children: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .filter(|path| {
            let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
            !name.starts_with('.') && !SKIP_DIRS.contains(&name)
        })
        .collect();
    children.sort();
    for child in children {
        walk(&child, source, depth + 1, out);
    }
}

/// Whether `dir` looks like a directory the game has actually run in.
///
/// A version folder that only holds `<id>.jar` and `<id>.json` is a version, not
/// an instance: vanilla and loader profiles both look like that, and listing
/// them would fill the library with entries the user never created.
pub fn holds_game_data(dir: &Path) -> bool {
    if dir.join(tntl_instance::PORTABLE_FILE).is_file() {
        return true;
    }
    if dir.join("instance.cfg").is_file() || dir.join("mmc-pack.json").is_file() {
        return true;
    }
    if dir.join("options.txt").is_file() {
        return true;
    }
    for nested in ["saves", "config", "logs"] {
        if dir.join(nested).is_dir() {
            return true;
        }
    }
    non_empty_dir(&dir.join("mods"))
}

fn non_empty_dir(path: &Path) -> bool {
    fs::read_dir(path).is_ok_and(|mut entries| entries.next().is_some())
}

fn resolve(path: &Path) -> PathBuf {
    tntl_store::simplify_path(&path.canonicalize().unwrap_or_else(|_| path.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(root: &Path, relative: &str) -> PathBuf {
        let path = root.join(relative);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn file(root: &Path, relative: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"x").unwrap();
    }

    #[test]
    fn a_versions_folder_yields_the_folders_with_game_data_only() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        // Vanilla and loader profiles: jar and json only.
        file(root, "versions/1.21.1/1.21.1.json");
        file(root, "versions/1.21.1/1.21.1.jar");
        file(root, "versions/neoforge-21.1.265/neoforge-21.1.265.json");
        // A real instance and a mod-only game directory.
        file(root, "versions/1.21.1-NeoForge/1.21.1-NeoForge.json");
        file(root, "versions/1.21.1-NeoForge/options.txt");
        dir(root, "versions/1.21.1-NeoForge/saves");
        file(root, "versions/1.21.1-neoforge/mods/example.jar");
        // The installation root itself looks like an instance (`options.txt`),
        // which must not make it one.
        file(root, "options.txt");

        let found: Vec<String> = scan_source(root)
            .into_iter()
            .map(|entry| entry.dir.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(found, vec!["1.21.1-NeoForge", "1.21.1-neoforge"]);
    }

    #[test]
    fn an_empty_version_folder_is_not_an_instance() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        file(root, "versions/1.21.1/1.21.1.json");
        dir(root, "versions/1.21.1/mods");
        assert!(scan_source(root).is_empty());
    }

    #[test]
    fn a_flat_game_directory_is_an_instance() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        file(root, "mods/example.jar");
        let found = scan_source(root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].dir, root);
    }

    #[test]
    fn a_prism_instance_folder_is_found_one_level_down() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        file(root, "pack/instance.cfg");
        file(root, "pack/minecraft/options.txt");
        let found = scan_source(root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].dir, root.join("pack"));
    }

    #[test]
    fn instances_created_by_the_launcher_are_found_beside_versions() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        file(root, "versions/1.21.1/1.21.1.json");
        file(root, "instances/Big Pack/options.txt");
        file(root, "instances/Big Pack/mods/example.jar");
        let found: Vec<String> = scan_source(root)
            .into_iter()
            .map(|entry| entry.dir.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(found, vec!["Big Pack"]);
    }

    #[test]
    fn adding_the_same_directory_twice_is_a_no_op() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let mut sources = GameSources::default();
        assert!(sources.add(root).unwrap());
        assert!(!sources.add(root).unwrap());
        assert_eq!(sources.paths.len(), 1);
        assert!(sources.remove(root));
        assert!(sources.paths.is_empty());
    }

    #[test]
    fn a_missing_source_produces_nothing_instead_of_an_error() {
        let temp = tempfile::tempdir().unwrap();
        assert!(scan_source(&temp.path().join("not-mounted")).is_empty());
    }
}
