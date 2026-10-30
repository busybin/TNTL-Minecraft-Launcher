//! Modrinth modpacks (`.mrpack`).
//!
//! A pack is a zip with `modrinth.index.json` at its root, a list of files to
//! download and two override trees (`overrides/` and `client-overrides/`) that
//! are copied into the game directory. Every downloaded file is verified
//! against the SHA-1 the index publishes.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tntl_source::{DownloadOutcome, DownloadRequest, Http, SourceError};
use tntl_store::{StoreError, safe_relative};

pub const INDEX_FILE: &str = "modrinth.index.json";
pub const OVERRIDES_DIR: &str = "overrides";
pub const CLIENT_OVERRIDES_DIR: &str = "client-overrides";
pub const SERVER_OVERRIDES_DIR: &str = "server-overrides";
const SUPPORTED_FORMAT: u32 = 1;
const MAX_INDEX_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum ModpackError {
    #[error(transparent)]
    Source(#[from] SourceError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid pack index at {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("invalid modpack: {0}")]
    Invalid(String),
    #[error("unexpected archive layout: {0}")]
    Zip(String),
}

fn io_error(path: &Path, source: io::Error) -> ModpackError {
    ModpackError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Mod loader a pack is built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PackLoader {
    Fabric,
    Quilt,
    Forge,
    NeoForge,
}

impl PackLoader {
    pub fn key(self) -> &'static str {
        match self {
            Self::Fabric => "fabric",
            Self::Quilt => "quilt",
            Self::Forge => "forge",
            Self::NeoForge => "neoforge",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key.trim().to_ascii_lowercase().as_str() {
            "fabric" | "fabric-loader" => Some(Self::Fabric),
            "quilt" | "quilt-loader" => Some(Self::Quilt),
            "forge" => Some(Self::Forge),
            "neoforge" => Some(Self::NeoForge),
            _ => None,
        }
    }

    /// Key the pack index uses in its `dependencies` map.
    pub fn dependency_key(self) -> &'static str {
        match self {
            Self::Fabric => "fabric-loader",
            Self::Quilt => "quilt-loader",
            Self::Forge => "forge",
            Self::NeoForge => "neoforge",
        }
    }

    /// Build string the loader installer expects. Forge maven versions carry
    /// the game version (`1.20.1-47.4.28`), NeoForge only did so up to 1.20.1.
    pub fn install_version(self, game: &str, value: &str) -> String {
        let prefix = format!("{game}-");
        let needs_prefix = self == Self::Forge || (self == Self::NeoForge && game == "1.20.1");
        if needs_prefix && !value.starts_with(&prefix) {
            format!("{prefix}{value}")
        } else {
            value.to_string()
        }
    }
}

impl std::fmt::Display for PackLoader {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.key())
    }
}

