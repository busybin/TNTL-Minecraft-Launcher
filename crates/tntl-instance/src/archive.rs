//! Whole-instance zip archives.
//!
//! Export keeps everything a user needs to move an instance to another
//! machine: `instance.json`, the game directory, mods, configs and saves.
//! Regenerable or launcher-local data (`natives/`, `logs/`, `crash-reports/`,
//! world backups) is skipped.
//!
//! Instances imported from another launcher point their game directory outside
//! the launcher root. Those files are archived under `minecraft/` and the
//! absolute paths are cleared, so the archive stays portable.

use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use zip::write::SimpleFileOptions;

use crate::{INSTANCE_FILE, Instance};

const SKIP: &[&str] = &["natives", "logs", "crash-reports", "backups"];
const GAME_PREFIX: &str = "minecraft";

#[derive(Debug, Error)]
pub enum ArchiveError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("zip error: {0}")]
    Zip(String),
    #[error("{0} is not an instance directory")]
    NotAnInstance(PathBuf),
    #[error("archive does not contain {INSTANCE_FILE}")]
    NoInstanceFile,
    #[error("archive entry escapes the instance directory: {0}")]
    UnsafeEntry(String),
    #[error("invalid instance name: {0}")]
    InvalidName(String),
    #[error("invalid instance: {0}")]
    Instance(String),
    #[error("invalid {INSTANCE_FILE}: {0}")]
    Json(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveReport {
    pub path: PathBuf,
    pub files: usize,
    pub bytes: u64,
}

fn io_err(path: &Path, source: io::Error) -> ArchiveError {
    ArchiveError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn skipped(relative: &Path) -> bool {
    relative.components().next().is_some_and(|part| match part {
        Component::Normal(name) => name.to_str().is_some_and(|name| SKIP.contains(&name)),
        _ => false,
    })
}

fn slash(path: &Path) -> String {
    path.components()
        .filter_map(|part| match part {
            Component::Normal(name) => Some(name.to_string_lossy().to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn add_file(
    zip: &mut zip::ZipWriter<BufWriter<File>>,
    path: &Path,
    name: String,
    options: SimpleFileOptions,
    sanitize: bool,
    files: &mut usize,
    bytes: &mut u64,
) -> Result<(), ArchiveError> {
    zip.start_file(name, options)
        .map_err(|error| ArchiveError::Zip(error.to_string()))?;
    if sanitize {
        let raw = fs::read(path).map_err(|error| io_err(path, error))?;
        let mut value: serde_json::Value =
            serde_json::from_slice(&raw).map_err(|error| ArchiveError::Json(error.to_string()))?;
        if let Some(object) = value.as_object_mut() {
            // Absolute paths are machine-specific; a restored instance uses
            // its own directory.
            object.insert("root".to_string(), serde_json::Value::Null);
            object.insert("game_dir".to_string(), serde_json::Value::Null);
        }
        let written = serde_json::to_vec_pretty(&value).map_err(|error| ArchiveError::Json(error.to_string()))?;
        zip.write_all(&written).map_err(|error| io_err(path, error))?;
        *bytes += written.len() as u64;
    } else {
        let mut input = File::open(path).map_err(|error| io_err(path, error))?;
        *bytes += io::copy(&mut input, zip).map_err(|error| io_err(path, error))?;
    }
    *files += 1;
    Ok(())
}

fn add_tree(
    zip: &mut zip::ZipWriter<BufWriter<File>>,
    base: &Path,
    prefix: &str,
    skip: Option<&Path>,
    options: SimpleFileOptions,
    files: &mut usize,
    bytes: &mut u64,
) -> Result<(), ArchiveError> {
    let mut stack = vec![base.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = fs::read_dir(&dir).map_err(|error| io_err(&dir, error))?;
        for entry in entries.flatten() {
            let path = entry.path();
            let meta = fs::symlink_metadata(&path).map_err(|error| io_err(&path, error))?;
            if meta.file_type().is_symlink() {
                continue;
            }
            let relative = path
                .strip_prefix(base)
                .map_err(|_| ArchiveError::UnsafeEntry(path.display().to_string()))?;
            if meta.is_dir() {
                if !skipped(relative) && skip != Some(path.as_path()) {
                    stack.push(path);
                }
                continue;
            }
            let name = if prefix.is_empty() {
                slash(relative)
            } else {
                format!("{prefix}/{}", slash(relative))
            };
            let sanitize = prefix.is_empty() && relative == Path::new(INSTANCE_FILE);
            add_file(zip, &path, name, options, sanitize, files, bytes)?;
        }
    }
    Ok(())
}

/// What a zip holds, so a dropped file can be routed without guessing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveKind {
    Instance,
    World,
    /// `.mrpack` (Modrinth index).
    Modpack,
    ResourcePack,
    Unknown,
}

pub fn kind(path: &Path) -> Result<ArchiveKind, ArchiveError> {
    let file = File::open(path).map_err(|error| io_err(path, error))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|error| ArchiveError::Zip(error.to_string()))?;
    let mut names = Vec::with_capacity(zip.len());
    for index in 0..zip.len() {
        let entry = zip
            .by_index(index)
            .map_err(|error| ArchiveError::Zip(error.to_string()))?;
        names.push(entry.name().to_string());
    }
    let has = |needle: &str| {
        names
            .iter()
            .any(|name| name == needle || name.ends_with(&format!("/{needle}")))
    };
    Ok(if has(INSTANCE_FILE) {
        ArchiveKind::Instance
    } else if has("modrinth.index.json") {
        ArchiveKind::Modpack
    } else if has("level.dat") {
        ArchiveKind::World
    } else if has("pack.mcmeta") {
        ArchiveKind::ResourcePack
    } else {
        ArchiveKind::Unknown
    })
}

/// Write `instance_dir` as a zip archive at `target`.
pub fn export(instance_dir: &Path, target: &Path) -> Result<ArchiveReport, ArchiveError> {
    if !instance_dir.join(INSTANCE_FILE).is_file() {
        return Err(ArchiveError::NotAnInstance(instance_dir.to_path_buf()));
    }
    let instance = Instance::load(instance_dir).map_err(|error| ArchiveError::Instance(error.to_string()))?;
    let game_dir = instance.game_dir(instance_dir);
    // The game directory is archived as `minecraft/`; if it lives inside the
    // instance, skip exactly that subtree during the first walk (not its first
    // path component, which would also drop sibling files).
    let skip = game_dir
        .strip_prefix(instance_dir)
        .ok()
        .filter(|relative| !relative.as_os_str().is_empty())
        .map(|_| game_dir.as_path());

    let temporary = target.with_extension("zip.part");
    let file = File::create(&temporary).map_err(|error| io_err(&temporary, error))?;
    let mut zip = zip::ZipWriter::new(BufWriter::new(file));
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    let mut files = 0usize;
    let mut bytes = 0u64;
    add_tree(&mut zip, instance_dir, "", skip, options, &mut files, &mut bytes)?;
    if game_dir != instance_dir && game_dir.is_dir() {
        add_tree(&mut zip, &game_dir, GAME_PREFIX, None, options, &mut files, &mut bytes)?;
    }
    zip.finish().map_err(|error| ArchiveError::Zip(error.to_string()))?;
    fs::rename(&temporary, target).map_err(|error| io_err(target, error))?;
    Ok(ArchiveReport {
        path: target.to_path_buf(),
        files,
        bytes,
    })
}

/// Extract an archive into `instances_dir/name` and return the new directory.
pub fn import(archive: &Path, instances_dir: &Path, name: &str) -> Result<PathBuf, ArchiveError> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\']) {
        return Err(ArchiveError::InvalidName(name.to_string()));
    }
    let file = File::open(archive).map_err(|error| io_err(archive, error))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|error| ArchiveError::Zip(error.to_string()))?;

    let mut prefix: Option<String> = None;
    for index in 0..zip.len() {
        let entry = zip
            .by_index(index)
            .map_err(|error| ArchiveError::Zip(error.to_string()))?;
        let entry_name = entry.name().trim_end_matches('/').to_string();
        if entry_name == INSTANCE_FILE {
            prefix = Some(String::new());
            break;
        }
        if let Some(dir) = entry_name.strip_suffix(&format!("/{INSTANCE_FILE}")) {
            if dir.contains('/') {
                return Err(ArchiveError::Zip(format!("{INSTANCE_FILE} is nested too deep")));
            }
            prefix = Some(format!("{dir}/"));
            break;
        }
    }
    let prefix = prefix.ok_or(ArchiveError::NoInstanceFile)?;

    let target = instances_dir.join(name);
    if target.exists() {
        return Err(ArchiveError::Io {
            path: target,
            source: io::Error::new(io::ErrorKind::AlreadyExists, "instance directory already exists"),
        });
    }
    fs::create_dir_all(&target).map_err(|error| io_err(&target, error))?;

    // A directory that only contains a broken `instance.json` is invisible in
    // the library and cannot be deleted from the UI, so validate before
    // reporting success and drop it on any error.
    let result = extract(&mut zip, &prefix, &target)
        .and_then(|()| Instance::load(&target).map_err(|error| ArchiveError::Instance(error.to_string())));
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&target);
        return Err(error);
    }
    Ok(target)
}

