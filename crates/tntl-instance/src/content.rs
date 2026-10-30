//! Resource packs and shaders: which files exist and which ones the game has
//! switched on.
//!
//! Resource packs live in `options.txt` (`resourcePacks`, a JSON array of
//! `file/<name>` entries, last entry wins). Shaders are owned by the shader
//! mod: Iris keeps the active pack in `config/iris.properties` and its Forge
//! fork Oculus in `config/oculus.properties` (`shaderPack`, `enableShaders`),
//! OptiFine in `optionsshaders.txt` (`shaderPack`, `OFF` when disabled).

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tntl_store::{StoreError, atomic_write};

pub const OPTIONS_FILE: &str = "options.txt";
pub const IRIS_CONFIG: &str = "config/iris.properties";
pub const OCULUS_CONFIG: &str = "config/oculus.properties";
pub const OPTIFINE_CONFIG: &str = "optionsshaders.txt";

#[derive(Debug, Error)]
pub enum ContentError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("cannot write the skin resource pack: {0}")]
    Zip(String),
    #[error("shaders cannot be switched: neither Iris nor OptiFine is installed in this instance")]
    NoShaderLoader,
    #[error("{0} is not enabled")]
    NotEnabled(String),
    #[error("invalid content file name: {0}")]
    InvalidName(String),
}

fn io_error(path: &Path, source: io::Error) -> ContentError {
    ContentError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// File names in a content directory, sorted. A missing directory is empty.
pub fn list(dir: &Path) -> io::Result<Vec<String>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut files: Vec<String> = entries
        .flatten()
        .filter(|entry| {
            let path = entry.path();
            path.is_file() || path.is_dir()
        })
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| !name.starts_with('.'))
        .collect();
    files.sort();
    Ok(files)
}

/// Resource packs the game has enabled, without the `file/` prefix.
pub fn enabled_resource_packs(game_dir: &Path) -> Result<Vec<String>, ContentError> {
    let options = Options::load(&game_dir.join(OPTIONS_FILE))?;
    Ok(pack_entries(&options)
        .iter()
        .map(|entry| entry.strip_prefix("file/").unwrap_or(entry).to_string())
        .collect())
}

/// Switch one resource pack on or off. The game must not be running: it writes
/// `options.txt` back on exit.
pub fn set_resource_pack(game_dir: &Path, file: &str, enabled: bool) -> Result<(), ContentError> {
    let path = game_dir.join(OPTIONS_FILE);
    let mut options = Options::load(&path)?;
    let mut packs = pack_entries(&options);
    let wanted = format!("file/{file}");
    let present = packs.iter().any(|entry| entry == &wanted);
    if present == enabled {
        // Already in the requested state: keep its place in the stacking order.
        return Ok(());
    }
    packs.retain(|entry| entry != &wanted);
    if enabled {
        // Vanilla is the base layer; keep the game from ending up with no pack.
        if !packs.iter().any(|entry| entry == "vanilla") {
            packs.insert(0, "vanilla".to_string());
        }
        packs.push(wanted);
    }
    let value = serde_json::to_string(&packs).unwrap_or_else(|_| "[]".to_string());
    options.set("resourcePacks", &value);
    options.save(&path)
}

