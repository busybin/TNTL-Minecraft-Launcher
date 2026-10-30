use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use tntl_model::{AssetIndex, Environment, Library, MavenName, VersionJson, allowed, classifier_for};
use tntl_store::RootLayout;

use crate::endpoints;
use crate::http::{DownloadOutcome, DownloadRequest, Http, SourceError};

#[derive(Debug, Clone, serde::Serialize)]
pub struct InstallEvent {
    pub stage: &'static str,
    /// Files finished in this stage.
    pub done: usize,
    pub total: usize,
    /// Bytes downloaded in this stage. Counts retried bytes more than once.
    pub bytes: u64,
    /// Expected bytes for this stage, when every request declared a size.
    pub bytes_total: Option<u64>,
}

impl InstallEvent {
    fn new(stage: &'static str, done: usize, total: usize) -> Self {
        Self {
            stage,
            done,
            total,
            bytes: 0,
            bytes_total: None,
        }
    }
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct InstallReport {
    pub version_id: String,
    pub downloaded: usize,
    pub skipped: usize,
    pub bytes: u64,
    /// Instance the caller registered from this install, if any.
    #[serde(default)]
    pub instance: Option<String>,
}

#[derive(Debug, Clone)]
pub struct InstallOptions {
    pub assets: bool,
    pub concurrency: usize,
    /// Hash every asset object instead of skipping the ones already on disk.
    /// Used by repair passes, where a file can exist but be corrupt.
    pub verify_assets: bool,
}

impl Default for InstallOptions {
    fn default() -> Self {
        Self {
            assets: true,
            concurrency: 8,
            verify_assets: false,
        }
    }
}

pub struct Installer<'a> {
    http: &'a Http,
    roots: RootLayout,
    env: Environment,
}

impl<'a> Installer<'a> {
    pub fn new(http: &'a Http, roots: RootLayout, env: Environment) -> Self {
        Self { http, roots, env }
    }

