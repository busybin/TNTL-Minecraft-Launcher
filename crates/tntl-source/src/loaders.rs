//! Fabric, Quilt and Legacy Fabric loader metadata.
//!
//! A loader is installed by writing its profile JSON into `versions/<id>/`
//! and letting `inheritsFrom` pull the vanilla version underneath it. The
//! profile only adds the loader's libraries (with `url` instead of a
//! `downloads` block), its main class and its JVM arguments.

use serde::Deserialize;
use tntl_model::VersionJson;

use crate::endpoints;
use crate::http::{Http, SourceError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LoaderKind {
    Fabric,
    Quilt,
    LegacyFabric,
    Forge,
    NeoForge,
}

impl LoaderKind {
    pub const ALL: [LoaderKind; 5] = [
        LoaderKind::Fabric,
        LoaderKind::Quilt,
        LoaderKind::LegacyFabric,
        LoaderKind::Forge,
        LoaderKind::NeoForge,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Self::Fabric => "fabric",
            Self::Quilt => "quilt",
            Self::LegacyFabric => "legacyfabric",
            Self::Forge => "forge",
            Self::NeoForge => "neoforge",
        }
    }

    pub fn from_key(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "fabric" => Some(Self::Fabric),
            "quilt" => Some(Self::Quilt),
            "legacyfabric" => Some(Self::LegacyFabric),
            "forge" => Some(Self::Forge),
            "neoforge" => Some(Self::NeoForge),
            _ => None,
        }
    }

    /// Forge and NeoForge publish installer jars; `tntl-forge` drives them.
    pub fn is_installer(self) -> bool {
        matches!(self, Self::Forge | Self::NeoForge)
    }

    fn meta(self) -> Option<&'static str> {
        match self {
            Self::Fabric => Some(endpoints::FABRIC_META),
            Self::Quilt => Some(endpoints::QUILT_META),
            Self::LegacyFabric => Some(endpoints::LEGACY_FABRIC_META),
            _ => None,
        }
    }

    fn maven(self) -> Option<&'static str> {
        match self {
            Self::Fabric => Some(endpoints::FABRIC_MAVEN),
            Self::Quilt => Some(endpoints::QUILT_MAVEN),
            Self::LegacyFabric => Some(endpoints::LEGACY_FABRIC_MAVEN),
            _ => None,
        }
    }
}

impl std::fmt::Display for LoaderKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.key())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LoaderVersion {
    pub version: String,
    pub stable: bool,
}

#[derive(Debug, Clone)]
pub struct LoaderProfile {
    pub json: VersionJson,
    pub bytes: Vec<u8>,
}

#[derive(Deserialize)]
struct Entry<L> {
    loader: L,
}

#[derive(Deserialize)]
struct FabricLoader {
    version: String,
    #[serde(default)]
    stable: bool,
}

#[derive(Deserialize)]
struct QuiltLoader {
    version: String,
}

/// Quilt metadata has no stability flag; prereleases carry a suffix.
fn quilt_stable(version: &str) -> bool {
    let lowered = version.to_ascii_lowercase();
    !lowered.contains("beta") && !lowered.contains("alpha") && !lowered.contains("rc") && !lowered.contains("pre")
}