/// Move an enabled resource pack one slot up (towards the top of the list, so
/// it overrides more packs) or down. Vanilla stays at the bottom.
pub fn move_resource_pack(game_dir: &Path, file: &str, up: bool) -> Result<(), ContentError> {
    let path = game_dir.join(OPTIONS_FILE);
    let mut options = Options::load(&path)?;
    let mut packs = pack_entries(&options);
    let wanted = format!("file/{file}");
    let Some(index) = packs.iter().position(|entry| entry == &wanted) else {
        return Err(ContentError::NotEnabled(file.to_string()));
    };
    // `vanilla` is the base layer and must stay first in `options.txt`. Later
    // entries are stacked on top, so "up" means towards the end of the array.
    let first = if packs.first().is_some_and(|entry| entry == "vanilla") {
        1
    } else {
        0
    };
    let target = if up {
        (index + 1 < packs.len()).then_some(index + 1)
    } else {
        index.checked_sub(1).filter(|target| *target >= first)
    };
    let Some(target) = target else {
        return Ok(());
    };
    packs.swap(index, target);
    let value = serde_json::to_string(&packs).unwrap_or_else(|_| "[]".to_string());
    options.set("resourcePacks", &value);
    options.save(&path)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShaderBackend {
    /// Iris and its Forge/NeoForge forks. `config` is the properties file the
    /// installed fork reads: Iris uses `config/iris.properties`, Oculus keeps
    /// `config/oculus.properties`.
    Iris {
        config: &'static str,
    },
    OptiFine,
}

/// Mod ids the jars in `mods/` declare, including the aliases they provide.
fn installed_mod_ids(game_dir: &Path) -> Vec<String> {
    crate::mods::scan(&game_dir.join("mods"))
        .map(|mods| {
            mods.into_iter()
                .flat_map(|module| std::iter::once(module.id).chain(module.provides))
                .map(|id| id.to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default()
}

/// Which shader mod owns the shader settings. Iris wins when several are
/// present; Oculus (Iris for Forge) is recognised by its mod id and file.
fn shader_backend(game_dir: &Path) -> Option<ShaderBackend> {
    let ids = installed_mod_ids(game_dir);
    let has = |needles: &[&str]| ids.iter().any(|id| needles.contains(&id.as_str())) || jar_named(game_dir, needles);
    if has(&["iris"]) {
        return Some(ShaderBackend::Iris { config: IRIS_CONFIG });
    }
    if has(&["oculus", "oculus-unofficial", "neoculus", "forgeoculus"]) {
        // Oculus writes its own file; an existing Iris file is kept when the
        // instance was set up with Iris before.
        let config = if game_dir.join(OCULUS_CONFIG).is_file() {
            OCULUS_CONFIG
        } else if game_dir.join(IRIS_CONFIG).is_file() {
            IRIS_CONFIG
        } else {
            OCULUS_CONFIG
        };
        return Some(ShaderBackend::Iris { config });
    }
    // A jar renamed past recognition still leaves its config behind.
    if game_dir.join(IRIS_CONFIG).is_file() {
        return Some(ShaderBackend::Iris { config: IRIS_CONFIG });
    }
    if game_dir.join(OCULUS_CONFIG).is_file() {
        return Some(ShaderBackend::Iris { config: OCULUS_CONFIG });
    }
    if game_dir.join(OPTIFINE_CONFIG).is_file() || has(&["optifine"]) {
        return Some(ShaderBackend::OptiFine);
    }
    None
}

/// Fallback for jars whose metadata cannot be read (OptiFine ships no
/// `mods.toml`): the first token of the file name.
fn jar_named(game_dir: &Path, needles: &[&str]) -> bool {
    let Ok(entries) = fs::read_dir(game_dir.join("mods")) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if !name.ends_with(".jar") {
            return false;
        }
        // Match the first token only: `spider-iris-fix.jar` is not Iris.
        name.trim_end_matches(".jar")
            .split(['-', '_', '.'])
            .next()
            .is_some_and(|token| needles.contains(&token))
    })
}

/// Shader packs the game can switch without help: Iris or OptiFine has to be
/// installed.
pub fn shaders_supported(game_dir: &Path) -> bool {
    shader_backend(game_dir).is_some()
}

/// Shader pack the shader mod has active, if any.
pub fn active_shader(game_dir: &Path) -> Result<Option<String>, ContentError> {
    match shader_backend(game_dir) {
        Some(ShaderBackend::Iris { config }) => {
            let path = game_dir.join(config);
            if !path.is_file() {
                return Ok(None);
            }
            let properties = Properties::load(&path)?;
            if properties.get("enableShaders").is_some_and(|value| value == "false") {
                return Ok(None);
            }
            Ok(properties
                .get("shaderPack")
                .map(str::trim)
                .filter(|name| !name.is_empty() && *name != "(internal)")
                .map(ToString::to_string))
        }
        Some(ShaderBackend::OptiFine) => {
            let path = game_dir.join(OPTIFINE_CONFIG);
            if !path.is_file() {
                return Ok(None);
            }
            let properties = Properties::load(&path)?;
            Ok(properties
                .get("shaderPack")
                .map(str::trim)
                .filter(|name| !name.is_empty() && !name.eq_ignore_ascii_case("off"))
                .map(ToString::to_string))
        }
        None => Ok(None),
    }
}

/// Switch one shader pack on, or turn shaders off.
pub fn set_shader(game_dir: &Path, file: &str, enabled: bool) -> Result<(), ContentError> {
    match shader_backend(game_dir) {
        Some(ShaderBackend::Iris { config }) => {
            let path = game_dir.join(config);
            let mut properties = Properties::load(&path)?;
            if enabled {
                properties.set("shaderPack", file);
                properties.set("enableShaders", "true");
            } else {
                // `(internal)` is what Iris and Oculus write when no pack is
                // selected; an empty value makes them try to load a pack called
                // "" and log a failure.
                properties.set("shaderPack", "(internal)");
                properties.set("enableShaders", "false");
            }
            properties.save(&path)
        }
        Some(ShaderBackend::OptiFine) => {
            let path = game_dir.join(OPTIFINE_CONFIG);
            let mut properties = Properties::load(&path)?;
            properties.set("shaderPack", if enabled { file } else { "OFF" });
            properties.save(&path)
        }
        None => Err(ContentError::NoShaderLoader),
    }
}

/// Name of the generated pack that paints an offline skin over the default one.
pub const SKIN_PACK: &str = "tntl-skin.zip";

/// What a resource pack declares about itself in `pack.mcmeta`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackMeta {
    /// `pack.description`, flattened to text when it is a JSON chat component.
    pub description: Option<String>,
    /// `pack.pack_format`.
    pub format: Option<u32>,
    /// Lowest format from `supported_formats`, 1.20.2 and newer only.
    pub min_format: Option<u32>,
    /// Highest format from `supported_formats`.
    pub max_format: Option<u32>,
}

const META_LIMIT: u64 = 1024 * 1024;
const ICON_LIMIT: u64 = 4 * 1024 * 1024;

/// Read `pack.mcmeta` from a pack zip or an unzipped pack folder. A missing or
/// broken file yields an empty record: it must not make the listing fail.
pub fn pack_meta(path: &Path) -> PackMeta {
    let Some(bytes) = read_pack_entry(path, "pack.mcmeta", META_LIMIT) else {
        return PackMeta::default();
    };
    parse_pack_meta(&bytes)
}

/// `pack.png` of a pack zip or folder, if it has one.
pub fn pack_icon(path: &Path) -> Option<Vec<u8>> {
    read_pack_entry(path, "pack.png", ICON_LIMIT)
}

fn read_pack_entry(path: &Path, name: &str, limit: u64) -> Option<Vec<u8>> {
    if path.is_dir() {
        let entry = path.join(name);
        let meta = fs::metadata(&entry).ok()?;
        if !meta.is_file() || meta.len() > limit {
            return None;
        }
        return fs::read(&entry).ok();
    }
    let file = fs::File::open(path).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;
    let entry = archive.by_name(name).ok()?;
    if entry.size() > limit {
        return None;
    }
    let mut bytes = Vec::new();
    entry.take(limit).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

fn parse_pack_meta(bytes: &[u8]) -> PackMeta {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return PackMeta::default();
    };
    let pack = value.get("pack").unwrap_or(&value);
    let number = |value: Option<&serde_json::Value>| value.and_then(|value| value.as_u64()).map(|value| value as u32);
    let (min_format, max_format) = match pack.get("supported_formats") {
        // 1.20.2 and newer: an array `[min, max]` or an object with both bounds.
        Some(serde_json::Value::Array(bounds)) => {
            let mut bounds = bounds.iter().filter_map(|value| value.as_u64());
            (
                bounds.next().map(|value| value as u32),
                bounds.next().map(|value| value as u32),
            )
        }
        Some(serde_json::Value::Object(bounds)) => {
            (number(bounds.get("min_inclusive")), number(bounds.get("max_inclusive")))
        }
        _ => (None, None),
    };
    PackMeta {
        description: pack.get("description").and_then(description_text),
        format: number(pack.get("pack_format")),
        min_format,
        max_format,
    }
}

/// A description is a plain string or a chat component tree.
fn description_text(value: &serde_json::Value) -> Option<String> {
    let mut text = String::new();
    collect_text(value, &mut text);
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    // Formatting codes are common in pack descriptions and are not readable.
    let cleaned: String = text
        .chars()
        .filter(|ch| *ch != '\u{a7}')
        // Minecraft's font puts icons in the private-use areas; they render as
        // empty boxes outside the game.
        .filter(|ch| !is_private_use(*ch))
        .collect();
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(cleaned.chars().take(240).collect())
}

fn is_private_use(ch: char) -> bool {
    matches!(ch as u32, 0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD)
}

fn collect_text(value: &serde_json::Value, out: &mut String) {
    match value {
        serde_json::Value::String(text) => out.push_str(text),
        serde_json::Value::Array(items) => {
            for item in items {
                collect_text(item, out);
            }
        }
        serde_json::Value::Object(fields) => {
            if let Some(text) = fields.get("text").and_then(|value| value.as_str()) {
                out.push_str(text);
            }
            if let Some(extra) = fields.get("extra") {
                collect_text(extra, out);
            }
        }
        _ => {}
    }
}

/// Format the running game expects for resource packs, used to flag packs
/// built for another version.
pub fn expected_pack_format(game_version: &str) -> Option<u32> {
    (!game_version.trim().is_empty()).then(|| pack_format(game_version))
}

/// Delete one file or unzipped pack folder from a content directory.
pub fn remove(dir: &Path, file: &str) -> Result<(), ContentError> {
    if file.is_empty()
        || file == "."
        || file == ".."
        || file.contains(['/', '\\', ':'])
        || file.starts_with('.')
        || file.chars().any(char::is_control)
    {
        return Err(ContentError::InvalidName(file.to_string()));
    }
    let path = dir.join(file);
    if path.is_dir() {
        fs::remove_dir_all(&path).map_err(|error| io_error(&path, error))
    } else if path.is_file() {
        fs::remove_file(&path).map_err(|error| io_error(&path, error))
    } else {
        Err(ContentError::Io {
            path,
            source: io::Error::new(io::ErrorKind::NotFound, "no such file"),
        })
    }
}

/// Write `png` over the default player skin through a resource pack and switch
/// it on. Offline accounts have no Mojang-signed skin, so this is the only way
/// their own skin shows up in single-player.
pub fn apply_default_skin(
    game_dir: &Path,
    game_version: &str,
    wide_png: &[u8],
    slim_png: &[u8],
) -> Result<PathBuf, ContentError> {
    let dir = game_dir.join("resourcepacks");
    fs::create_dir_all(&dir).map_err(|error| io_error(&dir, error))?;
    let path = dir.join(SKIN_PACK);
    let temporary = tntl_store::scratch_path(&path, "skin");
    let file = fs::File::create(&temporary).map_err(|error| io_error(&temporary, error))?;
    let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(file));
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    use std::io::Write;
    // `supported_formats` only exists from 1.20.2; older versions compare
    // `pack_format` directly, so it has to match the game.
    let meta = format!(
        r#"{{"pack":{{"pack_format":{},"supported_formats":{{"min_inclusive":15,"max_inclusive":99}},"description":"Launcher skin"}}}}"#,
        pack_format(game_version)
    );
    zip.start_file("pack.mcmeta", options)
        .map_err(|error| ContentError::Zip(error.to_string()))?;
    zip.write_all(meta.as_bytes())
        .map_err(|error| io_error(&temporary, error))?;
    // 1.19.3+ picks one of nine default profiles by UUID, so every default
    // skin has to be replaced or the player may still get `makena`. Older
    // versions read the flat steve/alex paths. The arm UVs differ per model, so
    // each path gets the matching layout.
    let mut entries: Vec<(String, &[u8])> = Vec::new();
    for name in ["steve", "alex", "ari", "efe", "kai", "makena", "noor", "sunny", "zuri"] {
        entries.push((format!("entity/player/wide/{name}.png"), wide_png));
    }
    for name in ["steve", "alex", "ari", "efe", "kai", "makena", "noor", "sunny", "zuri"] {
        entries.push((format!("entity/player/slim/{name}.png"), slim_png));
    }
    entries.push(("entity/steve.png".to_string(), wide_png));
    entries.push(("entity/alex.png".to_string(), slim_png));
    for (entry, png) in entries {
        zip.start_file(format!("assets/minecraft/textures/{entry}"), options)
            .map_err(|error| ContentError::Zip(error.to_string()))?;
        zip.write_all(png).map_err(|error| io_error(&temporary, error))?;
    }
    zip.finish().map_err(|error| ContentError::Zip(error.to_string()))?;
    // The same file name may already belong to the user. Only a pack whose
    // recorded hash matches ours is replacable; anything else is backed up
    // first, or it would be gone without a trace.
    let marker = dir.join(SKIN_MARKER);
    let ours = fs::read(&marker)
        .ok()
        .zip(tntl_store::sha1_file(&path).ok())
        .is_some_and(|(recorded, actual)| recorded == actual.as_bytes());
    if path.is_file() && !ours {
        tntl_store::backup_existing(&path).map_err(|error| io_error(&path, error))?;
    }
    fs::rename(&temporary, &path).map_err(|error| io_error(&path, error))?;
    if let Ok(hash) = tntl_store::sha1_file(&path) {
        let _ = atomic_write(&marker, hash.as_bytes());
    }
    set_resource_pack(game_dir, SKIN_PACK, true)?;
    Ok(path)
}