/// `modrinth.index.json`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PackIndex {
    pub format_version: u32,
    pub game: String,
    pub version_id: String,
    pub name: String,
    pub summary: String,
    pub files: Vec<PackFile>,
    pub dependencies: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PackFile {
    pub path: String,
    pub hashes: PackHashes,
    pub env: Option<PackEnv>,
    pub downloads: Vec<String>,
    pub file_size: u64,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct PackHashes {
    pub sha1: Option<String>,
    pub sha512: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct PackEnv {
    pub client: Option<String>,
    pub server: Option<String>,
}

impl PackFile {
    /// Files marked `unsupported` on the client stay out of the game directory.
    pub fn client_side(&self) -> bool {
        self.env
            .as_ref()
            .and_then(|env| env.client.as_deref())
            .is_none_or(|side| side != "unsupported")
    }
}

/// Re-check the files of an installed pack against the index kept next to the
/// instance and fetch whatever is missing or corrupt. Overrides stay untouched.
pub fn repair_index(
    http: &Http,
    game_dir: &Path,
    index: &PackIndex,
    progress: &mut dyn FnMut(PackEvent),
) -> Result<PackReport, ModpackError> {
    fs::create_dir_all(game_dir).map_err(|error| io_error(game_dir, error))?;
    let files: Vec<&PackFile> = index.files.iter().filter(|file| file.client_side()).collect();
    download_files(http, game_dir, &files, progress)
}

fn download_files(
    http: &Http,
    game_dir: &Path,
    files: &[&PackFile],
    progress: &mut dyn FnMut(PackEvent),
) -> Result<PackReport, ModpackError> {
    let mut report = PackReport::default();
    let files_total = files.len();
    let bytes_total: u64 = files.iter().map(|file| file.file_size).sum();
    let mut bytes_done = 0_u64;
    for (files_done, file) in files.iter().enumerate() {
        let relative = safe_relative(&file.path)
            .ok_or_else(|| ModpackError::Invalid(format!("unsafe file path: {}", file.path)))?;
        let dest = tntl_store::target_respecting_disabled(&game_dir.join(&relative));
        if !dest.starts_with(game_dir) {
            return Err(ModpackError::Invalid(format!("unsafe file path: {}", file.path)));
        }
        let url = file
            .downloads
            .first()
            .ok_or_else(|| ModpackError::Invalid(format!("{} has no download URL", file.path)))?;
        let mut request = DownloadRequest::new(url, &dest);
        if let Some(sha1) = &file.hashes.sha1 {
            request = request.with_sha1(sha1.clone());
        } else if let Some(sha512) = &file.hashes.sha512 {
            request = request.with_sha512(sha512.clone());
        }
        if file.file_size > 0 {
            request = request.with_size(file.file_size);
        }
        let total = (file.file_size > 0).then_some(file.file_size);
        let base = bytes_done;
        let mut sink = |done: u64| {
            progress(PackEvent::Downloading {
                path: file.path.clone(),
                done,
                total,
                files_done,
                files_total,
                bytes_done: base + done.min(file.file_size),
                bytes_total,
            });
        };
        let outcome = http.download(&request, &mut sink)?;
        report.files += 1;
        bytes_done += file.file_size;
        if matches!(outcome, DownloadOutcome::AlreadyValid) {
            report.skipped += 1;
            progress(PackEvent::Skipped {
                path: file.path.clone(),
            });
        } else {
            report.downloaded += 1;
            report.bytes += file.file_size;
            progress(PackEvent::Installed {
                path: file.path.clone(),
                bytes: file.file_size,
            });
        }
    }
    Ok(report)
}

/// Progress of one pack install.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum PackEvent {
    /// The `.mrpack` archive itself is coming down, before its index is read.
    Archive {
        done: u64,
        total: Option<u64>,
    },
    Downloading {
        path: String,
        done: u64,
        total: Option<u64>,
        /// Files finished so far, and how many the pack ships.
        files_done: usize,
        files_total: usize,
        /// Bytes over the whole pack, so the UI can draw one bar.
        bytes_done: u64,
        bytes_total: u64,
    },
    Installed {
        path: String,
        bytes: u64,
    },
    Skipped {
        path: String,
    },
    Extracting {
        prefix: String,
        entries: usize,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PackReport {
    pub files: usize,
    pub downloaded: usize,
    pub skipped: usize,
    pub bytes: u64,
    pub overrides: usize,
}

/// An opened `.mrpack` archive.
pub struct Modpack {
    pub index: PackIndex,
    archive: PathBuf,
}

/// Metadata written into an exported pack.
#[derive(Debug, Clone)]
pub struct ExportOptions<'a> {
    pub name: &'a str,
    pub version: &'a str,
    pub game: &'a str,
    pub loader: Option<(PackLoader, String)>,
    pub summary: &'a str,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExportReport {
    pub files: usize,
    pub bytes: u64,
}

/// Top-level game directories that do not belong in a shareable pack. An
/// imported `.minecraft` also holds the shared root (versions, libraries,
/// assets); copying that into `overrides/` would add hundreds of megabytes the
/// pack cannot use.
const EXPORT_SKIP: &[&str] = &[
    "logs",
    "crash-reports",
    "saves",
    "backups",
    "natives",
    "screenshots",
    "servers.dat",
    "usercache.json",
    "versions",
    "libraries",
    "assets",
    ".mixin.out",
];

/// Write an instance's game directory as a `.mrpack`.
///
/// Everything goes into `overrides/` instead of `files/`: the pack carries its
/// own jars, so it does not depend on the projects staying on Modrinth.
pub fn export(game_dir: &Path, output: &Path, options: &ExportOptions<'_>) -> Result<ExportReport, ModpackError> {
    let mut dependencies = BTreeMap::new();
    dependencies.insert("minecraft".to_string(), options.game.to_string());
    if let Some((loader, version)) = &options.loader {
        dependencies.insert(loader.dependency_key().to_string(), version.clone());
    }
    let index = PackIndex {
        format_version: SUPPORTED_FORMAT,
        game: "minecraft".to_string(),
        version_id: options.version.to_string(),
        name: options.name.to_string(),
        summary: options.summary.to_string(),
        files: Vec::new(),
        dependencies,
    };
    let index_json = serde_json::to_vec_pretty(&index)
        .map_err(|error| ModpackError::Invalid(format!("cannot serialize the pack index: {error}")))?;

    // Write next to the target and rename, so a failed export does not destroy
    // the file the user exported earlier under the same name.
    let mut staged_name = output.as_os_str().to_os_string();
    staged_name.push(".part");
    let staged = std::path::PathBuf::from(staged_name);
    let file = File::create(&staged).map_err(|error| io_error(&staged, error))?;
    let mut zip = zip::ZipWriter::new(file);
    let file_options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    zip.start_file(INDEX_FILE, file_options)
        .map_err(|error| ModpackError::Zip(error.to_string()))?;
    zip.write_all(&index_json).map_err(|error| io_error(output, error))?;

    let mut report = ExportReport::default();
    let mut stack = vec![game_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = fs::read_dir(&dir).map_err(|error| io_error(&dir, error))?;
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if dir == game_dir && EXPORT_SKIP.contains(&name.to_ascii_lowercase().as_str()) {
                continue;
            }
            let meta = fs::symlink_metadata(&path).map_err(|error| io_error(&path, error))?;
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                stack.push(path);
                continue;
            }
            if !meta.is_file() || is_junk(&name) {
                continue;
            }
            let relative = path
                .strip_prefix(game_dir)
                .map_err(|_| ModpackError::Invalid(path.display().to_string()))?;
            let entry_name = format!("{OVERRIDES_DIR}/{}", slash(relative));
            zip.start_file(entry_name, file_options)
                .map_err(|error| ModpackError::Zip(error.to_string()))?;
            let mut input = File::open(&path).map_err(|error| io_error(&path, error))?;
            io::copy(&mut input, &mut zip).map_err(|error| io_error(&path, error))?;
            report.files += 1;
            report.bytes += meta.len();
        }
    }
    zip.finish().map_err(|error| ModpackError::Zip(error.to_string()))?;
    std::fs::rename(&staged, output).map_err(|error| io_error(output, error))?;
    Ok(report)
}

