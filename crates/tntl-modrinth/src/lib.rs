//! Modrinth: project search, version lookup and dependency-aware installs.
//!
//! Only the documented v2 JSON API and the CDN links it returns are used. Every
//! downloaded jar is verified against the sha1 published by Modrinth.

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tntl_source::{DownloadOutcome, DownloadRequest, Http, SourceError, endpoints};

#[derive(Debug, Error)]
pub enum ModrinthError {
    #[error(transparent)]
    Source(#[from] SourceError),
    #[error("invalid response from {url}: {source}")]
    Json {
        url: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}")]
    Invalid(String),
}

/// What a search or a version list is restricted to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    /// Game version, e.g. `1.20.1`.
    pub game_version: String,
    /// Modrinth loader key: `fabric`, `quilt`, `forge` or `neoforge`.
    pub loader: String,
    /// Loader keys a build may carry instead of [`Self::loader`]. NeoForge
    /// 1.20.1 is a Forge fork and loads Forge mods, which Modrinth tags
    /// `forge` only.
    pub accept: Vec<String>,
}

impl Filter {
    pub fn new(game_version: impl Into<String>, loader: impl Into<String>) -> Self {
        let loader = loader.into();
        // Modrinth has no Legacy Fabric loader; those instances run ordinary
        // Fabric mods against the legacy game build.
        let loader = if loader.eq_ignore_ascii_case("legacyfabric") {
            "fabric".to_string()
        } else {
            loader
        };
        Self {
            game_version: game_version.into(),
            loader,
            accept: Vec::new(),
        }
    }

    /// A filter for project kinds that have no loader, e.g. resource packs.
    pub fn any_loader(game_version: impl Into<String>) -> Self {
        Self {
            game_version: game_version.into(),
            loader: String::new(),
            accept: Vec::new(),
        }
    }

    /// Also accept builds tagged for another loader.
    pub fn accepting(mut self, loader: impl Into<String>) -> Self {
        let loader = loader.into().to_ascii_lowercase();
        if !loader.is_empty() && loader != self.loader && !self.accept.contains(&loader) {
            self.accept.push(loader);
        }
        self
    }

    /// Loader keys a build may carry, own loader first.
    fn loaders(&self) -> Vec<&str> {
        let mut keys: Vec<&str> = Vec::new();
        if self.has_loader() {
            keys.push(self.loader.as_str());
        }
        keys.extend(self.accept.iter().map(String::as_str));
        keys
    }

    fn has_loader(&self) -> bool {
        !self.loader.is_empty()
    }

    /// An empty game version searches every game build: composite profiles
    /// name the pack version, which is not a Minecraft version at all.
    fn has_game_version(&self) -> bool {
        !self.game_version.trim().is_empty()
    }

    /// Whether a build fits this filter. Public so the version picker can offer
    /// only builds the instance can run.
    pub fn accepts(&self, version: &Version) -> bool {
        is_compatible(version, self)
    }
}

/// Ordering of a search page, mirroring Modrinth's `index` query parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchIndex {
    #[default]
    Relevance,
    Downloads,
    Follows,
    Newest,
    Updated,
}

impl SearchIndex {
    pub fn key(self) -> &'static str {
        match self {
            Self::Relevance => "relevance",
            Self::Downloads => "downloads",
            Self::Follows => "follows",
            Self::Newest => "newest",
            Self::Updated => "updated",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key.trim().to_ascii_lowercase().as_str() {
            "relevance" => Some(Self::Relevance),
            "downloads" => Some(Self::Downloads),
            "follows" => Some(Self::Follows),
            "newest" | "new" => Some(Self::Newest),
            "updated" => Some(Self::Updated),
            _ => None,
        }
    }
}

/// Kinds of Modrinth projects this launcher installs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectKind {
    Mod,
    ResourcePack,
    Shader,
}

impl ProjectKind {
    pub fn key(self) -> &'static str {
        match self {
            Self::Mod => "mod",
            Self::ResourcePack => "resourcepack",
            Self::Shader => "shader",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key.trim().to_ascii_lowercase().as_str() {
            "mod" | "mods" => Some(Self::Mod),
            "resourcepack" | "resourcepacks" | "resource_pack" => Some(Self::ResourcePack),
            "shader" | "shaders" | "shaderpack" | "shaderpacks" => Some(Self::Shader),
            _ => None,
        }
    }

    /// Directory inside the game folder the files belong to.
    pub fn dir(self) -> &'static str {
        match self {
            Self::Mod => "mods",
            Self::ResourcePack => "resourcepacks",
            Self::Shader => "shaderpacks",
        }
    }

    /// Mods are jars; resource packs and shaders are zips.
    fn accepts(self, filename: &str) -> bool {
        match self {
            Self::Mod => filename.ends_with(".jar"),
            Self::ResourcePack | Self::Shader => filename.ends_with(".zip") || filename.ends_with(".jar"),
        }
    }
}