    /// Download everything needed to run `id`: version JSON, client jar,
    /// libraries, natives, asset index, assets and the logging config.
    pub fn install(
        &self,
        id: &str,
        options: &InstallOptions,
        progress: &(dyn Fn(InstallEvent) + Send + Sync),
    ) -> Result<InstallReport, SourceError> {
        progress(InstallEvent::new("manifest", 1, 1));

        // Version chain, target first. A version JSON that already exists on
        // disk wins: loader profiles are written by us and never appear in the
        // Mojang manifest. The manifest itself is only fetched when a JSON is
        // missing, so repairing an installed instance works offline.
        let mut manifest = None;
        let mut chain = Vec::new();
        let mut current = Some(id.to_string());
        while let Some(version_id) = current {
            if chain.iter().any(|json: &VersionJson| json.id == version_id) {
                break;
            }
            let json = match self.load_local_version(&version_id) {
                Some(json) => json,
                None => {
                    let fetched = match manifest.take() {
                        Some(fetched) => fetched,
                        None => self.http.version_manifest()?,
                    };
                    let summary = self.http.find_version(&fetched, &version_id)?;
                    let json = self.fetch_and_store_version(&summary)?;
                    manifest = Some(fetched);
                    json
                }
            };
            current = json.inherits_from.clone();
            chain.push(json);
        }
        progress(InstallEvent::new("metadata", chain.len(), chain.len()));

        let mut queue: Vec<DownloadRequest> = Vec::new();
        let mut seen = HashSet::new();

        // Client jar of the first chain entry that has one.
        for version in &chain {
            if let Some(client) = version
                .downloads
                .as_ref()
                .and_then(|downloads| downloads.client.as_ref())
            {
                let dest = self.roots.client_jar(&version.id);
                if seen.insert(dest.clone()) {
                    let mut request = DownloadRequest::new(client.url.clone(), dest);
                    if let Some(sha1) = &client.sha1 {
                        request = request.with_sha1(sha1.clone());
                    }
                    if let Some(size) = client.size {
                        request = request.with_size(size);
                    }
                    queue.push(request);
                }
                break;
            }
        }

        let mut asset_index = None;
        for version in &chain {
            if asset_index.is_none() {
                asset_index.clone_from(&version.asset_index);
            }
            for library in &version.libraries {
                self.queue_library(library, &mut queue, &mut seen);
            }
            if let Some(logging) = version.logging.as_ref().and_then(|logging| logging.get("client"))
                && let Some(file) = &logging.file
                && let (Some(id), Some(url)) = (&file.id, &file.url)
            {
                let dest = self.roots.log_config(id);
                if seen.insert(dest.clone()) {
                    let mut request = DownloadRequest::new(url.clone(), dest);
                    if let Some(sha1) = &file.sha1 {
                        request = request.with_sha1(sha1.clone());
                    }
                    if let Some(size) = file.size {
                        request = request.with_size(size);
                    }
                    queue.push(request);
                }
            }
        }

        if let Some(index) = &asset_index {
            let dest = self.roots.asset_index(&index.id);
            if let Some(url) = &index.url
                && seen.insert(dest.clone())
            {
                let mut request = DownloadRequest::new(url.clone(), dest);
                if let Some(sha1) = &index.sha1 {
                    request = request.with_sha1(sha1.clone());
                }
                if let Some(size) = index.size {
                    request = request.with_size(size);
                }
                queue.push(request);
            }
        }

        let (downloaded, skipped, mut bytes) = self.download_all("libraries", &queue, options.concurrency, progress)?;

        if options.assets
            && let Some(index_ref) = &asset_index
        {
            let index_path = self.roots.asset_index(&index_ref.id);
            // A profile can name an index without publishing a URL. Without a
            // local copy there is simply nothing to install; failing here read
            // like a launcher bug.
            let index: AssetIndex = match std::fs::read(&index_path) {
                Ok(bytes_json) => serde_json::from_slice(&bytes_json).map_err(|error| SourceError::Io {
                    path: index_path.clone(),
                    source: std::io::Error::new(std::io::ErrorKind::InvalidData, error),
                })?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => AssetIndex::default(),
                Err(error) => {
                    return Err(SourceError::Io {
                        path: index_path.clone(),
                        source: error,
                    });
                }
            };
            let mut assets = Vec::new();
            let mut queued = HashSet::new();
            for object in index.objects.values() {
                let dest = self.roots.asset_object(&object.hash);
                if !queued.insert(dest.clone()) {
                    continue;
                }
                // A repair pass hands every object to the downloader so its
                // hash decides; normal installs only queue what is missing.
                if !options.verify_assets && dest.exists() {
                    continue;
                }
                let prefix = object.hash.get(..2).unwrap_or("00");
                assets.push(
                    DownloadRequest::new(format!("{}{prefix}/{}", endpoints::RESOURCES, object.hash), dest)
                        .with_sha1(object.hash.clone())
                        .with_size(object.size),
                );
            }
            let (asset_downloaded, asset_skipped, asset_bytes) =
                self.download_all("assets", &assets, options.concurrency, progress)?;
            bytes += asset_bytes;
            if index.r#virtual == Some(true) {
                // A repair pass already put a good object back, but an existing
                // hard link still points at the old inode, so relink everything.
                tntl_store::materialize_asset_index(
                    &self.roots,
                    &index_path,
                    &self.roots.assets_virtual_dir(&index_ref.id),
                    options.verify_assets,
                )?;
            }
            return Ok(InstallReport {
                version_id: id.to_string(),
                downloaded: downloaded + asset_downloaded,
                skipped: skipped + asset_skipped,
                bytes,
                ..Default::default()
            });
        }

