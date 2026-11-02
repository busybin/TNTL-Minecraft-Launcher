//! Per-instance record of which mod builds have actually reached the game.
//!
//! The bisect tool uses it to test newly installed jars first: a jar that was
//! part of a successful launch is unlikely to be the one that just broke the
//! game. The file is small JSON and never leaves the instance directory.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::mods::ModInfo;

pub const HISTORY_FILE: &str = "launch-history.json";
/// Keep the last few launches for the UI; the proven map is the important bit.
pub const MAX_LAUNCHES: usize = 20;
/// A long-lived instance can accumulate years of replaced jars.
const MAX_PROVEN: usize = 2000;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LaunchHistory {
    /// Fingerprint -> the build that once reached the menu.
    #[serde(default)]
    pub proven: BTreeMap<String, ProvenMod>,
    /// Newest first.
    #[serde(default)]
    pub launches: Vec<LaunchRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvenMod {
    pub id: String,
    pub version: String,
    pub file: String,
    /// Unix seconds of the successful launch.
    pub at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchOutcome {
    Ok,
    Crash,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchRecord {
    pub at: u64,
    pub outcome: LaunchOutcome,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    #[serde(default)]
    pub summary: String,
}

/// Identity of one jar build. File name is deliberately absent: enabling or
/// disabling a jar renames it, and that must not invalidate a proof.
pub fn fingerprint(module: &ModInfo) -> String {
    format!(
        "{}|{}|{}|{}",
        module.id.to_ascii_lowercase(),
        module.version,
        module.size,
        module.modified
    )
}

pub fn history_path(game_dir: &Path) -> PathBuf {
    game_dir.join(HISTORY_FILE)
}

/// Missing or unreadable history is simply empty: it must never block a launch.
pub fn load(game_dir: &Path) -> LaunchHistory {
    let path = history_path(game_dir);
    let Ok(bytes) = fs::read(&path) else {
        return LaunchHistory::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

pub fn save(game_dir: &Path, history: &LaunchHistory) -> std::io::Result<()> {
    tntl_store::atomic_write_json(&history_path(game_dir), history)
        .map_err(|error| std::io::Error::other(error.to_string()))
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

impl LaunchHistory {
    pub fn is_proven(&self, module: &ModInfo) -> bool {
        self.proven.contains_key(&fingerprint(module))
    }

    /// Record a successful launch: every enabled jar in that launch is proven.
    /// `all_mods` (including disabled jars) is used to forget builds that are
    /// no longer on disk, so a reinstall has to prove itself again.
    pub fn record_success(&mut self, all_mods: &[ModInfo], duration_ms: u64, exit_code: Option<i32>) {
        let installed: BTreeSet<String> = all_mods.iter().map(fingerprint).collect();
        self.proven.retain(|key, _| installed.contains(key));
        let at = now_secs();
        for module in all_mods.iter().filter(|module| module.enabled) {
            self.proven.insert(
                fingerprint(module),
                ProvenMod {
                    id: module.id.clone(),
                    version: module.version.clone(),
                    file: module.file.clone(),
                    at,
                },
            );
        }
        self.trim_proven();
        self.push_record(LaunchRecord {
            at,
            outcome: LaunchOutcome::Ok,
            exit_code,
            duration_ms,
            summary: String::new(),
        });
    }

    pub fn record_failure(&mut self, duration_ms: u64, exit_code: Option<i32>, summary: impl Into<String>) {
        self.push_record(LaunchRecord {
            at: now_secs(),
            outcome: LaunchOutcome::Crash,
            exit_code,
            duration_ms,
            summary: summary.into(),
        });
    }

    pub fn record_unknown(&mut self, duration_ms: u64, exit_code: Option<i32>, summary: impl Into<String>) {
        self.push_record(LaunchRecord {
            at: now_secs(),
            outcome: LaunchOutcome::Unknown,
            exit_code,
            duration_ms,
            summary: summary.into(),
        });
    }

    fn push_record(&mut self, record: LaunchRecord) {
        self.launches.insert(0, record);
        self.launches.truncate(MAX_LAUNCHES);
    }

    fn trim_proven(&mut self) {
        while self.proven.len() > MAX_PROVEN {
            let oldest = self
                .proven
                .iter()
                .min_by_key(|(_, proven)| proven.at)
                .map(|(key, _)| key.clone());
            match oldest {
                Some(key) => {
                    self.proven.remove(&key);
                }
                None => break,
            }
        }
    }
}

/// The properties the bisect planner needs, decoupled from the full scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModCandidate {
    pub file: String,
    pub id: String,
    pub version: String,
    pub size: u64,
    pub modified: u64,
    pub enabled: bool,
    pub proven: bool,
    pub blocked: bool,
    pub dependencies: Vec<String>,
    pub provides: Vec<String>,
    pub nested: Vec<String>,
}

impl ModCandidate {
    pub fn from_info(module: &ModInfo, proven: bool) -> Self {
        Self {
            file: module.file.clone(),
            id: module.id.clone(),
            version: module.version.clone(),
            size: module.size,
            modified: module.modified,
            enabled: module.enabled,
            proven,
            blocked: module.blocked.is_some(),
            dependencies: module.dependency.clone(),
            provides: module.provides.clone(),
            nested: module.nested.clone(),
        }
    }

    /// Ids this jar makes available to other mods.
    pub fn supplied_ids(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.id.as_str())
            .chain(self.provides.iter().map(String::as_str))
            .chain(self.nested.iter().map(String::as_str))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mods::ModLoader;

    fn module(file: &str, id: &str, version: &str, enabled: bool, modified: u64) -> ModInfo {
        ModInfo {
            file: file.to_string(),
            id: id.to_string(),
            name: id.to_string(),
            version: version.to_string(),
            loader: ModLoader::Fabric,
            enabled,
            size: 100,
            modified,
            missing_dependencies: Vec::new(),
            disabled_dependencies: Vec::new(),
            dependency: Vec::new(),
            duplicate: false,
            recognized: true,
            provides: Vec::new(),
            nested: Vec::new(),
            game_versions: Vec::new(),
            loader_versions: Vec::new(),
            java_versions: Vec::new(),
            breaks: Vec::new(),
            break_ranges: BTreeMap::new(),
            dependency_ranges: BTreeMap::new(),
            fabric_dependency: Vec::new(),
            fabric_dependency_ranges: BTreeMap::new(),
            blocked: None,
            proven: false,
        }
    }

    #[test]
    fn success_proves_only_enabled_jars() {
        let mut history = LaunchHistory::default();
        let mods = vec![
            module("a.jar", "a", "1", true, 10),
            module("b.jar.disabled", "b", "1", false, 11),
        ];
        history.record_success(&mods, 1000, Some(0));
        assert!(history.proven.values().any(|proven| proven.id == "a"));
        assert!(!history.proven.values().any(|proven| proven.id == "b"));
        assert_eq!(history.launches.len(), 1);
        assert_eq!(history.launches[0].outcome, LaunchOutcome::Ok);
    }

    #[test]
    fn a_reinstalled_build_has_to_prove_itself_again() {
        let mut history = LaunchHistory::default();
        let old = vec![module("a.jar", "a", "1", true, 10)];
        history.record_success(&old, 1000, Some(0));
        // Same id and version, new mtime: the old proof is pruned.
        let new = vec![module("a.jar", "a", "1", true, 99)];
        history.record_failure(500, Some(1), "boom");
        let mut next = history.clone();
        next.record_success(&new, 1000, Some(0));
        assert_eq!(next.proven.len(), 1);
        assert!(next.proven.values().any(|proven| proven.at > 0));
    }

    #[test]
    fn renaming_a_jar_keeps_its_proof() {
        let history = {
            let mut history = LaunchHistory::default();
            history.record_success(&[module("a.jar", "a", "1", true, 10)], 1000, Some(0));
            history
        };
        let disabled = module("a.jar.disabled", "a", "1", false, 10);
        assert!(history.is_proven(&disabled));
    }
}
