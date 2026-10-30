//! World datapacks: the zips and folders under `saves/<world>/datapacks`.
//!
//! Switching a pack off moves it to `<game_dir>/datapacks-disabled/<world>/`
//! instead of touching `level.dat`. The game then simply does not see it, and
//! moving it back is a rename.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DatapackError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("unknown world {0}")]
    World(String),
    #[error("invalid datapack name: {0}")]
    Name(String),
    #[error("{name} is not installed in {world}")]
    Missing { name: String, world: String },
    #[error("{name} is already installed in {world}")]
    Exists { name: String, world: String },
}

fn io_error(path: &Path, source: io::Error) -> DatapackError {
    DatapackError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatapackInfo {
    /// File or folder name inside `datapacks/`.
    pub name: String,
    pub bytes: u64,
    pub enabled: bool,
    /// A folder pack rather than a zip.
    pub folder: bool,
}

fn saves_dir(game_dir: &Path) -> PathBuf {
    game_dir.join("saves")
}

fn active_dir(game_dir: &Path, world: &str) -> PathBuf {
    saves_dir(game_dir).join(world).join("datapacks")
}

fn disabled_dir(game_dir: &Path, world: &str) -> PathBuf {
    game_dir.join("datapacks-disabled").join(world)
}

fn check_world(game_dir: &Path, world: &str) -> Result<(), DatapackError> {
    let valid = !world.is_empty()
        && world != "."
        && world != ".."
        && !world.contains('/')
        && !world.contains('\\')
        && !world.contains(':')
        && !world.chars().any(char::is_control)
        && saves_dir(game_dir).join(world).is_dir();
    if valid {
        Ok(())
    } else {
        Err(DatapackError::World(world.to_string()))
    }
}

fn check_name(name: &str) -> Result<(), DatapackError> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains(':')
        || name.starts_with('.')
        || name.chars().any(char::is_control)
    {
        return Err(DatapackError::Name(name.to_string()));
    }
    Ok(())
}

fn dir_size(path: &Path) -> u64 {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return 0;
    };
    if meta.is_file() {
        return meta.len();
    }
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries.flatten().map(|entry| dir_size(&entry.path())).sum()
}

fn entry_info(path: &Path, enabled: bool) -> Option<DatapackInfo> {
    let meta = fs::symlink_metadata(path).ok()?;
    let name = path.file_name()?.to_string_lossy().to_string();
    let folder = meta.is_dir();
    if !folder && !name.to_ascii_lowercase().ends_with(".zip") {
        return None;
    }
    Some(DatapackInfo {
        bytes: dir_size(path),
        folder,
        enabled,
        name,
    })
}

fn scan(dir: &Path, enabled: bool) -> Result<Vec<DatapackInfo>, DatapackError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(io_error(dir, error)),
    };
    let mut packs: Vec<DatapackInfo> = entries
        .flatten()
        .filter_map(|entry| entry_info(&entry.path(), enabled))
        .collect();
    packs.sort_by_key(|pack| pack.name.to_lowercase());
    Ok(packs)
}

/// Enabled and disabled packs of one world, enabled first.
pub fn list(game_dir: &Path, world: &str) -> Result<Vec<DatapackInfo>, DatapackError> {
    check_world(game_dir, world)?;
    let mut packs = scan(&active_dir(game_dir, world), true)?;
    packs.extend(scan(&disabled_dir(game_dir, world), false)?);
    Ok(packs)
}

/// Move one pack between the active and the disabled store.
pub fn set_enabled(game_dir: &Path, world: &str, name: &str, enabled: bool) -> Result<(), DatapackError> {
    check_world(game_dir, world)?;
    check_name(name)?;
    let (from, to) = if enabled {
        (
            disabled_dir(game_dir, world).join(name),
            active_dir(game_dir, world).join(name),
        )
    } else {
        (
            active_dir(game_dir, world).join(name),
            disabled_dir(game_dir, world).join(name),
        )
    };
    if !from.exists() {
        return Err(DatapackError::Missing {
            name: name.to_string(),
            world: world.to_string(),
        });
    }
    if to.exists() {
        return Err(DatapackError::Exists {
            name: name.to_string(),
            world: world.to_string(),
        });
    }
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;
    }
    fs::rename(&from, &to).map_err(|error| io_error(&from, error))
}

/// Disabled packs are stored per world; drop the folder when the world goes.
pub fn remove_world(game_dir: &Path, world: &str) -> Result<(), DatapackError> {
    check_world(game_dir, world)?;
    let dir = disabled_dir(game_dir, world);
    match fs::remove_dir_all(&dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(&dir, error)),
    }
}

/// Copy a zip (or folder) into the world. Returns the stored name, which may
/// carry a numeric suffix when the world already has a pack of that name.
pub fn add(game_dir: &Path, world: &str, source: &Path) -> Result<DatapackInfo, DatapackError> {
    check_world(game_dir, world)?;
    let name = source
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_default();
    check_name(&name)?;
    let dir = active_dir(game_dir, world);
    fs::create_dir_all(&dir).map_err(|error| io_error(&dir, error))?;
    let disabled = disabled_dir(game_dir, world);
    let taken =
        |candidate: &Path| candidate.exists() || candidate.file_name().is_some_and(|name| disabled.join(name).exists());
    let mut target = dir.join(&name);
    if taken(&target) {
        let (stem, extension) = match name.rsplit_once('.') {
            Some((stem, extension)) => (stem.to_string(), format!(".{extension}")),
            None => (name.clone(), String::new()),
        };
        let mut index = 2;
        loop {
            let candidate = dir.join(format!("{stem} ({index}){extension}"));
            if !taken(&candidate) {
                target = candidate;
                break;
            }
            index += 1;
        }
    }
    // Validate before copying: a non-zip file would otherwise land in the
    // world's datapacks directory and then be reported as a failure.
    if !source.is_dir() && !name.to_ascii_lowercase().ends_with(".zip") {
        return Err(DatapackError::Name(name));
    }
    if source.is_dir() {
        copy_dir(source, &target)?;
    } else {
        fs::copy(source, &target).map_err(|error| io_error(source, error))?;
    }
    entry_info(&target, true).ok_or_else(|| {
        if target.is_dir() {
            let _ = fs::remove_dir_all(&target);
        } else {
            let _ = fs::remove_file(&target);
        }
        DatapackError::Name(name)
    })
}