/// Resource pack format for a game version. From 1.20.2 the `supported_formats`
/// range is honoured, so 15 covers everything newer.
fn pack_format(game_version: &str) -> u32 {
    let mut parts = game_version.trim().split('.');
    let (Some(major), Some(minor)) = (
        parts.next().and_then(|part| part.parse::<u32>().ok()),
        parts.next().and_then(|part| part.parse::<u32>().ok()),
    ) else {
        return 15;
    };
    let patch = parts.next().and_then(|part| part.parse::<u32>().ok()).unwrap_or(0);
    match (major, minor, patch) {
        (1, 0..=8, _) => 1,
        (1, 9..=10, _) => 2,
        (1, 11..=12, _) => 3,
        (1, 13..=14, _) => 4,
        (1, 15, _) | (1, 16, 0..=1) => 5,
        (1, 16, _) => 6,
        (1, 17, _) => 7,
        (1, 18, _) => 8,
        (1, 19, 0..=2) => 9,
        (1, 19, 3) => 12,
        (1, 19, _) => 13,
        _ => 15,
    }
}

/// Marker holding the SHA-1 of the skin pack this launcher wrote, so a file the
/// user put there under the same name is never deleted.
const SKIN_MARKER: &str = ".tntl-skin.sha1";

/// Drop the generated skin pack again. A no-op when it was never written, and
/// it never removes a `tntl-skin.zip` this launcher did not create.
pub fn remove_default_skin(game_dir: &Path) -> Result<(), ContentError> {
    let dir = game_dir.join("resourcepacks");
    let path = dir.join(SKIN_PACK);
    let marker = dir.join(SKIN_MARKER);
    let ours = fs::read_to_string(&marker)
        .ok()
        .map(|recorded| recorded.trim().to_string())
        .filter(|recorded| !recorded.is_empty())
        .is_some_and(|recorded| {
            tntl_store::sha1_file(&path).is_ok_and(|actual| actual.eq_ignore_ascii_case(&recorded))
        });
    let enabled = enabled_resource_packs(game_dir)?.iter().any(|name| name == SKIN_PACK);
    if ours && path.exists() {
        fs::remove_file(&path).map_err(|error| io_error(&path, error))?;
    }
    if ours || enabled {
        let _ = fs::remove_file(&marker);
    }
    if enabled {
        set_resource_pack(game_dir, SKIN_PACK, false)?;
    }
    Ok(())
}

