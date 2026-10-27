//! FTB modpacks.
//!
//! The FTB App publishes its packs through `api.modpacks.ch`. A pack lists
//! versions, and a version lists every file with a direct download URL, a SHA-1
//! and a size, so a client install is a plain file download plus the loader the
//! version asks for. Nothing here talks to any launcher-specific service.

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use serde::Deserialize;
use thiserror::Error;
use tntl_modpack::{PackEvent, PackLoader, PackReport};
use tntl_source::{DownloadOutcome, DownloadRequest, Http, SourceError, endpoints};
use tntl_store::safe_relative;

#[derive(Debug, Error)]
pub enum FtbError {
    #[error(transparent)]
    Source(#[from] SourceError),
    #[error("{0}")]
    Invalid(String),
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Loader a pack version targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoaderTarget {
    pub kind: PackLoader,
    /// Build as the installer expects it, e.g. `1.20.1-47.4.28`.
    pub version: String,
}

/// One published version of an FTB pack.
#[derive(Debug, Clone)]
pub struct VersionSummary {
    pub id: u64,
    pub name: String,
    /// `release`, `beta` or `alpha`.
    pub kind: String,
    /// Unix timestamp of the last update.
    pub updated: i64,
    pub game_version: String,
    pub loader: Option<LoaderTarget>,
    /// Java build the pack asks for, informational only.
    pub java: Option<String>,
    pub recommended_memory_mb: Option<u32>,
}

impl VersionSummary {
    pub fn loader_key(&self) -> Option<String> {
        self.loader.as_ref().map(|loader| loader.kind.key().to_string())
    }
}

/// Pack as shown in a browse list.
#[derive(Debug, Clone)]
pub struct PackSummary {
    pub id: u64,
    pub name: String,
    pub synopsis: String,
    pub description: String,
    pub icon_url: Option<String>,
    pub installs: u64,
    pub plays: u64,
    pub tags: Vec<String>,
    pub latest: Option<VersionSummary>,
    pub versions: Vec<VersionSummary>,
}

impl PackSummary {
    pub fn version_count(&self) -> usize {
        self.versions.len()
    }
}

/// One downloadable file of a pack version.
#[derive(Debug, Clone)]
pub struct PackFile {
    /// Path inside the game directory, already joined from the API's
    /// directory and file name.
    pub path: String,
    pub url: String,
    pub sha1: Option<String>,
    pub size: u64,
    /// `mod`, `config`, `resource` or `script`.
    pub kind: String,
    pub client_only: bool,
    pub server_only: bool,
    pub optional: bool,
}

/// Full pack version: targets plus every file.
#[derive(Debug, Clone)]
pub struct PackVersion {
    pub id: u64,
    pub name: String,
    pub changelog: String,
    pub kind: String,
    pub updated: i64,
    pub game_version: String,
    pub loader: Option<LoaderTarget>,
    pub java: Option<String>,
    pub recommended_memory_mb: Option<u32>,
    pub files: Vec<PackFile>,
}

impl PackVersion {
    /// Loader key (`fabric`, `forge`, …) when the version has a loader.
    pub fn loader_key(&self) -> Option<String> {
        self.loader.as_ref().map(|loader| loader.kind.key().to_string())
    }

    /// Files a client needs: everything not marked server only.
    pub fn client_files(&self) -> impl Iterator<Item = &PackFile> {
        self.files.iter().filter(|file| !file.server_only)
    }
}

/// Client files without duplicate destinations. The API sometimes lists the
/// same path twice, and two workers downloading to one file would clobber each
/// other.
fn unique_client_files(version: &PackVersion) -> Vec<&PackFile> {
    let mut seen = HashSet::new();
    version
        .client_files()
        .filter(|file| seen.insert(file.path.trim_start_matches("./").to_string()))
        .collect()
}

/// Pack with its version history, newest last.
#[derive(Debug, Clone)]
pub struct Pack {
    pub id: u64,
    pub name: String,
    pub synopsis: String,
    pub description: String,
    pub icon_url: Option<String>,
    pub installs: u64,
    pub plays: u64,
    pub tags: Vec<String>,
    pub versions: Vec<VersionSummary>,
}

impl Pack {
    pub fn latest(&self) -> Option<&VersionSummary> {
        self.versions.last()
    }

    /// Newer versions than the given one, oldest first.
    pub fn versions_after(&self, version: u64) -> Vec<&VersionSummary> {
        let Some(index) = self.versions.iter().position(|entry| entry.id == version) else {
            return Vec::new();
        };
        self.versions[index + 1..].iter().collect()
    }
}

pub struct Ftb {
    http: Http,
}

impl Ftb {
    pub fn new(http: &Http) -> Self {
        Self { http: http.clone() }
    }

    /// Search the pack catalogue. An empty query lists the newest packs.
    pub fn search(&self, query: &str, limit: u32) -> Result<Vec<PackSummary>, FtbError> {
        let limit = limit.clamp(1, 50) as usize;
        let ids: Vec<u64> = match query.trim() {
            "" => {
                let list: IdList = self.http.get_json(&format!("{}/modpack/all", endpoints::FTB_API))?;
                list.packs.into_iter().take(limit).collect()
            }
            term => {
                let encoded: String = term
                    .bytes()
                    .map(|byte| match byte {
                        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                            (byte as char).to_string()
                        }
                        b' ' => "+".to_string(),
                        other => format!("%{other:02X}"),
                    })
                    .collect();
                let list: IdList = self
                    .http
                    .get_json(&format!("{}/modpack/search/{limit}?term={encoded}", endpoints::FTB_API))?;
                list.packs.into_iter().take(limit).collect()
            }
        };
        self.summaries(&ids)
    }

    /// Fetch a pack with its version history.
    pub fn pack(&self, id: u64) -> Result<Pack, FtbError> {
        let raw: RawPack = self.http.get_json(&format!("{}/modpack/{id}", endpoints::FTB_API))?;
        Ok(pack_from_raw(raw))
    }

    /// Fetch one version with its file list.
    pub fn version(&self, id: u64, version: u64) -> Result<PackVersion, FtbError> {
        let raw: RawVersionFull = self
            .http
            .get_json(&format!("{}/modpack/{id}/{version}", endpoints::FTB_API))?;
        let game = raw
            .targets
            .iter()
            .find(|target| target.name == "minecraft")
            .map(|target| target.version.clone())
            .ok_or_else(|| FtbError::Invalid(format!("FTB version {version} names no Minecraft version")))?;
        let loader = loader_target(&raw.targets, &game);
        Ok(PackVersion {
            id: raw.id,
            name: raw.name,
            changelog: raw.changelog,
            kind: raw.kind,
            updated: raw.updated,
            game_version: game,
            loader,
            java: raw
                .targets
                .iter()
                .find(|target| target.name == "java")
                .map(|target| target.version.clone()),
            recommended_memory_mb: raw
                .specs
                .as_ref()
                .map(|specs| specs.recommended.min(u32::MAX as u64) as u32),
            files: raw.files.into_iter().filter_map(file_from_raw).collect(),
        })
    }

    /// Download every file of the version into `game_dir`. Files that already
    /// pass their SHA-1 are skipped, so re-running is cheap.
    pub fn install(
        &self,
        version: &PackVersion,
        game_dir: &Path,
        concurrency: usize,
        progress: &(dyn Fn(PackEvent) + Send + Sync),
    ) -> Result<PackReport, FtbError> {
        fs::create_dir_all(game_dir).map_err(|source| FtbError::Io {
            path: game_dir.to_path_buf(),
            source,
        })?;
        let files = unique_client_files(version);
        let files_total = files.len();
        let bytes_total: u64 = files.iter().map(|file| file.size).sum();
        let workers = concurrency.clamp(1, 32).min(files.len().max(1));
        let next = AtomicUsize::new(0);
        let downloaded = AtomicUsize::new(0);
        let skipped = AtomicUsize::new(0);
        let bytes = AtomicUsize::new(0);
        let files_done = AtomicUsize::new(0);
        let bytes_done = AtomicU64::new(0);
        // Per-file progress of the downloads still running, so the pack bar
        // moves smoothly while several files arrive at once.
        let inflight: Mutex<HashMap<String, u64>> = Mutex::new(HashMap::new());
        let failure: Mutex<Option<FtbError>> = Mutex::new(None);

        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| {
                    loop {
                        if failure.lock().unwrap_or_else(|poison| poison.into_inner()).is_some() {
                            break;
                        }
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(file) = files.get(index) else {
                            break;
                        };
                        let Some(relative) = safe_relative(&file.path) else {
                            *failure.lock().unwrap_or_else(|poison| poison.into_inner()) =
                                Some(FtbError::Invalid(format!("unsafe file path: {}", file.path)));
                            break;
                        };
                        let dest = tntl_store::target_respecting_disabled(&game_dir.join(&relative));
                        if !dest.starts_with(game_dir) {
                            *failure.lock().unwrap_or_else(|poison| poison.into_inner()) =
                                Some(FtbError::Invalid(format!("unsafe file path: {}", file.path)));
                            break;
                        }
                        let mut request = DownloadRequest::new(&file.url, &dest);
                        if let Some(sha1) = &file.sha1 {
                            request = request.with_sha1(sha1.clone());
                        }
                        if file.size > 0 {
                            request = request.with_size(file.size);
                        }
                        let total = (file.size > 0).then_some(file.size);
                        let mut last_done = 0u64;
                        let result = {
                            let mut sink = |done: u64| {
                                if done == 0 || done.saturating_sub(last_done) >= 256 * 1024 {
                                    last_done = done;
                                    let overall = {
                                        let mut map = inflight.lock().unwrap_or_else(|poison| poison.into_inner());
                                        map.insert(file.path.clone(), done.min(file.size));
                                        bytes_done.load(Ordering::Relaxed) + map.values().copied().sum::<u64>()
                                    };
                                    progress(PackEvent::Downloading {
                                        path: file.path.clone(),
                                        done,
                                        total,
                                        files_done: files_done.load(Ordering::Relaxed),
                                        files_total,
                                        bytes_done: overall.min(bytes_total),
                                        bytes_total,
                                    });
                                }
                            };
                            self.http.download(&request, &mut sink)
                        };
                        if result.is_ok() {
                            inflight
                                .lock()
                                .unwrap_or_else(|poison| poison.into_inner())
                                .remove(&file.path);
                            bytes_done.fetch_add(file.size, Ordering::Relaxed);
                            files_done.fetch_add(1, Ordering::Relaxed);
                        }
                        match result {
                            Ok(DownloadOutcome::AlreadyValid) => {
                                skipped.fetch_add(1, Ordering::Relaxed);
                                progress(PackEvent::Skipped {
                                    path: file.path.clone(),
                                });
                            }
                            Ok(DownloadOutcome::Downloaded) => {
                                downloaded.fetch_add(1, Ordering::Relaxed);
                                bytes.fetch_add(file.size as usize, Ordering::Relaxed);
                                progress(PackEvent::Installed {
                                    path: file.path.clone(),
                                    bytes: file.size,
                                });
                            }
                            Err(error) => {
                                *failure.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(error.into());
                                break;
                            }
                        }
                    }
                });
            }
        });
        if let Some(error) = failure.into_inner().unwrap_or_else(|poison| poison.into_inner()) {
            return Err(error);
        }
        Ok(PackReport {
            files: files.len(),
            downloaded: downloaded.into_inner(),
            skipped: skipped.into_inner(),
            bytes: bytes.into_inner() as u64,
            overrides: 0,
        })
    }

    fn summaries(&self, ids: &[u64]) -> Result<Vec<PackSummary>, FtbError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let workers = ids.len().min(8);
        let fetched: Vec<Result<Vec<(u64, PackSummary)>, FtbError>> = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for worker in 0..workers {
                let http = self.http.clone();
                let chunk: Vec<u64> = ids.iter().skip(worker).step_by(workers).copied().collect();
                handles.push(scope.spawn(move || {
                    chunk
                        .into_iter()
                        .map(|id| {
                            let raw: RawPack = http.get_json(&format!("{}/modpack/{id}", endpoints::FTB_API))?;
                            Ok((id, summary_from_raw(raw)))
                        })
                        .collect()
                }));
            }
            handles
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .unwrap_or_else(|_| Err(FtbError::Invalid("pack lookup panicked".to_string())))
                })
                .collect()
        });
        let mut by_id = HashMap::new();
        for chunk in fetched {
            for (id, summary) in chunk? {
                by_id.insert(id, summary);
            }
        }
        Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
    }
}

