//! Mods in an instance: jar metadata, enable/disable state.
//!
//! Disabling a mod is the launcher convention of renaming `foo.jar` to
//! `foo.jar.disabled`; the loaders ignore anything that is not a `.jar`.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const DISABLED_SUFFIX: &str = ".disabled";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModLoader {
    Fabric,
    Quilt,
    Forge,
    NeoForge,
    /// A jar that ships both Fabric and Forge/NeoForge metadata.
    Universal,
    #[default]
    Unknown,
}

/// Why a jar cannot run on the instance it sits in. The instance's loader
/// version or game version is outside what the jar declares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockedReason {
    /// `loader` or `game`.
    pub kind: String,
    /// Human readable loader name, `Forge`/`NeoForge`/... or `Minecraft`.
    pub loader: String,
    /// Version predicates the jar declares.
    pub required: Vec<String>,
    /// What the instance actually runs.
    pub actual: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModInfo {
    /// File name as it is on disk, including the `.disabled` suffix.
    pub file: String,
    pub id: String,
    pub name: String,
    pub version: String,
    pub loader: ModLoader,
    pub enabled: bool,
    pub size: u64,
    /// Seconds since the Unix epoch when the file was last changed, 0 if unknown.
    #[serde(default)]
    pub modified: u64,
    /// Required mod ids that no installed jar provides.
    #[serde(default)]
    pub missing_dependencies: Vec<String>,
    /// Required mod ids that only switched-off jars provide: the loader will
    /// still refuse to start until they are enabled again.
    #[serde(default)]
    pub disabled_dependencies: Vec<String>,
    /// Every mod id this jar requires, satisfied or not. The union across
    /// enabled jars is the instance's dependency set.
    #[serde(default)]
    pub dependency: Vec<String>,
    /// Another installed jar declares the same mod id.
    #[serde(default)]
    pub duplicate: bool,
    /// The jar carries metadata for a loader. False means the file is not a
    /// mod, or the metadata is unreadable.
    #[serde(default)]
    pub recognized: bool,
    /// Extra ids this jar satisfies, such as `cloth-config2` from Cloth Config.
    #[serde(default)]
    pub provides: Vec<String>,
    /// Mod ids of libraries bundled inside this jar (`META-INF/jars/` or
    /// `META-INF/jarjar/`). The loader supplies its own copy, so these ids are
    /// satisfied but their version is not this jar's version.
    #[serde(default)]
    pub nested: Vec<String>,
    /// Declared Minecraft version predicates, e.g. `>=1.20.1 <1.21`.
    #[serde(default)]
    pub game_versions: Vec<String>,
    /// Declared loader version predicates.
    #[serde(default)]
    pub loader_versions: Vec<String>,
    /// Declared Java version predicates.
    #[serde(default)]
    pub java_versions: Vec<String>,
    /// Mod ids this jar declares incompatible.
    #[serde(default)]
    pub breaks: Vec<String>,
    /// Version predicates per broken id; an empty list means "any version".
    #[serde(default)]
    pub break_ranges: BTreeMap<String, Vec<String>>,
    /// Version predicates per required dependency id.
    #[serde(default)]
    pub dependency_ranges: BTreeMap<String, Vec<String>>,
    /// Dependencies declared by the Fabric metadata of a universal jar.
    #[serde(default)]
    pub fabric_dependency: Vec<String>,
    /// Version predicates for [`Self::fabric_dependency`].
    #[serde(default)]
    pub fabric_dependency_ranges: BTreeMap<String, Vec<String>>,
    /// Set when the instance's loader or game version cannot satisfy this jar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked: Option<BlockedReason>,
}

pub fn mods_dir(game_dir: &Path) -> PathBuf {
    game_dir.join("mods")
}

fn modified_secs(meta: &fs::Metadata) -> Option<u64> {
    meta.modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|elapsed| elapsed.as_secs())
}

/// Every jar in `dir` that looks like a mod. A missing directory is empty.
pub fn scan(dir: &Path) -> io::Result<Vec<ModInfo>> {
    scan_for(dir, None, "")
}

/// Scan with the instance's loader so universal jars use the matching
/// dependency set (Fabric API on Fabric, `mods.toml` elsewhere).
pub fn scan_for_loader(dir: &Path, loader: Option<ModLoader>) -> io::Result<Vec<ModInfo>> {
    scan_for(dir, loader, "")
}