/// Copy `path` to `<name>.tntl-bak` the first time we are about to edit it.
pub(crate) fn backup_once(path: &Path) -> Result<(), ContentError> {
    if !path.exists() {
        return Ok(());
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    let backup = path.with_file_name(format!("{name}.tntl-bak"));
    if backup.exists() {
        return Ok(());
    }
    fs::copy(path, &backup).map_err(|error| io_error(&backup, error))?;
    Ok(())
}

fn pack_entries(options: &Options) -> Vec<String> {
    let raw = options.get("resourcePacks").unwrap_or("[]");
    serde_json::from_str(raw).unwrap_or_default()
}

/// Read a UTF-8 text file, replacing invalid bytes instead of failing. One bad
/// byte in `options.txt` must not make the instance unlaunchable.
fn read_text(path: &Path) -> Result<String, ContentError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(String::new()),
        Err(error) => return Err(io_error(path, error)),
    };
    Ok(match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => String::from_utf8_lossy(error.as_bytes()).into_owned(),
    })
}
/// `options.txt`: one `key:value` per line, order preserved.
struct Options {
    lines: Vec<String>,
}

impl Options {
    fn load(path: &Path) -> Result<Self, ContentError> {
        let text = read_text(path)?;
        Ok(Self {
            lines: text.lines().map(ToString::to_string).collect(),
        })
    }