fn loader_target(targets: &[RawTarget], game: &str) -> Option<LoaderTarget> {
    targets.iter().find_map(|target| {
        let kind = PackLoader::from_key(&target.name)?;
        Some(LoaderTarget {
            kind,
            version: kind.install_version(game, &target.version),
        })
    })
}

fn summary_from_raw(raw: RawPack) -> PackSummary {
    let pack = pack_from_raw(raw);
    let latest = pack.latest().cloned();
    PackSummary {
        id: pack.id,
        name: pack.name,
        synopsis: pack.synopsis,
        description: pack.description,
        icon_url: pack.icon_url,
        installs: pack.installs,
        plays: pack.plays,
        tags: pack.tags,
        latest,
        versions: pack.versions,
    }
}

fn pack_from_raw(raw: RawPack) -> Pack {
    let icon_url = raw
        .art
        .iter()
        .find(|art| art.kind == "square")
        .or_else(|| raw.art.first())
        .map(|art| art.url.clone())
        .filter(|url| !url.is_empty());
    let game = raw
        .versions
        .iter()
        .rev()
        .find_map(|version| {
            version
                .targets
                .iter()
                .find(|target| target.name == "minecraft")
                .map(|target| target.version.clone())
        })
        .unwrap_or_default();
    let versions = raw
        .versions
        .into_iter()
        .map(|version| {
            let game_version = version
                .targets
                .iter()
                .find(|target| target.name == "minecraft")
                .map(|target| target.version.clone())
                .unwrap_or_else(|| game.clone());
            VersionSummary {
                id: version.id,
                name: version.name,
                kind: version.kind,
                updated: version.updated,
                loader: loader_target(&version.targets, &game_version),
                java: version
                    .targets
                    .iter()
                    .find(|target| target.name == "java")
                    .map(|target| target.version.clone()),
                recommended_memory_mb: version
                    .specs
                    .as_ref()
                    .map(|specs| specs.recommended.min(u32::MAX as u64) as u32),
                game_version,
            }
        })
        .collect();
    Pack {
        id: raw.id,
        name: raw.name,
        synopsis: raw.synopsis,
        description: raw.description,
        icon_url,
        installs: raw.installs,
        plays: raw.plays,
        tags: raw.tags.into_iter().map(|tag| tag.name).collect(),
        versions,
    }
}

