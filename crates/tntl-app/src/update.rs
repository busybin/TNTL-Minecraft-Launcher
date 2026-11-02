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
    /// Optional detached Ed25519 signature (`<asset>.sig`, hex).
    #[serde(default)]
    pub signature_url: Option<String>,
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

/// Which build this copy should install. A copy installed from a package (or
/// living in a system directory) cannot replace itself with an AppImage, so it
/// updates from the `.deb` instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InstallKind {
    /// Pick by platform alone.
    #[default]
    Auto,
    AppImage,
    /// Installed by the system package manager.
    Package,
    /// Installed by the NSIS setup.
    Setup,
    /// Stand-alone executable in a user directory.
    Portable,
}

/// Ask GitHub for the newest release. Pre-releases count: every build of this
/// project is published as one.
pub fn check() -> Result<UpdateStatus> {
    check_with(InstallKind::Auto)
}

pub fn check_with(kind: InstallKind) -> Result<UpdateStatus> {
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
    let (asset, note) = match pick_asset(&release.assets, kind) {
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

/// Build for this platform and this kind of install. Windows portable first (no
/// installer, no admin), then the NSIS package; Linux AppImage first, then the
/// deb. A copy installed by the package manager must never be offered an
/// AppImage: replacing `/usr/bin/...` is not something it can do.
fn pick_asset(assets: &[RawAsset], kind: InstallKind) -> Option<UpdateAsset> {
    let wanted: &[&str] = if cfg!(windows) {
        match kind {
            InstallKind::Setup => &["setup.exe"],
            _ => &["win64.exe", "setup.exe"],
        }
    } else if cfg!(target_os = "linux") {
        match kind {
            InstallKind::AppImage => &[".AppImage"],
            InstallKind::Package => &[".deb"],
            _ => &[".AppImage", ".deb"],
        }
    } else {
        &[]
    };
    for suffix in wanted {
        let found = assets.iter().find(|asset| {
            asset.name.ends_with(suffix) && safe_url(&asset.browser_download_url) && !asset.name.is_empty()
        });
        if let Some(asset) = found {
            let signature_url = assets
                .iter()
                .find(|candidate| {
                    candidate.name == format!("{}.sig", asset.name) && safe_url(&candidate.browser_download_url)
                })
                .map(|candidate| candidate.browser_download_url.clone());
            return Some(UpdateAsset {
                name: asset.name.clone(),
                url: asset.browser_download_url.clone(),
                size: asset.size,
                digest: asset.digest.clone(),
                signature_url,
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
    // Authenticity is opt-in: a build with TNTL_UPDATE_PUBKEY set refuses an
    // update that has no valid detached signature. Without a key the SHA-256
    // from GitHub still catches corruption, but not a compromised release.
    if let Some(public_key) = update_public_key() {
        let public_key = decode_hex(public_key.trim())?;
        let Some(signature_url) = asset.signature_url.as_deref().filter(|url| !url.is_empty()) else {
            bail!("this build requires a signed update, but the release has no .sig asset");
        };
        if !safe_url(signature_url) {
            bail!("refusing to download the signature from {signature_url}");
        }
        let signature = Http::new()
            .context("cannot create the HTTP client")?
            .get_bytes_with(signature_url, 8 * 1024, &[("User-Agent", "TNTL")])
            .context("cannot download the update signature")?;
        verify_signature(&dest, &public_key, &signature)?;
    }
    Ok(dest)
}

/// Signature is Ed25519 over the UTF-8 string `sha256:<hex digest>`.
fn verify_signature(path: &Path, public_key: &[u8], signature: &[u8]) -> Result<()> {
    use ring::signature::{ED25519, UnparsedPublicKey};
    let signature = decode_hex(std::str::from_utf8(signature).context("signature is not UTF-8")?.trim())?;
    let digest = format!("sha256:{}", sha256_file(path)?);
    UnparsedPublicKey::new(&ED25519, public_key)
        .verify(digest.as_bytes(), &signature)
        .map_err(|_| anyhow::anyhow!("the update signature does not match this build's public key"))
}

/// The release build embeds the key; debug builds may override it while
/// testing. A shipped binary ignores the variable so a local process cannot
/// swap the trust anchor of a published build.
fn update_public_key() -> Option<String> {
    let embedded = option_env!("TNTL_UPDATE_PUBKEY").map(str::to_string);
    let configured = if cfg!(debug_assertions) {
        std::env::var("TNTL_UPDATE_PUBKEY").ok().or(embedded)
    } else {
        embedded
    };
    configured.filter(|value| !value.trim().is_empty())
}

fn decode_hex(value: &str) -> Result<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        bail!("hex value has an odd length");
    }
    (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&value[index..index + 2], 16).map_err(|error| anyhow::anyhow!("{error}")))
        .collect()
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
    fn verifies_a_detached_ed25519_signature() {
        use ring::rand::SystemRandom;
        use ring::signature::{Ed25519KeyPair, KeyPair};
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let key = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let path = std::env::temp_dir().join(format!("tntl-update-sig-{}", std::process::id()));
        std::fs::write(&path, b"release bytes").unwrap();
        let digest = format!("sha256:{}", sha256_file(&path).unwrap());
        let signature = key.sign(digest.as_bytes());
        verify_signature(
            &path,
            key.public_key().as_ref(),
            hex::encode(signature.as_ref()).as_bytes(),
        )
        .unwrap();
        // A different key must not verify.
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let other = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        assert!(
            verify_signature(
                &path,
                other.public_key().as_ref(),
                hex::encode(signature.as_ref()).as_bytes()
            )
            .is_err()
        );
        std::fs::remove_file(path).unwrap();
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

    #[cfg(target_os = "linux")]
    fn linux_asset(name: &str, url: &str) -> RawAsset {
        RawAsset {
            name: name.to_string(),
            browser_download_url: url.to_string(),
            size: 1,
            digest: None,
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn picks_the_linux_asset_with_its_signature() {
        let base = "https://github.com/busybin/TNTL-Minecraft-Launcher/releases/download/V9";
        let asset = format!("{base}/TNTL_9_amd64.deb");
        let signature = format!("{base}/TNTL_9_amd64.deb.sig");
        let assets = vec![
            linux_asset("TNTL_9_amd64.deb", &asset),
            linux_asset("TNTL_9_amd64.deb.sig", &signature),
        ];
        let picked = pick_asset(&assets, InstallKind::Package).expect("deb asset");
        assert_eq!(picked.name, "TNTL_9_amd64.deb");
        assert_eq!(picked.signature_url.as_deref(), Some(signature.as_str()));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn ignores_a_signature_from_another_host() {
        let base = "https://github.com/busybin/TNTL-Minecraft-Launcher/releases/download/V9";
        let assets = vec![
            linux_asset("TNTL_9_amd64.deb", &format!("{base}/TNTL_9_amd64.deb")),
            linux_asset("TNTL_9_amd64.deb.sig", "https://evil.example/TNTL_9_amd64.deb.sig"),
        ];
        let picked = pick_asset(&assets, InstallKind::Package).expect("deb asset");
        assert_eq!(picked.signature_url, None);
    }
}
