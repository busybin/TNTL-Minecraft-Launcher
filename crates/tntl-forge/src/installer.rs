//! Installer pipeline: profile parsing, library downloads and processors.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;
use tntl_model::{Library, MavenName, VersionJson};
use tntl_source::{DownloadRequest, Http, InstallEvent, SourceError};
use tntl_store::{RootLayout, atomic_write, sha1_bytes, sha1_file};

use crate::{ForgeError, ForgeInstaller, ForgeKind, NEOFORGE_MAVEN, installer_url, installer_versions};

/// Result of a successful installer run.
pub struct ForgeInstall {
    /// Version id written to `versions/`.
    pub version_id: String,
    pub version_json: PathBuf,
    /// Client jar the game launches with: the patched one for modern builds,
    /// the vanilla one for legacy builds, which never touch it.
    pub client_jar: PathBuf,
    pub processors: usize,
    pub libraries: usize,
}

#[derive(Debug, Deserialize)]
struct InstallProfile {
    #[serde(default)]
    json: String,
    #[serde(default)]
    data: BTreeMap<String, SidedValue>,
    #[serde(default)]
    processors: Vec<Processor>,
    #[serde(default)]
    libraries: Vec<Library>,
    /// Present in installers that predate the 1.13 patching scheme.
    #[serde(default)]
    install: Option<LegacyInstall>,
    #[serde(default, rename = "versionInfo")]
    version_info: Option<VersionJson>,
}

/// Legacy installer section: the loader jar is a file inside the zip.
#[derive(Debug, Deserialize)]
struct LegacyInstall {
    #[serde(default, rename = "filePath")]
    file_path: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    minecraft: String,
}

#[derive(Debug, Deserialize)]
struct SidedValue {
    #[serde(default)]
    client: String,
}

#[derive(Debug, Deserialize)]
struct Processor {
    #[serde(default)]
    sides: Option<Vec<String>>,
    #[serde(default)]
    jar: String,
    #[serde(default)]
    classpath: Vec<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    outputs: Option<BTreeMap<String, String>>,
}

impl Processor {
    fn runs_on_client(&self) -> bool {
        self.sides
            .as_ref()
            .is_none_or(|sides| sides.iter().any(|side| side == "client"))
    }
}

pub(super) fn run(
    installer: &ForgeInstaller<'_>,
    kind: ForgeKind,
    game: &str,
    version: &str,
    progress: &(dyn Fn(InstallEvent) + Send + Sync),
) -> Result<ForgeInstall, ForgeError> {
    let roots = &installer.roots;
    let version = kind.maven_version(game, version);
    let client_jar = roots.client_jar(game);
    if !client_jar.is_file() {
        return Err(ForgeError::Inconsistent(format!(
            "{game} is not installed yet; the installer patches its client jar"
        )));
    }

    let installer_path = download_installer(installer, kind, game, &version, progress)?;
    let mut archive = open_archive(&installer_path)?;

    let profile_bytes = read_entry(&mut archive, "install_profile.json")?
        .ok_or_else(|| ForgeError::Zip("install_profile.json is missing".to_string()))?;
    let profile: InstallProfile = serde_json::from_slice(&profile_bytes).map_err(|error| ForgeError::Profile {
        version: version.to_string(),
        message: format!("install_profile.json: {error}"),
    })?;
    let mut json = profile_version(&mut archive, &profile, &version)?;
    // Forge 1.6 and older profiles are self-contained and name no parent.
    if json.inherits_from.is_none() {
        let parent = profile
            .install
            .as_ref()
            .map(|install| install.minecraft.trim().to_string())
            .filter(|minecraft| !minecraft.is_empty())
            .unwrap_or_else(|| game.to_string());
        json.inherits_from = Some(parent);
    }
    validate_profile(&json, game, &version)?;

    let version_dir = roots.version_dir(&json.id);
    fs::create_dir_all(&version_dir).map_err(|source| io(&version_dir, source))?;
    let target_json = roots.version_json(&json.id);
    let version_bytes = serde_json::to_vec_pretty(&json).map_err(|error| ForgeError::Profile {
        version: version.to_string(),
        message: format!("version json: {error}"),
    })?;

    // Profiles without processors predate the 1.13 patching scheme: the loader
    // jar ships inside the installer, and the vanilla client jar is used as-is.
    // Processors do not replace those jars, so extract installer-provided
    // artifacts in both branches.
    let extracted = install_bundled_jars(&profile, &json, &mut archive, roots)?;
    if profile.processors.is_empty() {
        drop(archive);
        installer_download_libraries(installer, &profile, progress)?;
        atomic_write(&target_json, &version_bytes)?;
        return Ok(ForgeInstall {
            version_id: json.id,
            version_json: target_json,
            client_jar,
            processors: 0,
            libraries: extracted,
        });
    }

    // Processors read `/data/*` entries (binary patches) from the installer.
    let work_dir = roots.root.join("cache").join("forge").join(&json.id);
    let _ = fs::remove_dir_all(&work_dir);
    fs::create_dir_all(&work_dir).map_err(|source| io(&work_dir, source))?;
    for value in profile.data.values() {
        if let Some(entry) = installer_entry(&value.client) {
            extract_entry(&mut archive, entry, &work_dir)?;
        }
    }
    drop(archive);

    installer_download_libraries(installer, &profile, progress)?;

    let mut pipeline = Pipeline {
        roots,
        game,
        work_dir,
        installer_jar: installer_path.clone(),
        profile,
        variables: BTreeMap::new(),
        outputs: BTreeMap::new(),
    };
    pipeline.collect_variables()?;
    let processors = pipeline.run_processors(installer.java(), progress)?;

    let patched = pipeline
        .variables
        .get("PATCHED")
        .map(PathBuf::from)
        .ok_or_else(|| ForgeError::Inconsistent("profile has no PATCHED variable".to_string()))?;
    if !patched.is_file() {
        return Err(ForgeError::Inconsistent(
            "the processor chain did not produce the patched client jar".to_string(),
        ));
    }

    let target_jar = roots.client_jar(&json.id);
    fs::copy(&patched, &target_jar).map_err(|source| io(&target_jar, source))?;
    atomic_write(&target_json, &version_bytes)?;
    // Version JSONs of patched installs rarely hash the client jar, so record
    // it next to whatever the processors reported to make repair possible.
    pipeline.outputs.insert(
        target_jar.clone(),
        sha1_file(&target_jar).map_err(|source| io(&target_jar, source))?,
    );
    write_outputs(roots, &json.id, &pipeline.outputs)?;

    Ok(ForgeInstall {
        version_id: json.id,
        version_json: target_json,
        client_jar: target_jar,
        processors,
        libraries: pipeline.profile.libraries.len(),
    })
}

