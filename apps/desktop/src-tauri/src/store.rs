//! Bootstrap state (which data directory is in use), the recent-directory list
//! and the classification of a directory the user just picked.
//!
//! Nothing here creates a data directory on its own: the launcher starts
//! without one and the first-run guide asks the user to pick or create it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const SCHEMA: u32 = 2;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Bootstrap {
    pub schema: u32,
    pub root: Option<String>,
    pub onboarded: bool,
    pub migrated_at: Option<u64>,
}

/// Where the pointer file lives. Local app data on Windows keeps it away from
/// the old `%APPDATA%\tntl` data directory; on Linux it is the config dir.
pub fn bootstrap_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let base = dirs::data_local_dir();
    #[cfg(not(windows))]
    let base = dirs::config_dir();
    base.map(|dir| dir.join("tntl"))
}

fn bootstrap_path() -> Option<PathBuf> {
    bootstrap_dir().map(|dir| dir.join("bootstrap.json"))
}

pub fn load_bootstrap() -> Bootstrap {
    let Some(path) = bootstrap_path() else {
        return Bootstrap::default();
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return Bootstrap::default();
    };
    let mut bootstrap: Bootstrap = serde_json::from_slice(&bytes).unwrap_or_default();
    if bootstrap.schema == 0 {
        bootstrap.schema = SCHEMA;
    }
    bootstrap
}

pub fn save_bootstrap(bootstrap: &Bootstrap) -> Result<(), String> {
    let path = bootstrap_path().ok_or_else(|| "no config directory available".to_string())?;
    tntl_store::atomic_write_json(&path, bootstrap).map_err(|error| error.to_string())
}

/// The directory the user picked, if it still exists.
pub fn configured_root() -> Option<PathBuf> {
    load_bootstrap()
        .root
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .map(|path| tntl_store::simplify_path(&path))
}

/// Placeholder used while no data directory is chosen. Nothing is written there
/// unless a command slips past the first-run guide.
pub fn unconfigured_root() -> PathBuf {
    bootstrap_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("unconfigured")
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RootConfig {
    pub schema: u32,
    pub roots: Vec<String>,
    pub current: Option<String>,
}

fn roots_path() -> Option<PathBuf> {
    bootstrap_dir().map(|dir| dir.join("roots.json"))
}

/// Older builds kept the list in the data directory / `%APPDATA%\tntl`.
fn legacy_roots_paths(root: Option<&Path>) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(dir) = dirs::config_dir() {
        paths.push(dir.join("tntl").join("roots.json"));
    }
    if let Some(root) = root {
        paths.push(root.join("roots.json"));
    }
    paths
}

pub fn load_roots() -> RootConfig {
    let Some(path) = roots_path() else {
        return RootConfig::default();
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return RootConfig::default();
    };
    let mut config: RootConfig = serde_json::from_slice(&bytes).unwrap_or_default();
    config.schema = SCHEMA;
    config.roots = config
        .roots
        .iter()
        .map(|entry| tntl_store::simplify_path(Path::new(entry)).display().to_string())
        .collect();
    config
}

pub fn save_roots(config: &RootConfig) -> Result<(), String> {
    let path = roots_path().ok_or_else(|| "no config directory available".to_string())?;
    tntl_store::atomic_write_json(&path, config).map_err(|error| error.to_string())
}

/// Save a data directory at the front of the list and make it current.
pub fn remember_root(root: &Path) -> Result<RootConfig, String> {
    let text = tntl_store::simplify_path(root).display().to_string();
    let mut config = load_roots();
    config.schema = SCHEMA;
    config.roots.retain(|entry| entry != &text);
    config.roots.insert(0, text.clone());
    config.current = Some(text);
    save_roots(&config)?;
    Ok(config)
}

/// Drop one entry. Files are never touched.
pub fn forget_root(path: &str) -> Result<RootConfig, String> {
    let target = tntl_store::simplify_path(Path::new(path)).display().to_string();
    let mut config = load_roots();
    config.roots.retain(|entry| entry != &target);
    if config.current.as_deref() == Some(target.as_str()) {
        config.current = None;
    }
    save_roots(&config)?;
    Ok(config)
}

#[derive(Debug, Clone, Serialize)]
pub struct PathInfo {
    pub path: String,
    /// `data`, `game`, `version`, `prism`, `mods`, `empty` or `foreign`.
    pub kind: String,
    pub instances: usize,
    /// Version ids found in a game/version directory.
    pub versions: Vec<String>,
    pub writable: bool,
}

impl PathInfo {
    pub fn is_data(&self) -> bool {
        self.kind == "data"
    }
}

/// What a picked directory is. Order matters: our own marker wins over
/// `versions/`, because a launcher data directory has one too.
pub fn classify(path: &Path) -> PathInfo {
    let path = tntl_store::simplify_path(&path.canonicalize().unwrap_or_else(|_| path.to_path_buf()));
    let text = path.display().to_string();
    let mut info = PathInfo {
        path: text,
        kind: "foreign".to_string(),
        instances: 0,
        versions: Vec::new(),
        writable: writable(&path),
    };
    if !path.exists() {
        info.kind = "empty".to_string();
        return info;
    }
    if !path.is_dir() {
        info.kind = "foreign".to_string();
        return info;
    }
    info.instances = count_instances(&path);
    let has = |name: &str| path.join(name).exists();
    if has(tntl_app::ROOT_MARKER) || info.instances > 0 {
        info.kind = "data".to_string();
        return info;
    }
    if has("mmc-pack.json") || has_prism_instances(&path) {
        info.kind = "prism".to_string();
        return info;
    }
    if has("versions") {
        info.kind = "game".to_string();
        info.versions = version_ids(&path);
        return info;
    }
    // `…/versions/<id>` picked directly.
    let is_version = path
        .parent()
        .and_then(|parent| parent.file_name())
        .is_some_and(|name| name == "versions")
        && path
            .file_name()
            .map(|name| path.join(format!("{}.json", name.to_string_lossy())).is_file())
            .unwrap_or(false);
    if is_version {
        info.kind = "version".to_string();
        if let Some(name) = path.file_name() {
            info.versions = vec![name.to_string_lossy().to_string()];
        }
        return info;
    }
    if has("mods") || has("saves") || has("config") {
        info.kind = "mods".to_string();
        return info;
    }
    if is_empty(&path) {
        info.kind = "empty".to_string();
    }
    info
}

