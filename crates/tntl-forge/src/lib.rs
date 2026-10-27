//! Forge and NeoForge installers.
//!
//! Unlike Fabric and Quilt, neither publishes a ready-made profile JSON. Their
//! installer jar carries `install_profile.json` (processor list and inputs), the
//! version JSON to write into `versions/<id>/`, and the binary patches. The
//! client is installed by running those processors with a Java runtime and
//! placing the patched jar they produce at `versions/<id>/<id>.jar`.
//!
//! Only the official maven repositories are used.

#![forbid(unsafe_code)]

mod installer;

use thiserror::Error;
use tntl_source::{Http, LoaderVersion, SourceError};
use tntl_store::RootLayout;

pub use installer::ForgeInstall;

/// Forge or NeoForge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ForgeKind {
    Forge,
    NeoForge,
}

/// File inside `versions/<id>/` recording what the processor chain produced.
pub const OUTPUTS_FILE: &str = ".tntl-forge.json";

pub const FORGE_MAVEN: &str = "https://maven.minecraftforge.net/";
pub const NEOFORGE_MAVEN: &str = "https://maven.neoforged.net/releases/";

impl ForgeKind {
    pub fn key(self) -> &'static str {
        match self {
            Self::Forge => "forge",
            Self::NeoForge => "neoforge",
        }
    }

    pub fn from_key(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "forge" => Some(Self::Forge),
            "neoforge" => Some(Self::NeoForge),
            _ => None,
        }
    }

    /// Forge kind behind a `LoaderKind`, when it is one of the installers.
    pub fn from_loader(kind: tntl_source::LoaderKind) -> Option<Self> {
        match kind {
            tntl_source::LoaderKind::Forge => Some(Self::Forge),
            tntl_source::LoaderKind::NeoForge => Some(Self::NeoForge),
            _ => None,
        }
    }

    /// NeoForge for 1.20.1 shipped as `net.neoforged:forge` using Forge's
    /// `1.20.1-<build>` numbering; everything newer is `net.neoforged:neoforge`.
    fn legacy_neoforge(self, game: &str) -> bool {
        self == Self::NeoForge && game == "1.20.1"
    }

    fn maven_root(self) -> &'static str {
        match self {
            Self::Forge => FORGE_MAVEN,
            Self::NeoForge => NEOFORGE_MAVEN,
        }
    }

    /// Directory of the artifact inside the maven repository.
    fn artifact_dir(self, game: &str) -> &'static str {
        match self {
            Self::Forge => "net/minecraftforge/forge",
            Self::NeoForge if self.legacy_neoforge(game) => "net/neoforged/forge",
            Self::NeoForge => "net/neoforged/neoforge",
        }
    }

    /// `<artifact>-<version>` part of the installer file name.
    fn artifact(self, game: &str) -> &'static str {
        match self {
            Self::Forge => "forge",
            Self::NeoForge if self.legacy_neoforge(game) => "forge",
            Self::NeoForge => "neoforge",
        }
    }

    /// Maven metadata listing every published build of this loader.
    fn metadata_url(self, game: &str) -> String {
        format!("{}{}/maven-metadata.xml", self.maven_root(), self.artifact_dir(game))
    }

    /// Maven version the installer path expects. Pack indexes and hand-typed
    /// builds often leave out the game prefix Forge and NeoForge 1.20.1 want.
    pub fn maven_version(self, game: &str, version: &str) -> String {
        let prefix = format!("{game}-");
        let prefixed = self == Self::Forge || self.legacy_neoforge(game);
        if prefixed && !version.starts_with(&prefix) {
            format!("{prefix}{version}")
        } else {
            version.to_string()
        }
    }
}

/// Installer jar URL for one build; `version` is the full maven version.
pub fn installer_url(kind: ForgeKind, game: &str, version: &str) -> String {
    format!(
        "{}{}/{}/{}-{}-installer.jar",
        kind.maven_root(),
        kind.artifact_dir(game),
        version,
        kind.artifact(game),
        version
    )
}

/// Maven versions to try for one build. Forge also republishes some 1.7.10 and
/// 1.8.9 builds with a trailing game suffix (`1.7.10-10.13.4.1614-1.7.10`), and
/// hand-typed build numbers do not carry it.
pub fn installer_versions(game: &str, version: &str) -> Vec<String> {
    let suffix = format!("-{game}");
    if version.ends_with(&suffix) {
        return vec![version.to_string()];
    }
    // Only the 1.7/1.8 era republished builds with a trailing game suffix.
    // Modern builds already start with the game version and must not be
    // probed as `1.20.1-47.2.0-1.20.1`.
    let legacy = game.starts_with("1.7.") || game.starts_with("1.8.");
    if legacy && version.starts_with(&format!("{game}-")) {
        vec![version.to_string(), format!("{version}{suffix}")]
    } else {
        vec![version.to_string()]
    }
}

impl std::fmt::Display for ForgeKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.key())
    }
}

