//! CurseForge modpack archives: `manifest.json` and the `overrides/` tree.
//!
//! The manifest only names project and file ids; the launcher asks the API for
//! each file to learn its name, folder and download link.

use std::fs::{self, File};
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::CurseForgeError;

pub const MANIFEST_FILE: &str = "manifest.json";
const MAX_MANIFEST_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct PackManifest {
    pub name: String,
    pub version: String,
    pub minecraft: PackMinecraft,
    pub files: Vec<ManifestFile>,
    /// Name of the override tree; every archive seen so far uses `overrides`.
    pub overrides: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct PackMinecraft {
    pub version: String,
    #[serde(rename = "modLoaders")]
    pub mod_loaders: Vec<PackModLoader>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct PackModLoader {
    /// `forge-47.4.28`, `fabric-0.16.0`, `neoforge-21.1.264`, …
    pub id: String,
    pub primary: bool,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ManifestFile {
    #[serde(rename = "projectID")]
    pub project_id: u64,
    #[serde(rename = "fileID")]
    pub file_id: u64,
    pub required: bool,
}

impl PackManifest {
    pub fn game_version(&self) -> Option<&str> {
        (!self.minecraft.version.trim().is_empty()).then_some(self.minecraft.version.trim())
    }

    /// Loader key and build from the primary entry, falling back to the first
    /// one. The key is what `tntl_source::LoaderKind::from_key` understands.
    pub fn loader(&self) -> Option<(String, String)> {
        let entry = self
            .minecraft
            .mod_loaders
            .iter()
            .find(|entry| entry.primary)
            .or_else(|| self.minecraft.mod_loaders.first())?;
        let (kind, version) = entry.id.split_once('-')?;
        let kind = kind.trim().to_ascii_lowercase();
        let version = version.trim();
        if version.is_empty() {
            return None;
        }
        Some((kind, version.to_string()))
    }

    pub fn display_name(&self) -> String {
        let name = self.name.trim();
        if name.is_empty() {
            "CurseForge modpack".to_string()
        } else {
            name.to_string()
        }
    }

    /// Directory the override tree is stored under.
    pub fn overrides_dir(&self) -> &str {
        let name = self.overrides.trim().trim_end_matches('/');
        if name.is_empty() { "overrides" } else { name }
    }
}

/// Read `manifest.json` from a CurseForge modpack zip.
pub fn open_manifest(archive: &Path) -> Result<PackManifest, CurseForgeError> {
    let file = File::open(archive).map_err(|source| CurseForgeError::Io {
        path: archive.to_path_buf(),
        source,
    })?;
    let mut zip = zip::ZipArchive::new(file).map_err(|error| CurseForgeError::Zip(error.to_string()))?;
    let mut entry = zip
        .by_name(MANIFEST_FILE)
        .map_err(|_| CurseForgeError::Invalid(format!("{} has no {MANIFEST_FILE}", archive.display())))?;
    if entry.size() > MAX_MANIFEST_BYTES {
        return Err(CurseForgeError::Invalid(format!("{MANIFEST_FILE} is too large")));
    }
    let mut bytes = Vec::new();
    io::Read::read_to_end(&mut entry, &mut bytes).map_err(|source| CurseForgeError::Io {
        path: archive.to_path_buf(),
        source,
    })?;
    let manifest: PackManifest = serde_json::from_slice(&bytes).map_err(|source| CurseForgeError::Json {
        url: archive.display().to_string(),
        source,
    })?;
    if manifest.game_version().is_none() {
        return Err(CurseForgeError::Invalid(
            "the pack manifest does not name a Minecraft version".to_string(),
        ));
    }
    Ok(manifest)
}

/// Copy the override tree into `game_dir` and return how many files were
/// written. Later entries win over downloaded files, matching the `.mrpack`
/// installer.
pub fn extract_overrides(
    archive: &Path,
    manifest: &PackManifest,
    game_dir: &Path,
    progress: &mut dyn FnMut(&str, usize),
) -> Result<usize, CurseForgeError> {
    let head = format!("{}/", manifest.overrides_dir());
    let file = File::open(archive).map_err(|source| CurseForgeError::Io {
        path: archive.to_path_buf(),
        source,
    })?;
    let mut zip = zip::ZipArchive::new(file).map_err(|error| CurseForgeError::Zip(error.to_string()))?;
    fs::create_dir_all(game_dir).map_err(|source| CurseForgeError::Io {
        path: game_dir.to_path_buf(),
        source,
    })?;
    let mut written = 0;
    let mut budget = tntl_store::EXTRACT_BUDGET;
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|error| CurseForgeError::Zip(error.to_string()))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        let Some(stripped) = name.strip_prefix(&head) else {
            continue;
        };
        let relative = tntl_store::safe_relative(stripped)
            .ok_or_else(|| CurseForgeError::Invalid(format!("unsafe archive entry: {name}")))?;
        let target = tntl_store::target_respecting_disabled(&game_dir.join(&relative));
        if !target.starts_with(game_dir) {
            return Err(CurseForgeError::Invalid(format!("unsafe archive entry: {name}")));
        }
        // Worlds are the player's, not the pack's.
        let first = relative
            .components()
            .next()
            .map(|part| part.as_os_str().to_string_lossy().to_ascii_lowercase());
        if first.as_deref() == Some("saves") {
            eprintln!("skipping pack override into saves/: {name}");
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|source| CurseForgeError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        // Keep whatever is already there instead of truncating it.
        if let Err(source) = tntl_store::backup_existing(&target) {
            return Err(CurseForgeError::Io { path: target, source });
        }
        let mut out = File::create(&target).map_err(|source| CurseForgeError::Io {
            path: target.clone(),
            source,
        })?;
        tntl_store::copy_entry(&mut entry, &mut out, &mut budget).map_err(|source| CurseForgeError::Io {
            path: target.clone(),
            source,
        })?;
        written += 1;
    }
    progress(manifest.overrides_dir(), written);
    Ok(written)
}

/// Game directory a CurseForge project class belongs in. `None` means the
/// launcher cannot place that kind of file.
pub fn content_dir(class_id: u32) -> Option<&'static str> {
    match class_id {
        6 => Some("mods"),
        12 => Some("resourcepacks"),
        6552 => Some("shaderpacks"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn archive(dir: &Path, manifest: &str) -> std::path::PathBuf {
        fs::create_dir_all(dir).unwrap();
        let path = dir.join("pack.zip");
        let file = File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file(MANIFEST_FILE, options).unwrap();
        zip.write_all(manifest.as_bytes()).unwrap();
        zip.start_file("overrides/config/a.txt", options).unwrap();
        zip.write_all(b"override").unwrap();
        zip.start_file("overrides/mods/x.jar.disabled", options).unwrap();
        zip.write_all(b"jar").unwrap();
        zip.finish().unwrap();
        path
    }

    const MANIFEST: &str = r#"{
        "minecraft": {
            "version": "1.20.1",
            "modLoaders": [
                {"id": "forge-47.3.0", "primary": false},
                {"id": "forge-47.4.28", "primary": true}
            ]
        },
        "name": "All the Mods 9",
        "version": "1.0.2",
        "files": [{"projectID": 238222, "fileID": 4567, "required": true}],
        "overrides": "overrides"
    }"#;

    #[test]
    fn parses_the_manifest_and_loaders() {
        let dir = std::env::temp_dir().join(format!("tntl-cf-pack-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = archive(&dir, MANIFEST);
        let manifest = open_manifest(&path).unwrap();
        assert_eq!(manifest.game_version(), Some("1.20.1"));
        assert_eq!(manifest.display_name(), "All the Mods 9");
        assert_eq!(manifest.loader(), Some(("forge".to_string(), "47.4.28".to_string())));
        assert_eq!(manifest.files[0].project_id, 238222);
        assert_eq!(manifest.overrides_dir(), "overrides");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn extracts_overrides_respecting_disabled_files() {
        let dir = std::env::temp_dir().join(format!("tntl-cf-overrides-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = archive(&dir, MANIFEST);
        let manifest = open_manifest(&path).unwrap();
        let game = dir.join("game");
        let mut seen = Vec::new();
        let count = extract_overrides(&path, &manifest, &game, &mut |prefix, entries| {
            seen.push((prefix.to_string(), entries));
        })
        .unwrap();
        assert_eq!(count, 2);
        assert_eq!(seen, [("overrides".to_string(), 2)]);
        assert_eq!(fs::read_to_string(game.join("config/a.txt")).unwrap(), "override");
        assert!(game.join("mods/x.jar.disabled").is_file());
        assert!(!game.join("mods/x.jar").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn maps_project_classes_to_folders() {
        assert_eq!(content_dir(6), Some("mods"));
        assert_eq!(content_dir(12), Some("resourcepacks"));
        assert_eq!(content_dir(6552), Some("shaderpacks"));
        assert_eq!(content_dir(17), None);
        assert!(crate::valid_file_name("create-1.20.1.jar"));
        assert!(!crate::valid_file_name("../escape.jar"));
        assert!(!crate::valid_file_name("a/b.jar"));
    }
}
