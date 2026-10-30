//! Turn a version tree into a [`LaunchPlan`].
//!
//! Resolution is deterministic: same inputs, same plan. It reads file metadata
//! to report what is missing but never downloads anything.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tntl_model::{
    AssetIndex, AssetRef, Environment, JavaVersion, Library, Logging, MavenName, VersionDownloads, VersionJson,
    allowed, classifier_for, expand_placeholders,
};
use tntl_store::{RootLayout, verify_file};

#[derive(Debug, Error)]
pub enum ResolveError {
    #[error("version chain is empty")]
    EmptyChain,
    #[error("version {0} has no main class")]
    MissingMainClass(String),
    #[error("invalid maven coordinate: {0}")]
    InvalidMavenName(String),
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyMode {
    /// Only check that files exist.
    Exists,
    /// Check size and SHA-1 as well. Slow, used by `verify`/`repair`.
    Hash,
}

pub use tntl_auth::Account;

/// Offline UUID, re-exported for callers that only depend on this crate.
pub use tntl_auth::offline_uuid;

#[derive(Debug, Clone)]
pub struct InstanceOptions {
    pub game_dir: PathBuf,
    pub natives_dir: PathBuf,
    pub java: PathBuf,
    pub min_memory_mb: Option<u32>,
    pub max_memory_mb: u32,
    pub extra_jvm_args: Vec<String>,
    pub extra_game_args: Vec<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub demo: bool,
    pub server: Option<(String, u16)>,
    /// Use `--quickPlayMultiplayer` instead of `--server/--port` (1.20+).
    pub quick_play: bool,
    pub env: BTreeMap<String, String>,
}

impl InstanceOptions {
    pub fn new(game_dir: impl Into<PathBuf>, natives_dir: impl Into<PathBuf>) -> Self {
        Self {
            game_dir: game_dir.into(),
            natives_dir: natives_dir.into(),
            java: PathBuf::from("java"),
            min_memory_mb: None,
            max_memory_mb: 2048,
            extra_jvm_args: Vec::new(),
            extra_game_args: Vec::new(),
            width: None,
            height: None,
            demo: false,
            server: None,
            quick_play: false,
            env: BTreeMap::new(),
        }
    }
}

/// One entry in `arguments.game` or `arguments.jvm`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeJar {
    pub path: PathBuf,
    pub excludes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissingKind {
    VersionJson,
    ClientJar,
    Library,
    Native,
    AssetIndex,
    Asset,
    LoggingConfig,
    Java,
}

impl MissingKind {
    /// Some files are nice to have but never stop the game. A missing log4j
    /// config is one: the game keeps its built-in logging defaults, and other
    /// launchers (HMCL, Prism) do not download it either, so blocking the
    /// launch on it makes their instances look broken.
    pub fn is_optional(self) -> bool {
        matches!(self, MissingKind::LoggingConfig)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissingObject {
    pub kind: MissingKind,
    pub path: PathBuf,
    pub sha1: Option<String>,
    pub size: Option<u64>,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub level: Level,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchPlan {
    pub version_id: String,
    pub java: PathBuf,
    pub jvm_args: Vec<String>,
    pub main_class: String,
    pub classpath: Vec<PathBuf>,
    pub natives_dir: PathBuf,
    pub natives: Vec<NativeJar>,
    pub game_args: Vec<String>,
    pub working_dir: PathBuf,
    pub env: BTreeMap<String, String>,
    pub asset_index: Option<PathBuf>,
    pub asset_dir: PathBuf,
    /// Plain name tree some pre-1.7 versions read instead of the object store.
    /// The launcher builds it right before spawning.
    #[serde(default)]
    pub legacy_assets: Option<LegacyAssetTree>,
    pub warnings: Vec<Diagnostic>,
    pub missing: Vec<MissingObject>,
}

/// A tree of asset names linked out of the object store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LegacyAssetTree {
    /// Index the tree is built from.
    pub index: PathBuf,
    /// Directory the game reads.
    pub dir: PathBuf,
    /// Launcher root that holds `assets/objects`.
    pub root: PathBuf,
}

impl LaunchPlan {
    /// A copy that is safe to print, log or paste into a bug report: the session
    /// credentials the game arguments carry are masked. Everything else (paths,
    /// classpath, warnings) is kept so the plan stays useful for debugging.
    pub fn redacted(&self) -> Self {
        const SECRET_FLAGS: [&str; 2] = ["--accessToken", "--session"];
        fn redact(args: &[String]) -> Vec<String> {
            let mut out = Vec::with_capacity(args.len());
            let mut mask_next = false;
            for arg in args {
                if mask_next {
                    out.push("<redacted>".to_string());
                    mask_next = false;
                    continue;
                }
                mask_next = SECRET_FLAGS.contains(&arg.as_str());
                out.push(arg.clone());
            }
            out
        }
        let mut plan = self.clone();
        plan.game_args = redact(&plan.game_args);
        plan.jvm_args = redact(&plan.jvm_args);
        for (key, value) in plan.env.iter_mut() {
            let upper = key.to_ascii_uppercase();
            if upper.contains("TOKEN") || upper.contains("SECRET") || upper.contains("PASSWORD") {
                *value = "<redacted>".to_string();
            }
        }
        plan
    }

    /// What actually keeps the game from starting.
    pub fn blocking_missing(&self) -> impl Iterator<Item = &MissingObject> {
        self.missing.iter().filter(|entry| !entry.kind.is_optional())
    }

    pub fn is_ready(&self) -> bool {
        self.blocking_missing().next().is_none()
    }

    pub fn executable(&self) -> bool {
        !self
            .blocking_missing()
            .any(|missing| matches!(missing.kind, MissingKind::Java))
    }
}

#[derive(Debug, Clone)]
pub struct ResolveInput {
    /// Target version first, then its parents (`inheritsFrom` chain).
    pub chain: Vec<VersionJson>,
    /// Directory name of the target version. Launchers rename a directory
    /// without rewriting the `id` inside its JSON (HMCL keeps
    /// `neoforge-21.1.264` in `versions/1.21.1-Neoforge`), and every file path
    /// and `--version` follows the directory, not the JSON.
    pub version_dir: Option<String>,
    pub roots: RootLayout,
    pub env: Environment,
    pub account: Account,
    pub options: InstanceOptions,
    pub verify: VerifyMode,
    /// Maven base used by libraries that carry a coordinate but no downloads.
    pub library_base_url: String,
    pub launcher_version: String,
}

/// What the merge of a version chain looks like.
#[derive(Debug, Clone, Default)]
pub struct MergedVersion {
    pub id: String,
    pub main_class: Option<String>,
    pub libraries: Vec<Library>,
    pub game_args: Vec<String>,
    pub jvm_args: Vec<String>,
    pub legacy_args: Option<String>,
    pub asset_index: Option<AssetRef>,
    pub assets: Option<String>,
    pub downloads: Option<VersionDownloads>,
    pub java_version: Option<JavaVersion>,
    pub logging: Option<BTreeMap<String, Logging>>,
    pub version_type: Option<String>,
}

/// Merge child-first chain. The child wins for scalars, libraries are
/// concatenated child-first with duplicates removed, and arguments are ordered
/// parent-first so the child's arguments come last.
pub fn merge(chain: &[VersionJson], env: &Environment) -> Result<MergedVersion, ResolveError> {
    let target = chain.first().ok_or(ResolveError::EmptyChain)?;
    let mut merged = MergedVersion {
        id: target.id.clone(),
        ..MergedVersion::default()
    };
    let mut seen = HashSet::new();
    for version in chain {
        for library in &version.libraries {
            let key = MavenName::parse(&library.name)
                .map(|name| {
                    format!(
                        "{}:{}:{:?}:{}",
                        name.group, name.artifact, name.classifier, name.extension
                    )
                })
                .unwrap_or_else(|| library.name.clone());
            if seen.insert(key) {
                merged.libraries.push(library.clone());
            }
        }
    }
    for version in chain.iter().rev() {
        if let Some(arguments) = &version.arguments {
            merged.game_args.extend(arguments.game_args(env));
            merged.jvm_args.extend(arguments.jvm_args(env));
        }
    }
    for version in chain {
        if merged.main_class.is_none() {
            merged.main_class.clone_from(&version.main_class);
        }
        if merged.legacy_args.is_none() {
            merged.legacy_args.clone_from(&version.minecraft_arguments);
        }
        if merged.asset_index.is_none() {
            merged.asset_index.clone_from(&version.asset_index);
        }
        if merged.assets.is_none() {
            merged.assets.clone_from(&version.assets);
        }
        if merged.downloads.is_none() {
            merged.downloads.clone_from(&version.downloads);
        }
        if merged.java_version.is_none() {
            merged.java_version.clone_from(&version.java_version);
        }
        if merged.logging.as_ref().is_none_or(|logging| logging.is_empty())
            && version.logging.as_ref().is_some_and(|logging| !logging.is_empty())
        {
            // Forge ships `"logging": {}` in its version JSON; an empty map
            // must not hide the parent's client log4j config.
            merged.logging.clone_from(&version.logging);
        }
        if merged.version_type.is_none() {
            merged.version_type.clone_from(&version.version_type);
        }
    }
    Ok(merged)
}

type ArtifactInfo = (PathBuf, Option<String>, Option<u64>, Option<String>);
type LoggingInfo = (String, PathBuf, Option<String>, Option<u64>, Option<String>);

struct Planner<'a> {
    input: &'a ResolveInput,
    merged: MergedVersion,
    classpath: Vec<PathBuf>,
    natives: Vec<NativeJar>,
    missing: Vec<MissingObject>,
    warnings: Vec<Diagnostic>,
}

impl<'a> Planner<'a> {
    fn probe(&self, path: &Path, sha1: Option<&str>, size: Option<u64>) -> bool {
        match self.input.verify {
            VerifyMode::Exists => path.exists(),
            VerifyMode::Hash => verify_file(path, sha1, size).is_ok(),
        }
    }