#[derive(Debug, Error)]
pub enum ForgeError {
    #[error(transparent)]
    Source(#[from] SourceError),
    #[error(transparent)]
    Store(#[from] tntl_store::StoreError),
    #[error("io error at {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid installer for {version}: {message}")]
    Profile { version: String, message: String },
    #[error("processor {processor} failed ({status}):\n{output}")]
    Processor {
        processor: String,
        status: String,
        output: String,
    },
    #[error("installer data is inconsistent: {0}")]
    Inconsistent(String),
    #[error("installer hash mismatch for {path}: expected {expected}, got {actual}")]
    Hash {
        path: std::path::PathBuf,
        expected: String,
        actual: String,
    },
    #[error("unexpected installer layout: {0}")]
    Zip(String),
}

/// SHA-1 of every file the processor chain created, relative to the root.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ProcessorOutputs {
    #[serde(default)]
    pub files: std::collections::BTreeMap<String, String>,
}

/// Files of an installed loader whose SHA-1 no longer matches the record the
/// installer wrote. Version JSONs rarely hash the patched client jar, so this
/// is the only way a repair pass can tell a broken install from a good one.
/// Installs made before the record existed report nothing.
pub fn broken_outputs(roots: &RootLayout, version_id: &str) -> Result<Vec<std::path::PathBuf>, ForgeError> {
    let path = roots.version_dir(version_id).join(OUTPUTS_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(ForgeError::Io { path, source });
        }
    };
    let outputs: ProcessorOutputs = serde_json::from_slice(&bytes).map_err(|error| ForgeError::Profile {
        version: version_id.to_string(),
        message: format!("{OUTPUTS_FILE}: {error}"),
    })?;
    Ok(outputs
        .files
        .iter()
        .filter(|(relative, sha1)| {
            !tntl_store::sha1_file(&roots.root.join(relative))
                .map(|actual| actual.eq_ignore_ascii_case(sha1))
                .unwrap_or(false)
        })
        .map(|(relative, _)| roots.root.join(relative))
        .collect())
}

/// Builds matching `game`, newest first, with `version` set to the maven
/// version the installer URL expects.
pub fn versions(http: &Http, kind: ForgeKind, game: &str) -> Result<Vec<LoaderVersion>, ForgeError> {
    let url = kind.metadata_url(game);
    let body = http.get_bytes(&url, 4 * 1024 * 1024)?;
    let text = String::from_utf8_lossy(&body);
    let prefix = match kind {
        ForgeKind::Forge => format!("{game}-"),
        ForgeKind::NeoForge if kind.legacy_neoforge(game) => format!("{game}-"),
        ForgeKind::NeoForge => neoforge_prefix(game).ok_or_else(|| ForgeError::Profile {
            version: game.to_string(),
            message: "NeoForge has no build numbering for this game version".to_string(),
        })?,
    };
    let mut versions: Vec<LoaderVersion> = maven_versions(&text)
        .into_iter()
        .filter(|version| version.starts_with(&prefix))
        .map(|version| LoaderVersion { stable: true, version })
        .collect();
    // Metadata lists 1.7.10 and 1.12 builds in the order Forge re-published
    // them, not in build order, so sort by the numeric build instead.
    versions.sort_by(|left, right| {
        build_key(&right.version, &prefix)
            .cmp(&build_key(&left.version, &prefix))
            .then_with(|| right.version.cmp(&left.version))
    });
    Ok(versions)
}

/// Numeric part of a build: `1.7.10-10.13.4.1614-1.7.10` -> [10, 13, 4, 1614].
fn build_key(version: &str, prefix: &str) -> Vec<u64> {
    let build = version.strip_prefix(prefix).unwrap_or(version);
    let build = build.split('-').next().unwrap_or(build);
    build.split('.').map_while(|part| part.parse().ok()).collect()
}

/// NeoForge build prefix for a game version: 1.21.1 -> `21.1.`, 1.21 -> `21.0.`.
fn neoforge_prefix(game: &str) -> Option<String> {
    let rest = game.strip_prefix("1.")?;
    let mut parts = rest.split('.');
    let major = parts.next()?;
    match (parts.next(), parts.next()) {
        (Some(minor), None) => Some(format!("{major}.{minor}.")),
        (None, _) => Some(format!("{major}.0.")),
        _ => None,
    }
}

/// `<version>` entries of a maven `maven-metadata.xml`, in file order.
fn maven_versions(text: &str) -> Vec<String> {
    let mut versions = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find("<version>") {
        let after = &rest[open + "<version>".len()..];
        let Some(close) = after.find("</version>") else {
            break;
        };
        let version = after[..close].trim();
        if !version.is_empty() {
            versions.push(version.to_string());
        }
        rest = &after[close + "</version>".len()..];
    }
    versions
}

/// Points at a launcher data directory and a Java runtime.
pub struct ForgeInstaller<'a> {
    http: &'a Http,
    roots: RootLayout,
    java: std::path::PathBuf,
}

impl<'a> ForgeInstaller<'a> {
    pub fn new(http: &'a Http, roots: RootLayout, java: impl Into<std::path::PathBuf>) -> Self {
        Self {
            http,
            roots,
            java: java.into(),
        }
    }

