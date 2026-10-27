//! Mojang's Java runtimes: listing and installing the JREs the official
//! launcher ships. Nothing here touches third-party JDK vendors.

#![forbid(unsafe_code)]

pub mod adoptium;

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tntl_source::{DownloadOutcome, DownloadRequest, Http, SourceError, endpoints};
use tntl_store::{StoreError, atomic_write_json, safe_relative};

pub const RUNTIME_MARKER: &str = "runtime.json";

#[derive(Debug, Error)]
pub enum RuntimeError {
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
    #[error("invalid runtime manifest: {0}")]
    Json(String),
    #[error("unsupported platform: {0}")]
    Platform(String),
    #[error("no Java runtime is published for {0}")]
    Unknown(String),
    #[error("unsafe runtime entry: {0}")]
    UnsafeEntry(String),
    #[error("checksum mismatch for {path}: expected {expected}, got {actual}")]
    Checksum {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    #[error("cannot unpack runtime archive: {0}")]
    Archive(String),
}

fn io_error(path: &Path, source: io::Error) -> RuntimeError {
    RuntimeError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// One published runtime build.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeOffer {
    pub component: String,
    pub version: String,
    pub released: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OfferEntry {
    manifest: ManifestRef,
    version: VersionRef,
}

#[derive(Debug, Deserialize)]
struct ManifestRef {
    url: String,
}

#[derive(Debug, Deserialize)]
struct VersionRef {
    name: String,
    #[serde(default)]
    released: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RuntimeFiles {
    #[serde(default)]
    files: BTreeMap<String, RuntimeFile>,
}

#[derive(Debug, Deserialize)]
struct RuntimeFile {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    downloads: RuntimeDownloads,
    #[serde(default)]
    executable: bool,
}

#[derive(Debug, Default, Deserialize)]
struct RuntimeDownloads {
    #[serde(default)]
    raw: Option<RuntimeDownload>,
}

#[derive(Debug, Deserialize)]
struct RuntimeDownload {
    url: String,
    #[serde(default)]
    sha1: Option<String>,
    #[serde(default)]
    size: u64,
}

/// Progress of one runtime install.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum RuntimeEvent {
    Manifest {
        component: String,
        version: String,
        files: usize,
    },
    Downloading {
        path: String,
        done: u64,
        total: Option<u64>,
    },
    Installed {
        path: String,
        bytes: u64,
    },
    Skipped {
        path: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledRuntime {
    pub component: String,
    pub version: String,
    pub home: PathBuf,
    pub java: PathBuf,
    pub files: usize,
    pub bytes: u64,
    pub skipped: bool,
}

/// Key of this platform in Mojang's runtime manifest.
pub fn platform_key() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("linux"),
        ("linux", "x86") => Some("linux-i386"),
        ("windows", "x86_64") => Some("windows-x64"),
        ("windows", "x86") => Some("windows-x86"),
        ("windows", "aarch64") => Some("windows-arm64"),
        _ => None,
    }
}

pub fn java_binary(home: &Path) -> PathBuf {
    home.join("bin").join(if cfg!(windows) { "java.exe" } else { "java" })
}

fn load_all(http: &Http) -> Result<BTreeMap<String, BTreeMap<String, Vec<OfferEntry>>>, RuntimeError> {
    let body = http.get_bytes(endpoints::JAVA_RUNTIMES, 8 * 1024 * 1024)?;
    serde_json::from_slice(&body).map_err(|error| RuntimeError::Json(error.to_string()))
}

/// Runtimes published for this platform, newest build first per component.
pub fn offers(http: &Http) -> Result<Vec<RuntimeOffer>, RuntimeError> {
    let platform = platform_key()
        .ok_or_else(|| RuntimeError::Platform(format!("{} {}", std::env::consts::OS, std::env::consts::ARCH)))?;
    let all = load_all(http)?;
    let components = all
        .get(platform)
        .ok_or_else(|| RuntimeError::Platform(platform.to_string()))?;
    let mut offers = Vec::new();
    for (component, entries) in components {
        if let Some(entry) = entries.first() {
            offers.push(RuntimeOffer {
                component: component.clone(),
                version: entry.version.name.clone(),
                released: entry.version.released.clone(),
            });
        }
    }
    offers.sort_by(|left, right| left.component.cmp(&right.component));
    Ok(offers)
}

/// Component that ships `major` for this platform, e.g. 17 -> `java-runtime-gamma`.
pub fn component_for_major(http: &Http, major: u32) -> Result<Option<RuntimeOffer>, RuntimeError> {
    let wanted = major.to_string();
    Ok(offers(http)?.into_iter().find(|offer| {
        let version = offer.version.split('.').next().unwrap_or_default();
        version == wanted
    }))
}

/// Download and unpack one runtime into `home`. A runtime that is already
/// complete is left alone.
pub fn install(
    http: &Http,
    home: &Path,
    component: &str,
    progress: &mut dyn FnMut(RuntimeEvent),
) -> Result<InstalledRuntime, RuntimeError> {
    let platform = platform_key()
        .ok_or_else(|| RuntimeError::Platform(format!("{} {}", std::env::consts::OS, std::env::consts::ARCH)))?;
    let all = load_all(http)?;
    let entry = all
        .get(platform)
        .and_then(|components| components.get(component))
        .and_then(|entries| entries.first())
        .ok_or_else(|| RuntimeError::Unknown(format!("{component} on {platform}")))?;
    let version = entry.version.name.clone();

    let java = java_binary(home);
    if java.is_file() && home.join(RUNTIME_MARKER).is_file() {
        let marker: Marker = fs::read(home.join(RUNTIME_MARKER))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        if marker.version == version {
            return Ok(InstalledRuntime {
                component: component.to_string(),
                version,
                home: home.to_path_buf(),
                java,
                files: marker.files,
                bytes: marker.bytes,
                skipped: true,
            });
        }
    }

    let body = http.get_bytes(&entry.manifest.url, 16 * 1024 * 1024)?;
    let manifest: RuntimeFiles =
        serde_json::from_slice(&body).map_err(|error| RuntimeError::Json(error.to_string()))?;
    let files: Vec<(&String, &RuntimeFile)> = manifest
        .files
        .iter()
        .filter(|(_, file)| file.kind == "file" && file.downloads.raw.is_some())
        .collect();
    progress(RuntimeEvent::Manifest {
        component: component.to_string(),
        version: version.clone(),
        files: files.len(),
    });

    fs::create_dir_all(home).map_err(|error| io_error(home, error))?;
    let mut written = 0;
    let mut bytes = 0u64;
    for (path, file) in files {
        let Some(raw) = file.downloads.raw.as_ref() else {
            continue;
        };
        let relative = safe_relative(path).ok_or_else(|| RuntimeError::UnsafeEntry(path.clone()))?;
        let dest = home.join(&relative);
        if !dest.starts_with(home) {
            return Err(RuntimeError::UnsafeEntry(path.clone()));
        }
        let mut request = DownloadRequest::new(&raw.url, &dest);
        if let Some(sha1) = &raw.sha1 {
            request = request.with_sha1(sha1.clone());
        }
        if raw.size > 0 {
            request = request.with_size(raw.size);
        }
        let total = (raw.size > 0).then_some(raw.size);
        let mut sink = |done: u64| {
            progress(RuntimeEvent::Downloading {
                path: path.clone(),
                done,
                total,
            });
        };
        let outcome = http.download(&request, &mut sink)?;
        if matches!(outcome, DownloadOutcome::AlreadyValid) {
            progress(RuntimeEvent::Skipped { path: path.clone() });
        } else {
            written += 1;
            bytes += raw.size;
            progress(RuntimeEvent::Installed {
                path: path.clone(),
                bytes: raw.size,
            });
        }
        if file.executable {
            make_executable(&dest)?;
        }
    }

    let marker = Marker {
        component: component.to_string(),
        version: version.clone(),
        files: written,
        bytes,
    };
    atomic_write_json(&home.join(RUNTIME_MARKER), &marker)?;
    if !java.is_file() {
        return Err(RuntimeError::Unknown(format!(
            "{component} did not contain {}",
            java.display()
        )));
    }
    Ok(InstalledRuntime {
        component: component.to_string(),
        version,
        home: home.to_path_buf(),
        java,
        files: written,
        bytes,
        skipped: false,
    })
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Marker {
    #[serde(default)]
    component: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    files: usize,
    #[serde(default)]
    bytes: u64,
}

#[cfg(unix)]
pub(crate) fn make_executable(path: &Path) -> Result<(), RuntimeError> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path).map_err(|error| io_error(path, error))?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).map_err(|error| io_error(path, error))
}