/// Persist the processor output record next to the installed version.
fn write_outputs(roots: &RootLayout, version_id: &str, outputs: &BTreeMap<PathBuf, String>) -> Result<(), ForgeError> {
    let mut record = crate::ProcessorOutputs::default();
    for (path, sha1) in outputs {
        let Ok(relative) = path.strip_prefix(&roots.root) else {
            continue;
        };
        record
            .files
            .insert(relative.to_string_lossy().replace('\\', "/"), sha1.clone());
    }
    let bytes = serde_json::to_vec_pretty(&record).map_err(|error| ForgeError::Profile {
        version: version_id.to_string(),
        message: format!("outputs: {error}"),
    })?;
    atomic_write(&roots.version_dir(version_id).join(crate::OUTPUTS_FILE), &bytes)?;
    Ok(())
}

/// Version JSON the profile describes: an installer entry, or the inline
/// `versionInfo` object older installers carry instead.
fn profile_version(
    archive: &mut zip::ZipArchive<File>,
    profile: &InstallProfile,
    version: &str,
) -> Result<VersionJson, ForgeError> {
    let entry = if profile.json.is_empty() {
        "version.json"
    } else {
        profile.json.trim_start_matches('/')
    };
    let Some(bytes) = read_entry(archive, entry)? else {
        return match &profile.version_info {
            Some(info) => Ok(info.clone()),
            None => Err(ForgeError::Zip(format!("{entry} is missing"))),
        };
    };
    serde_json::from_slice(&bytes).map_err(|error| ForgeError::Profile {
        version: version.to_string(),
        message: format!("{entry}: {error}"),
    })
}

/// A loader jar that travels inside the installer instead of a maven host.
struct BundledJar {
    entry: String,
    relative: String,
    sha1: Option<String>,
}

