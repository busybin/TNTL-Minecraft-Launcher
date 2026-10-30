//! Eclipse Temurin JDKs from the public Adoptium API.
//!
//! Mojang only ships a handful of runtimes, so a version that needs an older
//! or newer JVM has no offer there. Adoptium publishes every LTS and the
//! current feature release without an API key.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tntl_source::{DownloadRequest, Http, endpoints};
use tntl_store::atomic_write_json;

use crate::{InstalledRuntime, RUNTIME_MARKER, RuntimeError, RuntimeEvent, java_binary, make_executable};

/// LTS feature releases are what Minecraft versions tend to need.
pub const LTS_RELEASES: [u32; 5] = [8, 11, 17, 21, 25];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdoptiumOffer {
    /// `adoptium-17`; the directory below `java/`.
    pub component: String,
    pub major: u32,
    /// Release name such as `jdk-17.0.20.1+1`.
    pub version: String,
    pub size: u64,
}

#[derive(Deserialize)]
struct Asset {
    binary: Binary,
    release_name: String,
}

#[derive(Deserialize)]
struct Binary {
    package: Package,
}

#[derive(Deserialize)]
struct Package {
    link: String,
    #[serde(default)]
    checksum: Option<String>,
    #[serde(default)]
    name: String,
    #[serde(default)]
    size: u64,
}

#[derive(Deserialize)]
struct AvailableReleases {
    #[serde(default)]
    available_releases: Vec<u32>,
    #[serde(default)]
    most_recent_feature_release: Option<u32>,
}

/// Adoptium's names for this platform, if it publishes one.
fn platform() -> Option<(&'static str, &'static str)> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some(("linux", "x64")),
        ("linux", "x86") => Some(("linux", "x86")),
        ("linux", "aarch64") => Some(("linux", "aarch64")),
        ("windows", "x86_64") => Some(("windows", "x64")),
        ("windows", "x86") => Some(("windows", "x86")),
        ("windows", "aarch64") => Some(("windows", "aarch64")),
        _ => None,
    }
}

pub fn component(major: u32) -> String {
    format!("adoptium-{major}")
}

pub fn major_of(component: &str) -> Option<u32> {
    component.strip_prefix("adoptium-")?.parse().ok()
}

fn assets_url(major: u32, os: &str, arch: &str) -> String {
    format!(
        "{}/assets/latest/{major}/hotspot?architecture={arch}&image_type=jdk&os={os}&vendor=eclipse&page_size=1",
        endpoints::ADOPTIUM_API
    )
}

fn latest(http: &Http, major: u32) -> Result<Asset, RuntimeError> {
    let (os, arch) = platform()
        .ok_or_else(|| RuntimeError::Platform(format!("{} {}", std::env::consts::OS, std::env::consts::ARCH)))?;
    let url = assets_url(major, os, arch);
    let body = http.get_bytes(&url, 1024 * 1024)?;
    let assets: Vec<Asset> = serde_json::from_slice(&body).map_err(|error| RuntimeError::Json(error.to_string()))?;
    assets
        .into_iter()
        .next()
        .ok_or_else(|| RuntimeError::Unknown(format!("adoptium {major} on {os}/{arch}")))
}

/// Feature releases worth offering: the LTS line plus the newest release.
pub fn offers(http: &Http) -> Result<Vec<AdoptiumOffer>, RuntimeError> {
    platform().ok_or_else(|| RuntimeError::Platform(format!("{} {}", std::env::consts::OS, std::env::consts::ARCH)))?;
    let body = http.get_bytes(
        &format!("{}/info/available_releases", endpoints::ADOPTIUM_API),
        256 * 1024,
    )?;
    let available: AvailableReleases =
        serde_json::from_slice(&body).map_err(|error| RuntimeError::Json(error.to_string()))?;
    let mut majors: Vec<u32> = LTS_RELEASES.to_vec();
    if let Some(newest) = available.most_recent_feature_release {
        majors.push(newest);
    }
    majors.retain(|major| available.available_releases.is_empty() || available.available_releases.contains(major));
    majors.sort_unstable();
    majors.dedup();

    let mut offers = Vec::new();
    for major in majors {
        let Ok(asset) = latest(http, major) else {
            continue;
        };
        offers.push(AdoptiumOffer {
            component: component(major),
            major,
            version: asset.release_name,
            size: asset.binary.package.size,
        });
    }
    Ok(offers)
}