    fn missing(
        &mut self,
        kind: MissingKind,
        path: PathBuf,
        sha1: Option<String>,
        size: Option<u64>,
        url: Option<String>,
    ) {
        if self
            .missing
            .iter()
            .any(|entry| entry.kind == kind && entry.path == path)
        {
            return;
        }
        self.missing.push(MissingObject {
            kind,
            path,
            sha1,
            size,
            url,
        });
    }

    fn library_path(&self, library: &Library) -> Option<ArtifactInfo> {
        let name = MavenName::parse(&library.name)?;
        let (relative, artifact) = match &library.downloads {
            Some(downloads) => match &downloads.artifact {
                Some(artifact) => (artifact.path.clone().unwrap_or_else(|| name.path()), Some(artifact)),
                None => (name.path(), None),
            },
            None => (name.path(), None),
        };
        let url = artifact
            .map(|artifact| artifact.url.clone())
            .filter(|url| !url.is_empty())
            .or_else(|| {
                library
                    .url
                    .as_ref()
                    .map(|base| format!("{}{relative}", ensure_slash(base)))
            })
            .or_else(|| Some(format!("{}{relative}", ensure_slash(&self.input.library_base_url))));
        Some((
            self.input.roots.library(&relative),
            artifact
                .and_then(|artifact| artifact.sha1.clone())
                .or_else(|| library.sha1.clone()),
            artifact.and_then(|artifact| artifact.size).or(library.size),
            url,
        ))
    }

