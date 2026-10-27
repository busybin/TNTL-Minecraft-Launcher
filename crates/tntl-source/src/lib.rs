//! Official Mojang sources: manifest, version metadata and artifact downloads.

#![forbid(unsafe_code)]

pub mod cancel;
pub mod endpoints;
mod http;
pub mod install;
pub mod loaders;
pub mod network;

pub use http::{DownloadOutcome, DownloadRequest, Http, SourceError};
pub use install::{InstallEvent, InstallOptions, InstallReport, Installer};
pub use loaders::{LoaderKind, LoaderProfile, LoaderVersion};
pub use network::{NetworkConfig, ProxyMode, network_config, set_network_config};

use tntl_model::{VersionJson, VersionManifest, VersionSummary};

impl Http {
    pub fn version_manifest(&self) -> Result<VersionManifest, SourceError> {
        self.get_json(endpoints::VERSION_MANIFEST)
    }

    pub fn fetch_version(&self, summary: &VersionSummary) -> Result<VersionJson, SourceError> {
        self.get_json(&summary.url)
    }

    /// Look a version up by id in the manifest.
    pub fn find_version(&self, manifest: &VersionManifest, id: &str) -> Result<VersionSummary, SourceError> {
        manifest
            .versions
            .iter()
            .find(|version| version.id == id)
            .cloned()
            .ok_or_else(|| SourceError::UnknownVersion(id.to_string()))
    }
}