        Ok(InstallReport {
            version_id: id.to_string(),
            downloaded,
            skipped,
            bytes,
            ..Default::default()
        })
    }

    /// Read a version JSON that is already installed (vanilla or a loader
    /// profile we wrote). Malformed files fall back to the manifest.
    fn load_local_version(&self, id: &str) -> Option<VersionJson> {
        let bytes = std::fs::read(self.roots.version_json(id)).ok()?;
        let json: VersionJson = serde_json::from_slice(&bytes).ok()?;
        if json.id == id { Some(json) } else { None }
    }

    fn fetch_and_store_version(&self, summary: &tntl_model::VersionSummary) -> Result<VersionJson, SourceError> {
        let body = self.http.get_bytes(&summary.url, 16 * 1024 * 1024)?;
        if let Some(expected) = &summary.sha1 {
            let actual = tntl_store::sha1_bytes(&body);
            if !actual.eq_ignore_ascii_case(expected) {
                return Err(SourceError::Hash {
                    path: self.roots.version_json(&summary.id),
                    expected: expected.clone(),
                    actual,
                });
            }
        }
        let json: VersionJson = serde_json::from_slice(&body).map_err(|error| SourceError::Io {
            path: self.roots.version_json(&summary.id),
            source: std::io::Error::new(std::io::ErrorKind::InvalidData, error),
        })?;
        // Keep a hand-fixed or other-launcher JSON instead of overwriting it.
        if let Err(error) = tntl_store::backup_existing(&self.roots.version_json(&summary.id)) {
            return Err(SourceError::Io {
                path: self.roots.version_json(&summary.id),
                source: error,
            });
        }
        tntl_store::atomic_write(&self.roots.version_json(&summary.id), &body)?;
        Ok(json)
    }

    fn queue_library(
        &self,
        library: &Library,
        queue: &mut Vec<DownloadRequest>,
        seen: &mut HashSet<std::path::PathBuf>,
    ) {
        for request in library_requests(&self.env, &self.roots, library) {
            if seen.insert(request.dest.clone()) {
                queue.push(request);
            }
        }
    }

    fn download_all(
        &self,
        stage: &'static str,
        requests: &[DownloadRequest],
        concurrency: usize,
        progress: &(dyn Fn(InstallEvent) + Send + Sync),
    ) -> Result<(usize, usize, u64), SourceError> {
        if requests.is_empty() {
            progress(InstallEvent::new(stage, 0, 0));
            return Ok((0, 0, 0));
        }
        let concurrency = concurrency.clamp(1, 32).min(requests.len());
        let next = AtomicUsize::new(0);
        let downloaded = AtomicUsize::new(0);
        let skipped = AtomicUsize::new(0);
        let bytes = AtomicUsize::new(0);
        let errors: Mutex<Vec<SourceError>> = Mutex::new(Vec::new());
        let bytes_total = requests
            .iter()
            .map(|request| request.size)
            .try_fold(0u64, |sum, size| size.map(|size| sum + size));
        let started = std::time::Instant::now();
        let last_emit = AtomicUsize::new(0);
        let emit = |done: usize| {
            progress(InstallEvent {
                stage,
                done,
                total: requests.len(),
                bytes: bytes.load(Ordering::Relaxed) as u64,
                bytes_total,
            });
        };

        std::thread::scope(|scope| {
            for _ in 0..concurrency {
                scope.spawn(|| {
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(request) = requests.get(index) else {
                            break;
                        };
                        let mut last = 0u64;
                        let result = self.http.download(request, &mut |size| {
                            // A retry restarts this file's counter at zero. Only
                            // forward deltas are added, so the total never
                            // shrinks and retried bytes are not counted twice.
                            if size > last {
                                bytes.fetch_add((size - last) as usize, Ordering::Relaxed);
                            }
                            last = size;
                            // At most one event per 100 ms keeps the channel
                            // cheap while still tracking slow connections.
                            let elapsed = started.elapsed().as_millis() as usize;
                            if elapsed.saturating_sub(last_emit.load(Ordering::Relaxed)) >= 100
                                && last_emit.swap(elapsed, Ordering::Relaxed) != elapsed
                            {
                                emit(next.load(Ordering::Relaxed).min(requests.len()));
                            }
                        });
                        match result {
                            Ok(DownloadOutcome::AlreadyValid) => {
                                skipped.fetch_add(1, Ordering::Relaxed);
                                emit(next.load(Ordering::Relaxed).min(requests.len()));
                            }
                            Ok(DownloadOutcome::Downloaded) => {
                                downloaded.fetch_add(1, Ordering::Relaxed);
                                emit(next.load(Ordering::Relaxed).min(requests.len()));
                            }
                            Err(error) => {
                                errors.lock().unwrap_or_else(|poison| poison.into_inner()).push(error);
                            }
                        }
                    }
                });
            }
        });

        if let Some(error) = errors
            .into_inner()
            .unwrap_or_else(|poison| poison.into_inner())
            .into_iter()
            .next()
        {
            return Err(error);
        }
        let bytes_done = bytes.load(Ordering::Relaxed) as u64;
        progress(InstallEvent {
            stage,
            done: requests.len(),
            total: requests.len(),
            bytes: bytes_done,
            bytes_total,
        });
        Ok((
            downloaded.load(Ordering::Relaxed),
            skipped.load(Ordering::Relaxed),
            bytes_done,
        ))
    }
}