/// Download, unpack and register one Temurin JDK under `home`.
pub fn install(
    http: &Http,
    home: &Path,
    major: u32,
    progress: &mut dyn FnMut(RuntimeEvent),
) -> Result<InstalledRuntime, RuntimeError> {
    let component = component(major);
    let asset = latest(http, major)?;
    let version = asset.release_name.clone();
    let package = asset.binary.package;
    let java = java_binary(home);

    if java.is_file() && home.join(RUNTIME_MARKER).is_file() {
        let marker: Marker = fs::read(home.join(RUNTIME_MARKER))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        if marker.version == version {
            return Ok(InstalledRuntime {
                component,
                version,
                home: home.to_path_buf(),
                java,
                files: marker.files,
                bytes: marker.bytes,
                skipped: true,
            });
        }
    }

    progress(RuntimeEvent::Manifest {
        component: component.clone(),
        version: version.clone(),
        files: 1,
    });
    fs::create_dir_all(home).map_err(|error| io_error(home, error))?;

    let archive = home.with_file_name(format!(".{component}.download"));
    let mut request = DownloadRequest::new(&package.link, &archive);
    if package.size > 0 {
        request = request.with_size(package.size);
    }
    let total = (package.size > 0).then_some(package.size);
    let mut sink = |done: u64| {
        progress(RuntimeEvent::Downloading {
            path: package.name.clone(),
            done,
            total,
        });
    };
    let outcome = http.download(&request, &mut sink);

    // An already-valid archive from an earlier interrupted run is hashed again
    // below; the checksum check needs the file either way.
    if let Err(error) = outcome {
        let _ = fs::remove_file(&archive);
        return Err(error.into());
    }
    if let Some(expected) = package.checksum.as_deref()
        && let Err(error) = verify_sha256(&archive, expected)
    {
        let _ = fs::remove_file(&archive);
        return Err(error);
    }
    let files = unpack(&archive, home);
    let _ = fs::remove_file(&archive);
    let files = files?;

    if !java.is_file() {
        return Err(RuntimeError::Unknown(format!(
            "{component} did not contain {}",
            java.display()
        )));
    }
    make_executable(&java)?;
    let marker = Marker {
        component: component.clone(),
        version: version.clone(),
        files,
        bytes: package.size,
    };
    atomic_write_json(&home.join(RUNTIME_MARKER), &marker)?;
    progress(RuntimeEvent::Installed {
        path: package.name.clone(),
        bytes: package.size,
    });
    Ok(InstalledRuntime {
        component,
        version,
        home: home.to_path_buf(),
        java,
        files,
        bytes: package.size,
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

fn verify_sha256(path: &Path, expected: &str) -> Result<(), RuntimeError> {
    use sha2::{Digest, Sha256};
    let mut file = fs::File::open(path).map_err(|error| io_error(path, error))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| io_error(path, error))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual = hex::encode(hasher.finalize());
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(RuntimeError::Checksum {
            path: path.to_path_buf(),
            expected: expected.to_string(),
            actual,
        });
    }
    Ok(())
}

/// Unpack a JDK archive and strip its single top-level directory. Adoptium
/// serves zips for Windows and tarballs elsewhere, so the format is sniffed
/// from the file itself rather than from the download name.
fn unpack(archive: &Path, home: &Path) -> Result<usize, RuntimeError> {
    let mut magic = [0u8; 2];
    {
        let mut file = fs::File::open(archive).map_err(|error| io_error(archive, error))?;
        file.read_exact(&mut magic).map_err(|error| io_error(archive, error))?;
    }
    match magic {
        [b'P', b'K'] => unpack_zip(archive, home),
        [0x1f, 0x8b] => unpack_tar_gz(archive, home),
        _ => Err(RuntimeError::Archive(format!(
            "unsupported archive format: {}",
            archive.display()
        ))),
    }
}

fn unpack_zip(archive: &Path, home: &Path) -> Result<usize, RuntimeError> {
    let file = fs::File::open(archive).map_err(|error| io_error(archive, error))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|error| RuntimeError::Archive(error.to_string()))?;
    let mut written = 0;
    let mut budget = tntl_store::EXTRACT_BUDGET;
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|error| RuntimeError::Archive(error.to_string()))?;
        if entry.is_dir() {
            continue;
        }
        let Some(relative) = strip_top(entry.name()) else {
            continue;
        };
        let dest = home.join(&relative);
        if !dest.starts_with(home) {
            return Err(RuntimeError::UnsafeEntry(relative.to_string_lossy().into_owned()));
        }
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;
        }
        let mut out = fs::File::create(&dest).map_err(|error| io_error(&dest, error))?;
        tntl_store::copy_entry(&mut entry, &mut out, &mut budget).map_err(|error| io_error(&dest, error))?;
        written += 1;
    }
    Ok(written)
}