    /// Builds published for `game`, newest first.
    pub fn versions(&self, kind: ForgeKind, game: &str) -> Result<Vec<LoaderVersion>, ForgeError> {
        versions(self.http, kind, game)
    }

    /// Downloads the installer, runs the client processors and writes both the
    /// version JSON and the patched client jar. The vanilla version must
    /// already be installed: the processors patch its client jar.
    pub fn install(
        &self,
        kind: ForgeKind,
        game: &str,
        version: &str,
        progress: &(dyn Fn(tntl_source::InstallEvent) + Send + Sync),
    ) -> Result<ForgeInstall, ForgeError> {
        installer::run(self, kind, game, version, progress)
    }

    fn java(&self) -> &std::path::Path {
        &self.java
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broken_outputs_flags_changed_files() {
        let dir = std::env::temp_dir().join(format!("tntl-forge-outputs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let roots = RootLayout::new(&dir);
        let jar = roots.client_jar("1.20.1-forge-47.2.0");
        std::fs::create_dir_all(jar.parent().unwrap()).unwrap();
        std::fs::write(&jar, b"patched").unwrap();
        let record = serde_json::json!({
            "files": { "versions/1.20.1-forge-47.2.0/1.20.1-forge-47.2.0.jar": tntl_store::sha1_bytes(b"patched") }
        });
        std::fs::write(
            roots.version_dir("1.20.1-forge-47.2.0").join(OUTPUTS_FILE),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        assert!(broken_outputs(&roots, "1.20.1-forge-47.2.0").unwrap().is_empty());
        std::fs::write(&jar, b"replaced by something else").unwrap();
        assert_eq!(
            broken_outputs(&roots, "1.20.1-forge-47.2.0").unwrap(),
            vec![jar.clone()]
        );
        std::fs::remove_file(&jar).unwrap();
        assert_eq!(broken_outputs(&roots, "1.20.1-forge-47.2.0").unwrap(), vec![jar]);
        // Installs made before the record existed are not flagged.
        std::fs::remove_file(roots.version_dir("1.20.1-forge-47.2.0").join(OUTPUTS_FILE)).unwrap();
        assert!(broken_outputs(&roots, "1.20.1-forge-47.2.0").unwrap().is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reads_maven_versions() {
        let text = "<metadata><versioning><versions>\n\
                    <version>1.20.1-47.1.106</version>\n\
                    <version>21.1.80</version>\n\
                    </versions></versioning></metadata>";
        assert_eq!(maven_versions(text), ["1.20.1-47.1.106", "21.1.80"]);
        assert!(maven_versions("<metadata/>").is_empty());
    }

    #[test]
    fn compares_build_numbers() {
        let prefix = "1.7.10-";
        assert!(build_key("1.7.10-10.13.4.1614-1.7.10", prefix) > build_key("1.7.10-10.13.0.1156", prefix));
        assert!(build_key("21.1.264", "21.1.") > build_key("21.1.80", "21.1."));
        assert_eq!(build_key("1.7.10-10.13.4.1614-1.7.10", prefix), [10, 13, 4, 1614]);
    }

    #[test]
    fn tries_the_trailing_game_suffix() {
        assert_eq!(
            installer_versions("1.7.10", "1.7.10-10.13.4.1614"),
            ["1.7.10-10.13.4.1614", "1.7.10-10.13.4.1614-1.7.10"]
        );
        assert_eq!(
            installer_versions("1.7.10", "1.7.10-10.13.4.1614-1.7.10"),
            ["1.7.10-10.13.4.1614-1.7.10"]
        );
        assert_eq!(installer_versions("1.20.1", "1.20.1-47.2.0"), ["1.20.1-47.2.0"]);
        assert_eq!(installer_versions("1.20.1", "47.2.0"), ["47.2.0"]);
    }

    #[test]
    fn builds_installer_urls() {
        assert_eq!(
            installer_url(ForgeKind::Forge, "1.20.1", "1.20.1-47.2.0"),
            "https://maven.minecraftforge.net/net/minecraftforge/forge/1.20.1-47.2.0/forge-1.20.1-47.2.0-installer.jar"
        );
        assert_eq!(
            installer_url(ForgeKind::NeoForge, "1.21.1", "21.1.80"),
            "https://maven.neoforged.net/releases/net/neoforged/neoforge/21.1.80/neoforge-21.1.80-installer.jar"
        );
        assert_eq!(
            installer_url(ForgeKind::NeoForge, "1.20.1", "1.20.1-47.1.106"),
            "https://maven.neoforged.net/releases/net/neoforged/forge/1.20.1-47.1.106/forge-1.20.1-47.1.106-installer.jar"
        );
    }

    #[test]
    fn derives_neoforge_build_prefixes() {
        assert_eq!(neoforge_prefix("1.21.1").as_deref(), Some("21.1."));
        assert_eq!(neoforge_prefix("1.21").as_deref(), Some("21.0."));
        assert_eq!(neoforge_prefix("1.20.2").as_deref(), Some("20.2."));
        assert_eq!(neoforge_prefix("1.20.6").as_deref(), Some("20.6."));
        assert_eq!(neoforge_prefix("23w14a"), None);
    }
}