impl std::fmt::Display for ProjectKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.key())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SearchPage {
    #[serde(default)]
    pub hits: Vec<SearchHit>,
    #[serde(default)]
    pub offset: u32,
    #[serde(default)]
    pub limit: u32,
    #[serde(default)]
    pub total_hits: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SearchHit {
    pub project_id: String,
    pub slug: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub downloads: u64,
    #[serde(default)]
    pub icon_url: Option<String>,
    #[serde(default)]
    pub client_side: String,
    #[serde(default)]
    pub server_side: String,
    /// Set by the service layer: a mod with this id is already in `mods/`.
    #[serde(default)]
    pub installed: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Project {
    pub id: String,
    #[serde(default)]
    pub slug: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub project_type: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Version {
    pub id: String,
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version_number: String,
    #[serde(default)]
    pub version_type: String,
    #[serde(default)]
    pub game_versions: Vec<String>,
    #[serde(default)]
    pub loaders: Vec<String>,
    #[serde(default)]
    pub date_published: String,
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
    #[serde(default)]
    pub files: Vec<VersionFile>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GameVersionTag {
    pub version: String,
    /// `release`, `snapshot`, `alpha` or `beta`.
    #[serde(default)]
    pub version_type: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Dependency {
    #[serde(default)]
    pub version_id: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    /// `required`, `optional`, `incompatible` or `embedded`.
    #[serde(default)]
    pub dependency_type: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Hashes {
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(default)]
    pub sha512: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VersionFile {
    #[serde(default)]
    pub hashes: Hashes,
    pub url: String,
    pub filename: String,
    #[serde(default)]
    pub primary: bool,
    #[serde(default)]
    pub size: u64,
}

/// Progress reported while resolving and downloading mods.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum InstallEvent {
    Resolving {
        project: String,
    },
    Downloading {
        project: String,
        file: String,
        done: u64,
        total: Option<u64>,
    },
    Installed {
        project: String,
        title: String,
        file: String,
        version: String,
        bytes: u64,
        dependency: bool,
    },
    Skipped {
        project: String,
        title: String,
        dependency: bool,
    },
}

/// One build of a project, as offered by the version picker.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VersionChoice {
    pub id: String,
    pub version_number: String,
    /// `release`, `beta` or `alpha`.
    pub version_type: String,
    pub game_versions: Vec<String>,
    pub loaders: Vec<String>,
    pub date_published: String,
    /// File the build installs.
    pub file: String,
    pub size: u64,
}

impl VersionChoice {
    /// Build from a version, if it has a downloadable file.
    pub fn from_version(version: &Version) -> Option<Self> {
        let file = primary_file(version)?;
        Some(Self {
            id: version.id.clone(),
            version_number: version.version_number.clone(),
            version_type: version.version_type.clone(),
            game_versions: version.game_versions.clone(),
            loaders: version.loaders.clone(),
            date_published: version.date_published.clone(),
            file: file.filename.clone(),
            size: file.size,
        })
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct InstalledMod {
    pub project_id: String,
    pub slug: String,
    pub title: String,
    pub version: String,
    pub file: String,
    pub bytes: u64,
    pub dependency: bool,
    pub skipped: bool,
}

#[derive(Debug, Clone)]
struct Target {
    project: String,
    version_id: Option<String>,
    dependency: bool,
    /// Overwrite an installed copy instead of reporting "already installed".
    replace: bool,
}

/// A jar in an instance's `mods` directory, prepared for an update check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalMod {
    /// File name as it is on disk, `.disabled` suffix included.
    pub file: String,
    pub sha1: String,
    pub name: String,
    pub version: String,
}

/// A newer compatible build of an installed file (mod, resource pack or
/// shader pack).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModUpdate {
    /// Current file name, `.disabled` suffix included.
    pub file: String,
    pub name: String,
    pub project_id: String,
    pub kind: ProjectKind,
    pub current_version: String,
    pub latest_version: String,
    pub latest_file: String,
    pub url: String,
    pub sha1: Option<String>,
    pub size: u64,
}

pub struct Modrinth<'a> {
    http: &'a Http,
}

impl<'a> Modrinth<'a> {
    pub fn new(http: &'a Http) -> Self {
        Self { http }
    }

    /// Search projects of `kind` that run on the filter's game version and,
    /// when the kind has one, its loader.
    pub fn search(
        &self,
        query: &str,
        filter: &Filter,
        kind: ProjectKind,
        index: SearchIndex,
        limit: u32,
        offset: u32,
    ) -> Result<SearchPage, ModrinthError> {
        let url = search_url(query, filter, kind, index, limit, offset);
        let body = self.http.get_bytes(&url, 8 * 1024 * 1024)?;
        serde_json::from_slice(&body).map_err(|source| decode_error(&url, source))
    }

    pub fn project(&self, id: &str) -> Result<Project, ModrinthError> {
        let url = format!("{}/project/{}", endpoints::MODRINTH_API, encode(id));
        self.get_json(&url)
    }

    /// Versions of `project` matching the filter, newest first.
    pub fn versions(&self, project: &str, filter: &Filter) -> Result<Vec<Version>, ModrinthError> {
        let url = versions_url(project, filter);
        let mut versions: Vec<Version> = self.get_json(&url)?;
        versions.sort_by(|left, right| right.date_published.cmp(&left.date_published));
        Ok(versions)
    }

    /// Release game builds Modrinth tags projects with, newest first. Snapshots
    /// and pre-releases are excluded: they only clutter a version picker.
    pub fn game_versions(&self) -> Result<Vec<GameVersionTag>, ModrinthError> {
        let url = format!("{}/tag/game_version", endpoints::MODRINTH_API);
        let mut tags: Vec<GameVersionTag> = self.get_json(&url)?;
        tags.retain(|tag| tag.version_type == "release");
        Ok(tags)
    }

    pub fn version(&self, id: &str) -> Result<Version, ModrinthError> {
        let url = format!("{}/version/{}", endpoints::MODRINTH_API, encode(id));
        self.get_json(&url)
    }

    /// Version that publishes a file with this SHA-1, when Modrinth knows it.
    /// A miss (404) is not an error.
    pub fn version_by_hash(&self, sha1: &str) -> Result<Option<Version>, ModrinthError> {
        let url = format!(
            "{}/version_file/{}?algorithm=sha1",
            endpoints::MODRINTH_API,
            encode(sha1.trim())
        );
        match self.get_json::<Version>(&url) {
            Ok(version) => Ok(Some(version)),
            Err(ModrinthError::Source(SourceError::Status { status: 404, .. })) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Every version of a project, newest first, alphas and betas included.
    pub fn project_versions(&self, project: &str) -> Result<Vec<Version>, ModrinthError> {
        let url = format!("{}/project/{}/version", endpoints::MODRINTH_API, encode(project));
        let mut versions: Vec<Version> = self.get_json(&url)?;
        versions.sort_by(|left, right| right.date_published.cmp(&left.date_published));
        Ok(versions)
    }

    /// Modrinth modpack whose title matches `name` exactly.
    pub fn find_modpack(&self, name: &str) -> Result<Option<SearchHit>, ModrinthError> {
        let facets = serde_json::json!([["project_type:modpack"]]).to_string();
        let url = format!(
            "{}/search?query={}&limit=5&facets={}",
            endpoints::MODRINTH_API,
            encode(name.trim()),
            encode(&facets),
        );
        let page: SearchPage = self.get_json(&url)?;
        let wanted = name.trim();
        Ok(page
            .hits
            .into_iter()
            .find(|hit| hit.title.trim().eq_ignore_ascii_case(wanted)))
    }

    /// Modrinth modpacks matching `query`, most downloaded first. No game
    /// version or loader filter: the pack itself decides what it needs.
    pub fn search_packs(
        &self,
        query: &str,
        index: SearchIndex,
        limit: u32,
        offset: u32,
    ) -> Result<SearchPage, ModrinthError> {
        let facets = serde_json::json!([["project_type:modpack"]]).to_string();
        let url = format!(
            "{}/search?query={}&limit={}&offset={}&index={}&facets={}",
            endpoints::MODRINTH_API,
            encode(query),
            limit.min(100),
            offset,
            index.key(),
            encode(&facets),
        );
        self.get_json(&url)
    }

    fn latest(&self, project: &str, filter: &Filter) -> Result<Version, ModrinthError> {
        let versions = self.versions(project, filter)?;
        // Installing a stable project must not silently hand the user a beta.
        versions
            .iter()
            .find(|version| version.version_type == "release")
            .or_else(|| versions.first())
            .cloned()
            .ok_or_else(|| ModrinthError::Invalid(format!("{project} has no build for {}", describe(filter))))
    }

    fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T, ModrinthError> {
        let body = self.http.get_bytes(url, 8 * 1024 * 1024)?;
        serde_json::from_slice(&body).map_err(|source| decode_error(url, source))
    }

    /// Newest compatible builds for installed files. Files Modrinth does not
    /// know, and files that are already the newest build, are left out.
    pub fn updates(
        &self,
        locals: &[LocalMod],
        filter: &Filter,
        kind: ProjectKind,
    ) -> Result<Vec<ModUpdate>, ModrinthError> {
        let hashes: Vec<String> = locals
            .iter()
            .map(|local| local.sha1.trim().to_ascii_lowercase())
            .filter(|sha1| sha1.len() == 40 && sha1.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .collect();
        if hashes.is_empty() {
            return Ok(Vec::new());
        }
        let url = format!("{}/version_files/update", endpoints::MODRINTH_API);
        // Resource packs and shaders have no loader, so the filter is left out
        // for them instead of sending an empty one.
        let mut body = serde_json::json!({
            "hashes": hashes,
            "algorithm": "sha1",
        });
        if filter.has_game_version() {
            body["game_versions"] = serde_json::json!([filter.game_version.trim()]);
        }
        if filter.has_loader() {
            body["loaders"] = serde_json::json!([filter.loader]);
        }
        let payload = serde_json::to_string(&body).unwrap_or_else(|_| "{}".to_string());
        let bytes = self.http.post_json(&url, &payload)?;
        let latest: HashMap<String, Version> =
            serde_json::from_slice(&bytes).map_err(|source| decode_error(&url, source))?;

        let mut updates = Vec::new();
        for local in locals {
            let sha1 = local.sha1.trim().to_ascii_lowercase();
            let Some(version) = latest.get(&sha1) else {
                continue;
            };
            // `/version_files/update` returns the newest build matching the
            // filters, which can be older than the installed file. Compare
            // channel rank and publish date so an update never downgrades.
            let current = self.version_by_hash(&sha1)?;
            if let Some(current) = &current {
                if channel_rank(&version.version_type) < channel_rank(&current.version_type) {
                    continue;
                }
                if version.date_published < current.date_published {
                    continue;
                }
            }
            updates.extend(update_for(local, version, kind));
        }
        updates.sort_by_key(|update| update.name.to_lowercase());
        Ok(updates)
    }

    /// Download a newer build and drop the file it replaces. A disabled mod
    /// stays disabled: its replacement keeps the `.disabled` suffix.
    pub fn apply_update(
        &self,
        dir: &Path,
        update: &ModUpdate,
        progress: &mut dyn FnMut(InstallEvent),
    ) -> Result<InstalledMod, ModrinthError> {
        fs::create_dir_all(dir).map_err(|source| ModrinthError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        validate_filename(&update.latest_file, update.kind)?;
        let target_name = replacement_file(update);
        let dest = dir.join(&target_name);
        let mut request = DownloadRequest::new(&update.url, &dest);
        if let Some(sha1) = &update.sha1 {
            request = request.with_sha1(sha1.clone());
        }
        if update.size > 0 {
            request = request.with_size(update.size);
        }
        progress(InstallEvent::Resolving {
            project: update.name.clone(),
        });
        let mut report = |done: u64| {
            progress(InstallEvent::Downloading {
                project: update.name.clone(),
                file: target_name.clone(),
                done,
                total: if update.size > 0 { Some(update.size) } else { None },
            });
        };
        let outcome = self.http.download(&request, &mut report)?;
        let skipped = matches!(outcome, DownloadOutcome::AlreadyValid);

        let previous = dir.join(&update.file);
        // A build that only changes the case of its file name resolves to the
        // same entry on Windows and macOS; deleting it would remove the file
        // that was just downloaded.
        let same_entry = previous
            .file_name()
            .zip(dest.file_name())
            .is_some_and(|(old, new)| old.to_string_lossy().eq_ignore_ascii_case(&new.to_string_lossy()));
        if previous != dest && !same_entry && previous.is_file() {
            fs::remove_file(&previous).map_err(|source| ModrinthError::Io { path: previous, source })?;
        }
        if !skipped {
            progress(InstallEvent::Installed {
                project: update.project_id.clone(),
                title: update.name.clone(),
                file: target_name.clone(),
                version: update.latest_version.clone(),
                bytes: update.size,
                dependency: false,
            });
        }
        Ok(InstalledMod {
            project_id: update.project_id.clone(),
            slug: String::new(),
            title: update.name.clone(),
            version: update.latest_version.clone(),
            file: target_name,
            bytes: update.size,
            dependency: false,
            skipped,
        })
    }

    /// Download each target into `dir`. Mods also pull in their required
    /// dependencies; resource packs and shaders have none.
    ///
    /// Targets may be project ids or slugs. Projects whose id already appears
    /// in `installed_ids` are skipped, so dependencies are not downloaded a
    /// second time under another file name. A required dependency pinned to a
    /// version that does not match the filter falls back to the newest
    /// compatible build of the same project.
    pub fn install(
        &self,
        dir: &Path,
        targets: &[String],
        installed_ids: &[String],
        filter: &Filter,
        kind: ProjectKind,
        progress: &mut dyn FnMut(InstallEvent),
    ) -> Result<Vec<InstalledMod>, ModrinthError> {
        let queue: VecDeque<Target> = targets
            .iter()
            .map(|project| Target {
                project: project.clone(),
                version_id: None,
                dependency: false,
                replace: false,
            })
            .collect();
        self.install_queue(dir, queue, installed_ids, filter, kind, progress)
    }

    /// Install one specific build of a project and replace the copy the
    /// instance already has. The version picker uses this to downgrade or to
    /// take a build that is not the newest release.
    #[allow(clippy::too_many_arguments)]
    pub fn install_version(
        &self,
        dir: &Path,
        project: &str,
        version_id: &str,
        installed_ids: &[String],
        filter: &Filter,
        kind: ProjectKind,
        progress: &mut dyn FnMut(InstallEvent),
    ) -> Result<Vec<InstalledMod>, ModrinthError> {
        let queue: VecDeque<Target> = std::iter::once(Target {
            project: project.to_string(),
            version_id: Some(version_id.to_string()),
            dependency: false,
            replace: true,
        })
        .collect();
        self.install_queue(dir, queue, installed_ids, filter, kind, progress)
    }

    fn install_queue(
        &self,
        dir: &Path,
        mut queue: VecDeque<Target>,
        installed_ids: &[String],
        filter: &Filter,
        kind: ProjectKind,
        progress: &mut dyn FnMut(InstallEvent),
    ) -> Result<Vec<InstalledMod>, ModrinthError> {
        fs::create_dir_all(dir).map_err(|source| ModrinthError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let mut seen = HashSet::new();
        let mut installed = Vec::new();

        while let Some(target) = queue.pop_front() {
            let project = self.project(&target.project)?;
            if !seen.insert(project.id.clone()) {
                continue;
            }
            if !project.project_type.is_empty() && project.project_type != kind.key() {
                return Err(ModrinthError::Invalid(format!(
                    "{} is a {}, not a {}",
                    project.title,
                    project.project_type,
                    kind.key()
                )));
            }
            progress(InstallEvent::Resolving {
                project: project.title.clone(),
            });

            if !target.replace && is_installed(&project, installed_ids) {
                progress(InstallEvent::Skipped {
                    project: project.id.clone(),
                    title: project.title.clone(),
                    dependency: target.dependency,
                });
                continue;
            }

            let version = match target.version_id.as_deref() {
                Some(id) => {
                    let version = self.version(id)?;
                    if is_compatible(&version, filter) {
                        version
                    } else {
                        self.latest(named(&project), filter)?
                    }
                }
                None => self.latest(named(&project), filter)?,
            };

            let file = primary_file(&version).ok_or_else(|| {
                ModrinthError::Invalid(format!(
                    "version {} of {} has no files",
                    version.version_number, project.title
                ))
            })?;
            validate_filename(&file.filename, kind)?;
            let dest = dir.join(&file.filename);
            let mut request = DownloadRequest::new(&file.url, &dest);
            if let Some(sha1) = &file.hashes.sha1 {
                request = request.with_sha1(sha1.clone());
            }
            if file.size > 0 {
                request = request.with_size(file.size);
            }
            let mut report = |done: u64| {
                progress(InstallEvent::Downloading {
                    project: project.title.clone(),
                    file: file.filename.clone(),
                    done,
                    total: if file.size > 0 { Some(file.size) } else { None },
                });
            };
            let outcome = self.http.download(&request, &mut report)?;
            let skipped = matches!(outcome, DownloadOutcome::AlreadyValid);

            if skipped {
                progress(InstallEvent::Skipped {
                    project: project.id.clone(),
                    title: project.title.clone(),
                    dependency: target.dependency,
                });
            } else {
                progress(InstallEvent::Installed {
                    project: project.id.clone(),
                    title: project.title.clone(),
                    file: file.filename.clone(),
                    version: version.version_number.clone(),
                    bytes: file.size,
                    dependency: target.dependency,
                });
            }
            installed.push(InstalledMod {
                project_id: project.id.clone(),
                slug: project.slug.clone(),
                title: project.title.clone(),
                version: version.version_number.clone(),
                file: file.filename.clone(),
                bytes: file.size,
                dependency: target.dependency,
                skipped,
            });

            if kind == ProjectKind::Mod {
                queue.extend(required_dependencies(&version).into_iter().map(|dependency| Target {
                    project: dependency.project,
                    version_id: dependency.version_id,
                    dependency: true,
                    replace: false,
                }));
            }
        }
        Ok(installed)
    }
}

fn named(project: &Project) -> &str {
    if project.slug.is_empty() {
        &project.id
    } else {
        &project.slug
    }
}

/// Downloadable file of a version: the primary one, else the first.
fn primary_file(version: &Version) -> Option<&VersionFile> {
    version
        .files
        .iter()
        .find(|file| file.primary)
        .or_else(|| version.files.first())
}

/// File name the replacement jar gets on disk: disabled mods stay disabled.
fn replacement_file(update: &ModUpdate) -> String {
    if update.file.ends_with(".disabled") {
        format!("{}.disabled", update.latest_file)
    } else {
        update.latest_file.clone()
    }
}

/// Modrinth release channels, most stable first.
fn channel_rank(version_type: &str) -> u8 {
    match version_type {
        "release" => 3,
        "beta" => 2,
        _ => 1,
    }
}

/// Update entry for `local`, unless its file already is the newest build.
fn update_for(local: &LocalMod, version: &Version, kind: ProjectKind) -> Option<ModUpdate> {
    let file = primary_file(version)?;
    if file
        .hashes
        .sha1
        .as_deref()
        .is_some_and(|latest| latest.eq_ignore_ascii_case(local.sha1.trim()))
    {
        return None;
    }
    Some(ModUpdate {
        file: local.file.clone(),
        name: if local.name.is_empty() {
            local.file.clone()
        } else {
            local.name.clone()
        },
        project_id: version.project_id.clone(),
        kind,
        current_version: local.version.clone(),
        latest_version: version.version_number.clone(),
        latest_file: file.filename.clone(),
        url: file.url.clone(),
        sha1: file.hashes.sha1.clone(),
        size: file.size,
    })
}

/// Required dependencies of a version, in declaration order.
fn required_dependencies(version: &Version) -> Vec<Target> {
    version
        .dependencies
        .iter()
        .filter(|dependency| dependency.dependency_type == "required")
        .filter_map(|dependency| {
            dependency.project_id.as_ref().map(|project| Target {
                project: project.clone(),
                version_id: dependency.version_id.clone(),
                dependency: true,
                replace: false,
            })
        })
        .collect()
}

fn is_compatible(version: &Version, filter: &Filter) -> bool {
    (!filter.has_game_version()
        || version
            .game_versions
            .iter()
            .any(|game| game == filter.game_version.trim()))
        && (!filter.has_loader()
            || version
                .loaders
                .iter()
                .any(|loader| filter.loaders().contains(&loader.as_str())))
}

/// Jar metadata ids and Modrinth slugs differ in separators and case, so
/// compare them with everything but letters and digits removed.
pub fn matches_project(project: &str, installed_ids: &[String]) -> bool {
    let target = normalize(project);
    installed_ids.iter().any(|installed| normalize(installed) == target)
}

fn is_installed(project: &Project, installed_ids: &[String]) -> bool {
    matches_project(&project.slug, installed_ids) || matches_project(&project.id, installed_ids)
}

pub fn normalize(value: &str) -> String {
    value
        .to_ascii_lowercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect()
}

fn describe(filter: &Filter) -> String {
    let game = if filter.has_game_version() {
        filter.game_version.trim().to_string()
    } else {
        "any game version".to_string()
    };
    if filter.has_loader() {
        format!("{game} with {}", filter.loaders().join(" or "))
    } else {
        game
    }
}

fn validate_filename(name: &str, kind: ProjectKind) -> Result<(), ModrinthError> {
    if !tntl_store::valid_file_name(name) || !kind.accepts(name) {
        return Err(ModrinthError::Invalid(format!(
            "unsafe {} file name: {name}",
            kind.key()
        )));
    }
    Ok(())
}

fn search_url(query: &str, filter: &Filter, kind: ProjectKind, index: SearchIndex, limit: u32, offset: u32) -> String {
    let mut facets = vec![vec![format!("project_type:{}", kind.key())]];
    if filter.has_game_version() {
        facets.push(vec![format!("versions:{}", filter.game_version.trim())]);
    }
    if filter.has_loader() {
        facets.push(
            filter
                .loaders()
                .into_iter()
                .map(|loader| format!("categories:{loader}"))
                .collect(),
        );
    }
    if kind == ProjectKind::Mod {
        facets.push(vec![
            "client_side:required".to_string(),
            "client_side:optional".to_string(),
        ]);
    }
    format!(
        "{}/search?query={}&limit={}&offset={}&index={}&facets={}",
        endpoints::MODRINTH_API,
        encode(query),
        limit.min(100),
        offset,
        index.key(),
        encode(&serde_json::to_string(&facets).unwrap_or_else(|_| "[]".to_string())),
    )
}

/// Version list URL of `project`, narrowed to the filter's game version and
/// every loader it accepts.
fn versions_url(project: &str, filter: &Filter) -> String {
    let mut url = format!("{}/project/{}/version", endpoints::MODRINTH_API, encode(project));
    let mut query = Vec::new();
    if filter.has_game_version() {
        query.push(format!(
            "game_versions={}",
            encode(&json_array([filter.game_version.trim()]))
        ));
    }
    if filter.has_loader() {
        query.push(format!("loaders={}", encode(&json_array(filter.loaders()))));
    }
    if !query.is_empty() {
        url.push('?');
        url.push_str(&query.join("&"));
    }
    url
}

/// JSON array literal, used for the subset of query parameters that take one.
fn json_array<I, S>(values: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let quoted: Vec<String> = values
        .into_iter()
        .map(|value| serde_json::to_string(value.as_ref()).unwrap_or_else(|_| "\"\"".to_string()))
        .collect();
    format!("[{}]", quoted.join(","))
}

/// Percent-encode everything that is not an unreserved URL character.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn decode_error(url: &str, source: serde_json::Error) -> ModrinthError {
    ModrinthError::Json {
        url: url.to_string(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_query_values() {
        assert_eq!(encode("sodium extra"), "sodium%20extra");
        assert_eq!(encode("a/b?c=d"), "a%2Fb%3Fc%3Dd");
        assert_eq!(encode("1.20.1"), "1.20.1");
    }

    #[test]
    fn legacy_fabric_searches_as_fabric() {
        let filter = Filter::new("1.12.2", "legacyfabric");
        assert_eq!(filter.loader, "fabric");
        assert_eq!(Filter::new("1.20.1", "Forge").loader, "Forge");
    }

    #[test]
    fn builds_search_facets() {
        let url = search_url(
            "jei",
            &Filter::new("1.20.1", "fabric"),
            ProjectKind::Mod,
            SearchIndex::Relevance,
            20,
            0,
        );
        let facets = url.split("facets=").nth(1).unwrap();
        let decoded = facets
            .replace("%5B", "[")
            .replace("%5D", "]")
            .replace("%22", "\"")
            .replace("%3A", ":")
            .replace("%2C", ",");
        assert_eq!(
            decoded,
            r#"[["project_type:mod"],["versions:1.20.1"],["categories:fabric"],["client_side:required","client_side:optional"]]"#
        );
        assert!(url.contains("query=jei"));
        assert!(url.starts_with(endpoints::MODRINTH_API));
    }

    #[test]
    fn accepts_a_fallback_loader() {
        let filter = Filter::new("1.20.1", "neoforge").accepting("forge");
        let url = versions_url("rechiseled", &filter);
        let decoded = url
            .replace("%5B", "[")
            .replace("%5D", "]")
            .replace("%22", "\"")
            .replace("%2C", ",")
            .replace("%20", " ");
        assert!(decoded.contains(r#"loaders=["neoforge","forge"]"#), "{decoded}");

        let url = search_url("rechiseled", &filter, ProjectKind::Mod, SearchIndex::Relevance, 5, 0);
        let decoded = url
            .replace("%5B", "[")
            .replace("%5D", "]")
            .replace("%22", "\"")
            .replace("%2C", ",")
            .replace("%3A", ":");
        assert!(
            decoded.contains(r#"["categories:neoforge","categories:forge"]"#),
            "{decoded}"
        );

        let forge_only: Version =
            serde_json::from_str(r#"{"id":"v","game_versions":["1.20.1"],"loaders":["forge"]}"#).unwrap();
        assert!(is_compatible(&forge_only, &filter));
        assert!(!is_compatible(&forge_only, &Filter::new("1.20.1", "neoforge")));
    }

    #[test]
    fn keeps_only_required_dependencies() {
        let version: Version = serde_json::from_str(
            r#"{
              "id": "v1",
              "dependencies": [
                {"project_id": "p1", "version_id": "v1", "dependency_type": "required"},
                {"project_id": "p2", "dependency_type": "optional"},
                {"version_id": "v3", "dependency_type": "required"},
                {"project_id": "p4", "dependency_type": "incompatible"}
              ]
            }"#,
        )
        .unwrap();
        let dependencies = required_dependencies(&version);
        assert_eq!(dependencies.len(), 1);
        assert_eq!(dependencies[0].project, "p1");
        assert_eq!(dependencies[0].version_id.as_deref(), Some("v1"));
    }

    #[test]
    fn rejects_unsafe_file_names() {
        assert!(validate_filename("sodium-0.5.jar", ProjectKind::Mod).is_ok());
        assert!(validate_filename("../evil.jar", ProjectKind::Mod).is_err());
        assert!(validate_filename("nested/evil.jar", ProjectKind::Mod).is_err());
        assert!(validate_filename("notes.txt", ProjectKind::Mod).is_err());
        assert!(validate_filename("notes.txt", ProjectKind::ResourcePack).is_err());
        assert!(validate_filename("Better Grass.zip", ProjectKind::ResourcePack).is_ok());
    }

    #[test]
    fn searches_every_game_version_when_unspecified() {
        let url = search_url(
            "",
            &Filter::any_loader(""),
            ProjectKind::Mod,
            SearchIndex::Downloads,
            20,
            0,
        );
        let facets = url.split("facets=").nth(1).unwrap();
        let decoded = facets
            .replace("%5B", "[")
            .replace("%5D", "]")
            .replace("%22", "\"")
            .replace("%3A", ":")
            .replace("%2C", ",");
        assert_eq!(
            decoded,
            r#"[["project_type:mod"],["client_side:required","client_side:optional"]]"#
        );
    }

    #[test]
    fn filters_versions_by_game_and_loader() {
        let version: Version =
            serde_json::from_str(r#"{"id":"v","game_versions":["1.20.1"],"loaders":["fabric"]}"#).unwrap();
        assert!(is_compatible(&version, &Filter::new("1.20.1", "fabric")));
        assert!(!is_compatible(&version, &Filter::new("1.19.2", "fabric")));
        assert!(!is_compatible(&version, &Filter::new("1.20.1", "neoforge")));
        assert!(is_compatible(&version, &Filter::new("", "fabric")));
    }

    #[test]
    fn recognizes_installed_projects() {
        let project: Project =
            serde_json::from_str(r#"{"id":"AANobbMI","slug":"sodium","title":"Sodium","project_type":"mod"}"#).unwrap();
        assert!(is_installed(&project, &["sodium".to_string()]));
        assert!(is_installed(&project, &["Sodium".to_string()]));
        assert!(!is_installed(&project, &["iris".to_string()]));
        assert!(is_installed(&project, &["aanobbmi".to_string()]));
    }

    #[test]
    fn matches_installed_ids_across_separators() {
        assert!(matches_project("fabric-api", &["fabric_api".to_string()]));
        assert!(matches_project("fabric-api", &["FabricAPI".to_string()]));
        assert!(!matches_project("fabric-api", &["fabric".to_string()]));
        assert!(!matches_project("iris", &["sodium".to_string()]));
    }

    #[test]
    fn reports_only_newer_builds() {
        let local = LocalMod {
            file: "sodium-fabric-0.5.11.jar".to_string(),
            sha1: "a".repeat(40),
            name: "Sodium".to_string(),
            version: "0.5.11".to_string(),
        };

        let current: Version = serde_json::from_str(&format!(
            r#"{{"id":"v1","project_id":"AANobbMI","version_number":"0.5.11",
                 "files":[{{"primary":true,"filename":"sodium-fabric-0.5.11.jar","url":"https://cdn.example/v1.jar",
                            "hashes":{{"sha1":"{}"}},"size":10}}]}}"#,
            "a".repeat(40)
        ))
        .unwrap();
        assert!(update_for(&local, &current, ProjectKind::Mod).is_none());

        let newer: Version = serde_json::from_str(&format!(
            r#"{{"id":"v2","project_id":"AANobbMI","version_number":"0.6.0",
                 "files":[{{"primary":true,"filename":"sodium-fabric-0.6.0.jar","url":"https://cdn.example/v2.jar",
                            "hashes":{{"sha1":"{}"}},"size":20}}]}}"#,
            "b".repeat(40)
        ))
        .unwrap();
        let update = update_for(&local, &newer, ProjectKind::Mod).unwrap();
        assert_eq!(update.file, "sodium-fabric-0.5.11.jar");
        assert_eq!(update.current_version, "0.5.11");
        assert_eq!(update.latest_version, "0.6.0");
        assert_eq!(update.latest_file, "sodium-fabric-0.6.0.jar");
        assert_eq!(update.size, 20);
        assert_eq!(replacement_file(&update), "sodium-fabric-0.6.0.jar");
    }

    #[test]
    fn disabled_mods_stay_disabled_after_an_update() {
        let update = ModUpdate {
            file: "sodium.jar.disabled".to_string(),
            name: "Sodium".to_string(),
            project_id: "AANobbMI".to_string(),
            kind: ProjectKind::Mod,
            current_version: "1.0".to_string(),
            latest_version: "1.1".to_string(),
            latest_file: "sodium-1.1.jar".to_string(),
            url: "https://cdn.example/sodium-1.1.jar".to_string(),
            sha1: None,
            size: 0,
        };
        assert_eq!(replacement_file(&update), "sodium-1.1.jar.disabled");
    }

    #[test]
    fn parses_the_update_lookup_response() {
        let latest: HashMap<String, Version> = serde_json::from_str(&format!(
            r#"{{"{}":{{"id":"v2","project_id":"P7dR8mSH","version_number":"0.92.12+1.20.1",
                          "files":[{{"primary":true,"filename":"fabric-api.jar","url":"https://cdn.example/f.jar"}}]}}}}"#,
            "c".repeat(40)
        ))
        .unwrap();
        assert_eq!(
            latest["cccccccccccccccccccccccccccccccccccccccc"].version_number,
            "0.92.12+1.20.1"
        );
    }

    #[test]
    fn parses_search_page() {
        let page: SearchPage = serde_json::from_str(
            r#"{
              "hits": [{
                "project_id": "P7dR8mSH", "slug": "fabric-api", "title": "Fabric API",
                "description": "Hooks", "author": "modmuss50", "categories": ["fabric"],
                "downloads": 1000, "icon_url": "https://cdn.modrinth.com/x.png",
                "client_side": "required", "server_side": "required"
              }],
              "offset": 0, "limit": 20, "total_hits": 1
            }"#,
        )
        .unwrap();
        assert_eq!(page.total_hits, 1);
        assert_eq!(page.hits[0].slug, "fabric-api");
        assert_eq!(page.hits[0].icon_url.as_deref(), Some("https://cdn.modrinth.com/x.png"));
    }
}