#[cfg(not(unix))]
pub(crate) fn make_executable(_path: &Path) -> Result<(), RuntimeError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_this_platform() {
        let key = platform_key();
        if cfg!(target_os = "linux") {
            assert!(key == Some("linux") || key == Some("linux-i386"));
        }
        if cfg!(target_os = "windows") {
            assert!(key.is_some());
        }
    }

    #[test]
    fn parses_a_runtime_manifest() {
        let manifest: RuntimeFiles = serde_json::from_str(
            r#"{"files":{
                 "bin/java":{"type":"file","executable":true,
                   "downloads":{"raw":{"url":"https://example/bin/java","sha1":"aa","size":10}}},
                 "lib":{"type":"directory"},
                 "bin/java.exe":{"type":"link","target":"java"}
               }}"#,
        )
        .unwrap();
        let files: Vec<&str> = manifest
            .files
            .iter()
            .filter(|(_, file)| file.kind == "file" && file.downloads.raw.is_some())
            .map(|(path, _)| path.as_str())
            .collect();
        assert_eq!(files, ["bin/java"]);
        assert!(manifest.files["bin/java"].executable);
    }

    #[test]
    fn java_binary_lives_in_bin() {
        let home = Path::new("/runtimes/java-runtime-gamma");
        assert_eq!(
            java_binary(home),
            home.join("bin").join(if cfg!(windows) { "java.exe" } else { "java" })
        );
    }
}