fn is_junk(name: &str) -> bool {
    name == ".DS_Store" || name.ends_with(".log") || name.ends_with(".tmp") || name.ends_with(".part")
}

fn slash(path: &Path) -> String {
    path.components()
        .filter_map(|part| match part {
            std::path::Component::Normal(value) => Some(value.to_string_lossy().to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

impl Modpack {
    pub fn open(path: &Path) -> Result<Self, ModpackError> {
        let file = File::open(path).map_err(|error| io_error(path, error))?;
        let mut archive = zip::ZipArchive::new(file).map_err(|error| ModpackError::Zip(error.to_string()))?;
        let mut index = Vec::new();
        {
            let mut entry = archive
                .by_name(INDEX_FILE)
                .map_err(|_| ModpackError::Invalid(format!("{} has no {INDEX_FILE}", path.display())))?;
            if entry.size() > MAX_INDEX_BYTES {
                return Err(ModpackError::Invalid(format!("{INDEX_FILE} is too large")));
            }
            io::copy(&mut entry, &mut index).map_err(|error| io_error(path, error))?;
        }
        let index: PackIndex = serde_json::from_slice(&index).map_err(|source| ModpackError::Json {
            path: path.to_path_buf(),
            source,
        })?;
        if index.format_version != SUPPORTED_FORMAT {
            return Err(ModpackError::Invalid(format!(
                "pack format {} is not supported",
                index.format_version
            )));
        }
        if !index.game.is_empty() && index.game != "minecraft" {
            return Err(ModpackError::Invalid(format!("pack is for {}", index.game)));
        }
        for file in &index.files {
            if safe_relative(&file.path).is_none() {
                return Err(ModpackError::Invalid(format!("unsafe file path: {}", file.path)));
            }
            if file.client_side() && file.downloads.is_empty() {
                return Err(ModpackError::Invalid(format!("{} has no download URL", file.path)));
            }
        }
        Ok(Self {
            index,
            archive: path.to_path_buf(),
        })
    }

    pub fn game_version(&self) -> Result<String, ModpackError> {
        self.index
            .dependencies
            .get("minecraft")
            .cloned()
            .ok_or_else(|| ModpackError::Invalid("the pack does not name a Minecraft version".to_string()))
    }

    /// Loader and build the pack asks for, if any.
    pub fn loader(&self) -> Option<(PackLoader, Option<String>)> {
        let game = self.index.dependencies.get("minecraft").cloned().unwrap_or_default();
        [
            PackLoader::Fabric,
            PackLoader::Quilt,
            PackLoader::NeoForge,
            PackLoader::Forge,
        ]
        .into_iter()
        .find_map(|loader| {
            self.index
                .dependencies
                .get(loader.dependency_key())
                .map(|version| (loader, Some(loader.install_version(&game, version))))
        })
    }

    /// Display name of the pack, falling back to its version id.
    pub fn name(&self) -> String {
        let name = self.index.name.trim();
        if name.is_empty() {
            self.index.version_id.trim().to_string()
        } else {
            name.to_string()
        }
    }

    /// Files the client needs, in index order.
    pub fn client_files(&self) -> impl Iterator<Item = &PackFile> {
        self.index.files.iter().filter(|file| file.client_side())
    }

    /// Download every client file and copy the override trees into `game_dir`.
    /// Overrides are extracted last, so they win over downloaded files.
    pub fn install(
        &self,
        http: &Http,
        game_dir: &Path,
        progress: &mut dyn FnMut(PackEvent),
    ) -> Result<PackReport, ModpackError> {
        fs::create_dir_all(game_dir).map_err(|error| io_error(game_dir, error))?;
        let files: Vec<&PackFile> = self.client_files().collect();
        let mut report = download_files(http, game_dir, &files, &mut |event| progress(event))?;
        report.overrides = self.extract_overrides(game_dir, progress)?;
        Ok(report)
    }

    /// Copy `overrides/` and then `client-overrides/` into the game directory.
    fn extract_overrides(&self, game_dir: &Path, progress: &mut dyn FnMut(PackEvent)) -> Result<usize, ModpackError> {
        let file = File::open(&self.archive).map_err(|error| io_error(&self.archive, error))?;
        let mut archive = zip::ZipArchive::new(file).map_err(|error| ModpackError::Zip(error.to_string()))?;
        let mut written = 0;
        for prefix in [OVERRIDES_DIR, CLIENT_OVERRIDES_DIR] {
            let head = format!("{prefix}/");
            let mut entries = 0;
            for index in 0..archive.len() {
                let mut entry = archive
                    .by_index(index)
                    .map_err(|error| ModpackError::Zip(error.to_string()))?;
                if entry.is_dir() {
                    continue;
                }
                let name = entry.name().to_string();
                let Some(stripped) = name.strip_prefix(&head) else {
                    continue;
                };
                let relative = safe_relative(stripped)
                    .ok_or_else(|| ModpackError::Invalid(format!("unsafe archive entry: {name}")))?;
                let target = tntl_store::target_respecting_disabled(&game_dir.join(&relative));
                if !target.starts_with(game_dir) {
                    return Err(ModpackError::Invalid(format!("unsafe archive entry: {name}")));
                }
                // Worlds are the player's, not the pack's: an override that
                // ships `saves/...` would replace an existing world.
                if relative.starts_with("saves") {
                    eprintln!("skipping pack override into saves/: {name}");
                    continue;
                }
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;
                }
                // Keep whatever is already there (configs the user edited,
                // options.txt) instead of truncating it.
                if let Err(error) = tntl_store::backup_existing(&target) {
                    return Err(io_error(&target, error));
                }
                let mut out = File::create(&target).map_err(|error| io_error(&target, error))?;
                io::copy(&mut entry, &mut out).map_err(|error| io_error(&target, error))?;
                entries += 1;
            }
            written += entries;
            progress(PackEvent::Extracting {
                prefix: prefix.to_string(),
                entries,
            });
        }
        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn index_json(files: &str, dependencies: &str) -> String {
        format!(
            r#"{{
              "formatVersion": 1,
              "game": "minecraft",
              "versionId": "abc123",
              "name": "Fabulously Optimized",
              "summary": "A pack",
              "files": [{files}],
              "dependencies": {{{dependencies}}}
            }}"#
        )
    }

    fn build_pack(dir: &Path, index: &str) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let path = dir.join("pack.mrpack");
        let file = File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file(INDEX_FILE, options).unwrap();
        zip.write_all(index.as_bytes()).unwrap();
        zip.start_file("overrides/config/a.txt", options).unwrap();
        zip.write_all(b"override").unwrap();
        zip.start_file("client-overrides/config/a.txt", options).unwrap();
        zip.write_all(b"client").unwrap();
        zip.start_file("server-overrides/config/a.txt", options).unwrap();
        zip.write_all(b"server").unwrap();
        zip.finish().unwrap();
        path
    }

    #[test]
    fn parses_the_index_and_loader() {
        let dir = std::env::temp_dir().join(format!("tntl-modpack-parse-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = build_pack(
            &dir,
            &index_json(
                r#"{"path":"mods/sodium.jar","hashes":{"sha1":"aa","sha512":"bb"},
                    "downloads":["https://cdn.example/sodium.jar"],"fileSize":42}"#,
                r#""minecraft":"1.20.1","fabric-loader":"0.16.0""#,
            ),
        );
        let pack = Modpack::open(&path).unwrap();
        assert_eq!(pack.game_version().unwrap(), "1.20.1");
        assert_eq!(pack.name(), "Fabulously Optimized");
        assert_eq!(pack.loader(), Some((PackLoader::Fabric, Some("0.16.0".to_string()))));
        assert_eq!(pack.client_files().count(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn maps_loader_builds_to_maven_versions() {
        assert_eq!(PackLoader::Forge.install_version("1.20.1", "47.4.28"), "1.20.1-47.4.28");
        assert_eq!(
            PackLoader::Forge.install_version("1.20.1", "1.20.1-47.4.28"),
            "1.20.1-47.4.28"
        );
        assert_eq!(
            PackLoader::NeoForge.install_version("1.20.1", "47.1.106"),
            "1.20.1-47.1.106"
        );
        assert_eq!(PackLoader::NeoForge.install_version("1.21.1", "21.1.264"), "21.1.264");
        assert_eq!(PackLoader::Fabric.install_version("1.20.1", "0.16.0"), "0.16.0");
        assert_eq!(PackLoader::from_key("quilt-loader"), Some(PackLoader::Quilt));
    }

    #[test]
    fn export_leaves_the_shared_root_out_of_overrides() {
        let dir = std::env::temp_dir().join(format!("tntl-modpack-export-root-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let game = dir.join("game");
        fs::create_dir_all(game.join("mods")).unwrap();
        fs::write(game.join("mods/a.jar"), b"jar").unwrap();
        fs::create_dir_all(game.join("versions/1.20.1")).unwrap();
        fs::write(game.join("versions/1.20.1/1.20.1.json"), b"{}").unwrap();
        fs::create_dir_all(game.join("libraries/net")).unwrap();
        fs::write(game.join("libraries/net/a.jar"), b"lib").unwrap();
        fs::create_dir_all(game.join("assets/objects/aa")).unwrap();
        fs::write(game.join("assets/objects/aa/bb"), b"obj").unwrap();
        let output = dir.join("out.mrpack");
        let options = ExportOptions {
            name: "Pack",
            version: "1",
            game: "1.20.1",
            loader: None,
            summary: "",
        };
        export(&game, &output, &options).unwrap();
        let file = fs::File::open(&output).unwrap();
        let mut zip = zip::ZipArchive::new(file).unwrap();
        let names: Vec<String> = (0..zip.len())
            .map(|index| zip.by_index(index).unwrap().name().to_string())
            .collect();
        assert!(names.iter().any(|name| name == "overrides/mods/a.jar"), "{names:?}");
        // An imported game directory is the other launcher's root; the pack
        // must not carry its versions, libraries and assets.
        for prefix in ["overrides/versions/", "overrides/libraries/", "overrides/assets/"] {
            assert!(
                !names.iter().any(|name| name.starts_with(prefix)),
                "{prefix} in {names:?}"
            );
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn skips_files_the_client_may_not_have() {
        let dir = std::env::temp_dir().join(format!("tntl-modpack-env-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = build_pack(
            &dir,
            &index_json(
                r#"{"path":"mods/client.jar","hashes":{"sha1":"aa"},"downloads":["https://cdn.example/a.jar"],
                    "env":{"client":"required","server":"unsupported"}},
                   {"path":"mods/server.jar","hashes":{"sha1":"bb"},"downloads":["https://cdn.example/b.jar"],
                    "env":{"client":"unsupported","server":"required"}}"#,
                r#""minecraft":"1.20.1""#,
            ),
        );
        let pack = Modpack::open(&path).unwrap();
        let files: Vec<&str> = pack.client_files().map(|file| file.path.as_str()).collect();
        assert_eq!(files, ["mods/client.jar"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_unsafe_paths_and_unknown_formats() {
        let dir = std::env::temp_dir().join(format!("tntl-modpack-bad-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = build_pack(
            &dir,
            &index_json(
                r#"{"path":"../escape.jar","hashes":{"sha1":"aa"},"downloads":["https://cdn.example/x.jar"]}"#,
                r#""minecraft":"1.20.1""#,
            ),
        );
        assert!(Modpack::open(&path).is_err());
        fs::remove_dir_all(&dir).unwrap();

        let path = build_pack(&dir, r#"{"formatVersion":2,"game":"minecraft"}"#);
        assert!(Modpack::open(&path).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn extracts_overrides_with_client_files_winning() {
        let dir = std::env::temp_dir().join(format!("tntl-modpack-overrides-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = build_pack(&dir, &index_json("", r#""minecraft":"1.20.1""#));
        let pack = Modpack::open(&path).unwrap();
        let game_dir = dir.join("game");
        let http = Http::new().unwrap();
        let mut events = 0;
        let report = pack
            .install(&http, &game_dir, &mut |_| {
                events += 1;
            })
            .unwrap();
        assert_eq!(report.overrides, 2);
        assert_eq!(events, 2);
        assert_eq!(fs::read_to_string(game_dir.join("config/a.txt")).unwrap(), "client");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn exports_game_directory_as_overrides() {
        let dir = std::env::temp_dir().join(format!("tntl-modpack-export-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let game = dir.join("game");
        fs::create_dir_all(game.join("mods")).unwrap();
        fs::create_dir_all(game.join("config")).unwrap();
        fs::create_dir_all(game.join("logs")).unwrap();
        fs::write(game.join("mods/example.jar"), b"jar").unwrap();
        fs::write(game.join("config/options.txt"), b"fov:70").unwrap();
        fs::write(game.join("logs/latest.log"), b"noise").unwrap();

        let out = dir.join("Test.mrpack");
        let report = export(
            &game,
            &out,
            &ExportOptions {
                name: "Test Pack",
                version: "1.0.0",
                game: "1.20.1",
                loader: Some((PackLoader::Fabric, "0.15.0".to_string())),
                summary: "test",
            },
        )
        .unwrap();
        assert_eq!(report.files, 2);

        let pack = Modpack::open(&out).unwrap();
        assert!(pack.index.files.is_empty());
        assert_eq!(
            pack.index.dependencies.get("fabric-loader").map(String::as_str),
            Some("0.15.0")
        );
        assert_eq!(
            pack.index.dependencies.get("minecraft").map(String::as_str),
            Some("1.20.1")
        );

        let target = dir.join("restored");
        let http = Http::new().unwrap();
        pack.install(&http, &target, &mut |_| {}).unwrap();
        assert_eq!(fs::read(target.join("mods/example.jar")).unwrap(), b"jar");
        assert!(target.join("config/options.txt").exists());
        assert!(!target.join("logs").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reports_overall_bytes_and_file_counts() {
        use std::io::Read;
        use std::net::TcpListener;

        let body = vec![7_u8; 256 * 1024];
        let sha1 = tntl_store::sha1_bytes(&body);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let length = body.len();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut head = [0_u8; 4096];
            let _ = socket.read(&mut head);
            let response = format!("HTTP/1.1 200 OK\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n");
            std::io::Write::write_all(&mut socket, response.as_bytes()).unwrap();
            std::io::Write::write_all(&mut socket, &body).unwrap();
        });

        let dir = std::env::temp_dir().join(format!("tntl-modpack-bytes-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let index = index_json(
            &format!(
                r#"{{"path":"mods/a.jar","hashes":{{"sha1":"{sha1}"}},
                    "downloads":["http://{address}/a.jar"],"fileSize":{length}}}"#
            ),
            r#""minecraft":"1.20.1""#,
        );
        let path = build_pack(&dir, &index);
        let pack = Modpack::open(&path).unwrap();
        let game_dir = dir.join("game");
        let http = Http::new().unwrap();
        let mut seen = Vec::new();
        pack.install(&http, &game_dir, &mut |event| seen.push(event)).unwrap();
        server.join().unwrap();
        let last = seen
            .iter()
            .rev()
            .find_map(|event| match event {
                PackEvent::Downloading {
                    files_total,
                    bytes_total,
                    ..
                } => Some((*files_total, *bytes_total)),
                _ => None,
            })
            .unwrap();
        assert_eq!(last, (1, length as u64));
        let downloaded = seen
            .iter()
            .rev()
            .find_map(|event| match event {
                PackEvent::Downloading {
                    files_done, bytes_done, ..
                } => Some((*files_done, *bytes_done)),
                _ => None,
            })
            .unwrap();
        assert_eq!(downloaded, (0, length as u64));
        assert!(seen.iter().any(|event| matches!(event, PackEvent::Installed { .. })));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn updates_files_the_user_disabled() {
        use std::io::Read;
        use std::net::TcpListener;

        let body = b"new jar".to_vec();
        let expected = body.clone();
        let sha1 = tntl_store::sha1_bytes(&body);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let length = body.len();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut head = [0_u8; 4096];
            let _ = socket.read(&mut head);
            let response = format!("HTTP/1.1 200 OK\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n");
            std::io::Write::write_all(&mut socket, response.as_bytes()).unwrap();
            std::io::Write::write_all(&mut socket, &body).unwrap();
        });

        let dir = std::env::temp_dir().join(format!("tntl-modpack-disabled-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let index = index_json(
            &format!(
                r#"{{"path":"mods/a.jar","hashes":{{"sha1":"{sha1}"}},
                    "downloads":["http://{address}/a.jar"],"fileSize":{length}}}"#
            ),
            r#""minecraft":"1.20.1""#,
        );
        let path = build_pack(&dir, &index);
        let pack = Modpack::open(&path).unwrap();
        let game_dir = dir.join("game");
        fs::create_dir_all(game_dir.join("mods")).unwrap();
        fs::write(game_dir.join("mods/a.jar.disabled"), b"old").unwrap();
        let http = Http::new().unwrap();
        pack.install(&http, &game_dir, &mut |_| {}).unwrap();
        server.join().unwrap();
        assert_eq!(fs::read(game_dir.join("mods/a.jar.disabled")).unwrap(), expected);
        assert!(!game_dir.join("mods/a.jar").exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