fn unpack_tar_gz(archive: &Path, home: &Path) -> Result<usize, RuntimeError> {
    let file = fs::File::open(archive).map_err(|error| io_error(archive, error))?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let mut written = 0;
    let mut budget = tntl_store::EXTRACT_BUDGET;
    for entry in tar
        .entries()
        .map_err(|error| RuntimeError::Archive(error.to_string()))?
    {
        let mut entry = entry.map_err(|error| RuntimeError::Archive(error.to_string()))?;
        let path = entry
            .path()
            .map_err(|error| RuntimeError::Archive(error.to_string()))?
            .into_owned();
        // JDK archives only use directories and regular files; links are
        // skipped so a crafted archive cannot point outside the runtime.
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let size = entry.size();
        if size > budget {
            return Err(RuntimeError::Archive(
                "archive expands past the extraction limit".to_string(),
            ));
        }
        budget -= size;
        let Some(relative) = strip_top(&path.to_string_lossy()) else {
            continue;
        };
        let dest = home.join(&relative);
        if !dest.starts_with(home) {
            return Err(RuntimeError::UnsafeEntry(relative.to_string_lossy().into_owned()));
        }
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;
        }
        entry.unpack(&dest).map_err(|error| io_error(&dest, error))?;
        written += 1;
    }
    Ok(written)
}

/// Drop the leading `jdk-17.0.20.1+1/` component every archive carries.
fn strip_top(path: &str) -> Option<PathBuf> {
    if path.starts_with('/') || path.starts_with('\\') {
        return None;
    }
    let mut components = Path::new(path).components();
    match components.next()? {
        std::path::Component::Normal(_) => {}
        _ => return None,
    }
    let mut out = PathBuf::new();
    for component in components {
        match component {
            std::path::Component::Normal(part) => out.push(part),
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

fn io_error(path: &Path, source: io::Error) -> RuntimeError {
    RuntimeError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_the_archive_root() {
        assert_eq!(
            strip_top("jdk-17.0.20.1+1/bin/java").as_deref(),
            Some(Path::new("bin/java"))
        );
        assert_eq!(strip_top("jdk-17/"), None);
        assert_eq!(strip_top("../evil/bin/java"), None);
        assert_eq!(strip_top("jdk-17/../../evil"), None);
        assert_eq!(strip_top("/jdk-17/bin/java"), None);
    }

    #[test]
    fn reads_components() {
        assert_eq!(component(21), "adoptium-21");
        assert_eq!(major_of("adoptium-21"), Some(21));
        assert_eq!(major_of("java-runtime-gamma"), None);
    }

    #[test]
    fn parses_assets() {
        let body = br#"[{"release_name":"jdk-21.0.9+10","binary":{"package":{
            "link":"https://example/jdk.tar.gz","checksum":"ab","name":"jdk.tar.gz","size":123}}}]"#;
        let assets: Vec<Asset> = serde_json::from_slice(body).unwrap();
        assert_eq!(assets[0].release_name, "jdk-21.0.9+10");
        assert_eq!(assets[0].binary.package.size, 123);
        assert_eq!(assets[0].binary.package.checksum.as_deref(), Some("ab"));
    }
}
