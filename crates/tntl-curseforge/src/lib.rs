//! CurseForge content: search, files, dependency-aware installs and updates.
//!
//! The API is keyed per developer account, so the launcher never ships a key:
//! every user pastes their own in the settings and it only travels in the
//! `x-api-key` header. Only the documented v1 endpoints are used, and a file
//! the author does not allow third-party distribution for is reported instead
//! of being fetched from somewhere else.

#![forbid(unsafe_code)]

mod pack;
pub use pack::{
    MANIFEST_FILE, ManifestFile, PackManifest, PackMinecraft, PackModLoader, content_dir, extract_overrides,
    open_manifest,
};

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tntl_modrinth::{Filter, InstallEvent, InstalledMod, ModUpdate, ProjectKind, SearchHit, SearchIndex, SearchPage};
use tntl_source::{DownloadOutcome, DownloadRequest, Http, SourceError, endpoints};

/// Minecraft on CurseForge.
const GAME: u32 = 432;

/// Section ids of the content kinds the launcher installs.
fn class_id(kind: ProjectKind) -> u32 {
    match kind {
        ProjectKind::Mod => 6,
        ProjectKind::ResourcePack => 12,
        ProjectKind::Shader => 6552,
    }
}

/// CurseForge loader enum. `legacyfabric` is Fabric with an older game range.
fn loader_type(loader: &str) -> Option<u32> {
    match loader.trim().to_ascii_lowercase().as_str() {
        "forge" => Some(1),
        "fabric" | "legacyfabric" => Some(4),
        "quilt" => Some(5),
        "neoforge" => Some(6),
        _ => None,
    }
}

/// Search ordering. CurseForge has no follow count, so both popularity keys
/// map to its popularity sort.
fn sort_field(index: SearchIndex) -> u32 {
    match index {
        SearchIndex::Relevance | SearchIndex::Follows => 2,
        SearchIndex::Downloads => 6,
        SearchIndex::Newest => 11,
        SearchIndex::Updated => 3,
    }
}