/// Scan with the instance's loader and game version: a jar that ships both
/// `mods.toml` and `neoforge.mods.toml` declares different dependencies in
/// each, and only the one the game reads may be trusted.
pub fn scan_for(dir: &Path, loader: Option<ModLoader>, game_version: &str) -> io::Result<Vec<ModInfo>> {
    let neoforge_style = loader == Some(ModLoader::NeoForge) && neoforge_metadata(game_version);
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut mods = Vec::new();
    for entry in entries.flatten() {
        if !entry.path().is_file() {
            continue;
        }
        let file = entry.file_name().to_string_lossy().to_string();
        let (stem, enabled) = split_enabled(&file);
        if !stem.ends_with(".jar") {
            continue;
        }
        let meta = entry.metadata().ok();
        let size = meta.as_ref().map(|meta| meta.len()).unwrap_or_default();
        let modified = meta.as_ref().and_then(modified_secs).unwrap_or_default();
        let (metadata, nested) = read_jar(&entry.path(), neoforge_style, loader);
        let mut nested = nested;
        nested.sort();
        nested.dedup();
        nested.retain(|id| metadata.id.as_deref() != Some(id.as_str()) && !metadata.provides.contains(id));
        let mut provides = metadata.provides.clone();
        provides.extend(nested.iter().cloned());
        let fabric_depends = metadata.fabric_depends.clone();
        let fabric_ranges = metadata.fabric_dependency_ranges.clone();
        let fallback = stem.trim_end_matches(".jar").to_string();
        let info = ModInfo {
            id: metadata.id.clone().unwrap_or_else(|| fallback.clone()),
            name: metadata.name.clone().unwrap_or_else(|| fallback.clone()),
            version: metadata.version.clone().unwrap_or_else(|| fallback.clone()),
            loader: metadata.loader,
            enabled,
            size,
            modified,
            file,
            missing_dependencies: Vec::new(),
            disabled_dependencies: Vec::new(),
            dependency: Vec::new(),
            duplicate: false,
            recognized: metadata.recognized,
            provides: provides.clone(),
            nested: nested.clone(),
            game_versions: metadata.game_versions,
            loader_versions: metadata.loader_versions,
            java_versions: metadata.java_versions,
            breaks: metadata.breaks,
            break_ranges: metadata.break_ranges,
            dependency_ranges: metadata.dependency_ranges,
            fabric_dependency: fabric_depends.clone(),
            fabric_dependency_ranges: fabric_ranges.clone(),
            blocked: None,
        };
        mods.push((info, metadata.depends.clone(), fabric_depends, fabric_ranges, provides));
    }
    let mut counts = std::collections::BTreeMap::new();
    for (info, ..) in &mods {
        if info.enabled {
            *counts.entry(info.id.to_ascii_lowercase()).or_insert(0usize) += 1;
        }
    }
    for (info, ..) in &mut mods {
        info.duplicate = info.enabled && counts.get(&info.id.to_ascii_lowercase()).copied().unwrap_or(0) > 1;
    }
    // Forge mod ids are not case-sensitive: a jar may declare `Mekanism` while
    // a dependent asks for `mekanism`. Comparing the raw strings reported the
    // dependency as missing and offered a second copy of the same jar.
    // Disabled jars count as installed: proposing to download a second copy of
    // a switched-off library would put two jars with the same mod id in `mods/`,
    // which the loader refuses to start with.
    let installed: Vec<String> = mods
        .iter()
        .flat_map(|(info, ..)| std::iter::once(info.id.clone()).chain(info.provides.iter().cloned()))
        .map(|id| id.to_ascii_lowercase())
        .collect();
    // Ids the loader will actually see at start-up.
    let switched_on: Vec<String> = mods
        .iter()
        .filter(|(info, ..)| info.enabled)
        .flat_map(|(info, ..)| std::iter::once(info.id.clone()).chain(info.provides.iter().cloned()))
        .map(|id| id.to_ascii_lowercase())
        .collect();
    for (info, forge_depends, fabric_depends, fabric_ranges, _) in &mut mods {
        // Everything this mod requires that the loader itself does not
        // provide: the frontend unions it into "this instance's
        // dependencies", while `missing_dependencies` drives the download hint.
        let use_fabric = matches!(loader, Some(ModLoader::Fabric | ModLoader::Quilt))
            && info.loader == ModLoader::Universal
            && !fabric_depends.is_empty();
        if use_fabric {
            info.dependency_ranges = fabric_ranges.clone();
        }
        info.dependency = if use_fabric { fabric_depends } else { forge_depends }
            .iter()
            .filter(|dependency| sane_id(dependency))
            .cloned()
            .collect();
        info.dependency.sort();
        info.dependency.dedup();
        info.missing_dependencies = info
            .dependency
            .iter()
            .filter(|dependency| !installed.iter().any(|id| id == &dependency.to_ascii_lowercase()))
            .cloned()
            .collect();
        // A dependency whose jar is present but switched off stops the game
        // just as hard as a missing one; the fix is enabling it, not downloading.
        info.disabled_dependencies = if info.enabled {
            info.dependency
                .iter()
                .filter(|dependency| installed.iter().any(|id| id == &dependency.to_ascii_lowercase()))
                .filter(|dependency| !switched_on.iter().any(|id| id == &dependency.to_ascii_lowercase()))
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
    }
    let mut mods: Vec<ModInfo> = mods.into_iter().map(|(info, ..)| info).collect();
    mods.sort_by_key(|entry| entry.name.to_lowercase());
    Ok(mods)
}

/// `foo.jar.disabled` -> (`foo.jar`, false); `foo.jar` -> (`foo.jar`, true).
pub fn split_enabled(file: &str) -> (String, bool) {
    match file.strip_suffix(DISABLED_SUFFIX) {
        Some(stem) => (stem.to_string(), false),
        None => (file.to_string(), true),
    }
}

/// Rename a jar to switch it on or off. Returns the new path.
pub fn set_enabled(path: &Path, enabled: bool) -> io::Result<PathBuf> {
    let file = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "mod path has no file name"))?;
    let (stem, is_enabled) = split_enabled(&file);
    if is_enabled == enabled {
        return Ok(path.to_path_buf());
    }
    let target = if enabled {
        path.with_file_name(stem)
    } else {
        path.with_file_name(format!("{stem}{DISABLED_SUFFIX}"))
    };
    if target.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} already exists", target.display()),
        ));
    }
    fs::rename(path, &target)?;
    Ok(target)
}

#[derive(Default)]
struct Metadata {
    id: Option<String>,
    name: Option<String>,
    version: Option<String>,
    loader: ModLoader,
    depends: Vec<String>,
    /// Extra ids the jar satisfies, such as `cloth-config2` from Cloth Config.
    provides: Vec<String>,
    /// The jar carries readable metadata for its loader.
    recognized: bool,
    game_versions: Vec<String>,
    loader_versions: Vec<String>,
    java_versions: Vec<String>,
    breaks: Vec<String>,
    break_ranges: BTreeMap<String, Vec<String>>,
    dependency_ranges: BTreeMap<String, Vec<String>>,
    fabric_depends: Vec<String>,
    fabric_dependency_ranges: BTreeMap<String, Vec<String>>,
}