    fn get(&self, key: &str) -> Option<&str> {
        let prefix = format!("{key}:");
        self.lines
            .iter()
            .find_map(|line| line.strip_prefix(&prefix).map(str::trim))
    }

    fn set(&mut self, key: &str, value: &str) {
        let prefix = format!("{key}:");
        let line = format!("{key}:{value}");
        match self.lines.iter().position(|entry| entry.starts_with(&prefix)) {
            Some(index) => self.lines[index] = line,
            None => self.lines.push(line),
        }
    }

    fn save(&self, path: &Path) -> Result<(), ContentError> {
        let mut text = self.lines.join("\n");
        text.push('\n');
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;
        }
        backup_once(path)?;
        atomic_write(path, text.as_bytes())?;
        Ok(())
    }
}

/// `iris.properties`: Java properties, only `key=value` lines are touched.
struct Properties {
    lines: Vec<String>,
}

impl Properties {
    fn load(path: &Path) -> Result<Self, ContentError> {
        let text = read_text(path)?;
        Ok(Self {
            lines: text.lines().map(ToString::to_string).collect(),
        })
    }

    fn get(&self, key: &str) -> Option<&str> {
        let prefix = format!("{key}=");
        self.lines
            .iter()
            .find_map(|line| line.strip_prefix(&prefix).map(str::trim))
    }

    fn set(&mut self, key: &str, value: &str) {
        let prefix = format!("{key}=");
        let line = format!("{key}={value}");
        match self.lines.iter().position(|entry| entry.starts_with(&prefix)) {
            Some(index) => self.lines[index] = line,
            None => self.lines.push(line),
        }
    }