fn count_instances(root: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(root.join("instances")) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| entry.path().join(tntl_instance::INSTANCE_FILE).is_file())
        .count()
}

fn has_prism_instances(root: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(root.join("instances")) else {
        return false;
    };
    entries
        .flatten()
        .any(|entry| entry.path().join("mmc-pack.json").is_file())
}

fn version_ids(root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root.join("versions")) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|id| root.join("versions").join(id).join(format!("{id}.json")).is_file())
        .collect();
    ids.sort();
    ids
}

fn is_empty(path: &Path) -> bool {
    std::fs::read_dir(path)
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(false)
}

fn writable(path: &Path) -> bool {
    if !path.exists() {
        // A directory that does not exist yet is only usable if its parent is.
        return path
            .parent()
            .map(|parent| parent.is_dir() && writable(parent))
            .unwrap_or(false);
    }
    let probe = path.join(".tntl-write-probe");
    let ok = std::fs::write(&probe, b"ok").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

#[derive(Debug, Clone, Serialize)]
pub struct MigrationReport {
    pub root: String,
    pub instances: usize,
    pub suggested: Vec<String>,
    pub dropped: Vec<String>,
    pub backup: String,
    pub settings_reset: bool,
}

/// Old data directory that predates the bootstrap file.
pub fn legacy_root() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(dir) = dirs::data_dir() {
        candidates.push(dir.join("tntl"));
    }
    #[cfg(windows)]
    if let Some(dir) = dirs::data_local_dir() {
        candidates.push(dir.join("tntl"));
    }
    candidates
        .into_iter()
        .find(|path| {
            path.is_dir()
                && (path.join("launcher.json").is_file()
                    || path.join("instances").is_dir()
                    || path.join(tntl_app::ROOT_MARKER).is_file())
        })
}

/// One-time clean-up of the settings older builds left behind. Instances are
/// never removed: only settings, and only after a backup.
pub fn migrate_legacy(root: &Path) -> Result<MigrationReport, String> {
    let root = tntl_store::simplify_path(root);
    let launcher = tntl_app::Launcher::from_root(Some(root.clone()));
    if !root.is_dir() {
        return Err(format!("{} does not exist", root.display()));
    }
    launcher
        .mark_root()
        .map_err(|error| format!("cannot mark {}: {error:#}", root.display()))?;

    // Back up before touching anything.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    let backup = bootstrap_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(format!("legacy-{stamp}"));
    std::fs::create_dir_all(&backup).map_err(|error| error.to_string())?;
    let mut copied: Vec<String> = Vec::new();
    for file in ["launcher.json", "accounts.json", "roots.json"] {
        let source = root.join(file);
        if source.is_file() && std::fs::copy(&source, backup.join(file)).is_ok() {
            copied.push(file.to_string());
        }
    }
    for path in legacy_roots_paths(Some(&root)) {
        if path.is_file() {
            let name = format!("legacy-roots-{}.json", copied.len());
            let _ = std::fs::copy(&path, backup.join(&name));
        }
    }

    // Rebuild the directory list: only real data directories stay as roots,
    // game directories become import suggestions.
    let mut config = RootConfig {
        schema: SCHEMA,
        roots: Vec::new(),
        current: None,
    };
    let mut suggested: Vec<String> = Vec::new();
    let mut dropped: Vec<String> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut candidates: Vec<String> = load_roots().roots;
    for path in legacy_roots_paths(Some(&root)) {
        if let Ok(bytes) = std::fs::read(&path)
            && let Ok(old) = serde_json::from_slice::<RootConfig>(&bytes)
        {
            candidates.extend(old.roots);
        }
    }
    candidates.push(root.display().to_string());
    for entry in candidates {
        let path = tntl_store::simplify_path(Path::new(&entry));
        if !path.exists() {
            dropped.push(entry);
            continue;
        }
        let info = classify(&path);
        let text = info.path.clone();
        if seen.contains(&text) {
            continue;
        }
        seen.push(text.clone());
        match info.kind.as_str() {
            "data" => config.roots.push(text),
            "game" | "version" | "prism" | "mods" => suggested.push(text),
            _ => dropped.push(text),
        }
    }
    let root_text = root.display().to_string();
    if !config.roots.contains(&root_text) {
        config.roots.insert(0, root_text.clone());
    }
    config.current = Some(root_text.clone());
    save_roots(&config)?;

    // Settings: keep the theme (appearance), reset the rest. Language lives in
    // the frontend's own storage and survives on its own.
    let previous = launcher.settings();
    let settings = tntl_app::LauncherSettings {
        theme: previous.theme,
        onboarded: true,
        ..Default::default()
    };
    let settings_reset = launcher.save_settings(&settings).is_ok();

    let instances = count_instances(&root);
    let bootstrap = Bootstrap {
        schema: SCHEMA,
        root: Some(root_text.clone()),
        onboarded: true,
        migrated_at: Some(stamp),
    };
    save_bootstrap(&bootstrap)?;
    Ok(MigrationReport {
        root: root_text,
        instances,
        suggested,
        dropped,
        backup: backup.display().to_string(),
        settings_reset,
    })
}