/// Mod ids the loader itself provides.
const BUILTIN_MODS: &[&str] = &[
    "minecraft",
    "java",
    "fabricloader",
    "fabric-loader",
    "quilt_loader",
    "quilt-loader",
    "forge",
    "neoforge",
    "javafml",
    "lowcodefml",
];

/// Metadata and bundled module ids of one jar. Both live in the same archive,
/// so it is opened once.
fn read_jar(path: &Path, neoforge_style: bool, loader: Option<ModLoader>) -> (Metadata, Vec<String>) {
    let Ok(file) = fs::File::open(path) else {
        return (Metadata::default(), Vec::new());
    };
    let Ok(mut archive) = zip::ZipArchive::new(file) else {
        return (Metadata::default(), Vec::new());
    };
    let metadata = read_metadata(&mut archive, neoforge_style, loader).unwrap_or_default();
    let nested = read_nested_ids(&mut archive);
    (metadata, nested)
}

/// NeoForge reads `META-INF/neoforge.mods.toml` from 1.20.2 on; 1.20.1 still
/// reads the Forge `mods.toml`, and jars supporting both ship both files with
/// different dependency lists.
fn neoforge_metadata(game_version: &str) -> bool {
    let version = game_version.trim();
    if version.is_empty() {
        return true;
    }
    let mut parts = version.split(['.', '-', '+']);
    let major = parts.next().and_then(|part| part.parse::<u32>().ok());
    let minor = parts.next().and_then(|part| part.parse::<u32>().ok());
    let patch = parts.next().and_then(|part| part.parse::<u32>().ok());
    match (major, minor, patch) {
        // Snapshots are named `24w14a`; assume a modern build there.
        (Some(major), Some(minor), Some(patch)) => (major, minor, patch) >= (1, 20, 2),
        _ => true,
    }
}

fn read_metadata(
    archive: &mut zip::ZipArchive<fs::File>,
    neoforge_style: bool,
    loader: Option<ModLoader>,
) -> io::Result<Metadata> {
    let fabric = read_entry(archive, "fabric.mod.json")?
        .map(|bytes| parse_json(&bytes, ModLoader::Fabric, &["id", "name", "version"]));
    let quilt = if fabric.is_none() {
        read_entry(archive, "quilt.mod.json")?.map(|bytes| parse_quilt(&bytes))
    } else {
        None
    };
    let mut forge = None;
    let candidates = if neoforge_style {
        [
            ("META-INF/neoforge.mods.toml", ModLoader::NeoForge),
            ("META-INF/mods.toml", ModLoader::Forge),
        ]
    } else {
        [
            ("META-INF/mods.toml", ModLoader::Forge),
            ("META-INF/neoforge.mods.toml", ModLoader::NeoForge),
        ]
    };
    for (name, loader) in candidates {
        if let Some(bytes) = read_entry(archive, name)? {
            forge = Some(parse_mods_toml(&bytes, loader));
            break;
        }
    }
    Ok(match (fabric.or(quilt), forge) {
        (Some(fabric), Some(forge)) => combine(fabric, forge, loader),
        (Some(only), None) | (None, Some(only)) => only,
        (None, None) => Metadata::default(),
    })
}

/// A universal jar (for example Serilum's mods) carries both metadata files.
/// Only the set the instance's loader actually reads may be trusted: keeping
/// the Forge game/loader bounds on a Fabric instance made compatible jars look
/// broken, and the other way round it hid the stricter Fabric requirement.
fn combine(fabric: Metadata, forge: Metadata, loader: Option<ModLoader>) -> Metadata {
    let fabric_side = matches!(loader, Some(ModLoader::Fabric | ModLoader::Quilt));
    let primary = if fabric_side { &fabric } else { &forge };
    let fallback = if fabric_side { &forge } else { &fabric };
    let mut provides = forge.provides.clone();
    provides.extend(fabric.provides.iter().cloned());
    provides.sort();
    provides.dedup();
    // Keep both dependency sets: `scan_for` exposes the Fabric one to the UI
    // for universal jars, and the instance's loader picks which list is real.
    Metadata {
        id: primary.id.clone().or_else(|| fallback.id.clone()),
        name: primary.name.clone().or_else(|| fallback.name.clone()),
        version: primary.version.clone().or_else(|| fallback.version.clone()),
        loader: ModLoader::Universal,
        depends: primary.depends.clone(),
        provides,
        recognized: true,
        game_versions: primary.game_versions.clone(),
        loader_versions: primary.loader_versions.clone(),
        java_versions: primary.java_versions.clone(),
        breaks: primary.breaks.clone(),
        break_ranges: primary.break_ranges.clone(),
        dependency_ranges: primary.dependency_ranges.clone(),
        fabric_depends: fabric.depends,
        fabric_dependency_ranges: fabric.dependency_ranges,
    }
}

/// Whether a jar built for `module` can run on an instance running `instance`.
/// Shared by the mod list, the install guard and the doctor so they never
/// disagree about a jar.
pub fn compatible(instance: ModLoader, module: ModLoader, game_version: &str) -> bool {
    match (instance, module) {
        (ModLoader::Fabric, ModLoader::Fabric) => true,
        (ModLoader::Quilt, ModLoader::Quilt | ModLoader::Fabric) => true,
        (ModLoader::Forge, ModLoader::Forge) => true,
        (ModLoader::NeoForge, ModLoader::NeoForge) => true,
        // NeoForge 1.20.1 was a Forge fork and still loads Forge mods.
        (ModLoader::NeoForge, ModLoader::Forge) => game_version.starts_with("1.20.1"),
        // Jars with both metadata files run on either loader.
        (_, ModLoader::Universal) => true,
        _ => false,
    }
}