impl Http {
    /// Loader builds published for `game`, newest first.
    pub fn loader_versions(&self, kind: LoaderKind, game: &str) -> Result<Vec<LoaderVersion>, SourceError> {
        let meta = kind
            .meta()
            .ok_or_else(|| SourceError::Loader(format!("{kind} is installed from its own installer")))?;
        let url = format!("{meta}/versions/loader/{game}");
        // Legacy Fabric answers 400 for a game it never supported (it stops at
        // 1.13.2). That is "no builds", not a broken launcher.
        let body = match self.get_bytes(&url, 4 * 1024 * 1024) {
            Ok(body) => body,
            Err(SourceError::Status { status: 400 | 404, .. }) => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let versions = match kind {
            LoaderKind::Fabric | LoaderKind::LegacyFabric => serde_json::from_slice::<Vec<Entry<FabricLoader>>>(&body)
                .map_err(|error| decode_error(&url, error))?
                .into_iter()
                .map(|entry| LoaderVersion {
                    version: entry.loader.version,
                    stable: entry.loader.stable,
                })
                .collect::<Vec<_>>(),
            LoaderKind::Quilt => serde_json::from_slice::<Vec<Entry<QuiltLoader>>>(&body)
                .map_err(|error| decode_error(&url, error))?
                .into_iter()
                .map(|entry| LoaderVersion {
                    stable: quilt_stable(&entry.loader.version),
                    version: entry.loader.version,
                })
                .collect(),
            _ => {
                return Err(SourceError::Loader(format!(
                    "{kind} is installed from its own installer"
                )));
            }
        };
        Ok(versions)
    }

    /// Ready-made version JSON for `game` + loader build.
    pub fn loader_profile(&self, kind: LoaderKind, game: &str, loader: &str) -> Result<LoaderProfile, SourceError> {
        let meta = kind
            .meta()
            .ok_or_else(|| SourceError::Loader(format!("{kind} is installed from its own installer")))?;
        let url = format!("{meta}/versions/loader/{game}/{loader}/profile/json");
        let bytes = self.get_bytes(&url, 4 * 1024 * 1024)?;
        let json: VersionJson = serde_json::from_slice(&bytes).map_err(|error| decode_error(&url, error))?;
        if json.id.is_empty() {
            return Err(SourceError::Loader(format!("{url} did not return a version id")));
        }
        if json.libraries.iter().any(|library| library.url.is_none()) {
            // Profiles are expected to carry explicit maven roots; without one
            // the libraries would silently fall back to Mojang's repository.
            let fallback = kind
                .maven()
                .ok_or_else(|| SourceError::Loader(format!("{kind} has no maven repository")))?;
            let mut json = json;
            for library in &mut json.libraries {
                if library.url.is_none() {
                    library.url = Some(fallback.to_string());
                }
            }
            let bytes = serde_json::to_vec(&json).map_err(|error| decode_error(&url, error))?;
            return Ok(LoaderProfile { json, bytes });
        }
        Ok(LoaderProfile { json, bytes })
    }
}

fn decode_error(url: &str, error: serde_json::Error) -> SourceError {
    SourceError::Loader(format!("invalid loader metadata from {url}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fabric_list() {
        let body = br#"[
          {"loader":{"version":"0.16.9","stable":true,"maven":"net.fabricmc:fabric-loader:0.16.9"}},
          {"loader":{"version":"0.17.0-beta.1","stable":false}}
        ]"#;
        let entries: Vec<Entry<FabricLoader>> = serde_json::from_slice(body).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].loader.version, "0.16.9");
        assert!(entries[0].loader.stable);
        assert!(!entries[1].loader.stable);
    }

    #[test]
    fn parses_quilt_list_without_stable_flag() {
        let body = br#"[{"loader":{"version":"0.27.0"}},{"loader":{"version":"0.28.0-beta.1"}}]"#;
        let entries: Vec<Entry<QuiltLoader>> = serde_json::from_slice(body).unwrap();
        assert_eq!(entries[0].loader.version, "0.27.0");
        assert!(quilt_stable(&entries[0].loader.version));
        assert!(!quilt_stable(&entries[1].loader.version));
    }

    #[test]
    fn parses_loader_profile() {
        let body = br#"{
          "id":"fabric-loader-0.16.9-1.20.1",
          "inheritsFrom":"1.20.1",
          "mainClass":"net.fabricmc.loader.impl.launch.knot.KnotClient",
          "libraries":[{"name":"org.ow2.asm:asm:9.7.1","url":"https://maven.fabricmc.net/","sha1":"f0ed"}]
        }"#;
        let json: VersionJson = serde_json::from_slice(body).unwrap();
        assert_eq!(json.id, "fabric-loader-0.16.9-1.20.1");
        assert_eq!(json.inherits_from.as_deref(), Some("1.20.1"));
        assert_eq!(json.libraries[0].sha1.as_deref(), Some("f0ed"));
        assert_eq!(json.libraries[0].url.as_deref(), Some(endpoints::FABRIC_MAVEN));
    }
}