fn bundled_jars(profile: &InstallProfile, json: &VersionJson) -> Vec<BundledJar> {
    let mut jars = Vec::new();
    if let Some(install) = &profile.install
        && !install.file_path.is_empty()
    {
        let relative = MavenName::parse(&install.path)
            .map(|name| name.path())
            .unwrap_or_else(|| format!("{}/{}", install.path.trim_end_matches('/'), install.file_path));
        jars.push(BundledJar {
            entry: install.file_path.clone(),
            relative,
            sha1: None,
        });
    }
    // Both lists can name an artifact with an empty URL, which means the
    // installer ships it under `maven/` instead of a repository. 1.13-1.16.5
    // profiles do this for the launch jar, which is declared in `version.json`.
    for library in profile.libraries.iter().chain(json.libraries.iter()) {
        let Some(artifact) = library
            .downloads
            .as_ref()
            .and_then(|downloads| downloads.artifact.as_ref())
        else {
            continue;
        };
        let Some(path) = artifact.path.as_ref().filter(|path| !path.is_empty()) else {
            continue;
        };
        if !artifact.url.trim().is_empty() {
            continue;
        }
        if jars.iter().any(|jar| &jar.relative == path) {
            continue;
        }
        jars.push(BundledJar {
            entry: format!("maven/{path}"),
            relative: path.clone(),
            sha1: artifact.sha1.clone(),
        });
    }
    jars
}

fn install_bundled_jars(
    profile: &InstallProfile,
    json: &VersionJson,
    archive: &mut zip::ZipArchive<File>,
    roots: &RootLayout,
) -> Result<usize, ForgeError> {
    let jars = bundled_jars(profile, json);
    for jar in &jars {
        let bytes = read_entry(archive, &jar.entry)?
            .ok_or_else(|| ForgeError::Zip(format!("installer entry {} is missing", jar.entry)))?;
        let dest = roots.library(&jar.relative);
        if let Some(expected) = &jar.sha1 {
            let actual = sha1_bytes(&bytes);
            if !actual.eq_ignore_ascii_case(expected) {
                return Err(ForgeError::Hash {
                    path: dest,
                    expected: expected.clone(),
                    actual,
                });
            }
        }
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|source| io(parent, source))?;
        }
        atomic_write(&dest, &bytes)?;
    }
    Ok(jars.len())
}

/// Libraries whose jar the installer provides; downloading them is pointless.
fn installer_provides(profile: &InstallProfile, library: &Library) -> bool {
    if let Some(install) = &profile.install
        && !install.path.trim().is_empty()
        && library.name.trim() == install.path.trim()
    {
        return true;
    }
    library
        .downloads
        .as_ref()
        .and_then(|downloads| downloads.artifact.as_ref())
        .is_some_and(|artifact| {
            artifact.url.trim().is_empty() && artifact.path.as_ref().is_some_and(|path| !path.is_empty())
        })
}

fn validate_profile(json: &VersionJson, game: &str, version: &str) -> Result<(), ForgeError> {
    let invalid = |message: String| ForgeError::Profile {
        version: version.to_string(),
        message,
    };
    if json.id.is_empty() || json.id.contains(['/', '\\']) {
        return Err(invalid(format!("unusable version id {:?}", json.id)));
    }
    if json.inherits_from.as_deref() != Some(game) {
        return Err(invalid(format!(
            "version {} inherits from {:?}, expected {game}",
            json.id, json.inherits_from
        )));
    }
    Ok(())
}