    fn natives(&mut self) {
        let os = self.input.env.os_name();
        let bits = self.input.env.arch.native_bits();
        let libraries = std::mem::take(&mut self.merged.libraries);
        for library in &libraries {
            if !allowed(library.rules.as_deref().unwrap_or_default(), &self.input.env) {
                continue;
            }
            let Some(classifier) = classifier_for(os, bits, library) else {
                continue;
            };
            let excludes = library
                .extract
                .as_ref()
                .and_then(|extract| extract.exclude.clone())
                .unwrap_or_else(|| vec!["META-INF/".to_string()]);
            let Some(name) = MavenName::parse(&library.name) else {
                continue;
            };
            let native = library
                .downloads
                .as_ref()
                .and_then(|downloads| downloads.classifiers.as_ref())
                .and_then(|classifiers| classifiers.get(&classifier));
            let relative = native
                .and_then(|artifact| artifact.path.clone())
                .unwrap_or_else(|| name.with_classifier(&classifier).path());
            let sha1 = native
                .and_then(|artifact| artifact.sha1.clone())
                .or_else(|| library.sha1.clone());
            let size = native.and_then(|artifact| artifact.size).or(library.size);
            let url = native
                .map(|artifact| artifact.url.clone())
                .filter(|url| !url.is_empty())
                .or_else(|| {
                    library
                        .url
                        .as_ref()
                        .map(|base| format!("{}{relative}", ensure_slash(base)))
                })
                .unwrap_or_else(|| format!("{}{relative}", ensure_slash(&self.input.library_base_url)));
            let path = self.input.roots.library(&relative);
            if !self.probe(&path, sha1.as_deref(), size) {
                self.missing(MissingKind::Native, path.clone(), sha1, size, Some(url));
            }
            self.natives.push(NativeJar { path, excludes });
        }
        self.merged.libraries = libraries;
    }
}

fn ensure_slash(url: &str) -> String {
    if url.ends_with('/') {
        url.to_string()
    } else {
        format!("{url}/")
    }
}

pub fn plan(input: &ResolveInput) -> Result<LaunchPlan, ResolveError> {
    let merged = merge(&input.chain, &input.env)?;
    let version_id = merged.id.clone();
    let version_dir = input.version_dir.clone().unwrap_or_else(|| version_id.clone());
    let main_class = merged
        .main_class
        .clone()
        .ok_or_else(|| ResolveError::MissingMainClass(merged.id.clone()))?;
    let mut planner = Planner {
        input,
        merged,
        classpath: Vec::new(),
        natives: Vec::new(),
        missing: Vec::new(),
        warnings: Vec::new(),
    };

    // Client jar: prefer the target version, fall back to an ancestor's jar
    // (loader versions such as Forge ship no jar of their own). Each chain
    // entry's directory is the id we loaded it by: the requested version for
    // the first entry, the previous entry's `inheritsFrom` for the rest.
    let mut client_jar = None;
    let mut client_download: Option<(String, tntl_model::Artifact)> = None;
    let mut parent_id: Option<&str> = Some(version_dir.as_str());
    for version in &input.chain {
        let dir_id = parent_id.unwrap_or(version.id.as_str());
        parent_id = version.inherits_from.as_deref();
        let path = input.roots.client_jar(dir_id);
        let download = version
            .downloads
            .as_ref()
            .and_then(|downloads| downloads.client.clone());
        let sha1 = download.as_ref().and_then(|download| download.sha1.clone());
        let size = download.as_ref().and_then(|download| download.size);
        if planner.probe(&path, sha1.as_deref(), size) {
            client_jar = Some(path);
            break;
        }
        if client_download.is_none()
            && let Some(download) = download
        {
            client_download = Some((dir_id.to_string(), download));
        }
    }
    let client_jar = match client_jar {
        Some(path) => path,
        None => {
            let (id, download) = match client_download {
                Some(value) => value,
                None => (version_dir.clone(), tntl_model::Artifact::default()),
            };
            let path = input.roots.client_jar(&id);
            planner.missing(
                MissingKind::ClientJar,
                path.clone(),
                download.sha1.clone(),
                download.size,
                Some(download.url.clone()).filter(|url| !url.is_empty()),
            );
            path
        }
    };
    planner.classpath.push(client_jar);

    let mut seen = HashSet::new();
    let libraries = std::mem::take(&mut planner.merged.libraries);
    for library in &libraries {
        if !allowed(library.rules.as_deref().unwrap_or_default(), &input.env) {
            continue;
        }
        // Natives-only libraries (LWJGL 2 platform jars) have no plain
        // artifact; the classifier jar is all the game needs.
        let natives_only = library
            .downloads
            .as_ref()
            .and_then(|downloads| downloads.artifact.as_ref())
            .is_none()
            && classifier_for(input.env.os_name(), input.env.arch.native_bits(), library).is_some();
        if natives_only {
            continue;
        }
        let Some((path, sha1, size, url)) = planner.library_path(library) else {
            planner.warnings.push(Diagnostic {
                level: Level::Warning,
                code: "invalid-library".to_string(),
                message: format!("library has an invalid maven coordinate: {}", library.name),
            });
            continue;
        };
        if !seen.insert(path.clone()) {
            continue;
        }
        if !planner.probe(&path, sha1.as_deref(), size) {
            planner.missing(MissingKind::Library, path.clone(), sha1, size, url);
        }
        planner.classpath.push(path);
    }
    planner.merged.libraries = libraries;
    planner.natives();

    // Asset index.
    let mut asset_index = None;
    if let Some(reference) = &planner.merged.asset_index {
        let path = input.roots.asset_index(&reference.id);
        asset_index = Some(path.clone());
        if !planner.probe(&path, reference.sha1.as_deref(), reference.size) {
            planner.missing(
                MissingKind::AssetIndex,
                path,
                reference.sha1.clone(),
                reference.size,
                reference.url.clone(),
            );
        }
    }

    // Asset objects are only enumerated in hash mode: a full check touches
    // thousands of files and is what `tntl verify` is for.
    if input.verify == VerifyMode::Hash
        && let Some(index_path) = &asset_index
        && index_path.exists()
        && let Ok(bytes) = std::fs::read(index_path)
        && let Ok(index) = serde_json::from_slice::<AssetIndex>(&bytes)
    {
        for object in index.objects.values() {
            let path = input.roots.asset_object(&object.hash);
            if !planner.probe(&path, Some(&object.hash), Some(object.size)) {
                let prefix = object.hash.get(..2).unwrap_or("00");
                planner.missing(
                    MissingKind::Asset,
                    path,
                    Some(object.hash.clone()),
                    Some(object.size),
                    Some(format!(
                        "https://resources.download.minecraft.net/{prefix}/{}",
                        object.hash
                    )),
                );
            }
        }
    }

    // Logging config.
    let mut logging_arg = None;
    let client_logging = planner
        .merged
        .logging
        .as_ref()
        .and_then(|logging| logging.get("client"))
        .cloned();
    if let Some(logging) = client_logging
        && let Some((_id, path, sha1, size, url)) = logging_config(input, &logging)
    {
        if planner.probe(&path, sha1.as_deref(), size) {
            if let Some(argument) = &logging.argument {
                logging_arg = Some((argument.clone(), path));
            }
        } else {
            // Never hand the JVM a `-Dlog4j.configurationFile` that points at
            // a file we do not have; log4j would report a broken configuration.
            planner.missing(MissingKind::LoggingConfig, path.clone(), sha1, size, url);
        }
    }

    if !planner.probe(&input.options.java, None, None) {
        planner.missing(MissingKind::Java, input.options.java.clone(), None, None, None);
    }

    let features = |name: &str| input.env.features.get(name).copied().unwrap_or(false);
    let classpath_separator = if input.env.os_name() == "windows" { ";" } else { ":" };
    let classpath = planner
        .classpath
        .iter()
        .map(|path| path.to_string_lossy())
        .collect::<Vec<_>>()
        .join(classpath_separator);

    let parsed_index = asset_index
        .as_deref()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<AssetIndex>(&bytes).ok());
    let legacy_tree = legacy_asset_layout(
        &input.roots,
        planner.merged.asset_index.as_ref(),
        parsed_index.as_ref(),
        &input.options.game_dir,
    );
    let assets_root = legacy_tree
        .as_deref()
        .map(|dir| dir.to_path_buf())
        .unwrap_or_else(|| input.roots.assets_dir());