/// Ids shipped inside `META-INF/jars/` (Fabric) or `META-INF/jarjar/`
/// (Forge/NeoForge), so bundled dependencies do not show up as missing.
fn read_nested_ids(archive: &mut zip::ZipArchive<fs::File>) -> Vec<String> {
    const MAX_JAR: u64 = 16 * 1024 * 1024;
    const NESTED_DIRS: [&str; 2] = ["META-INF/jars/", "META-INF/jarjar/"];
    let names: Vec<String> = (0..archive.len())
        .filter_map(|index| archive.by_index(index).ok().map(|entry| entry.name().to_string()))
        .filter(|name| name.ends_with(".jar") && NESTED_DIRS.iter().any(|dir| name.starts_with(dir)))
        .collect();
    let mut ids = Vec::new();
    for name in names.iter().take(256) {
        let Ok(entry) = archive.by_name(name) else {
            continue;
        };
        if entry.size() > MAX_JAR {
            continue;
        }
        let mut bytes = Vec::new();
        if entry.take(MAX_JAR).read_to_end(&mut bytes).is_err() {
            continue;
        }
        let Ok(mut nested) = zip::ZipArchive::new(io::Cursor::new(bytes)) else {
            continue;
        };
        for candidate in [
            "fabric.mod.json",
            "quilt.mod.json",
            "META-INF/neoforge.mods.toml",
            "META-INF/mods.toml",
        ] {
            let Ok(entry) = nested.by_name(candidate) else {
                continue;
            };
            if entry.size() > 1024 * 1024 {
                continue;
            }
            let mut body = Vec::new();
            if entry.take(1024 * 1024).read_to_end(&mut body).is_err() {
                continue;
            }
            let metadata = match candidate {
                "fabric.mod.json" => parse_json(&body, ModLoader::Fabric, &["id", "name", "version"]),
                "quilt.mod.json" => parse_quilt(&body),
                "META-INF/neoforge.mods.toml" => parse_mods_toml(&body, ModLoader::NeoForge),
                _ => parse_mods_toml(&body, ModLoader::Forge),
            };
            ids.extend(metadata.id);
            ids.extend(metadata.provides);
            break;
        }
    }
    ids
}