fn download_installer(
    installer: &ForgeInstaller<'_>,
    kind: ForgeKind,
    game: &str,
    version: &str,
    progress: &(dyn Fn(InstallEvent) + Send + Sync),
) -> Result<PathBuf, ForgeError> {
    let dir = installer.roots.root.join("cache").join("installers");
    fs::create_dir_all(&dir).map_err(|source| io(&dir, source))?;

    progress(event("installer", 0, 1, 0));
    let mut missing = None;
    for candidate in installer_versions(game, version) {
        let url = installer_url(kind, game, &candidate);
        let name = url.rsplit('/').next().unwrap_or("installer.jar");
        let path = dir.join(name);
        // A request without a hash accepts any file that is already there, so
        // a truncated copy from an interrupted run would fail the sidecar
        // check on every later attempt. Drop it and fetch once more.
        for attempt in 0..2 {
            match installer.http.download(&DownloadRequest::new(&url, &path), &mut |_| {}) {
                Ok(_) => match verify_sidecar(installer.http, &url, &path) {
                    Ok(()) => {
                        progress(event(
                            "installer",
                            1,
                            1,
                            fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0),
                        ));
                        return Ok(path);
                    }
                    Err(ForgeError::Hash { .. }) if attempt == 0 => {
                        let _ = fs::remove_file(&path);
                    }
                    Err(error) => return Err(error),
                },
                Err(SourceError::Status { status: 404, .. }) => {
                    missing = Some(url);
                    break;
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
    let url = missing.unwrap_or_else(|| installer_url(kind, game, version));
    Err(SourceError::Status { url, status: 404 }.into())
}

/// Maven publishes `<file>.sha1` next to every artifact; check it when present.
fn verify_sidecar(http: &Http, url: &str, path: &Path) -> Result<(), ForgeError> {
    let sidecar = format!("{url}.sha1");
    let body = match http.get_bytes(&sidecar, 4096) {
        Ok(body) => body,
        Err(SourceError::Status { status: 404, .. }) => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let expected = String::from_utf8_lossy(&body).trim().to_string();
    if expected.is_empty() {
        return Ok(());
    }
    let actual = sha1_file(path).map_err(|source| io(path, source))?;
    if !actual.eq_ignore_ascii_case(&expected) {
        return Err(ForgeError::Hash {
            path: path.to_path_buf(),
            expected,
            actual,
        });
    }
    Ok(())
}

fn installer_download_libraries(
    installer: &ForgeInstaller<'_>,
    profile: &InstallProfile,
    progress: &(dyn Fn(InstallEvent) + Send + Sync),
) -> Result<(), ForgeError> {
    let total = profile.libraries.len();
    let mut bytes = 0u64;
    for (index, library) in profile.libraries.iter().enumerate() {
        if installer_provides(profile, library) {
            continue;
        }
        let Some(artifact) = library_artifact(library, &installer.roots) else {
            continue;
        };
        let mut request = DownloadRequest::new(artifact.url, artifact.dest);
        if let Some(sha1) = artifact.sha1 {
            request = request.with_sha1(sha1);
        }
        if let Some(size) = artifact.size {
            request = request.with_size(size);
            bytes += size;
        }
        installer.http.download(&request, &mut |_| {})?;
        progress(event("libraries", index + 1, total, bytes));
    }
    Ok(())
}

struct Artifact {
    url: String,
    dest: PathBuf,
    sha1: Option<String>,
    size: Option<u64>,
}

fn library_artifact(library: &Library, roots: &RootLayout) -> Option<Artifact> {
    let name = MavenName::parse(&library.name)?;
    let artifact = library
        .downloads
        .as_ref()
        .and_then(|downloads| downloads.artifact.as_ref());
    let relative = artifact
        .and_then(|artifact| artifact.path.clone())
        .unwrap_or_else(|| name.path());
    let url = artifact
        .map(|artifact| artifact.url.clone())
        .filter(|url| !url.is_empty())
        .or_else(|| library.url.as_ref().map(|base| join_maven(base, &relative)))
        .unwrap_or_else(|| coordinates_url(&library.name));
    Some(Artifact {
        url,
        dest: roots.library(&relative),
        sha1: artifact
            .and_then(|artifact| artifact.sha1.clone())
            .or_else(|| library.sha1.clone()),
        size: artifact.and_then(|artifact| artifact.size).or(library.size),
    })
}

struct Pipeline<'i> {
    roots: &'i RootLayout,
    game: &'i str,
    work_dir: PathBuf,
    installer_jar: PathBuf,
    profile: InstallProfile,
    variables: BTreeMap<String, String>,
    /// Files the processor chain produced, keyed by absolute path with the
    /// SHA-1 they had right after the processors finished.
    outputs: BTreeMap<PathBuf, String>,
}

impl Pipeline<'_> {
    fn collect_variables(&mut self) -> Result<(), ForgeError> {
        for (name, value) in &self.profile.data {
            let resolved = self.resolve_value(&value.client)?;
            self.variables.insert(name.clone(), resolved);
        }
        let synthetic = [
            ("SIDE", "client".to_string()),
            ("MINECRAFT_JAR", self.roots.client_jar(self.game).display().to_string()),
            ("MINECRAFT_VERSION", self.game.to_string()),
            ("ROOT", self.roots.root.display().to_string()),
            ("INSTALLER", self.installer_jar.display().to_string()),
            ("LIBRARY_DIR", self.roots.libraries_dir().display().to_string()),
        ];
        for (name, value) in synthetic {
            self.variables.entry(name.to_string()).or_insert(value);
        }
        // Outputs such as the patched jar land in the library directory; the
        // processors write files, they do not create directories.
        let dirs: Vec<PathBuf> = self
            .variables
            .values()
            .filter(|value| Path::new(value).is_absolute())
            .filter_map(|value| Path::new(value).parent().map(Path::to_path_buf))
            .collect();
        for dir in dirs {
            let _ = fs::create_dir_all(dir);
        }
        Ok(())
    }

    /// Profile data values are either quoted literals, `/data/*` entries of the
    /// installer, or maven coordinates. Outputs are coordinates too: the
    /// processors write them into the library directory.
    fn resolve_value(&self, raw: &str) -> Result<String, ForgeError> {
        let trimmed = raw.trim();
        if let Some(inner) = trimmed.strip_prefix('\'').and_then(|rest| rest.strip_suffix('\'')) {
            return Ok(inner.to_string());
        }
        if let Some(inner) = trimmed.strip_prefix('[').and_then(|rest| rest.strip_suffix(']')) {
            return Ok(self.coordinate(inner)?.display().to_string());
        }
        if trimmed.starts_with('/') {
            let path = self.work_dir.join(trimmed.trim_start_matches('/'));
            if !path.is_file() {
                return Err(ForgeError::Inconsistent(format!(
                    "installer data file {} was not extracted",
                    trimmed
                )));
            }
            return Ok(path.display().to_string());
        }
        Ok(trimmed.to_string())
    }

    /// Local path of a maven coordinate. Everything the processors read was
    /// downloaded with the profile libraries; the rest are their outputs, which
    /// are written to the same directory.
    fn coordinate(&self, raw: &str) -> Result<PathBuf, ForgeError> {
        let name = MavenName::parse(raw)
            .ok_or_else(|| ForgeError::Inconsistent(format!("unusable maven coordinate {raw}")))?;
        let relative = self
            .profile
            .libraries
            .iter()
            .find(|library| library.name == raw.trim())
            .and_then(|library| {
                library
                    .downloads
                    .as_ref()
                    .and_then(|downloads| downloads.artifact.as_ref())
                    .and_then(|artifact| artifact.path.clone())
            })
            .unwrap_or_else(|| name.path());
        Ok(self.roots.library(&relative))
    }

    fn run_processors(
        &mut self,
        java: &Path,
        progress: &(dyn Fn(InstallEvent) + Send + Sync),
    ) -> Result<usize, ForgeError> {
        let indexes: Vec<usize> = self
            .profile
            .processors
            .iter()
            .enumerate()
            .filter(|(_, processor)| processor.runs_on_client())
            .map(|(index, _)| index)
            .collect();
        for (step, &index) in indexes.iter().enumerate() {
            progress(event("processors", step, indexes.len(), 0));
            let (classpath, main, args, outputs) = {
                let processor = &self.profile.processors[index];
                let jar = self.coordinate(&processor.jar)?;
                // The processor jar is not always listed in its own classpath.
                let mut classpath = vec![jar.clone()];
                for entry in &processor.classpath {
                    classpath.push(self.coordinate(entry)?);
                }
                let main = manifest_main_class(&jar)?;
                let args = processor
                    .args
                    .iter()
                    .map(|arg| expand(arg, &self.variables, &mut |raw| self.coordinate(raw)))
                    .collect::<Result<Vec<_>, _>>()?;
                (classpath, main, args, processor.outputs.clone())
            };
            run_java(java, &classpath, &main, &args, &self.work_dir)?;
            self.check_outputs(outputs.as_ref())?;
        }
        progress(event("processors", indexes.len(), indexes.len(), 0));
        Ok(indexes.len())
    }

    fn check_outputs(&mut self, outputs: Option<&BTreeMap<String, String>>) -> Result<(), ForgeError> {
        let Some(outputs) = outputs else {
            return Ok(());
        };
        for (target, hash) in outputs {
            let target = expand(target, &self.variables, &mut |raw| self.coordinate(raw))?;
            let expected = expand(hash, &self.variables, &mut |raw| self.coordinate(raw))?;
            let expected = expected.trim();
            if expected.is_empty() {
                continue;
            }
            let path = PathBuf::from(&target);
            let actual = sha1_file(&path).map_err(|source| io(&path, source))?;
            if !actual.eq_ignore_ascii_case(expected) {
                return Err(ForgeError::Hash {
                    path,
                    expected: expected.to_string(),
                    actual,
                });
            }
            self.outputs.insert(path, actual);
        }
        Ok(())
    }
}

/// Maven URL for a coordinate the installer profile does not describe.
fn coordinates_url(raw: &str) -> String {
    let relative = MavenName::parse(raw)
        .map(|name| name.path())
        .unwrap_or_else(|| raw.to_string());
    let group = raw.split(':').next().unwrap_or_default();
    let root = if group.starts_with("net.neoforged") {
        NEOFORGE_MAVEN
    } else if group.starts_with("net.minecraftforge") || group.starts_with("de.oceanlabs.mcp") {
        crate::FORGE_MAVEN
    } else if group.starts_with("net.minecraft") {
        "https://libraries.minecraft.net/"
    } else {
        "https://repo1.maven.org/maven2/"
    };
    join_maven(root, &relative)
}

fn join_maven(base: &str, relative: &str) -> String {
    format!("{}/{relative}", base.trim_end_matches('/'))
}

fn manifest_main_class(jar: &Path) -> Result<String, ForgeError> {
    let mut archive = open_archive(jar)?;
    let mut entry = archive
        .by_name("META-INF/MANIFEST.MF")
        .map_err(|error| ForgeError::Zip(format!("{}: {error}", jar.display())))?;
    let mut text = String::new();
    entry
        .by_ref()
        .take(64 * 1024)
        .read_to_string(&mut text)
        .map_err(|error| ForgeError::Zip(format!("{}: {error}", jar.display())))?;
    let mut current = String::new();
    for line in text.lines() {
        if let Some(continuation) = line.strip_prefix(' ') {
            current.push_str(continuation);
            continue;
        }
        if let Some(main) = current.strip_prefix("Main-Class:") {
            return Ok(main.trim().to_string());
        }
        current = line.to_string();
    }
    if let Some(main) = current.strip_prefix("Main-Class:") {
        return Ok(main.trim().to_string());
    }
    Err(ForgeError::Zip(format!(
        "{} has no Main-Class in its manifest",
        jar.display()
    )))
}

fn run_java(java: &Path, classpath: &[PathBuf], main: &str, args: &[String], cwd: &Path) -> Result<(), ForgeError> {
    /// A processor that never exits (broken patch, deadlock in the patcher)
    /// used to hang the install forever with no progress and no way to cancel.
    const TIMEOUT: Duration = Duration::from_secs(30 * 60);
    let joined = std::env::join_paths(classpath)
        .map_err(|error| ForgeError::Inconsistent(format!("classpath is not usable: {error}")))?;
    // Output goes to a file: the tail is still readable after the process is
    // killed for a timeout or a cancel, and a chatty processor cannot fill a
    // pipe nobody drains.
    let log_path = cwd.join("processor.log");
    let log = File::create(&log_path).map_err(|source| io(&log_path, source))?;
    let errors = log.try_clone().map_err(|source| io(&log_path, source))?;
    let mut command = Command::new(java);
    command
        .arg("-cp")
        .arg(joined)
        .arg(main)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(errors));
    // A GUI launcher must not open a console window for each of the six to ten
    // processors a Forge installer runs.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn().map_err(|source| io(java, source))?;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|source| io(java, source))? {
            break status;
        }
        let reason = if tntl_source::cancel::cancelled() {
            Some("cancelled".to_string())
        } else if started.elapsed() > TIMEOUT {
            Some(format!("timed out after {} minutes", TIMEOUT.as_secs() / 60))
        } else {
            None
        };
        if let Some(reason) = reason {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ForgeError::Processor {
                processor: main.to_string(),
                status: reason,
                output: tail_of(&log_path),
            });
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    if status.success() {
        return Ok(());
    }
    Err(ForgeError::Processor {
        processor: main.to_string(),
        status: status.to_string(),
        output: tail_of(&log_path),
    })
}

