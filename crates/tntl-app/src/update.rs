//! Updates from the project's GitHub releases.
//!
//! The launcher never installs anything by itself: it checks the release
//! feed, downloads the asset that matches this platform into the config
//! directory, verifies the size and (when GitHub reports one) the SHA-256
//! digest, and hands the file to the shell, which knows how to replace a
//! running executable.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tntl_source::{DownloadOutcome, DownloadRequest, Http};

/// Repository the launcher updates from.
pub const REPO: &str = "busybin/TNTL-Minecraft-Launcher";
const API: &str = "https://api.github.com/repos";

/// Only these hosts may serve an update. `api.github.com` hands out the URLs;
/// everything else in the response is data, not something to fetch blindly.
const ASSET_HOSTS: [&str; 3] = [
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateAsset {
    pub name: String,
    pub url: String,
    pub size: u64,
    pub digest: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateRelease {
    pub tag: String,
    pub version: u32,
    pub title: String,
    pub notes: String,
    pub prerelease: bool,
    pub published_at: String,
    pub html_url: String,
    pub asset: Option<UpdateAsset>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateStatus {
    /// Integer version this build reports.
    pub current: u32,
    pub latest: u32,
    pub available: bool,
    pub release: Option<UpdateRelease>,
    /// Why no asset was picked, for the UI to explain.
    pub note: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawRelease {
    #[serde(default)]
    tag_name: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    assets: Vec<RawAsset>,
}

#[derive(Debug, Deserialize)]
struct RawAsset {
    #[serde(default)]
    name: String,
    #[serde(default)]
    browser_download_url: String,
    #[serde(default)]
    size: u64,
    /// `sha256:<hex>` on newer API responses.
    #[serde(default)]
    digest: Option<String>,
}

/// Current integer version of this build (`V5` style versions live in the major).
pub fn current_version() -> u32 {
    env!("CARGO_PKG_VERSION_MAJOR").parse().unwrap_or(0)
}

/// Ask GitHub for the newest release. Pre-releases count: every build of this
/// project is published as one.
pub fn check() -> Result<UpdateStatus> {
    let http = Http::new().context("cannot create the HTTP client")?;
    let url = format!("{API}/{REPO}/releases?per_page=20");
    let body = http
        .get_bytes_with(
            &url,
            2 * 1024 * 1024,
            &[("Accept", "application/vnd.github+json"), ("User-Agent", "TNTL")],
        )
        .context("cannot read the release list")?;
    let raw: Vec<RawRelease> = serde_json::from_slice(&body).context("unexpected release list")?;
    let current = current_version();
    let mut best: Option<(u32, &RawRelease)> = None;
    for release in raw.iter().filter(|release| !release.draft) {
        let Some(version) = parse_version(&release.tag_name) else {
            continue;
        };
        if best.is_none_or(|(best_version, _)| version > best_version) {
            best = Some((version, release));
        }
    }
    let Some((latest, release)) = best else {
        return Ok(UpdateStatus {
            current,
            latest: current,
            available: false,
            release: None,
            note: Some("no release found".to_string()),
        });
    };
    let (asset, note) = match pick_asset(&release.assets) {
        Some(asset) => (Some(asset), None),
        None => (
            None,
            Some(format!(
                "release {} has no build for {}",
                release.tag_name,
                std::env::consts::OS
            )),
        ),
    };
    Ok(UpdateStatus {
        current,
        latest,
        available: latest > current,
        release: Some(UpdateRelease {
            tag: release.tag_name.clone(),
            version: latest,
            title: release
                .name
                .clone()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| release.tag_name.clone()),
            notes: release.body.clone().unwrap_or_default(),
            prerelease: release.prerelease,
            published_at: release.published_at.clone().unwrap_or_default(),
            html_url: release.html_url.clone().unwrap_or_default(),
            asset,
        }),
        note,
    })
}

/// `V5`, `v5`, `5` → `5`.
pub fn parse_version(tag: &str) -> Option<u32> {
    let digits: String = tag
        .trim()
        .trim_start_matches(['v', 'V'])
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

/// Build for this platform. Windows portable first (no installer, no admin),
/// then the NSIS package; Linux AppImage first, then the deb.
fn pick_asset(assets: &[RawAsset]) -> Option<UpdateAsset> {
    let wanted: &[&str] = if cfg!(windows) {
        &["win64.exe", "setup.exe"]
    } else if cfg!(target_os = "linux") {
        &[".AppImage", ".deb"]
    } else {
        &[]
    };
    for suffix in wanted {
        let found = assets.iter().find(|asset| {
            asset.name.ends_with(suffix) && safe_url(&asset.browser_download_url) && !asset.name.is_empty()
        });
        if let Some(asset) = found {
            return Some(UpdateAsset {
                name: asset.name.clone(),
                url: asset.browser_download_url.clone(),
                size: asset.size,
                digest: asset.digest.clone(),
            });
        }
    }
    None
}

/// HTTPS on a GitHub host, pinned to this repository. A release asset URL is
/// `https://github.com/<owner>/<repo>/releases/download/...`; accepting the
/// host alone would let any repository be an update source.
pub fn safe_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let host = rest.split('/').next().unwrap_or_default();
    if !ASSET_HOSTS.contains(&host) {
        return false;
    }
    if host == "github.com" {
        let path = rest.split_once('/').map(|(_, path)| path).unwrap_or_default();
        return path.starts_with(&format!("{REPO}/releases/download/"));
    }
    // GitHub's CDN hosts are opaque, but the link came from the API response
    // for our repository.
    true
}

/// Download the asset into `dir` and verify it. Returns the file path.
pub fn download(asset: &UpdateAsset, dir: &Path, progress: &mut dyn FnMut(u64)) -> Result<PathBuf> {
    if !safe_url(&asset.url) {
        bail!("refusing to download from {}", asset.url);
    }
    let name = asset.name.rsplit(['/', '\\']).next().unwrap_or_default().to_string();
    if name.is_empty() || name.contains("..") {
        bail!("unusable update file name");
    }
    let dest = dir.join(&name);
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    // Without a digest the only check would be a size the same response
    // provides; require at least one of them and never reuse an old file.
    let digest = asset.digest.as_deref().and_then(|value| value.strip_prefix("sha256:"));
    if digest.is_none() && asset.size == 0 {
        bail!("the release does not publish a checksum for this file");
    }
    if dest.exists() {
        let _ = std::fs::remove_file(&dest);
    }
    let http = Http::new().context("cannot create the HTTP client")?;
    let mut request = DownloadRequest::new(&asset.url, &dest);
    if asset.size > 0 {
        request = request.with_size(asset.size);
    }
    let outcome = http
        .download(&request, progress)
        .context("cannot download the update")?;
    if matches!(outcome, DownloadOutcome::AlreadyValid) && asset.size > 0 {
        // Already there from an earlier attempt; still verify below.
    }
    if let Some(expected) = digest.filter(|value| !value.is_empty()) {
        let actual = sha256_file(&dest)?;
        if !actual.eq_ignore_ascii_case(expected) {
            // Never keep a file that does not match what GitHub published.
            let _ = std::fs::remove_file(&dest);
            bail!("downloaded update does not match its SHA-256; deleted");
        }
    }
    let size = std::fs::metadata(&dest).map(|meta| meta.len()).unwrap_or_default();
    if asset.size > 0 && size != asset.size {
        bail!("downloaded update is {size} bytes, expected {}", asset.size);
    }
    Ok(dest)
}

fn sha256_file(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_use_integer_versions() {
        assert_eq!(parse_version("V5"), Some(5));
        assert_eq!(parse_version("v12"), Some(12));
        assert_eq!(parse_version("7"), Some(7));
        assert_eq!(parse_version("V5 (beta test)"), Some(5));
        assert_eq!(parse_version("nightly"), None);
    }

    #[test]
    fn only_github_hosts_are_accepted() {
        assert!(safe_url(
            "https://github.com/busybin/TNTL-Minecraft-Launcher/releases/download/V5/x.exe"
        ));
        assert!(safe_url("https://objects.githubusercontent.com/abc"));
        assert!(!safe_url("http://github.com/x"));
        assert!(!safe_url("https://evil.example/x.exe"));
        assert!(!safe_url("https://github.com.evil.example/x"));
        assert!(!safe_url("file:///etc/passwd"));
    }
}