fn extract(zip: &mut zip::ZipArchive<File>, prefix: &str, target: &Path) -> Result<(), ArchiveError> {
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|error| ArchiveError::Zip(error.to_string()))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        let Some(relative) = name.strip_prefix(prefix) else {
            continue;
        };
        let path = PathBuf::from(relative);
        if path.as_os_str().is_empty() || path.components().any(|part| !matches!(part, Component::Normal(_))) {
            return Err(ArchiveError::UnsafeEntry(name));
        }
        let destination = target.join(&path);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|error| io_err(parent, error))?;
        }
        let mut output = File::create(&destination).map_err(|error| io_err(&destination, error))?;
        io::copy(&mut entry, &mut output).map_err(|error| io_err(&destination, error))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tntl-archive-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn zip_entry(path: &Path, name: &str) -> String {
        let mut zip = zip::ZipArchive::new(File::open(path).unwrap()).unwrap();
        let mut file = zip.by_name(name).unwrap();
        let mut text = String::new();
        file.read_to_string(&mut text).unwrap();
        text
    }

    fn zip_names(path: &Path) -> Vec<String> {
        let mut zip = zip::ZipArchive::new(File::open(path).unwrap()).unwrap();
        (0..zip.len())
            .map(|index| zip.by_index(index).unwrap().name().to_string())
            .collect()
    }

    #[test]
    fn round_trips_an_instance() {
        let dir = temp_dir("round");
        let source = dir.join("instances/Test");
        fs::create_dir_all(source.join("minecraft/saves/world")).unwrap();
        fs::create_dir_all(source.join("minecraft/logs")).unwrap();
        fs::create_dir_all(source.join("natives")).unwrap();
        fs::create_dir_all(source.join("logs")).unwrap();
        fs::write(source.join("instance.json"), r#"{"name":"Test","version_id":"1.20.1"}"#).unwrap();
        fs::write(source.join("minecraft/saves/world/level.dat"), b"nbt").unwrap();
        fs::write(source.join("minecraft/logs/latest.log"), b"noise").unwrap();
        fs::write(source.join("logs/latest.log"), b"noise").unwrap();
        fs::write(source.join("natives/lib.so"), b"so").unwrap();

        let archive = dir.join("Test.zip");
        let report = export(&source, &archive).unwrap();
        assert_eq!(report.files, 2);
        let names = zip_names(&archive);
        assert!(names.contains(&"minecraft/saves/world/level.dat".to_string()));
        assert!(!names.iter().any(|name| name.contains("logs")));
        assert!(!names.iter().any(|name| name.contains("natives")));

        let target = dir.join("imported");
        fs::create_dir_all(&target).unwrap();
        let imported = import(&archive, &target, "Test").unwrap();
        assert!(imported.join("instance.json").is_file());
        assert!(imported.join("minecraft/saves/world/level.dat").is_file());
        assert!(!imported.join("logs").exists());
        assert!(!imported.join("natives").exists());

        assert!(import(&archive, &target, "Test").is_err());
        assert!(import(&archive, &target, "../escape").is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn archives_a_game_directory_outside_the_instance() {
        let dir = temp_dir("external");
        let game = dir.join("external-game");
        fs::create_dir_all(game.join("saves/world")).unwrap();
        fs::write(game.join("saves/world/level.dat"), b"nbt").unwrap();
        fs::create_dir_all(game.join("logs")).unwrap();
        fs::write(game.join("logs/latest.log"), b"noise").unwrap();
        let source = dir.join("instances/Imported");
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join("instance.json"),
            serde_json::json!({
                "name": "Imported",
                "version_id": "1.20.1",
                "root": game.display().to_string(),
                "game_dir": game.display().to_string(),
            })
            .to_string(),
        )
        .unwrap();

        let archive = dir.join("Imported.zip");
        let report = export(&source, &archive).unwrap();
        assert_eq!(report.files, 2);
        let names = zip_names(&archive);
        assert!(names.contains(&"minecraft/saves/world/level.dat".to_string()));
        assert!(!names.iter().any(|name| name.contains("logs")));
        let instance = zip_entry(&archive, "instance.json");
        assert!(instance.contains("\"game_dir\": null"), "{instance}");
        assert!(instance.contains("\"root\": null"), "{instance}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn keeps_a_single_top_level_directory() {
        let dir = temp_dir("nested");
        let archive = dir.join("Pack.zip");
        let file = File::create(&archive).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        zip.start_file("Pack/instance.json", options).unwrap();
        zip.write_all(br#"{"name":"Pack","version_id":"1.20.1"}"#).unwrap();
        zip.start_file("Pack/minecraft/options.txt", options).unwrap();
        zip.write_all(b"lang:en_us\n").unwrap();
        zip.finish().unwrap();

        let target = dir.join("imported");
        fs::create_dir_all(&target).unwrap();
        let imported = import(&archive, &target, "Pack").unwrap();
        assert!(imported.join("instance.json").is_file());
        assert!(imported.join("minecraft/options.txt").is_file());
        fs::remove_dir_all(dir).unwrap();
    }
}