fn file_from_raw(raw: RawFile) -> Option<PackFile> {
    let directory = raw.path.trim();
    let directory = directory.strip_prefix("./").unwrap_or(directory);
    let directory = directory.trim_end_matches('/');
    let path = if directory.is_empty() || directory == "." {
        raw.name.clone()
    } else {
        format!("{directory}/{}", raw.name)
    };
    if raw.url.is_empty() || raw.sha1.as_deref().unwrap_or_default().is_empty() {
        return None;
    }
    Some(PackFile {
        path,
        url: raw.url,
        sha1: raw.sha1,
        size: raw.size,
        kind: raw.kind,
        client_only: raw.clientonly,
        server_only: raw.serveronly,
        optional: raw.optional,
    })
}

#[derive(Debug, Deserialize)]
struct IdList {
    #[serde(default)]
    packs: Vec<u64>,
}

#[derive(Debug, Default, Deserialize)]
struct RawPack {
    id: u64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    synopsis: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    art: Vec<RawArt>,
    #[serde(default)]
    installs: u64,
    #[serde(default)]
    plays: u64,
    #[serde(default)]
    tags: Vec<RawTag>,
    #[serde(default)]
    versions: Vec<RawVersion>,
}

#[derive(Debug, Default, Deserialize)]
struct RawArt {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    url: String,
}