/// Last lines a processor wrote, for the error message.
fn tail_of(path: &Path) -> String {
    let text = std::fs::read(path)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default();
    let tail: Vec<&str> = text.lines().rev().take(20).collect();
    tail.into_iter().rev().collect::<Vec<_>>().join("\n")
}

/// Replace `{VARIABLE}` and `[maven:coordinate]` placeholders in one argument.
fn expand<F>(text: &str, variables: &BTreeMap<String, String>, resolve: &mut F) -> Result<String, ForgeError>
where
    F: FnMut(&str) -> Result<PathBuf, ForgeError>,
{
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let brace = rest.find('{');
        let bracket = rest.find('[');
        let (open, close) = match (brace, bracket) {
            (None, None) => {
                out.push_str(rest);
                return Ok(out);
            }
            (Some(brace), None) => (brace, '}'),
            (None, Some(bracket)) => (bracket, ']'),
            (Some(brace), Some(bracket)) if brace < bracket => (brace, '}'),
            (Some(_), Some(bracket)) => (bracket, ']'),
        };
        out.push_str(&rest[..open]);
        let Some(length) = rest[open + 1..].find(close) else {
            return Err(ForgeError::Inconsistent(format!(
                "unterminated placeholder in installer argument {text}"
            )));
        };
        let token = &rest[open + 1..open + 1 + length];
        if close == '}' {
            let value = variables
                .get(token)
                .ok_or_else(|| ForgeError::Inconsistent(format!("installer profile uses unknown variable {token}")))?;
            out.push_str(value);
        } else {
            out.push_str(&resolve(token)?.display().to_string());
        }
        rest = &rest[open + 1 + length + 1..];
    }
}