    // A legacy parent can contribute `minecraftArguments` while a child adds
    // modern `arguments.game`; using the legacy string alone would drop the
    // child's flags. Prefer the structured list and append what it lacks.
    let mut game_args = planner.merged.game_args.clone();
    if let Some(legacy) = planner.merged.legacy_args.as_deref() {
        for token in legacy.split_whitespace() {
            if !game_args.iter().any(|existing| existing == token) {
                game_args.push(token.to_string());
            }
        }
    }
    for arg in &mut game_args {
        let value = expand_placeholders(arg, &|key| {
            lookup_variable(
                key,
                &input.options,
                &input.account,
                &input.roots,
                &planner.merged,
                &assets_root,
                &classpath,
                classpath_separator,
                &input.launcher_version,
                &version_dir,
            )
        });
        *arg = value;
    }
    if let Some((host, port)) = &input.options.server {
        if input.options.quick_play {
            game_args.push("--quickPlayMultiplayer".to_string());
            game_args.push(format!("{host}:{port}"));
        } else {
            game_args.push("--server".to_string());
            game_args.push(host.clone());
            game_args.push("--port".to_string());
            game_args.push(port.to_string());
        }
    }
    // Modern version JSONs gate `--demo` on the `is_demo_user` feature, so the
    // argument is already in the list. `minecraftArguments` builds (1.5 and
    // older) never mention it and need it appended.
    if (input.options.demo || features("is_demo_user"))
        && planner.merged.legacy_args.is_some()
        && !game_args.iter().any(|arg| arg == "--demo")
    {
        game_args.push("--demo".to_string());
    }
    game_args.extend(input.options.extra_game_args.iter().cloned());