fn read_entry(archive: &mut zip::ZipArchive<fs::File>, name: &str) -> io::Result<Option<Vec<u8>>> {
    let entry = match archive.by_name(name) {
        Ok(entry) => entry,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(error) => return Err(io::Error::new(io::ErrorKind::InvalidData, error)),
    };
    if entry.size() > 1024 * 1024 {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    entry
        .take(1024 * 1024)
        .read_to_end(&mut bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(Some(bytes))
}

fn parse_json(bytes: &[u8], loader: ModLoader, keys: &[&str]) -> Metadata {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return Metadata {
            loader,
            ..Metadata::default()
        };
    };
    let field = |key: &str| {
        value
            .get(key)
            .and_then(|value| value.as_str())
            .filter(|text| !text.is_empty() && !text.contains("${file.jarVersion}"))
            .map(ToString::to_string)
    };
    let (breaks, break_ranges) = json_breaks(value.get("breaks"));
    let mut metadata = Metadata {
        id: field(keys[0]),
        name: field(keys[1]),
        version: field(keys[2]),
        loader,
        provides: provides(value.get("provides")),
        breaks,
        break_ranges,
        recognized: true,
        ..Metadata::default()
    };
    if let Some(depends) = value.get("depends").and_then(|depends| depends.as_object()) {
        for (id, value) in depends {
            let ranges = constraints(Some(value));
            apply_dependency(&mut metadata, id, ranges);
        }
    }
    metadata
}

/// A dependency value is a predicate string or an array of them.
fn constraints(value: Option<&serde_json::Value>) -> Vec<String> {
    match value {
        Some(serde_json::Value::String(text)) => vec![text.clone()],
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.as_str())
            .map(ToString::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// `provides` is an array of ids, or of objects that carry an `id`.
fn provides(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(|value| value.as_array())
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.as_str().or_else(|| entry.get("id").and_then(|id| id.as_str())))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// `breaks` is an object keyed by mod id (Fabric) or an array of ids/objects
/// (Quilt). A value may carry a version predicate: `{"other": "<1.0"}` only
/// conflicts with those builds, while no value means every build.
fn json_breaks(value: Option<&serde_json::Value>) -> (Vec<String>, BTreeMap<String, Vec<String>>) {
    let mut ids = Vec::new();
    let mut ranges = BTreeMap::new();
    match value {
        Some(serde_json::Value::Object(map)) => {
            for (id, value) in map {
                let predicates = constraints(Some(value));
                if !predicates.is_empty() {
                    ranges.insert(id.clone(), predicates);
                }
                ids.push(id.clone());
            }
        }
        Some(serde_json::Value::Array(items)) => {
            for item in items {
                let Some(id) = item.as_str().or_else(|| item.get("id").and_then(|id| id.as_str())) else {
                    continue;
                };
                let predicates = constraints(item.get("versions"));
                if !predicates.is_empty() {
                    ranges.insert(id.to_string(), predicates);
                }
                ids.push(id.to_string());
            }
        }
        _ => {}
    }
    ids.sort();
    ids.dedup();
    (ids, ranges)
}

fn apply_dependency(metadata: &mut Metadata, id: &str, ranges: Vec<String>) {
    match id {
        "minecraft" => metadata.game_versions.extend(ranges),
        "java" => metadata.java_versions.extend(ranges),
        "fabricloader" | "fabric-loader" | "quilt_loader" | "quilt-loader" | "forge" | "neoforge" | "javafml"
        | "lowcodefml" => metadata.loader_versions.extend(ranges),
        _ if BUILTIN_MODS.contains(&id) => {}
        _ if sane_id(id) => {
            metadata.depends.push(id.to_string());
            if !ranges.is_empty() {
                metadata
                    .dependency_ranges
                    .entry(id.to_string())
                    .or_default()
                    .extend(ranges);
            }
        }
        _ => {}
    }
}

/// `quilt.mod.json` nests the same information under `quilt_loader`.
fn parse_quilt(bytes: &[u8]) -> Metadata {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return Metadata {
            loader: ModLoader::Quilt,
            ..Metadata::default()
        };
    };
    let loader = &value["quilt_loader"];
    let text = |value: &serde_json::Value| {
        value
            .as_str()
            .filter(|text| !text.is_empty() && !text.contains("${file.jarVersion}"))
            .map(ToString::to_string)
    };
    let (breaks, break_ranges) = json_breaks(loader.get("breaks"));
    let mut metadata = Metadata {
        id: text(&loader["id"]),
        name: text(&loader["metadata"]["name"]).or_else(|| text(&loader["id"])),
        version: text(&loader["version"]),
        loader: ModLoader::Quilt,
        provides: provides(loader.get("provides")),
        breaks,
        break_ranges,
        recognized: true,
        ..Metadata::default()
    };
    if let Some(depends) = loader["depends"].as_array() {
        for entry in depends {
            let Some(id) = entry.as_str().or_else(|| entry.get("id").and_then(|id| id.as_str())) else {
                continue;
            };
            let ranges = constraints(entry.get("versions"));
            apply_dependency(&mut metadata, id, ranges);
        }
    }
    metadata
}

/// Arrays in real `mods.toml` files are wrapped over several lines
/// (`provides = [\n  "a",\n  "b"\n]`). The line-oriented reader has to see
/// one logical line or every alias after the first is lost.
fn logical_lines(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut pending: Option<String> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if let Some(mut buffer) = pending.take() {
            buffer.push(' ');
            buffer.push_str(line.split('#').next().unwrap_or_default());
            if line.contains(']') {
                lines.push(buffer);
            } else {
                pending = Some(buffer);
            }
            continue;
        }
        if let Some((_, value)) = line.split_once('=')
            && value.trim().starts_with('[')
            && !value.split('#').next().unwrap_or_default().contains(']')
        {
            pending = Some(line.to_string());
            continue;
        }
        lines.push(line.to_string());
    }
    if let Some(buffer) = pending {
        lines.push(buffer);
    }
    lines
}

/// Forge and NeoForge keep mod metadata in a TOML file. Only the first
/// `[[mods]]` table is needed, so a full TOML parser is overkill.
fn parse_mods_toml(bytes: &[u8], loader: ModLoader) -> Metadata {
    let text = String::from_utf8_lossy(bytes);
    let mut metadata = Metadata {
        loader,
        ..Metadata::default()
    };
    let mut inside = false;
    let mut saw_mods = false;
    let mut dependency: Option<Dependency> = None;
    for raw in logical_lines(&text) {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            let header = line.split('#').next().unwrap_or_default().trim();
            if let Some(dep) = dependency.take() {
                dep.apply(&mut metadata);
            }
            if header.starts_with("[[dependencies.") {
                dependency = Some(Dependency::default());
                inside = false;
            } else {
                inside = header == "[[mods]]";
                saw_mods |= inside;
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = toml_value(value);
        if let Some(dep) = dependency.as_mut() {
            match key {
                "modId" => dep.mod_id = Some(value),
                "mandatory" => dep.mandatory = value.eq_ignore_ascii_case("true"),
                "type" => dep.kind = Some(value),
                "side" => dep.side = Some(value),
                "versionRange" => dep.version_range = Some(value),
                _ => {}
            }
            continue;
        }
        if !inside || value.is_empty() {
            continue;
        }
        // `${...}` is a template token that Forge fills in at runtime, so the
        // literal text would only show up as garbage in the mod list.
        if value.contains("${") {
            continue;
        }
        match key {
            "modId" => metadata.id = Some(value),
            "displayName" => metadata.name = Some(value),
            "version" => metadata.version = Some(value),
            "provides" => metadata.provides = toml_array(&value),
            _ => {}
        }
    }
    if let Some(dep) = dependency {
        dep.apply(&mut metadata);
    }
    metadata.recognized = saw_mods && metadata.id.is_some();
    metadata.depends.sort();
    metadata.depends.dedup();
    metadata.provides.sort();
    metadata.provides.dedup();
    metadata
}

struct Dependency {
    mod_id: Option<String>,
    mandatory: bool,
    kind: Option<String>,
    side: Option<String>,
    version_range: Option<String>,
}

impl Default for Dependency {
    fn default() -> Self {
        Self {
            mod_id: None,
            mandatory: true,
            kind: None,
            side: None,
            version_range: None,
        }
    }
}

impl Dependency {
    fn apply(mut self, metadata: &mut Metadata) {
        let Some(id) = self.mod_id.take() else {
            return;
        };
        let ranges = self
            .version_range
            .take()
            .filter(|range| !range.is_empty() && range != "*")
            .map(|range| vec![range])
            .unwrap_or_default();
        let incompatible = self.kind.as_deref() == Some("incompatible");
        if incompatible {
            if !ranges.is_empty() {
                metadata.break_ranges.entry(id.clone()).or_default().extend(ranges);
            }
            metadata.breaks.push(id);
            return;
        }
        let optional = !self.mandatory || matches!(self.kind.as_deref(), Some("optional" | "discouraged"));
        let server_only = self.side.as_deref() == Some("SERVER");
        if optional || server_only {
            return;
        }
        apply_dependency(metadata, &id, ranges);
    }
}

fn toml_value(value: &str) -> String {
    let value = value.trim();
    // A quoted value may be followed by an inline comment, e.g.
    // `modId="forge" #mandatory`. Stop at the quote that closes the value;
    // everything after it is a comment. Splitting on `#` would have kept the
    // quote inside the id ("forge" #mandatory) and turned the loader itself
    // into a missing dependency.
    for quote in ['"', '\''] {
        if let Some(rest) = value.strip_prefix(quote)
            && let Some(end) = rest.find(quote)
        {
            return rest[..end].to_string();
        }
    }
    let unquoted = match value.split_once('#') {
        Some((head, _)) => head.trim(),
        _ => value,
    };
    unquoted.trim_matches(['"', '\'']).trim().to_string()
}

/// `provides = ["fabric", "fabric-api"]` inside `[[mods]]`. Forge and NeoForge
/// use this to publish alias ids; without it a port such as Forgified Fabric
/// API looks like it does not satisfy a mod that depends on `fabric-api`.
fn toml_array(value: &str) -> Vec<String> {
    let body = value.trim();
    let body = body.strip_prefix('[').unwrap_or(body);
    let body = body.strip_suffix(']').unwrap_or(body);
    body.split(',')
        .map(str::trim)
        .map(toml_value)
        .filter(|item| sane_id(item))
        .collect()
}

/// A dependency id that a loader can ever resolve: letters, digits and the
/// separators used by real mods.
fn sane_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tntl-mods-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("mods")).unwrap();
        dir
    }

    fn write_zip(path: &Path, entries: &[(&str, &str)]) {
        let file = fs::File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        for (name, body) in entries {
            zip.start_file(*name, options).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn reads_fabric_metadata() {
        let dir = temp_dir("fabric");
        write_zip(
            &dir.join("mods/example.jar"),
            &[(
                "fabric.mod.json",
                r#"{"id":"example","name":"Example Mod","version":"1.2.3"}"#,
            )],
        );
        let mods = scan(&dir.join("mods")).unwrap();
        assert_eq!(mods.len(), 1);
        assert_eq!(mods[0].id, "example");
        assert_eq!(mods[0].name, "Example Mod");
        assert_eq!(mods[0].version, "1.2.3");
        assert_eq!(mods[0].loader, ModLoader::Fabric);
        assert!(mods[0].enabled);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn universal_jars_use_the_instance_dependency_set() {
        let dir = temp_dir("universal");
        write_zip(
            &dir.join("mods/collective.jar"),
            &[
                (
                    "fabric.mod.json",
                    r#"{"id":"collective","name":"Collective","version":"8.65","depends":{"fabric":"*","minecraft":">=1.20","java":">=17"}}"#,
                ),
                (
                    "META-INF/mods.toml",
                    "modLoader=\"javafml\"\n[[mods]]\nmodId=\"collective\"\ndisplayName=\"Collective\"\nversion=\"8.65\"\n[[dependencies.collective]]\nmodId=\"minecraft\"\nmandatory=true\ntype=\"required\"\nversionRange=\"[1.20.1]\"\n",
                ),
            ],
        );
        let neoforge = scan_for_loader(&dir.join("mods"), Some(ModLoader::NeoForge)).unwrap();
        assert_eq!(neoforge[0].loader, ModLoader::Universal);
        assert!(neoforge[0].dependency.is_empty());
        assert!(neoforge[0].missing_dependencies.is_empty());
        assert_eq!(neoforge[0].fabric_dependency, vec!["fabric"]);

        let fabric = scan_for_loader(&dir.join("mods"), Some(ModLoader::Fabric)).unwrap();
        assert_eq!(fabric[0].dependency, vec!["fabric"]);
        assert_eq!(fabric[0].missing_dependencies, vec!["fabric"]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn neoforge_provides_ids_are_read_from_mods_toml() {
        // Forgified Fabric API declares `modId = "fabric_api"` plus
        // `provides = ["fabric", "fabric-api"]`; without the aliases a Fabric
        // mod running on NeoForge reports `fabric-api` as missing.
        let dir = temp_dir("provides");
        write_zip(
            &dir.join("mods/fapi.jar"),
            &[(
                "META-INF/neoforge.mods.toml",
                "modLoader=\"javafml\"\n[[mods]]\nmodId=\"fabric_api\"\nversion=\"0.116.7\"\nprovides=[\"fabric\", \"fabric-api\"]\n",
            )],
        );
        let mods = scan_for_loader(&dir.join("mods"), Some(ModLoader::NeoForge)).unwrap();
        assert_eq!(mods[0].id, "fabric_api");
        assert!(mods[0].provides.contains(&"fabric-api".to_string()));
        assert!(mods[0].provides.contains(&"fabric".to_string()));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reads_quilt_metadata() {
        let dir = temp_dir("quilt");
        write_zip(
            &dir.join("mods/example.jar"),
            &[(
                "quilt.mod.json",
                r#"{"quilt_loader":{"id":"example","version":"2.0.0","metadata":{"name":"Quilt Example"}}}"#,
            )],
        );
        let mods = scan(&dir.join("mods")).unwrap();
        assert_eq!(mods[0].id, "example");
        assert_eq!(mods[0].name, "Quilt Example");
        assert_eq!(mods[0].loader, ModLoader::Quilt);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reads_forge_metadata() {
        let dir = temp_dir("forge");
        write_zip(
            &dir.join("mods/example.jar"),
            &[(
                "META-INF/mods.toml",
                "modLoader=\"javafml\"\n[[mods]]\nmodId=\"example\"\ndisplayName=\"Forge Example\"\nversion=\"3.1\"\n",
            )],
        );
        let mods = scan(&dir.join("mods")).unwrap();
        assert_eq!(mods[0].id, "example");
        assert_eq!(mods[0].name, "Forge Example");
        assert_eq!(mods[0].version, "3.1");
        assert_eq!(mods[0].loader, ModLoader::Forge);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn falls_back_to_the_file_name() {
        let dir = temp_dir("plain");
        write_zip(&dir.join("mods/some-mod-1.0.jar"), &[("pack.mcmeta", "{}")]);
        let mods = scan(&dir.join("mods")).unwrap();
        assert_eq!(mods[0].id, "some-mod-1.0");
        assert_eq!(mods[0].name, "some-mod-1.0");
        assert_eq!(mods[0].loader, ModLoader::Unknown);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn toggling_renames_the_file() {
        let dir = temp_dir("toggle");
        let jar = dir.join("mods/example.jar");
        write_zip(&jar, &[("fabric.mod.json", r#"{"id":"example"}"#)]);

        let disabled = set_enabled(&jar, false).unwrap();
        assert_eq!(disabled.file_name().unwrap(), "example.jar.disabled");
        assert!(disabled.exists());
        let mods = scan(&dir.join("mods")).unwrap();
        assert_eq!(mods.len(), 1);
        assert!(!mods[0].enabled);
        assert_eq!(mods[0].file, "example.jar.disabled");

        let enabled = set_enabled(&disabled, true).unwrap();
        assert_eq!(enabled.file_name().unwrap(), "example.jar");
        assert!(scan(&dir.join("mods")).unwrap()[0].enabled);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn mods_toml_inline_comments_stay_out_of_ids() {
        // A quoted value with a trailing `#comment` used to produce the id
        // `forge" #mandatory`, so the loader itself showed up as a missing
        // dependency and the deps button looked up Modrinth for it.
        let toml = br#"
[[dependencies."generator"]]
modId="forge"
mandatory=true
versionRange="[41,)"

[[dependencies."generator"]]
modId="woodenhopper" #mandatory
versionRange="[1.6,)"
"#;
        let metadata = parse_mods_toml(toml, ModLoader::Forge);
        assert_eq!(metadata.depends, vec!["woodenhopper"]);
    }

    #[test]
    fn mods_toml_template_version_is_dropped() {
        // Forge expands `${file.jarVersion}` while loading; the raw token is
        // not a version and used to be shown in the mod list.
        let toml = br#"
[[mods]]
modId="controlling"
displayName="Controlling"
version="${file.jarVersion}"
"#;
        let metadata = parse_mods_toml(toml, ModLoader::Forge);
        assert_eq!(metadata.id.as_deref(), Some("controlling"));
        assert_eq!(metadata.version, None);
    }

    #[test]
    fn reports_missing_dependencies() {
        let dir = temp_dir("deps");
        write_zip(
            &dir.join("mods/lib.jar"),
            &[(
                "fabric.mod.json",
                r#"{"id":"lib","name":"Lib","version":"1","provides":["lib-alias"]}"#,
            )],
        );
        write_zip(
            &dir.join("mods/example.jar"),
            &[(
                "fabric.mod.json",
                r#"{"id":"example","name":"Example","version":"1","depends":{"lib":">=1","lib-alias":"*","minecraft":"*","fabricloader":">=0.14","absent":"*"}}"#,
            )],
        );
        let mods = scan(&dir.join("mods")).unwrap();
        let example = mods.iter().find(|entry| entry.id == "example").unwrap();
        assert_eq!(example.missing_dependencies, vec!["absent"]);
        let lib = mods.iter().find(|entry| entry.id == "lib").unwrap();
        assert!(lib.missing_dependencies.is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn dependency_ids_are_compared_without_case() {
        // Forge ids keep the case the author chose (`Mekanism`), while a
        // dependent may ask for `mekanism`. A case-sensitive comparison called
        // the dependency missing and offered a second copy of the same jar.
        let dir = temp_dir("deps-case");
        write_zip(
            &dir.join("mods/Mekanism.jar"),
            &[("META-INF/mods.toml", "[[mods]]\nmodId=\"Mekanism\"\nversion=\"10.4\"\n")],
        );
        write_zip(
            &dir.join("mods/addon.jar"),
            &[(
                "META-INF/mods.toml",
                "[[mods]]\nmodId=\"addon\"\nversion=\"1\"\n\n[[dependencies.addon]]\nmodId=\"mekanism\"\nmandatory=true\nversionRange=\"[10,)\"\n",
            )],
        );
        let mods = scan(&dir.join("mods")).unwrap();
        let addon = mods.iter().find(|entry| entry.id == "addon").unwrap();
        assert!(
            addon.missing_dependencies.is_empty(),
            "{:?}",
            addon.missing_dependencies
        );
        assert!(
            addon.disabled_dependencies.is_empty(),
            "{:?}",
            addon.disabled_dependencies
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reports_dependencies_that_are_switched_off() {
        // The jar is there but disabled: the game still fails to start, and
        // the fix is enabling it, so it must not be offered as a download.
        let dir = temp_dir("disabled-deps");
        write_zip(
            &dir.join("mods/lib.jar.disabled"),
            &[("fabric.mod.json", r#"{"id":"lib","version":"1"}"#)],
        );
        write_zip(
            &dir.join("mods/example.jar"),
            &[(
                "fabric.mod.json",
                r#"{"id":"example","version":"1","depends":{"lib":">=1"}}"#,
            )],
        );
        let mods = scan(&dir.join("mods")).unwrap();
        let example = mods.iter().find(|entry| entry.id == "example").unwrap();
        assert!(example.missing_dependencies.is_empty());
        assert_eq!(example.disabled_dependencies, vec!["lib"]);
        let lib = mods.iter().find(|entry| entry.id == "lib").unwrap();
        assert!(lib.disabled_dependencies.is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn ignores_dependencies_bundled_inside_the_jar() {
        let dir = temp_dir("nested");
        let mut inner = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        inner.start_file("fabric.mod.json", options).unwrap();
        inner.write_all(br#"{"id":"spruceui"}"#).unwrap();
        let nested = inner.finish().unwrap().into_inner();

        let path = dir.join("mods/example.jar");
        let file = fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("fabric.mod.json", options).unwrap();
        zip.write_all(br#"{"id":"example","depends":{"spruceui":"*"}}"#)
            .unwrap();
        zip.start_file("META-INF/jars/spruceui.jar", options).unwrap();
        zip.write_all(&nested).unwrap();
        zip.finish().unwrap();

        let mods = scan(&dir.join("mods")).unwrap();
        assert!(mods[0].missing_dependencies.is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn ignores_forge_jarjar_dependencies() {
        // Create 6 ships Flywheel and Ponder in META-INF/jarjar/ and marks
        // them mandatory; not knowing that made the deps button look for
        // unrelated projects with the same names.
        let dir = temp_dir("jarjar");
        let mut inner = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        inner.start_file("META-INF/mods.toml", options).unwrap();
        inner
            .write_all(b"modLoader=\"javafml\"\n[[mods]]\nmodId=\"flywheel\"\nversion=\"1.0.5\"\n")
            .unwrap();
        let nested = inner.finish().unwrap().into_inner();

        let path = dir.join("mods/create.jar");
        let file = fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("META-INF/mods.toml", options).unwrap();
        zip.write_all(
            b"modLoader=\"javafml\"\n[[mods]]\nmodId=\"create\"\nversion=\"6.0.8\"\n\
[[dependencies.create]]\nmodId=\"flywheel\"\nmandatory=true\nversionRange=\"[1.0.0,2.0)\"\n",
        )
        .unwrap();
        zip.start_file("META-INF/jarjar/flywheel.jar", options).unwrap();
        zip.write_all(&nested).unwrap();
        zip.finish().unwrap();

        let mods = scan_for_loader(&dir.join("mods"), Some(ModLoader::NeoForge)).unwrap();
        assert_eq!(mods[0].dependency, vec!["flywheel"]);
        assert!(mods[0].missing_dependencies.is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn neoforge_1_20_1_reads_the_forge_metadata() {
        // Sodium Options API ships both files; only the NeoForge one requires
        // Reese's Sodium Options, and NeoForge 1.20.1 reads `mods.toml`.
        let dir = temp_dir("neo-metadata");
        write_zip(
            &dir.join("mods/api.jar"),
            &[
                (
                    "META-INF/mods.toml",
                    "modLoader=\"javafml\"\n[[mods]]\nmodId=\"api\"\nversion=\"1\"\n\
[[dependencies.api]]\nmodId=\"minecraft\"\nmandatory=true\nversionRange=\"[1.20.1]\"\n",
                ),
                (
                    "META-INF/neoforge.mods.toml",
                    "modLoader=\"javafml\"\n[[mods]]\nmodId=\"api\"\nversion=\"1\"\n\
[[dependencies.api]]\nmodId=\"minecraft\"\nmandatory=true\nversionRange=\"[1.20.1]\"\n\
[[dependencies.api]]\nmodId=\"reeses_sodium_options\"\nmandatory=true\nversionRange=\"*\"\n",
                ),
            ],
        );
        let old = scan_for(&dir.join("mods"), Some(ModLoader::NeoForge), "1.20.1").unwrap();
        assert!(
            old[0].missing_dependencies.is_empty(),
            "{:?}",
            old[0].missing_dependencies
        );

        let new = scan_for(&dir.join("mods"), Some(ModLoader::NeoForge), "1.21.1").unwrap();
        assert_eq!(new[0].missing_dependencies, vec!["reeses_sodium_options"]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_directory_is_empty() {
        let dir = temp_dir("missing");
        fs::remove_dir_all(&dir).unwrap();
        assert!(scan(&dir).unwrap().is_empty());
    }

    #[test]
    fn reads_forge_dependencies() {
        let dir = temp_dir("forgedeps");
        write_zip(
            &dir.join("mods/example.jar"),
            &[(
                "META-INF/mods.toml",
                "modLoader=\"javafml\"\n[[mods]]\nmodId=\"example\"\nversion=\"1\"\n\
                 [[dependencies.example]]\nmodId=\"lib\"\nmandatory=true\ntype=\"required\"\nside=\"BOTH\"\n\
                 [[dependencies.example]]\nmodId=\"absent\"\nmandatory=true\ntype=\"required\"\nside=\"BOTH\"\n\
                 [[dependencies.example]]\nmodId=\"optional-lib\"\nmandatory=false\ntype=\"optional\"\n\
                 [[dependencies.example]]\nmodId=\"forge\"\nmandatory=true\ntype=\"required\"\n",
            )],
        );
        write_zip(
            &dir.join("mods/lib.jar"),
            &[("META-INF/mods.toml", "[[mods]]\nmodId=\"lib\"\n")],
        );
        let mods = scan(&dir.join("mods")).unwrap();
        let example = mods.iter().find(|entry| entry.id == "example").unwrap();
        assert_eq!(example.missing_dependencies, vec!["absent"]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reads_version_ranges_and_breaks() {
        let dir = temp_dir("ranges");
        write_zip(
            &dir.join("mods/example.jar"),
            &[(
                "fabric.mod.json",
                r#"{"id":"example","version":"1","depends":{"minecraft":"1.20.x","java":">=17","lib":"[2,)"},"breaks":{"optifine":"*"}}"#,
            )],
        );
        let mods = scan(&dir.join("mods")).unwrap();
        assert_eq!(mods[0].game_versions, vec!["1.20.x"]);
        assert_eq!(mods[0].java_versions, vec![">=17"]);
        assert_eq!(mods[0].breaks, vec!["optifine"]);
        assert_eq!(
            mods[0].dependency_ranges.get("lib").cloned(),
            Some(vec!["[2,)".to_string()])
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn marks_duplicate_mod_ids() {
        let dir = temp_dir("duplicate");
        write_zip(
            &dir.join("mods/example-1.jar"),
            &[("fabric.mod.json", r#"{"id":"example","version":"1"}"#)],
        );
        write_zip(
            &dir.join("mods/example-2.jar"),
            &[("fabric.mod.json", r#"{"id":"example","version":"2"}"#)],
        );
        let mods = scan(&dir.join("mods")).unwrap();
        assert_eq!(mods.len(), 2);
        assert!(mods.iter().all(|entry| entry.duplicate));
        fs::remove_dir_all(dir).unwrap();
    }
}