fn open_archive(path: &Path) -> Result<zip::ZipArchive<File>, ForgeError> {
    let file = File::open(path).map_err(|source| io(path, source))?;
    zip::ZipArchive::new(file).map_err(|error| ForgeError::Zip(format!("{}: {error}", path.display())))
}

fn read_entry(archive: &mut zip::ZipArchive<File>, name: &str) -> Result<Option<Vec<u8>>, ForgeError> {
    let mut entry = match archive.by_name(name) {
        Ok(entry) => entry,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(error) => return Err(ForgeError::Zip(format!("{name}: {error}"))),
    };
    let mut bytes = Vec::new();
    entry
        .by_ref()
        .take(16 * 1024 * 1024)
        .read_to_end(&mut bytes)
        .map_err(|error| ForgeError::Zip(format!("{name}: {error}")))?;
    Ok(Some(bytes))
}

fn extract_entry(archive: &mut zip::ZipArchive<File>, name: &str, root: &Path) -> Result<(), ForgeError> {
    let relative = Path::new(name);
    if relative.is_absolute() || relative.components().any(|part| matches!(part, Component::ParentDir)) {
        return Err(ForgeError::Zip(format!("unsafe installer entry {name}")));
    }
    let Some(bytes) = read_entry(archive, name)? else {
        return Err(ForgeError::Zip(format!("installer entry {name} is missing")));
    };
    let dest = root.join(relative);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|source| io(parent, source))?;
    }
    fs::write(&dest, bytes).map_err(|source| io(&dest, source))?;
    Ok(())
}