/// Everything one library contributes to an install: its main artifact and,
/// when the library declares a native for this OS, the classifier jar.
///
/// Mojang's JSON carries a `downloads` block with absolute URLs. Loader
/// profiles (Fabric, Quilt, Forge) instead ship `name` + `url` and expect the
/// launcher to build `url/group/path/artifact-version.jar`.
fn library_requests(env: &Environment, roots: &RootLayout, library: &Library) -> Vec<DownloadRequest> {
    if !allowed(library.rules.as_deref().unwrap_or_default(), env) {
        return Vec::new();
    }
    let Some(name) = MavenName::parse(&library.name) else {
        return Vec::new();
    };
    let artifact = library
        .downloads
        .as_ref()
        .and_then(|downloads| downloads.artifact.as_ref());
    let base = artifact
        .map(|artifact| artifact.url.clone())
        .filter(|url| !url.is_empty());
    let relative = artifact
        .and_then(|artifact| artifact.path.clone())
        .unwrap_or_else(|| name.path());

    let mut requests = Vec::new();
    let native = classifier_for(env.os_name(), env.arch.native_bits(), library);
    // A natives-only library (Legacy Fabric's patched LWJGL is one) publishes
    // nothing at the plain coordinate; only the classifier jars exist.
    let natives_only = native.is_some() && artifact.is_none();
    let url = base.unwrap_or_else(|| join_maven(library.url.as_deref().unwrap_or(endpoints::LIBRARIES), &relative));
    if !natives_only {
        let request = artifact_request(
            url,
            &relative,
            artifact
                .and_then(|artifact| artifact.sha1.clone())
                .or_else(|| library.sha1.clone()),
            artifact.and_then(|artifact| artifact.size).or(library.size),
            roots,
        );
        requests.push(request);
    }

    if let Some(classifier) = native {
        let native = library
            .downloads
            .as_ref()
            .and_then(|downloads| downloads.classifiers.as_ref())
            .and_then(|classifiers| classifiers.get(&classifier));
        let relative = native
            .and_then(|artifact| artifact.path.clone())
            .unwrap_or_else(|| name.with_classifier(&classifier).path());
        let base = native
            .map(|artifact| artifact.url.clone())
            .filter(|url| !url.is_empty());
        let url = base.unwrap_or_else(|| join_maven(library.url.as_deref().unwrap_or(endpoints::LIBRARIES), &relative));
        requests.push(artifact_request(
            url,
            &relative,
            native
                .and_then(|artifact| artifact.sha1.clone())
                .or_else(|| library.sha1.clone()),
            native.and_then(|artifact| artifact.size),
            roots,
        ));
    }
    requests
}

fn join_maven(base: &str, relative: &str) -> String {
    let base = base.trim();
    let base = base.strip_suffix('/').unwrap_or(base);
    format!("{base}/{relative}")
}

fn artifact_request(
    url: String,
    relative: &str,
    sha1: Option<String>,
    size: Option<u64>,
    roots: &RootLayout,
) -> DownloadRequest {
    let mut request = DownloadRequest::new(url, roots.library(relative));
    if let Some(sha1) = sha1 {
        request = request.with_sha1(sha1);
    }
    if let Some(size) = size {
        request = request.with_size(size);
    }
    request
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library(json: &str) -> Library {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn mojang_library_uses_its_download_url() {
        let roots = RootLayout::new("/tmp/tntl-test");
        let library = library(
            r#"{"name":"com.example:thing:1.0","downloads":{"artifact":{
                 "path":"com/example/thing/1.0/thing-1.0.jar",
                 "url":"https://libraries.minecraft.net/com/example/thing/1.0/thing-1.0.jar",
                 "sha1":"aa","size":7}}}"#,
        );
        let requests = library_requests(&Environment::current(), &roots, &library);
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].url,
            "https://libraries.minecraft.net/com/example/thing/1.0/thing-1.0.jar"
        );
        assert_eq!(requests[0].dest, roots.library("com/example/thing/1.0/thing-1.0.jar"));
        assert_eq!(requests[0].sha1.as_deref(), Some("aa"));
        assert_eq!(requests[0].size, Some(7));
    }

    #[test]
    fn loader_library_builds_a_maven_url_from_its_base() {
        let roots = RootLayout::new("/tmp/tntl-test");
        // Shape used by Fabric/Quilt profiles: no downloads block, base url,
        // and hash metadata at the library level.
        let library = library(
            r#"{"name":"org.ow2.asm:asm:9.7.1","url":"https://maven.fabricmc.net/",
                "sha1":"f0ed","size":126093}"#,
        );
        let requests = library_requests(&Environment::current(), &roots, &library);
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].url,
            "https://maven.fabricmc.net/org/ow2/asm/asm/9.7.1/asm-9.7.1.jar"
        );
        assert_eq!(requests[0].dest, roots.library("org/ow2/asm/asm/9.7.1/asm-9.7.1.jar"));
        assert_eq!(requests[0].sha1.as_deref(), Some("f0ed"));
        assert_eq!(requests[0].size, Some(126093));
    }

    #[test]
    fn legacy_library_without_url_falls_back_to_mojang() {
        let roots = RootLayout::new("/tmp/tntl-test");
        let library = library(r#"{"name":"net.sf.jopt-simple:jopt-simple:4.5"}"#);
        let requests = library_requests(&Environment::current(), &roots, &library);
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].url,
            "https://libraries.minecraft.net/net/sf/jopt-simple/jopt-simple/4.5/jopt-simple-4.5.jar"
        );
    }

    #[test]
    fn rule_excluded_library_is_not_queued() {
        let roots = RootLayout::new("/tmp/tntl-test");
        let library =
            library(r#"{"name":"com.example:thing:1.0","rules":[{"action":"allow","os":{"name":"windows"}}]}"#);
        let requests = library_requests(&Environment::current(), &roots, &library);
        if Environment::current().os_name() != "windows" {
            assert!(requests.is_empty());
        }
    }
}