    let mut jvm_args = Vec::new();
    // A hand-edited or imported instance can carry 0 or a min above max; the
    // JVM refuses both with a message users read as a launcher bug.
    let max_memory = input.options.max_memory_mb.clamp(256, 65536);
    if let Some(min) = input.options.min_memory_mb {
        jvm_args.push(format!("-Xms{}M", min.min(max_memory)));
    }
    jvm_args.push(format!("-Xmx{max_memory}M"));
    if let Some(argument) = logging_arg {
        let (template, path) = argument;
        jvm_args.push(expand_placeholders(&template, &|key| {
            (key == "path").then(|| path.display().to_string())
        }));
    }
    let expanded_jvm = planner
        .merged
        .jvm_args
        .iter()
        .map(|arg| {
            expand_placeholders(arg, &|key| {
                lookup_variable(
                    key,
                    &input.options,
                    &input.account,
                    &input.roots,
                    &planner.merged,
                    &assets_root,
                    &classpath,
                    classpath_separator,
                    &input.launcher_version,
                    &version_dir,
                )
            })
        })
        .collect::<Vec<_>>();
    jvm_args.extend(expanded_jvm);
    if !jvm_args
        .iter()
        .any(|arg| arg == "-cp" || arg == "-classpath" || arg.starts_with("-cp="))
    {
        jvm_args.push("-cp".to_string());
        jvm_args.push(classpath.clone());
    }
    jvm_args.extend(input.options.extra_jvm_args.iter().cloned());
    // The live console decodes the JVM's stdout as UTF-8. On Windows the
    // output encoding follows the console code page (GBK on Chinese systems),
    // which turned every non-ASCII log line into mojibake in the GUI, the saved
    // launch log and the crash summary. These properties only affect the
    // standard streams; file names and file contents keep the platform default.
    if input.env.os_name() == "windows" {
        for flag in [
            "-Dstdout.encoding=UTF-8",
            "-Dstderr.encoding=UTF-8",
            "-Dsun.stdout.encoding=UTF-8",
            "-Dsun.stderr.encoding=UTF-8",
        ] {
            let key = flag.split('=').next().unwrap_or(flag);
            if !jvm_args
                .iter()
                .any(|arg| arg == key || arg.starts_with(&format!("{key}=")))
            {
                jvm_args.push(flag.to_string());
            }
        }
    }
    // Versions before 1.13 list no JVM arguments at all, so nothing points the
    // JVM at the extracted natives.
    if !jvm_args.iter().any(|arg| arg.starts_with("-Djava.library.path")) {
        jvm_args.push(format!("-Djava.library.path={}", input.options.natives_dir.display()));
    }
    // FML 1.6 and older check the certificate of the client jar. JDK builds
    // that refuse SHA-1 signed jars report no certificates at all, so the
    // check has to be relaxed or Forge refuses to start.
    if legacy_tree.is_some()
        && planner.merged.main_class.as_deref() == Some("net.minecraft.launchwrapper.Launch")
        && planner
            .merged
            .legacy_args
            .as_deref()
            .is_some_and(|args| args.contains("cpw.mods.fml.common.launcher.FMLTweaker"))
    {
        jvm_args.push("-Dfml.ignoreInvalidMinecraftCertificates=true".to_string());
    }