fn installer_entry(value: &str) -> Option<&str> {
    value.trim().strip_prefix('/')
}

fn event(stage: &'static str, done: usize, total: usize, bytes: u64) -> InstallEvent {
    InstallEvent {
        stage,
        done,
        total,
        bytes,
        bytes_total: None,
    }
}

fn io(path: &Path, source: std::io::Error) -> ForgeError {
    ForgeError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolver(raw: &str) -> Result<PathBuf, ForgeError> {
        Ok(PathBuf::from(format!("/libs/{raw}")))
    }

    #[test]
    fn expands_variables_and_coordinates() {
        let mut variables = BTreeMap::new();
        variables.insert("MINECRAFT_JAR".to_string(), "/game/client.jar".to_string());
        let mut resolve = resolver;
        let expanded = expand(
            "--input {MINECRAFT_JAR} --apply [net.example:patch:1.0@lzma] --clean",
            &variables,
            &mut resolve,
        )
        .unwrap();
        assert_eq!(
            expanded,
            "--input /game/client.jar --apply /libs/net.example:patch:1.0@lzma --clean"
        );
    }

    #[test]
    fn rejects_unknown_variables() {
        let variables = BTreeMap::new();
        let mut resolve = resolver;
        let error = expand("{NOPE}", &variables, &mut resolve).unwrap_err();
        assert!(error.to_string().contains("unknown variable NOPE"));
    }

    #[test]
    fn rejects_unterminated_placeholders() {
        let mut variables = BTreeMap::new();
        variables.insert("A".to_string(), "b".to_string());
        let mut resolve = resolver;
        assert!(expand("{A", &variables, &mut resolve).is_err());
        assert!(expand("[a:b:1", &variables, &mut resolve).is_err());
    }

    #[test]
    fn processor_sides_default_to_every_side() {
        let processor: Processor = serde_json::from_str(r#"{"jar":"a:b:1","args":[]}"#).unwrap();
        assert!(processor.runs_on_client());
        let processor: Processor = serde_json::from_str(r#"{"jar":"a:b:1","sides":["server"],"args":[]}"#).unwrap();
        assert!(!processor.runs_on_client());
        let processor: Processor = serde_json::from_str(r#"{"jar":"a:b:1","sides":["client"],"args":[]}"#).unwrap();
        assert!(processor.runs_on_client());
    }

    #[test]
    fn parses_installer_entries() {
        assert_eq!(installer_entry("/data/client.lzma"), Some("data/client.lzma"));
        assert_eq!(installer_entry("'literal'"), None);
    }

    #[test]
    fn reads_legacy_profiles() {
        let profile: InstallProfile = serde_json::from_str(
            r#"{
                "install": {
                    "filePath": "forge-1.7.10-10.13.4.1614-1.7.10-universal.jar",
                    "path": "net.minecraftforge:forge:1.7.10-10.13.4.1614-1.7.10",
                    "minecraft": "1.7.10"
                },
                "versionInfo": {
                    "id": "1.7.10-Forge10.13.4.1614-1.7.10",
                    "inheritsFrom": "1.7.10",
                    "libraries": [{
                        "name": "net.minecraftforge:forge:1.7.10-10.13.4.1614-1.7.10",
                        "url": "https://maven.minecraftforge.net/"
                    }]
                }
            }"#,
        )
        .unwrap();
        assert!(profile.processors.is_empty());
        let infos = bundled_jars(&profile, &VersionJson::default());
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].entry, "forge-1.7.10-10.13.4.1614-1.7.10-universal.jar");
        assert_eq!(
            infos[0].relative,
            "net/minecraftforge/forge/1.7.10-10.13.4.1614-1.7.10/forge-1.7.10-10.13.4.1614-1.7.10.jar"
        );
        let info = profile.version_info.unwrap();
        assert_eq!(info.inherits_from.as_deref(), Some("1.7.10"));
    }

    #[test]
    fn reads_bundled_artifacts_without_a_url() {
        let profile: InstallProfile = serde_json::from_str(
            r#"{
                "json": "/version.json",
                "processors": [],
                "libraries": [{
                    "name": "net.minecraftforge:forge:1.12.2-14.23.5.2860",
                    "downloads": {"artifact": {
                        "path": "net/minecraftforge/forge/1.12.2-14.23.5.2860/forge-1.12.2-14.23.5.2860.jar",
                        "url": "",
                        "sha1": "029250575d3aa2cf80b56dffb66238a1eeaea2ac",
                        "size": 4466148
                    }}
                }]
            }"#,
        )
        .unwrap();
        let infos = bundled_jars(&profile, &VersionJson::default());
        assert_eq!(infos.len(), 1);
        assert_eq!(
            infos[0].entry,
            "maven/net/minecraftforge/forge/1.12.2-14.23.5.2860/forge-1.12.2-14.23.5.2860.jar"
        );
        assert_eq!(
            infos[0].sha1.as_deref(),
            Some("029250575d3aa2cf80b56dffb66238a1eeaea2ac")
        );
        assert!(installer_provides(&profile, &profile.libraries[0]));
    }

    #[test]
    fn extracts_processor_profiles_bundled_jars() {
        // 1.13-1.16.5 profiles have processors *and* ship the launch jar in
        // `maven/`, declared with an empty URL in version.json.
        let profile: InstallProfile = serde_json::from_str(
            r#"{
                "json": "/version.json",
                "processors": [{"jar": "net.minecraftforge:installer:1"}],
                "libraries": [{
                    "name": "net.minecraftforge:forge:1.16.5-36.2.34:universal",
                    "downloads": {"artifact": {
                        "path": "net/minecraftforge/forge/1.16.5-36.2.34/forge-1.16.5-36.2.34-universal.jar",
                        "url": ""
                    }}
                }]
            }"#,
        )
        .unwrap();
        let json: VersionJson = serde_json::from_str(
            r#"{"id": "1.16.5-forge-36.2.34", "inheritsFrom": "1.16.5", "libraries": [{
                "name": "net.minecraftforge:forge:1.16.5-36.2.34",
                "downloads": {"artifact": {
                    "path": "net/minecraftforge/forge/1.16.5-36.2.34/forge-1.16.5-36.2.34.jar",
                    "url": ""
                }}
            }]}"#,
        )
        .unwrap();
        let infos = bundled_jars(&profile, &json);
        let entries: Vec<&str> = infos.iter().map(|info| info.entry.as_str()).collect();
        assert!(entries.contains(&"maven/net/minecraftforge/forge/1.16.5-36.2.34/forge-1.16.5-36.2.34.jar"));
        assert!(entries.contains(&"maven/net/minecraftforge/forge/1.16.5-36.2.34/forge-1.16.5-36.2.34-universal.jar"));
    }

    #[test]
    fn downloads_libraries_that_have_a_url() {
        let profile: InstallProfile = serde_json::from_str(
            r#"{"libraries": [{
                "name": "net.minecraftforge:binarypatcher:1.1.1",
                "downloads": {"artifact": {
                    "path": "net/minecraftforge/binarypatcher/1.1.1/binarypatcher-1.1.1.jar",
                    "url": "https://maven.minecraftforge.net/net/minecraftforge/binarypatcher/1.1.1/binarypatcher-1.1.1.jar"
                }}
            }]}"#,
        )
        .unwrap();
        assert!(!installer_provides(&profile, &profile.libraries[0]));
        assert!(bundled_jars(&profile, &VersionJson::default()).is_empty());
    }
}