    fn save(&self, path: &Path) -> Result<(), ContentError> {
        let mut text = self.lines.join("\n");
        text.push('\n');
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;
        }
        backup_once(path)?;
        atomic_write(path, text.as_bytes())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tntl-content-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_zip(path: &Path, entries: &[(&str, &str)]) {
        use std::io::Write;
        let file = fs::File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        for (name, body) in entries {
            zip.start_file(*name, options).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }

    fn write_mod_jar(path: &Path, id: &str) {
        write_zip(
            path,
            &[("fabric.mod.json", &format!(r#"{{"id":"{id}","version":"1"}}"#))],
        );
    }

    #[test]
    fn toggles_resource_packs_and_keeps_other_options() {
        let dir = temp_dir("packs");
        fs::write(
            dir.join(OPTIONS_FILE),
            "version:3955\nresourcePacks:[\"vanilla\"]\nfov:0.0\n",
        )
        .unwrap();

        set_resource_pack(&dir, "Better Grass.zip", true).unwrap();
        let options = fs::read_to_string(dir.join(OPTIONS_FILE)).unwrap();
        assert!(options.contains(r#"resourcePacks:["vanilla","file/Better Grass.zip"]"#));
        assert!(options.contains("fov:0.0"));
        assert_eq!(enabled_resource_packs(&dir).unwrap(), ["vanilla", "Better Grass.zip"]);

        set_resource_pack(&dir, "Better Grass.zip", false).unwrap();
        assert_eq!(enabled_resource_packs(&dir).unwrap(), ["vanilla"]);
        let options = fs::read_to_string(dir.join(OPTIONS_FILE)).unwrap();
        assert!(options.contains("version:3955"));
        assert!(options.contains("fov:0.0"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn creates_options_when_the_game_never_ran() {
        let dir = temp_dir("fresh");
        set_resource_pack(&dir, "pack.zip", true).unwrap();
        let options = fs::read_to_string(dir.join(OPTIONS_FILE)).unwrap();
        assert_eq!(options, "resourcePacks:[\"vanilla\",\"file/pack.zip\"]\n");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn shaders_need_iris() {
        let dir = temp_dir("shaders");
        assert!(!shaders_supported(&dir));
        assert!(set_shader(&dir, "bsl.zip", true).is_err());

        fs::create_dir_all(dir.join("config")).unwrap();
        fs::write(dir.join(IRIS_CONFIG), "shaderPack=\nenableShaders=true\n").unwrap();
        assert!(shaders_supported(&dir));
        assert_eq!(active_shader(&dir).unwrap(), None);

        set_shader(&dir, "BSL v8.zip", true).unwrap();
        assert_eq!(active_shader(&dir).unwrap().as_deref(), Some("BSL v8.zip"));
        let properties = fs::read_to_string(dir.join(IRIS_CONFIG)).unwrap();
        assert!(properties.contains("shaderPack=BSL v8.zip"));
        assert!(properties.contains("enableShaders=true"));

        set_shader(&dir, "BSL v8.zip", false).unwrap();
        assert_eq!(active_shader(&dir).unwrap(), None);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn optifine_shaders_use_their_own_file() {
        let dir = temp_dir("optifine");
        fs::create_dir_all(dir.join("mods")).unwrap();
        fs::write(dir.join("mods/random-mod.jar"), b"jar").unwrap();
        assert!(!shaders_supported(&dir));

        fs::write(dir.join("mods/OptiFine_1.20.1_HD_U_I5.jar"), b"jar").unwrap();
        assert!(shaders_supported(&dir));
        assert_eq!(active_shader(&dir).unwrap(), None);

        set_shader(&dir, "ComplementaryReimagined_r5.2.zip", true).unwrap();
        let properties = fs::read_to_string(dir.join(OPTIFINE_CONFIG)).unwrap();
        assert!(properties.contains("shaderPack=ComplementaryReimagined_r5.2.zip"));
        assert_eq!(
            active_shader(&dir).unwrap().as_deref(),
            Some("ComplementaryReimagined_r5.2.zip")
        );

        // Other OptiFine options must survive a toggle.
        fs::write(
            dir.join(OPTIFINE_CONFIG),
            "shadowQuality=1\nshaderPack=ComplementaryReimagined_r5.2.zip\n",
        )
        .unwrap();
        set_shader(&dir, "ComplementaryReimagined_r5.2.zip", false).unwrap();
        let properties = fs::read_to_string(dir.join(OPTIFINE_CONFIG)).unwrap();
        assert!(properties.contains("shaderPack=OFF"));
        assert!(properties.contains("shadowQuality=1"));
        assert_eq!(active_shader(&dir).unwrap(), None);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn iris_wins_when_both_shader_mods_are_installed() {
        let dir = temp_dir("both-shaders");
        fs::create_dir_all(dir.join("config")).unwrap();
        fs::create_dir_all(dir.join("mods")).unwrap();
        fs::write(dir.join(IRIS_CONFIG), "shaderPack=\nenableShaders=true\n").unwrap();
        fs::write(dir.join("mods/OptiFine_1.20.1_HD_U_I5.jar"), b"jar").unwrap();

        set_shader(&dir, "BSL v8.zip", true).unwrap();
        assert!(
            fs::read_to_string(dir.join(IRIS_CONFIG))
                .unwrap()
                .contains("shaderPack=BSL v8.zip")
        );
        assert!(!dir.join(OPTIFINE_CONFIG).exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn oculus_keeps_its_own_config_file() {
        // Oculus is Iris for Forge/NeoForge and reads `config/oculus.properties`;
        // writing iris.properties there would look like the toggle did nothing.
        let dir = temp_dir("oculus");
        fs::create_dir_all(dir.join("mods")).unwrap();
        write_mod_jar(&dir.join("mods/oculus-mc1.20.1-1.8.0.jar"), "oculus");
        assert!(shaders_supported(&dir));

        set_shader(&dir, "ComplementaryReimagined_r5.2.zip", true).unwrap();
        assert!(!dir.join(IRIS_CONFIG).exists());
        let properties = fs::read_to_string(dir.join(OCULUS_CONFIG)).unwrap();
        assert!(properties.contains("shaderPack=ComplementaryReimagined_r5.2.zip"));
        assert!(properties.contains("enableShaders=true"));
        assert_eq!(
            active_shader(&dir).unwrap().as_deref(),
            Some("ComplementaryReimagined_r5.2.zip")
        );

        set_shader(&dir, "ComplementaryReimagined_r5.2.zip", false).unwrap();
        let properties = fs::read_to_string(dir.join(OCULUS_CONFIG)).unwrap();
        assert!(properties.contains("shaderPack=(internal)"));
        assert!(properties.contains("enableShaders=false"));
        assert_eq!(active_shader(&dir).unwrap(), None);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn disabled_iris_shaders_use_the_internal_pack() {
        let dir = temp_dir("iris-off");
        fs::create_dir_all(dir.join("config")).unwrap();
        fs::write(dir.join(IRIS_CONFIG), "shaderPack=BSL v8.zip\nenableShaders=true\n").unwrap();
        set_shader(&dir, "BSL v8.zip", false).unwrap();
        let properties = fs::read_to_string(dir.join(IRIS_CONFIG)).unwrap();
        assert!(properties.contains("shaderPack=(internal)"));
        assert!(properties.contains("enableShaders=false"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn resource_pack_order_can_be_changed() {
        let dir = temp_dir("pack-order");
        fs::write(
            dir.join(OPTIONS_FILE),
            "resourcePacks:[\"vanilla\",\"file/a.zip\",\"file/b.zip\",\"file/c.zip\"]\n",
        )
        .unwrap();

        // "up" raises priority: the entry moves towards the end of options.txt.
        move_resource_pack(&dir, "a.zip", true).unwrap();
        assert_eq!(
            enabled_resource_packs(&dir).unwrap(),
            ["vanilla", "b.zip", "a.zip", "c.zip"]
        );
        move_resource_pack(&dir, "a.zip", true).unwrap();
        assert_eq!(
            enabled_resource_packs(&dir).unwrap(),
            ["vanilla", "b.zip", "c.zip", "a.zip"]
        );
        // Already on top: nothing to do.
        move_resource_pack(&dir, "a.zip", true).unwrap();
        assert_eq!(
            enabled_resource_packs(&dir).unwrap(),
            ["vanilla", "b.zip", "c.zip", "a.zip"]
        );
        move_resource_pack(&dir, "a.zip", false).unwrap();
        assert_eq!(
            enabled_resource_packs(&dir).unwrap(),
            ["vanilla", "b.zip", "a.zip", "c.zip"]
        );
        // Vanilla is the base layer: nothing moves below it.
        move_resource_pack(&dir, "b.zip", false).unwrap();
        assert_eq!(
            enabled_resource_packs(&dir).unwrap(),
            ["vanilla", "b.zip", "a.zip", "c.zip"]
        );
        move_resource_pack(&dir, "b.zip", true).unwrap();
        assert_eq!(
            enabled_resource_packs(&dir).unwrap(),
            ["vanilla", "a.zip", "b.zip", "c.zip"]
        );

        assert!(move_resource_pack(&dir, "missing.zip", true).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn enabling_an_enabled_pack_keeps_its_priority() {
        let dir = temp_dir("pack-idempotent");
        fs::write(
            dir.join(OPTIONS_FILE),
            "resourcePacks:[\"vanilla\",\"file/a.zip\",\"file/b.zip\"]\n",
        )
        .unwrap();
        set_resource_pack(&dir, "a.zip", true).unwrap();
        assert_eq!(enabled_resource_packs(&dir).unwrap(), ["vanilla", "a.zip", "b.zip"]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reads_pack_metadata_and_icon() {
        let dir = temp_dir("pack-meta");
        let path = dir.join("pack.zip");
        write_zip(
            &path,
            &[
                (
                    "pack.mcmeta",
                    r#"{"pack":{"pack_format":15,"supported_formats":[12,20],"description":{"text":"Nice ","extra":[{"text":"\u00a7aGrass"}]}}}"#,
                ),
                ("pack.png", "png"),
            ],
        );
        let meta = pack_meta(&path);
        assert_eq!(meta.description.as_deref(), Some("Nice aGrass"));
        assert_eq!(meta.format, Some(15));
        assert_eq!(meta.min_format, Some(12));
        assert_eq!(meta.max_format, Some(20));
        assert_eq!(pack_icon(&path).as_deref(), Some(b"png".as_slice()));

        // Unzipped packs are folders.
        let folder = dir.join("unpacked");
        fs::create_dir_all(&folder).unwrap();
        fs::write(
            folder.join("pack.mcmeta"),
            r#"{"pack":{"pack_format":34,"description":"Folder pack"}}"#,
        )
        .unwrap();
        fs::write(folder.join("pack.png"), b"icon").unwrap();
        let meta = pack_meta(&folder);
        assert_eq!(meta.description.as_deref(), Some("Folder pack"));
        assert_eq!(meta.format, Some(34));
        assert_eq!(meta.min_format, None);
        assert_eq!(pack_icon(&folder).as_deref(), Some(b"icon".as_slice()));

        // A broken mcmeta must not break the listing.
        let broken = dir.join("broken.zip");
        write_zip(&broken, &[("pack.mcmeta", "not json")]);
        assert_eq!(pack_meta(&broken), PackMeta::default());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn removes_files_and_folders_but_stays_inside_the_directory() {
        let dir = temp_dir("remove");
        fs::write(dir.join("a.zip"), b"zip").unwrap();
        fs::create_dir_all(dir.join("folder/pack")).unwrap();
        fs::write(dir.join("folder/pack.mcmeta"), b"{}").unwrap();
        remove(&dir, "a.zip").unwrap();
        assert!(!dir.join("a.zip").exists());
        remove(&dir, "folder").unwrap();
        assert!(!dir.join("folder").exists());
        assert!(remove(&dir, "../outside.zip").is_err());
        assert!(remove(&dir, "a/b.zip").is_err());
        assert!(remove(&dir, "").is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_directory_lists_nothing() {
        let dir = temp_dir("missing");
        assert!(list(&dir.join("resourcepacks")).unwrap().is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn skin_pack_uses_the_layout_of_each_default_model() {
        use std::io::Read;
        let dir = temp_dir("skin-models");
        apply_default_skin(&dir, "1.20.1", b"wide-layout", b"slim-layout").unwrap();
        let file = fs::File::open(dir.join("resourcepacks").join(SKIN_PACK)).unwrap();
        let mut zip = zip::ZipArchive::new(file).unwrap();
        for (entry, expected) in [
            (
                "assets/minecraft/textures/entity/player/wide/steve.png",
                b"wide-layout".as_slice(),
            ),
            (
                "assets/minecraft/textures/entity/player/wide/alex.png",
                b"wide-layout".as_slice(),
            ),
            (
                "assets/minecraft/textures/entity/player/slim/alex.png",
                b"slim-layout".as_slice(),
            ),
            (
                "assets/minecraft/textures/entity/player/slim/steve.png",
                b"slim-layout".as_slice(),
            ),
            ("assets/minecraft/textures/entity/steve.png", b"wide-layout".as_slice()),
            ("assets/minecraft/textures/entity/alex.png", b"slim-layout".as_slice()),
        ] {
            let mut data = Vec::new();
            zip.by_name(entry).unwrap().read_to_end(&mut data).unwrap();
            assert_eq!(data, expected, "{entry}");
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn skin_pack_format_follows_the_game_version() {
        assert_eq!(pack_format("1.8.9"), 1);
        assert_eq!(pack_format("1.12.2"), 3);
        assert_eq!(pack_format("1.16.1"), 5);
        assert_eq!(pack_format("1.16.5"), 6);
        assert_eq!(pack_format("1.18.2"), 8);
        assert_eq!(pack_format("1.19.2"), 9);
        assert_eq!(pack_format("1.19.3"), 12);
        assert_eq!(pack_format("1.19.4"), 13);
        assert_eq!(pack_format("1.20.1"), 15);
        assert_eq!(pack_format("1.21.4"), 15);
        assert_eq!(pack_format("2.0"), 15);
    }
}