#[derive(Debug, Error)]
pub enum CurseForgeError {
    #[error(transparent)]
    Source(#[from] SourceError),
    #[error(transparent)]
    Store(#[from] tntl_store::StoreError),
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
    /// Missing, expired or rate limited key.
    #[error("CurseForge rejected the API key (HTTP {0}); check the key in the settings")]
    Key(u16),
    /// The author turned off third-party downloads for this file.
    #[error("{name} does not allow downloads through the CurseForge API; get {file} from curseforge.com")]
    Unavailable { name: String, file: String },
    #[error("unexpected archive layout: {0}")]
    Zip(String),
    #[error("{0}")]
    Invalid(String),
}

#[derive(Debug, Clone, Deserialize)]
struct Envelope<T> {
    data: T,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Paged<T> {
    data: Vec<T>,
    pagination: Pagination,
}

#[derive(Debug, Clone, Default)]
pub struct PackSearchPage {
    pub mods: Vec<Mod>,
    pub offset: u32,
    pub limit: u32,
    pub total: u32,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Pagination {
    index: u32,
    page_size: u32,
    total_count: u32,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Mod {
    pub id: u64,
    pub name: String,
    pub slug: String,
    pub summary: String,
    pub download_count: u64,
    pub class_id: u32,
    pub logo: Option<Asset>,
    pub authors: Vec<Author>,
    pub categories: Vec<Category>,
    pub latest_files: Vec<File>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Asset {
    pub url: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Author {
    pub name: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Category {
    pub name: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct File {
    pub id: u64,
    pub mod_id: u64,
    #[serde(default = "default_true")]
    pub is_available: bool,
    pub display_name: String,
    pub file_name: String,
    /// 1 release, 2 beta, 3 alpha.
    pub release_type: u32,
    pub file_date: String,
    pub file_length: u64,
    pub download_url: Option<String>,
    pub game_versions: Vec<String>,
    pub hashes: Vec<FileHash>,
    pub dependencies: Vec<FileDependency>,
    pub file_fingerprint: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FileHash {
    pub value: String,
    /// 1 SHA-1, 2 MD5.
    pub algo: u32,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FileDependency {
    pub mod_id: u64,
    /// 1 embedded, 2 optional, 3 required, 4 tool, 5 incompatible, 6 include.
    pub relation_type: u32,
}

/// A file the launcher downloaded, with everything an update check needs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Installed {
    pub project_id: u64,
    pub title: String,
    pub file_id: u64,
    pub file_name: String,
    pub version: String,
    pub file_date: String,
    pub release_type: u32,
    pub bytes: u64,
    pub dependency: bool,
    pub skipped: bool,
}

impl Installed {
    pub fn source(&self) -> SourceEntry {
        SourceEntry {
            project_id: self.project_id,
            file_id: self.file_id,
            file_date: self.file_date.clone(),
            version: self.version.clone(),
            release_type: self.release_type,
        }
    }
}

/// Where a file in the game directory came from, kept so updates can find the
/// project again without hashing anything.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceEntry {
    #[serde(default)]
    pub project_id: u64,
    pub file_id: u64,
    pub file_date: String,
    #[serde(default)]
    pub version: String,
    #[serde(default = "release_type_default")]
    pub release_type: u32,
}

fn release_type_default() -> u32 {
    1
}

fn default_true() -> bool {
    true
}

/// `<instance>/content.sources.json`: `mods/foo.jar` maps to its CurseForge
/// file. Modrinth files are recognised by hash, so only CurseForge needs this.
pub const SOURCES_FILE: &str = "content.sources.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContentSources {
    #[serde(default)]
    pub files: BTreeMap<String, SourceEntry>,
}

impl ContentSources {
    pub fn load(path: &Path) -> Self {
        fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<(), CurseForgeError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| CurseForgeError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        tntl_store::atomic_write_json(path, self)?;
        Ok(())
    }

    fn key(kind: ProjectKind, file: &str) -> String {
        format!("{}/{}", kind.dir(), strip_disabled(file))
    }

    pub fn record(&mut self, kind: ProjectKind, file: &str, entry: SourceEntry) {
        self.files.insert(Self::key(kind, file), entry);
    }

    pub fn remove(&mut self, kind: ProjectKind, file: &str) {
        self.files.remove(&Self::key(kind, file));
    }

    /// Entries of one kind as `(file name in the directory, source)`.
    pub fn entries(&self, kind: ProjectKind) -> Vec<(String, SourceEntry)> {
        let prefix = format!("{}/", kind.dir());
        self.files
            .iter()
            .filter_map(|(key, entry)| key.strip_prefix(&prefix).map(|file| (file.to_string(), entry.clone())))
            .collect()
    }
}

/// File name without the `.disabled` suffix that switched it off.
pub fn strip_disabled(file: &str) -> &str {
    file.strip_suffix(".disabled").unwrap_or(file)
}

pub struct CurseForge<'a> {
    http: &'a Http,
    key: String,
}

impl<'a> CurseForge<'a> {
    pub fn new(http: &'a Http, key: &str) -> Result<Self, CurseForgeError> {
        let key = key.trim();
        if key.is_empty() {
            return Err(CurseForgeError::Invalid(
                "CurseForge needs an API key; create one on curseforge.com and paste it in the settings".to_string(),
            ));
        }
        Ok(Self {
            http,
            key: key.to_string(),
        })
    }

    fn headers(&self) -> [(&str, &str); 2] {
        [("x-api-key", self.key.as_str()), ("accept", "application/json")]
    }

    /// Search projects of `kind` for the filter's game version and loader.
    pub fn search(
        &self,
        query: &str,
        filter: &Filter,
        kind: ProjectKind,
        index: SearchIndex,
        limit: u32,
        offset: u32,
    ) -> Result<SearchPage, CurseForgeError> {
        let mut page = self.search_page(query, filter, kind, index, limit, offset)?;
        // NeoForge 1.20.1 accepts Forge mods, but CurseForge only takes one
        // loader per request, so ask for each accepted loader as well.
        if kind != ProjectKind::Mod || filter.accept.is_empty() {
            return Ok(page);
        }
        let mut seen: HashSet<String> = page.hits.iter().map(|hit| hit.project_id.clone()).collect();
        for loader in &filter.accept {
            let mut alternate = filter.clone();
            alternate.loader = loader.clone();
            alternate.accept.clear();
            let Ok(more) = self.search_page(query, &alternate, kind, index, limit, offset) else {
                continue;
            };
            page.total_hits = page.total_hits.max(more.total_hits);
            for hit in more.hits {
                if page.hits.len() >= limit as usize {
                    break;
                }
                if seen.insert(hit.project_id.clone()) {
                    page.hits.push(hit);
                }
            }
        }
        Ok(page)
    }

    fn search_page(
        &self,
        query: &str,
        filter: &Filter,
        kind: ProjectKind,
        index: SearchIndex,
        limit: u32,
        offset: u32,
    ) -> Result<SearchPage, CurseForgeError> {
        let url = search_url(query, filter, kind, index, limit, offset);
        let page: Paged<Mod> = self.get_json(&url)?;
        Ok(SearchPage {
            hits: page.data.into_iter().map(search_hit).collect(),
            offset: page.pagination.index,
            limit: page.pagination.page_size,
            total_hits: page.pagination.total_count,
        })
    }

    /// Modpacks, newest or most downloaded first. The pack browser shows each
    /// project's most recent builds from `latest_files`.
    pub fn search_packs(
        &self,
        query: &str,
        index: SearchIndex,
        limit: u32,
        offset: u32,
    ) -> Result<PackSearchPage, CurseForgeError> {
        let url = pack_search_url(query, index, limit, offset);
        let page: Paged<Mod> = self.get_json(&url)?;
        Ok(PackSearchPage {
            mods: page.data,
            offset: page.pagination.index,
            limit: page.pagination.page_size,
            total: page.pagination.total_count,
        })
    }

    pub fn project(&self, id: u64) -> Result<Mod, CurseForgeError> {
        let url = format!("{}/mods/{id}", endpoints::CURSEFORGE_API);
        let envelope: Envelope<Mod> = self.get_json(&url)?;
        Ok(envelope.data)
    }

    /// Files of a project that match the filter, newest first.
    pub fn files(&self, project: u64, filter: &Filter) -> Result<Vec<File>, CurseForgeError> {
        let mut files = self.files_for_loader(project, filter)?;
        // NeoForge 1.20.1 runs Forge builds, but the API filters by one loader.
        for loader in &filter.accept {
            let mut alternate = filter.clone();
            alternate.loader = loader.clone();
            alternate.accept.clear();
            let Ok(more) = self.files_for_loader(project, &alternate) else {
                continue;
            };
            for file in more {
                if files.iter().all(|known| known.id != file.id) {
                    files.push(file);
                }
            }
        }
        files.sort_by(|left, right| right.file_date.cmp(&left.file_date));
        Ok(files)
    }

    fn files_for_loader(&self, project: u64, filter: &Filter) -> Result<Vec<File>, CurseForgeError> {
        // The endpoint caps pageSize at 50; walk the pages so an older release
        // is still found behind a wall of betas. The page cap only guards
        // against a server that never reaches its own total.
        let mut data = Vec::new();
        let mut index = 0u32;
        for _ in 0..200 {
            let page: Paged<File> = self.get_json(&files_url(project, filter, index))?;
            let received = page.data.len() as u32;
            let total = page.pagination.total_count;
            data.extend(page.data);
            if received == 0 {
                break;
            }
            index = index.saturating_add(page.pagination.page_size.max(received));
            if total == 0 || index >= total {
                break;
            }
        }
        let mut files: Vec<File> = data
            .into_iter()
            .filter(|file| file.is_available && file.id != 0)
            .filter(|file| compatible(file, filter))
            .collect();
        files.sort_by(|left, right| right.file_date.cmp(&left.file_date));
        Ok(files)
    }

    pub fn file(&self, project: u64, file: u64) -> Result<File, CurseForgeError> {
        let url = format!("{}/mods/{project}/files/{file}", endpoints::CURSEFORGE_API);
        let envelope: Envelope<File> = self.get_json(&url)?;
        Ok(envelope.data)
    }

    /// Link the file can be downloaded from. CurseForge stops handing these
    /// out for files whose author disabled third-party downloads.
    pub fn download_url(&self, file: &File) -> Result<String, CurseForgeError> {
        if let Some(url) = file.download_url.as_deref().filter(|url| !url.trim().is_empty()) {
            return Ok(url.to_string());
        }
        let url = format!(
            "{}/mods/{}/files/{}/download-url",
            endpoints::CURSEFORGE_API,
            file.mod_id,
            file.id
        );
        let body = match self.http.get_bytes_with(&url, 8 * 1024 * 1024, &self.headers()) {
            Ok(body) => body,
            // CurseForge answers 403 here for files whose author disabled
            // third-party downloads; that is not a bad API key.
            Err(SourceError::Status { status: 403 | 404, .. }) => {
                return Err(CurseForgeError::Unavailable {
                    name: self.project_name(file.mod_id),
                    file: file.file_name.clone(),
                });
            }
            Err(error) => return Err(error.into()),
        };
        let envelope: Envelope<Option<String>> =
            serde_json::from_slice(&body).map_err(|source| CurseForgeError::Json {
                url: url.clone(),
                source,
            })?;
        envelope
            .data
            .filter(|url| !url.trim().is_empty())
            .ok_or_else(|| CurseForgeError::Unavailable {
                name: self.project_name(file.mod_id),
                file: file.file_name.clone(),
            })
    }

    fn project_name(&self, project: u64) -> String {
        self.title(project, &mut HashMap::new())
            .unwrap_or_else(|_| project.to_string())
    }

    /// Download each project (and, for mods, its required dependencies) into
    /// `dir`. Projects may be numeric ids or slugs; ids in `installed_ids` are
    /// skipped.
    pub fn install(
        &self,
        dir: &Path,
        targets: &[String],
        installed_ids: &[String],
        filter: &Filter,
        kind: ProjectKind,
        progress: &mut dyn FnMut(InstallEvent),
    ) -> Result<Vec<Installed>, CurseForgeError> {
        fs::create_dir_all(dir).map_err(|source| CurseForgeError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let mut queue: VecDeque<(u64, bool)> = VecDeque::new();
        for target in targets {
            queue.push_back((self.resolve(target, kind)?, false));
        }
        let mut seen = HashSet::new();
        let mut names: HashMap<u64, String> = HashMap::new();
        let mut installed = Vec::new();

        while let Some((project, dependency)) = queue.pop_front() {
            if !seen.insert(project) {
                continue;
            }
            let title = self.title(project, &mut names)?;
            progress(InstallEvent::Resolving { project: title.clone() });
            if installed_ids
                .iter()
                .any(|id| id == &project.to_string() || id.eq_ignore_ascii_case(&title))
            {
                progress(InstallEvent::Skipped {
                    project: project.to_string(),
                    title,
                    dependency,
                });
                continue;
            }
            let files = self.files(project, filter)?;
            let file = files
                .iter()
                .find(|file| file.release_type == 1)
                .or_else(|| files.first())
                .ok_or_else(|| CurseForgeError::Invalid(format!("{title} has no build for {}", describe(filter))))?;
            if !accepts(kind, &file.file_name) || !valid_file_name(&file.file_name) {
                return Err(CurseForgeError::Invalid(format!(
                    "{title} published {}, which is not a usable {} file",
                    file.file_name,
                    kind.key()
                )));
            }
            let url = self.download_url(file)?;
            let dest = dir.join(&file.file_name);
            let mut request = DownloadRequest::new(&url, &dest);
            if let Some(sha1) = file.hashes.iter().find(|hash| hash.algo == 1) {
                request = request.with_sha1(sha1.value.clone());
            }
            if file.file_length > 0 {
                request = request.with_size(file.file_length);
            }
            let mut report = |done: u64| {
                progress(InstallEvent::Downloading {
                    project: title.clone(),
                    file: file.file_name.clone(),
                    done,
                    total: if file.file_length > 0 {
                        Some(file.file_length)
                    } else {
                        None
                    },
                });
            };
            let outcome = self.http.download(&request, &mut report)?;
            let skipped = matches!(outcome, DownloadOutcome::AlreadyValid);
            if skipped {
                progress(InstallEvent::Skipped {
                    project: project.to_string(),
                    title: title.clone(),
                    dependency,
                });
            } else {
                progress(InstallEvent::Installed {
                    project: project.to_string(),
                    title: title.clone(),
                    file: file.file_name.clone(),
                    version: display_version(file),
                    bytes: file.file_length,
                    dependency,
                });
            }
            installed.push(Installed {
                project_id: project,
                title: title.clone(),
                file_id: file.id,
                file_name: file.file_name.clone(),
                version: display_version(file),
                file_date: file.file_date.clone(),
                release_type: file.release_type.max(1),
                bytes: file.file_length,
                dependency,
                skipped,
            });

            if kind == ProjectKind::Mod {
                for dependency in file.dependencies.iter().filter(|entry| entry.relation_type == 3) {
                    if !seen.contains(&dependency.mod_id) {
                        queue.push_back((dependency.mod_id, true));
                    }
                }
            }
        }
        Ok(installed)
    }

    /// Newer builds for files that were installed from CurseForge. The release
    /// channel of the installed file is kept: a release is not replaced by a
    /// beta. Files the API has no newer build for are left out.
    pub fn updates(
        &self,
        entries: &[(String, SourceEntry)],
        filter: &Filter,
        kind: ProjectKind,
    ) -> Result<Vec<(ModUpdate, SourceEntry)>, CurseForgeError> {
        let mut names: HashMap<u64, String> = HashMap::new();
        let mut updates = Vec::new();
        for (local, entry) in entries {
            let files = match self.files(entry.project_id, filter) {
                Ok(files) => files,
                // A project or build that disappeared is not worth failing the
                // whole check; a bad key or a network error is.
                Err(CurseForgeError::Invalid(_)) | Err(CurseForgeError::Unavailable { .. }) => continue,
                Err(CurseForgeError::Source(SourceError::Status { status: 404, .. })) => continue,
                Err(error) => return Err(error),
            };
            let Some(file) = files
                .iter()
                .find(|file| file.file_date > entry.file_date && file.release_type <= entry.release_type.max(1))
            else {
                continue;
            };
            let Ok(url) = self.download_url(file) else {
                continue;
            };
            if !valid_file_name(&file.file_name) {
                continue;
            }
            let title = self
                .title(entry.project_id, &mut names)
                .unwrap_or_else(|_| entry.project_id.to_string());
            updates.push((
                ModUpdate {
                    file: local.clone(),
                    name: title,
                    project_id: entry.project_id.to_string(),
                    kind,
                    current_version: entry.version.clone(),
                    latest_version: display_version(file),
                    latest_file: file.file_name.clone(),
                    url,
                    sha1: file
                        .hashes
                        .iter()
                        .find(|hash| hash.algo == 1)
                        .map(|hash| hash.value.clone()),
                    size: file.file_length,
                },
                SourceEntry {
                    project_id: entry.project_id,
                    file_id: file.id,
                    file_date: file.file_date.clone(),
                    version: display_version(file),
                    release_type: file.release_type.max(1),
                },
            ));
        }
        updates.sort_by_key(|(update, _)| update.name.to_lowercase());
        Ok(updates)
    }

    /// Project id for a numeric id or a slug.
    fn resolve(&self, target: &str, kind: ProjectKind) -> Result<u64, CurseForgeError> {
        let target = target.trim();
        if let Ok(id) = target.parse::<u64>() {
            return Ok(id);
        }
        let url = format!(
            "{}/mods/search?gameId={GAME}&classId={}&slug={}",
            endpoints::CURSEFORGE_API,
            class_id(kind),
            encode(target)
        );
        let page: Paged<Mod> = self.get_json(&url)?;
        page.data
            .into_iter()
            .find(|entry| entry.slug.eq_ignore_ascii_case(target))
            .map(|entry| entry.id)
            .ok_or_else(|| CurseForgeError::Invalid(format!("{target} is not on CurseForge")))
    }

    fn title(&self, project: u64, cache: &mut HashMap<u64, String>) -> Result<String, CurseForgeError> {
        if let Some(name) = cache.get(&project) {
            return Ok(name.clone());
        }
        let name = self.project(project)?.name;
        cache.insert(project, name.clone());
        Ok(name)
    }

    fn get_json<T: DeserializeOwned>(&self, url: &str) -> Result<T, CurseForgeError> {
        let body = match self.http.get_bytes_with(url, 8 * 1024 * 1024, &self.headers()) {
            Ok(body) => body,
            Err(SourceError::Status { status, .. }) if status == 401 || status == 403 => {
                return Err(CurseForgeError::Key(status));
            }
            Err(error) => return Err(error.into()),
        };
        serde_json::from_slice(&body).map_err(|source| CurseForgeError::Json {
            url: url.to_string(),
            source,
        })
    }
}

/// Modrinth-shaped hit so both sources share one search view.
fn search_hit(entry: Mod) -> SearchHit {
    SearchHit {
        project_id: entry.id.to_string(),
        slug: entry.slug,
        title: entry.name,
        description: entry.summary,
        author: entry
            .authors
            .first()
            .map(|author| author.name.clone())
            .unwrap_or_default(),
        categories: entry.categories.into_iter().map(|category| category.name).collect(),
        downloads: entry.download_count,
        icon_url: entry.logo.map(|logo| logo.url).filter(|url| !url.is_empty()),
        client_side: String::new(),
        server_side: String::new(),
        installed: false,
    }
}

fn search_url(query: &str, filter: &Filter, kind: ProjectKind, index: SearchIndex, limit: u32, offset: u32) -> String {
    let mut url = format!(
        "{}/mods/search?gameId={GAME}&classId={}&sortField={}&sortOrder=desc&pageSize={}&index={}",
        endpoints::CURSEFORGE_API,
        class_id(kind),
        sort_field(index),
        limit.clamp(1, 50),
        offset
    );
    if !filter.game_version.trim().is_empty() {
        url.push_str(&format!("&gameVersion={}", encode(&filter.game_version)));
    }
    if kind == ProjectKind::Mod
        && let Some(loader) = loader_type(&filter.loader)
    {
        url.push_str(&format!("&modLoaderType={loader}"));
    }
    if !query.trim().is_empty() {
        url.push_str(&format!("&searchFilter={}", encode(query.trim())));
    }
    url
}

fn pack_search_url(query: &str, index: SearchIndex, limit: u32, offset: u32) -> String {
    let mut url = format!(
        "{}/mods/search?gameId={GAME}&classId=4471&sortField={}&sortOrder=desc&pageSize={}&index={}",
        endpoints::CURSEFORGE_API,
        sort_field(index),
        limit.clamp(1, 50),
        offset
    );
    if !query.trim().is_empty() {
        url.push_str(&format!("&searchFilter={}", encode(query.trim())));
    }
    url
}

fn files_url(project: u64, filter: &Filter, index: u32) -> String {
    let mut url = format!(
        "{}/mods/{project}/files?pageSize=50&index={index}",
        endpoints::CURSEFORGE_API
    );
    if !filter.game_version.trim().is_empty() {
        url.push_str(&format!("&gameVersion={}", encode(&filter.game_version)));
    }
    if let Some(loader) = loader_type(&filter.loader) {
        url.push_str(&format!("&modLoaderType={loader}"));
    }
    url
}

fn compatible(file: &File, filter: &Filter) -> bool {
    let game = filter.game_version.trim();
    if game.is_empty() {
        return true;
    }
    file.game_versions
        .iter()
        .any(|version| version.eq_ignore_ascii_case(game))
}

fn accepts(kind: ProjectKind, filename: &str) -> bool {
    let lower = filename.to_ascii_lowercase();
    match kind {
        ProjectKind::Mod => lower.ends_with(".jar"),
        ProjectKind::ResourcePack | ProjectKind::Shader => lower.ends_with(".zip") || lower.ends_with(".jar"),
    }
}

/// The API returns author-controlled file names; only a bare file name may be
/// joined to a content directory.
pub fn valid_file_name(filename: &str) -> bool {
    !filename.is_empty()
        && !filename.contains(['/', '\\', ':'])
        && filename != ".."
        && filename != "."
        && !filename.chars().any(char::is_control)
}

fn display_version(file: &File) -> String {
    if file.display_name.trim().is_empty() {
        file.file_name.clone()
    } else {
        file.display_name.clone()
    }
}

fn describe(filter: &Filter) -> String {
    if filter.loader.is_empty() {
        filter.game_version.clone()
    } else {
        format!("{} {}", filter.game_version, filter.loader)
    }
}

/// Percent-encode a query value.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Shared install result for interfaces that speak Modrinth's vocabulary.
pub fn installed_mod(entry: &Installed) -> InstalledMod {
    InstalledMod {
        project_id: entry.project_id.to_string(),
        slug: String::new(),
        title: entry.title.clone(),
        version: entry.version.clone(),
        file: entry.file_name.clone(),
        bytes: entry.bytes,
        dependency: entry.dependency,
        skipped: entry.skipped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_kinds_and_loaders() {
        assert_eq!(class_id(ProjectKind::Mod), 6);
        assert_eq!(class_id(ProjectKind::ResourcePack), 12);
        assert_eq!(class_id(ProjectKind::Shader), 6552);
        assert_eq!(loader_type("legacyfabric"), Some(4));
        assert_eq!(loader_type("neoforge"), Some(6));
        assert_eq!(loader_type(""), None);
        assert_eq!(sort_field(SearchIndex::Downloads), 6);
    }

    #[test]
    fn builds_search_urls() {
        let filter = Filter::new("1.20.1", "fabric");
        let url = search_url("sodium & iris", &filter, ProjectKind::Mod, SearchIndex::Updated, 21, 20);
        assert!(url.contains("gameId=432"));
        assert!(url.contains("classId=6"));
        assert!(url.contains("gameVersion=1.20.1"));
        assert!(url.contains("modLoaderType=4"));
        assert!(url.contains("sortField=3"));
        assert!(url.contains("pageSize=21"));
        assert!(url.contains("index=20"));
        assert!(url.contains("searchFilter=sodium%20%26%20iris"));
        let url = search_url(
            "",
            &Filter::any_loader("1.20.1"),
            ProjectKind::Shader,
            SearchIndex::Relevance,
            0,
            0,
        );
        assert!(url.contains("classId=6552"));
        assert!(!url.contains("modLoaderType"));
        assert!(url.contains("pageSize=1"));
    }

    #[test]
    fn parses_search_and_files() {
        let body = r#"{
            "data": [{
                "id": 306612,
                "name": "Sodium",
                "slug": "sodium",
                "summary": "Rendering engine",
                "downloadCount": 1234,
                "classId": 6,
                "logo": {"url": "https://example.invalid/logo.png"},
                "authors": [{"name": "jellysquid3"}],
                "categories": [{"name": "Optimization"}],
                "latestFiles": []
            }],
            "pagination": {"index": 0, "pageSize": 50, "resultCount": 1, "totalCount": 1}
        }"#;
        let page: Paged<Mod> = serde_json::from_str(body).unwrap();
        assert_eq!(page.pagination.total_count, 1);
        let hit = search_hit(page.data.into_iter().next().unwrap());
        assert_eq!(hit.project_id, "306612");
        assert_eq!(hit.author, "jellysquid3");
        assert_eq!(hit.categories, vec!["Optimization".to_string()]);
        assert_eq!(hit.icon_url.as_deref(), Some("https://example.invalid/logo.png"));

        let body = r#"{
            "data": [{
                "id": 4003223,
                "modId": 306612,
                "isAvailable": true,
                "displayName": "Sodium 0.5.11",
                "fileName": "sodium-fabric-0.5.11.jar",
                "releaseType": 1,
                "fileDate": "2024-06-10T12:00:00Z",
                "fileLength": 900000,
                "downloadUrl": "https://example.invalid/sodium.jar",
                "gameVersions": ["1.20.1", "Fabric"],
                "hashes": [{"value": "abc", "algo": 1}],
                "dependencies": [{"modId": 306612, "relationType": 3}]
            }],
            "pagination": {"index": 0, "pageSize": 50, "resultCount": 1, "totalCount": 1}
        }"#;
        let page: Paged<File> = serde_json::from_str(body).unwrap();
        let file = &page.data[0];
        assert_eq!(file.id, 4003223);
        assert_eq!(file.mod_id, 306612);
        assert!(file.is_available);
        assert_eq!(file.release_type, 1);
        assert_eq!(file.hashes[0].algo, 1);
        assert_eq!(file.dependencies[0].relation_type, 3);
        assert_eq!(display_version(file), "Sodium 0.5.11");
        assert!(compatible(file, &Filter::new("1.20.1", "fabric")));
        assert!(!compatible(file, &Filter::new("1.19.2", "fabric")));
        assert!(accepts(ProjectKind::Mod, "sodium-fabric-0.5.11.jar"));
        assert!(!accepts(ProjectKind::ResourcePack, "mod.jar.disabled"));
    }

    #[test]
    fn keeps_sources_sidecar() {
        let mut sources = ContentSources::default();
        sources.record(
            ProjectKind::Mod,
            "sodium.jar.disabled",
            SourceEntry {
                project_id: 306612,
                file_id: 4003223,
                file_date: "2024-06-10T12:00:00Z".to_string(),
                version: "Sodium 0.5.11".to_string(),
                release_type: 1,
            },
        );
        assert_eq!(strip_disabled("sodium.jar.disabled"), "sodium.jar");
        let entries = sources.entries(ProjectKind::Mod);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "sodium.jar");
        assert!(sources.entries(ProjectKind::Shader).is_empty());
        let text = serde_json::to_string(&sources).unwrap();
        let back: ContentSources = serde_json::from_str(&text).unwrap();
        assert_eq!(back.files, sources.files);
        let mut sources = back;
        sources.remove(ProjectKind::Mod, "sodium.jar");
        assert!(sources.files.is_empty());
    }

    #[test]
    fn rejects_empty_keys() {
        let http = Http::new().unwrap();
        assert!(matches!(CurseForge::new(&http, "  "), Err(CurseForgeError::Invalid(_))));
        assert!(CurseForge::new(&http, "abc").is_ok());
    }
}