    Ok(LaunchPlan {
        version_id,
        java: input.options.java.clone(),
        jvm_args,
        main_class,
        classpath: planner.classpath,
        natives_dir: input.options.natives_dir.clone(),
        natives: planner.natives,
        game_args,
        working_dir: input.options.game_dir.clone(),
        env: input.options.env.clone(),
        legacy_assets: legacy_tree.map(|dir| LegacyAssetTree {
            index: asset_index.clone().unwrap_or_default(),
            dir,
            root: input.roots.root.clone(),
        }),
        asset_index,
        asset_dir: assets_root,
        warnings: planner.warnings,
        missing: planner.missing,
    })
}

fn logging_config(input: &ResolveInput, logging: &Logging) -> Option<LoggingInfo> {
    let file = logging.file.as_ref()?;
    let id = file.id.clone().unwrap_or_else(|| "client-log4j.xml".to_string());
    let path = input.roots.log_config(&id);
    Some((id, path, file.sha1.clone(), file.size, file.url.clone()))
}

/// Directory the game reads assets from, when the index is a plain name tree.
///
/// 1.6 and older (`virtual`) read a tree next to the object store. 1.5 and
/// older (`mapToResources`) read `<gameDir>/resources`, which is why the
/// launcher packs it there instead of into the shared root.
fn legacy_asset_layout(
    roots: &RootLayout,
    reference: Option<&AssetRef>,
    index: Option<&AssetIndex>,
    game_dir: &Path,
) -> Option<PathBuf> {
    let reference = reference?;
    let index = index?;
    if index.map_to_resources == Some(true) {
        return Some(game_dir.join("resources"));
    }
    (index.r#virtual == Some(true)).then(|| roots.assets_virtual_dir(&reference.id))
}

#[allow(clippy::too_many_arguments)]
fn lookup_variable(
    key: &str,
    options: &InstanceOptions,
    account: &Account,
    roots: &RootLayout,
    merged: &MergedVersion,
    assets_root: &Path,
    classpath: &str,
    classpath_separator: &str,
    launcher_version: &str,
    version_dir: &str,
) -> Option<String> {
    let value = match key {
        "auth_player_name" | "auth_name" => account.username.clone(),
        "auth_uuid" => account.uuid.clone(),
        "auth_access_token" => account.access_token.clone(),
        // 1.5/1.6 pass `--session` and have no `--uuid`; the UUID only reaches
        // the client through the session string.
        "auth_session" => format!("token:{}:{}", account.access_token, account.uuid),
        "user_type" => account.user_type.clone(),
        "user_properties" => "{}".to_string(),
        // Telemetry-only values. Leaving the placeholders in makes the game
        // parse the literal `${clientid}` text.
        "clientid" | "auth_xuid" => String::new(),
        "version_name" => version_dir.to_string(),
        "version_type" => merged.version_type.clone().unwrap_or_else(|| "release".to_string()),
        "game_directory" => options.game_dir.display().to_string(),
        "assets_root" | "game_assets" => assets_root.display().to_string(),
        "assets_index_name" => merged
            .asset_index
            .as_ref()
            .map(|index| index.id.clone())
            .or_else(|| merged.assets.clone())
            .unwrap_or_else(|| "legacy".to_string()),
        "natives_directory" => options.natives_dir.display().to_string(),
        "launcher_name" => "tntl".to_string(),
        "launcher_version" => launcher_version.to_string(),
        "classpath" => classpath.to_string(),
        "classpath_separator" => classpath_separator.to_string(),
        "library_directory" => roots.libraries_dir().display().to_string(),
        "resolution_width" => options.width.unwrap_or(854).to_string(),
        "resolution_height" => options.height.unwrap_or(480).to_string(),
        _ => return None,
    };
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tntl_model::OsName;

    fn fixture() -> VersionJson {
        serde_json::from_str(include_str!("../tests/fixtures/1.20.1-short.json")).unwrap()
    }

    fn input(os: OsName) -> ResolveInput {
        ResolveInput {
            chain: vec![fixture()],
            version_dir: None,
            roots: RootLayout::new("/nonexistent/root"),
            env: Environment::fixed(os, tntl_model::Arch::X86_64),
            account: Account::offline("Player").unwrap(),
            options: InstanceOptions::new("/game", "/natives"),
            verify: VerifyMode::Exists,
            library_base_url: "https://libraries.minecraft.net/".to_string(),
            launcher_version: "test".to_string(),
        }
    }

    #[test]
    fn resolves_linux_plan() {
        let plan = super::plan(&input(OsName::Linux)).unwrap();
        assert_eq!(plan.main_class, "net.minecraft.client.main.Main");
        assert!(plan.game_args.iter().any(|arg| arg == "--username"));
        assert!(plan.game_args.iter().any(|arg| arg == "Player"));
        assert!(plan.jvm_args.iter().any(|arg| arg == "-Djava.library.path=/natives"));
        assert!(plan.classpath.iter().any(|path| path.ends_with("lwjgl-3.3.1.jar")));
        assert!(
            !plan
                .classpath
                .iter()
                .any(|path| path.ends_with("lwjgl-3.3.1-natives-windows.jar"))
        );
        assert_eq!(plan.natives.len(), 1);
        assert!(plan.missing.iter().any(|m| m.kind == MissingKind::ClientJar));
    }

    #[test]
    fn custom_resolution_is_passed_when_the_feature_is_on() {
        let mut plan_input = input(OsName::Linux);
        plan_input.options.width = Some(1280);
        plan_input.options.height = Some(720);
        plan_input.env = plan_input.env.clone().with_feature("has_custom_resolution", true);
        let plan = super::plan(&plan_input).unwrap();
        assert!(
            plan.game_args.iter().any(|arg| arg == "--width"),
            "{:?}",
            plan.game_args
        );
        assert!(plan.game_args.iter().any(|arg| arg == "1280"));
        assert!(plan.game_args.iter().any(|arg| arg == "720"));

        let plain = super::plan(&input(OsName::Linux)).unwrap();
        assert!(!plain.game_args.iter().any(|arg| arg == "--width"));
    }

    #[test]
    fn a_missing_log_config_does_not_block_the_launch() {
        // HMCL and Prism never fetch assets/log_configs, so their instances
        // used to be refused with "instance ... is incomplete". The game runs
        // on its built-in logging defaults without that file.
        let mut plan = super::plan(&input(OsName::Linux)).unwrap();
        plan.missing.clear();
        plan.missing.push(MissingObject {
            kind: MissingKind::LoggingConfig,
            path: PathBuf::from("assets/log_configs/client-1.12.xml"),
            sha1: None,
            size: None,
            url: None,
        });
        assert!(plan.is_ready());
        assert!(plan.executable());
        assert!(!plan.missing.is_empty());
        // A real file still blocks.
        plan.missing.push(MissingObject {
            kind: MissingKind::Library,
            path: PathBuf::from("libraries/a.jar"),
            sha1: None,
            size: None,
            url: None,
        });
        assert!(!plan.is_ready());
    }

    #[test]
    fn impossible_memory_settings_still_produce_a_usable_jvm_line() {
        // A hand-edited or imported instance can say 0 MB, or a minimum above
        // the maximum; the JVM then refuses to start with a message that
        // reads like a launcher bug.
        let mut plan_input = input(OsName::Linux);
        plan_input.options.max_memory_mb = 0;
        plan_input.options.min_memory_mb = Some(8192);
        let plan = super::plan(&plan_input).unwrap();
        let heap: Vec<&String> = plan
            .jvm_args
            .iter()
            .filter(|arg| arg.starts_with("-Xms") || arg.starts_with("-Xmx"))
            .collect();
        assert_eq!(heap, ["-Xms256M", "-Xmx256M"]);
    }

    #[test]
    fn legacy_sessions_carry_the_uuid() {
        let legacy: VersionJson = serde_json::from_str(
            r#"{"id":"1.6.4","mainClass":"net.minecraft.client.main.Main",
                "minecraftArguments":"--username ${auth_player_name} --session ${auth_session}"}"#,
        )
        .unwrap();
        let mut input = input(OsName::Linux);
        input.chain = vec![legacy];
        let plan = super::plan(&input).unwrap();
        let expected = format!("token:0:{}", input.account.uuid);
        assert!(
            plan.game_args.iter().any(|arg| arg == &expected),
            "{:?}",
            plan.game_args
        );
    }

    #[test]
    fn empty_logging_does_not_hide_the_parent_config() {
        let child: VersionJson =
            serde_json::from_str(r#"{"id":"child","inheritsFrom":"1.20.1","logging":{}}"#).unwrap();
        let mut parent = fixture();
        parent.logging = Some(
            serde_json::from_str(
                r#"{"client":{"argument":"-Dlog4j.configurationFile=${path}","file":{"id":"client-1.12.xml","sha1":"00","size":1,"url":"https://example.invalid/client-1.12.xml"}}}"#,
            )
            .unwrap(),
        );
        let merged = merge(
            &[child, parent],
            &Environment::fixed(OsName::Linux, tntl_model::Arch::X86_64),
        )
        .unwrap();
        assert!(merged.logging.as_ref().is_some_and(|logging| !logging.is_empty()));
    }

    #[test]
    fn windows_uses_semicolon_and_windows_natives() {
        let plan = super::plan(&input(OsName::Windows)).unwrap();
        let classpath = plan
            .jvm_args
            .windows(2)
            .find(|pair| pair[0] == "-cp")
            .map(|pair| pair[1].clone())
            .unwrap();
        assert!(classpath.contains(';'));
        assert!(plan.natives[0].path.to_string_lossy().contains("natives-windows"));
        // The console stream has to be UTF-8 even on a GBK Windows install.
        assert!(plan.jvm_args.iter().any(|arg| arg == "-Dstdout.encoding=UTF-8"));
        assert!(plan.jvm_args.iter().any(|arg| arg == "-Dsun.stdout.encoding=UTF-8"));
    }

    #[test]
    fn renamed_version_directories_resolve_their_own_files() {
        // HMCL keeps the installer's id in the JSON of a renamed directory.
        let mut input = input(OsName::Linux);
        input.version_dir = Some("1.20.1-modpack".to_string());
        let plan = super::plan(&input).unwrap();
        let missing = plan
            .missing
            .iter()
            .find(|missing| missing.kind == MissingKind::ClientJar)
            .unwrap();
        assert!(
            missing.path.to_string_lossy().contains("1.20.1-modpack"),
            "{}",
            missing.path.display()
        );
        assert!(plan.game_args.iter().any(|arg| arg == "1.20.1-modpack"));
        assert!(plan.classpath[0].ends_with("1.20.1-modpack/1.20.1-modpack.jar"));
    }

    #[test]
    fn linux_does_not_force_the_stream_encoding() {
        let plan = super::plan(&input(OsName::Linux)).unwrap();
        assert!(!plan.jvm_args.iter().any(|arg| arg.starts_with("-Dstdout.encoding")));
    }

    #[test]
    fn legacy_asset_indexes_point_the_game_at_a_name_tree() {
        let dir = std::env::temp_dir().join(format!("tntl-virtual-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let roots = RootLayout::new(&dir);
        let reference = AssetRef {
            id: "legacy".to_string(),
            ..AssetRef::default()
        };
        let virtual_index = AssetIndex {
            r#virtual: Some(true),
            ..AssetIndex::default()
        };
        assert_eq!(
            legacy_asset_layout(&roots, Some(&reference), Some(&virtual_index), Path::new("/game")),
            Some(roots.assets_virtual_dir("legacy"))
        );
        // 1.5 and older read the tree from their own game directory.
        let resources = AssetIndex {
            map_to_resources: Some(true),
            ..AssetIndex::default()
        };
        assert_eq!(
            legacy_asset_layout(&roots, Some(&reference), Some(&resources), Path::new("/game")),
            Some(PathBuf::from("/game/resources"))
        );
        assert_eq!(
            legacy_asset_layout(
                &roots,
                Some(&reference),
                Some(&AssetIndex::default()),
                Path::new("/game")
            ),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn demo_mode_reaches_legacy_arguments_once() {
        let legacy: VersionJson = serde_json::from_str(
            r#"{"id":"1.5.2","mainClass":"net.minecraft.client.main.Main",
                "minecraftArguments":"--username ${auth_player_name}"}"#,
        )
        .unwrap();
        let mut plan_input = input(OsName::Linux);
        plan_input.chain = vec![legacy];
        plan_input.options.demo = true;
        let plan = super::plan(&plan_input).unwrap();
        assert_eq!(
            plan.game_args.iter().filter(|arg| *arg == "--demo").count(),
            1,
            "args: {:?}",
            plan.game_args
        );
        let plain = super::plan(&input(OsName::Linux)).unwrap();
        assert!(!plain.game_args.iter().any(|arg| arg == "--demo"));
        // Modern version JSONs gate `--demo` on `is_demo_user`; the plan must
        // not append a second copy.
        let modern: VersionJson = serde_json::from_str(
            r#"{"id":"1.20.1","mainClass":"net.minecraft.client.main.Main",
                "arguments":{"game":["--username","${auth_player_name}",
                  {"rules":[{"action":"allow","features":{"is_demo_user":true}}],"value":"--demo"}]}}"#,
        )
        .unwrap();
        let mut modern_input = input(OsName::Linux);
        modern_input.chain = vec![modern];
        modern_input.options.demo = true;
        modern_input.env = modern_input.env.clone().with_feature("is_demo_user", true);
        let plan = super::plan(&modern_input).unwrap();
        assert_eq!(plan.game_args.iter().filter(|arg| *arg == "--demo").count(), 1);
    }

    #[test]
    fn legacy_forge_ignores_the_jar_certificate() {
        let dir = std::env::temp_dir().join(format!("tntl-legacy-forge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let roots = RootLayout::new(&dir);
        std::fs::create_dir_all(roots.assets_dir().join("indexes")).unwrap();
        std::fs::write(roots.asset_index("legacy"), r#"{"virtual":true,"objects":{}}"#).unwrap();
        let forge: VersionJson = serde_json::from_str(
            r#"{"id":"1.6.4-Forge9.11.1.1345","inheritsFrom":"1.6.4",
                "mainClass":"net.minecraft.launchwrapper.Launch",
                "minecraftArguments":"--username ${auth_player_name} --tweakClass cpw.mods.fml.common.launcher.FMLTweaker",
                "assetIndex":{"id":"legacy"}}"#,
        )
        .unwrap();
        let parent: VersionJson = serde_json::from_str(
            r#"{"id":"1.6.4","mainClass":"net.minecraft.client.main.Main","assetIndex":{"id":"legacy"}}"#,
        )
        .unwrap();
        let mut input = input(OsName::Linux);
        input.chain = vec![forge, parent];
        input.roots = roots;
        let plan = super::plan(&input).unwrap();
        assert!(
            plan.jvm_args
                .iter()
                .any(|arg| arg == "-Dfml.ignoreInvalidMinecraftCertificates=true")
        );
        assert_eq!(plan.asset_dir, input.roots.assets_virtual_dir("legacy"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn offline_uuid_is_stable() {
        assert_eq!(offline_uuid("Player"), offline_uuid("Player"));
        assert_eq!(offline_uuid("Player").len(), 32);
        assert_ne!(offline_uuid("Player"), offline_uuid("Other"));
    }

    #[test]
    fn merges_parent_libraries_once() {
        let child: VersionJson = serde_json::from_str(
            r#"{"id":"child","inheritsFrom":"1.20.1","mainClass":"child.Main",
                "libraries":[{"name":"com.example:child:1.0","downloads":{"artifact":{"path":"com/example/child/1.0/child-1.0.jar","url":"https://libraries.minecraft.net/com/example/child/1.0/child-1.0.jar","sha1":"00"}}}]}"#,
        )
        .unwrap();
        let merged = merge(
            &[child.clone(), fixture()],
            &Environment::fixed(OsName::Linux, tntl_model::Arch::X86_64),
        )
        .unwrap();
        assert_eq!(merged.main_class.as_deref(), Some("child.Main"));
        assert_eq!(
            merged
                .libraries
                .iter()
                .filter(|library| library.name == "org.lwjgl:lwjgl:3.3.1")
                .count(),
            1
        );
        assert!(merged.libraries[0].name.starts_with("com.example:child"));
    }

    #[test]
    fn redacted_plan_masks_session_credentials() {
        let plan = LaunchPlan {
            version_id: "1.20.1".to_string(),
            java: PathBuf::from("/usr/bin/java"),
            jvm_args: vec!["-Dfoo=bar".to_string()],
            main_class: "net.minecraft.client.main.Main".to_string(),
            classpath: Vec::new(),
            natives_dir: PathBuf::from("/tmp/natives"),
            natives: Vec::new(),
            game_args: vec![
                "--username".to_string(),
                "Steve".to_string(),
                "--accessToken".to_string(),
                "secret-token".to_string(),
                "--session".to_string(),
                "token:secret-token:uuid".to_string(),
            ],
            working_dir: PathBuf::from("/tmp"),
            env: BTreeMap::from([("LAUNCHER_TOKEN".to_string(), "secret-token".to_string())]),
            asset_index: None,
            asset_dir: PathBuf::from("/tmp/assets"),
            legacy_assets: None,
            warnings: Vec::new(),
            missing: Vec::new(),
        };
        let redacted = plan.redacted();
        let joined = redacted.game_args.join(" ");
        assert!(!joined.contains("secret-token"));
        assert!(joined.contains("--username Steve"));
        assert!(redacted.env["LAUNCHER_TOKEN"] == "<redacted>");
        // The original plan keeps the real values; only the copy is masked.
        assert!(plan.game_args.join(" ").contains("secret-token"));
    }
}