fn copy_dir(source: &Path, target: &Path) -> Result<(), DatapackError> {
    fs::create_dir_all(target).map_err(|error| io_error(target, error))?;
    for entry in fs::read_dir(source).map_err(|error| io_error(source, error))?.flatten() {
        let path = entry.path();
        let meta = fs::symlink_metadata(&path).map_err(|error| io_error(&path, error))?;
        // Following links would copy files from outside the pack, and a link
        // that points at its own parent recursed until the stack ran out.
        if meta.file_type().is_symlink() {
            continue;
        }
        let destination = target.join(entry.file_name());
        if meta.is_dir() {
            copy_dir(&path, &destination)?;
        } else {
            fs::copy(&path, &destination).map_err(|error| io_error(&path, error))?;
        }
    }
    Ok(())
}

/// Remove a pack whether it is enabled or not.
pub fn delete(game_dir: &Path, world: &str, name: &str) -> Result<(), DatapackError> {
    check_world(game_dir, world)?;
    check_name(name)?;
    let mut removed = false;
    // Older builds could leave a copy in both stores; remove both.
    for path in [
        active_dir(game_dir, world).join(name),
        disabled_dir(game_dir, world).join(name),
    ] {
        if !path.exists() {
            continue;
        }
        if path.is_dir() {
            fs::remove_dir_all(&path).map_err(|error| io_error(&path, error))?;
        } else {
            fs::remove_file(&path).map_err(|error| io_error(&path, error))?;
        }
        removed = true;
    }
    if removed {
        Ok(())
    } else {
        Err(DatapackError::Missing {
            name: name.to_string(),
            world: world.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world(tag: &str) -> PathBuf {
        let game_dir = std::env::temp_dir().join(format!("tntl-datapacks-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&game_dir);
        fs::create_dir_all(saves_dir(&game_dir).join("demo")).unwrap();
        game_dir
    }

    #[test]
    fn enable_disable_round_trip() {
        let game_dir = world("round-trip");
        let pack = game_dir.join("demo.zip");
        fs::write(&pack, b"zip").unwrap();
        let added = add(&game_dir, "demo", &pack).unwrap();
        assert!(added.enabled);
        set_enabled(&game_dir, "demo", "demo.zip", false).unwrap();
        let packs = list(&game_dir, "demo").unwrap();
        assert_eq!(packs.len(), 1);
        assert!(!packs[0].enabled);
        set_enabled(&game_dir, "demo", "demo.zip", true).unwrap();
        assert!(list(&game_dir, "demo").unwrap()[0].enabled);
    }

    #[test]
    fn duplicate_names_get_a_suffix() {
        let game_dir = world("duplicate");
        let pack = game_dir.join("pack.zip");
        fs::write(&pack, b"zip").unwrap();
        add(&game_dir, "demo", &pack).unwrap();
        assert_eq!(add(&game_dir, "demo", &pack).unwrap().name, "pack (2).zip");
    }

    #[test]
    fn does_not_shadow_a_disabled_copy() {
        let game_dir = world("shadow");
        let pack = game_dir.join("pack.zip");
        fs::write(&pack, b"zip").unwrap();
        add(&game_dir, "demo", &pack).unwrap();
        set_enabled(&game_dir, "demo", "pack.zip", false).unwrap();
        assert_eq!(add(&game_dir, "demo", &pack).unwrap().name, "pack (2).zip");
        let packs = list(&game_dir, "demo").unwrap();
        assert_eq!(packs.len(), 2);
        assert_eq!(packs.iter().filter(|pack| pack.enabled).count(), 1);
    }

    #[test]
    fn rejects_paths_outside_the_world() {
        let game_dir = world("traversal");
        assert!(list(&game_dir, "../demo").is_err());
        assert!(delete(&game_dir, "demo", "../pack.zip").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn folder_packs_do_not_follow_symlinks() {
        // A link inside the pack pointed at its own parent: copying it
        // recursed until the stack ran out, and links to the home directory
        // copied files the pack never contained.
        let game_dir = world("symlinks");
        let folder = game_dir.join("Pack");
        fs::create_dir_all(folder.join("data")).unwrap();
        fs::write(folder.join("data/a.txt"), b"a").unwrap();
        fs::write(game_dir.join("secret.txt"), b"secret").unwrap();
        std::os::unix::fs::symlink(&game_dir, folder.join("escape")).unwrap();
        std::os::unix::fs::symlink(&folder, folder.join("data/loop")).unwrap();
        let added = add(&game_dir, "demo", &folder).unwrap();
        assert!(added.folder);
        let stored = game_dir.join("saves/demo/datapacks/Pack");
        assert!(stored.join("data/a.txt").is_file());
        assert!(!stored.join("escape/secret.txt").exists());
        assert!(!stored.join("data/loop").exists());
    }
}