#[derive(Debug, Default, Deserialize)]
struct RawTag {
    #[serde(default)]
    name: String,
}

#[derive(Debug, Default, Deserialize)]
struct RawVersion {
    id: u64,
    #[serde(default)]
    name: String,
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    updated: i64,
    #[serde(default)]
    targets: Vec<RawTarget>,
    #[serde(default)]
    specs: Option<RawSpecs>,
}

#[derive(Debug, Default, Deserialize)]
struct RawVersionFull {
    id: u64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    changelog: String,
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    updated: i64,
    #[serde(default)]
    targets: Vec<RawTarget>,
    #[serde(default)]
    specs: Option<RawSpecs>,
    #[serde(default)]
    files: Vec<RawFile>,
}

#[derive(Debug, Default, Deserialize)]
struct RawTarget {
    #[serde(default)]
    name: String,
    #[serde(default)]
    version: String,
}

#[derive(Debug, Default, Deserialize)]
struct RawSpecs {
    #[serde(default)]
    recommended: u64,
}

#[derive(Debug, Default, Deserialize)]
struct RawFile {
    #[serde(default)]
    name: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    sha1: Option<String>,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    clientonly: bool,
    #[serde(default)]
    serveronly: bool,
    #[serde(default)]
    optional: bool,
    #[serde(rename = "type", default)]
    kind: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack_json() -> RawPack {
        serde_json::from_str(
            r#"{
              "id": 117,
              "name": "FTB Skies Expert",
              "synopsis": "Expert pack",
              "art": [
                {"id": 1, "type": "splash", "url": "https://example/splash.png"},
                {"id": 2, "type": "square", "url": "https://example/icon.png"}
              ],
              "installs": 12,
              "plays": 34,
              "tags": [{"id": 1, "name": "Tech"}],
              "versions": [
                {"id": 1, "name": "1.0.0", "type": "beta", "updated": 10,
                 "targets": [{"name": "forge", "version": "43.2.6"}, {"name": "minecraft", "version": "1.19.2"},
                             {"name": "java", "version": "17.0.2+8"}],
                 "specs": {"minimum": 4096, "recommended": 6144}},
                {"id": 2, "name": "1.9.1", "type": "release", "updated": 20,
                 "targets": [{"name": "neoforge", "version": "21.1.74"}, {"name": "minecraft", "version": "1.21.1"}]}
              ]
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn parses_a_pack_and_its_versions() {
        let summary = summary_from_raw(pack_json());
        assert_eq!(summary.name, "FTB Skies Expert");
        assert_eq!(summary.icon_url.as_deref(), Some("https://example/icon.png"));
        assert_eq!(summary.tags, ["Tech"]);
        assert_eq!(summary.version_count(), 2);
        let latest = summary.latest.unwrap();
        assert_eq!(latest.name, "1.9.1");
        assert_eq!(latest.game_version, "1.21.1");
        assert_eq!(latest.loader_key().as_deref(), Some("neoforge"));
        assert_eq!(latest.loader.as_ref().unwrap().version, "21.1.74");
    }

    #[test]
    fn maps_forge_and_neoforge_builds() {
        let pack = pack_from_raw(pack_json());
        let first = &pack.versions[0];
        assert_eq!(first.game_version, "1.19.2");
        assert_eq!(first.loader.as_ref().unwrap().version, "1.19.2-43.2.6");
        assert_eq!(first.recommended_memory_mb, Some(6144));
        assert_eq!(pack.versions_after(1).len(), 1);
    }

    #[test]
    fn joins_file_paths_and_drops_incomplete_entries() {
        let raw: RawFile = serde_json::from_str(
            r#"{"name":"ae2.jar","path":"./mods","url":"https://edge/files/ae2.jar","sha1":"aa","size":5,"type":"mod"}"#,
        )
        .unwrap();
        let file = file_from_raw(raw).unwrap();
        assert_eq!(file.path, "mods/ae2.jar");

        let root: RawFile = serde_json::from_str(
            r#"{"name":"options.txt","path":".","url":"https://edge/options.txt","sha1":"bb","size":5}"#,
        )
        .unwrap();
        assert_eq!(file_from_raw(root).unwrap().path, "options.txt");

        let missing_sha: RawFile =
            serde_json::from_str(r#"{"name":"x.jar","path":"./mods","url":"https://edge/x.jar","size":5}"#).unwrap();
        assert!(file_from_raw(missing_sha).is_none());
    }

    #[test]
    fn duplicate_paths_are_downloaded_once() {
        let file = |path: &str, server_only: bool| PackFile {
            path: path.to_string(),
            url: format!("https://edge/{path}"),
            sha1: Some("aa".to_string()),
            size: 5,
            kind: "mod".to_string(),
            client_only: false,
            server_only,
            optional: false,
        };
        let version = PackVersion {
            id: 1,
            name: "1.0.0".to_string(),
            changelog: String::new(),
            kind: "release".to_string(),
            updated: 0,
            game_version: "1.20.1".to_string(),
            loader: None,
            java: None,
            recommended_memory_mb: None,
            files: vec![
                file("./mods/a.jar", false),
                file("mods/a.jar", false),
                file("mods/b.jar", false),
                file("mods/server.jar", true),
            ],
        };
        let files = unique_client_files(&version);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "./mods/a.jar");
        assert_eq!(files[1].path, "mods/b.jar");
    }
}
