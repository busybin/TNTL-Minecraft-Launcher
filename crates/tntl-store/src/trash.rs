//! A small per-instance recycle bin. Deleting content moves it here instead
//! of unlinking it, so a wrong click can be undone until the user empties it.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::{StoreError, atomic_write_json, valid_file_name};

/// One recycled file or directory, as shown by the instance's delete history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrashEntry {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub bytes: u64,
    pub deleted_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Record {
    kind: String,
    name: String,
    original: PathBuf,
    bytes: u64,
    deleted_ms: u64,
}

/// `<instance>/.trash`, holding one directory per deleted item.
pub struct Trash {
    root: PathBuf,
}

impl Trash {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Move `path` into the bin. The caller keeps ownership of `kind` and
    /// `name`; both are only labels for the history view.
    pub fn put(&self, path: &Path, kind: &str, name: &str) -> Result<TrashEntry, StoreError> {
        if !path.exists() {
            return Err(StoreError::Io {
                path: path.to_path_buf(),
                source: std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"),
            });
        }
        fs::create_dir_all(&self.root).map_err(|source| StoreError::Io {
            path: self.root.clone(),
            source,
        })?;
        let deleted_ms = unix_millis();
        let id = self.unique_id(deleted_ms);
        let dir = self.root.join(&id);
        fs::create_dir(&dir).map_err(|source| StoreError::Io {
            path: dir.clone(),
            source,
        })?;
        let payload = dir.join("payload");
        if let Err(error) = move_path(path, &payload) {
            let _ = fs::remove_dir_all(&dir);
            return Err(error);
        }
        let bytes = size_of(&payload).unwrap_or(0);
        let record = Record {
            kind: kind.to_string(),
            name: name.to_string(),
            original: path.to_path_buf(),
            bytes,
            deleted_ms,
        };
        if let Err(error) = atomic_write_json(&dir.join("entry.json"), &record) {
            let _ = move_path(&payload, path);
            let _ = fs::remove_dir_all(&dir);
            return Err(error);
        }
        Ok(TrashEntry {
            id,
            kind: record.kind,
            name: record.name,
            bytes,
            deleted_ms,
        })
    }

    /// Newest first. An unreadable entry is ignored rather than hiding the
    /// rest of the history.
    pub fn list(&self) -> Vec<TrashEntry> {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut list = Vec::new();
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let Ok(bytes) = fs::read(entry.path().join("entry.json")) else {
                continue;
            };
            let Ok(record) = serde_json::from_slice::<Record>(&bytes) else {
                continue;
            };
            list.push(TrashEntry {
                id: entry.file_name().to_string_lossy().to_string(),
                kind: record.kind,
                name: record.name,
                bytes: record.bytes,
                deleted_ms: record.deleted_ms,
            });
        }
        list.sort_by_key(|entry| std::cmp::Reverse(entry.deleted_ms));
        list
    }

    /// Put an item back where it came from. `allowed` bounds the restore to
    /// the instance and its game directory, so a tampered manifest cannot
    /// write outside them.
    pub fn restore(&self, id: &str, allowed: &[PathBuf]) -> Result<PathBuf, StoreError> {
        let (dir, record) = self.load(id)?;
        let original = record.original.clone();
        if !restore_allowed(&original, allowed) {
            return Err(StoreError::Io {
                path: original,
                source: std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "restore target is outside the instance",
                ),
            });
        }
        if original.exists() {
            return Err(StoreError::Io {
                path: original.clone(),
                source: std::io::Error::new(std::io::ErrorKind::AlreadyExists, "the original path is occupied"),
            });
        }
        move_path(&dir.join("payload"), &original)?;
        let _ = fs::remove_dir_all(&dir);
        Ok(original)
    }

    pub fn purge(&self, id: &str) -> Result<(), StoreError> {
        let (dir, _) = self.load(id)?;
        fs::remove_dir_all(&dir).map_err(|source| StoreError::Io { path: dir, source })
    }

    pub fn empty(&self) -> Result<usize, StoreError> {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return Ok(0);
        };
        let mut removed = 0;
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            fs::remove_dir_all(entry.path()).map_err(|source| StoreError::Io {
                path: entry.path(),
                source,
            })?;
            removed += 1;
        }
        Ok(removed)
    }

    fn load(&self, id: &str) -> Result<(PathBuf, Record), StoreError> {
        if !valid_file_name(id) || id.starts_with('.') {
            return Err(StoreError::Io {
                path: self.root.join(id),
                source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid trash id"),
            });
        }
        let dir = self.root.join(id);
        let bytes = fs::read(dir.join("entry.json")).map_err(|source| StoreError::Io {
            path: dir.join("entry.json"),
            source,
        })?;
        let record = serde_json::from_slice(&bytes).map_err(|source| StoreError::Io {
            path: dir.join("entry.json"),
            source: std::io::Error::new(std::io::ErrorKind::InvalidData, source),
        })?;
        Ok((dir, record))
    }

    fn unique_id(&self, millis: u64) -> String {
        let mut attempt = 0u32;
        loop {
            let id = format!("{millis:x}-{attempt:x}");
            if !self.root.join(&id).exists() {
                return id;
            }
            attempt += 1;
        }
    }
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

fn size_of(path: &Path) -> std::io::Result<u64> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Ok(0);
    }
    if meta.is_file() {
        return Ok(meta.len());
    }
    let mut total = 0;
    for entry in fs::read_dir(path)? {
        total += size_of(&entry?.path())?;
    }
    Ok(total)
}

/// A recorded path is only restorable when its parent resolves inside one of
/// the allowed roots. A lexical check is not enough: `..` components and
/// symlinked folders would otherwise let a tampered entry write outside the
/// instance.
fn restore_allowed(original: &Path, allowed: &[PathBuf]) -> bool {
    if original
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return false;
    }
    let Some(parent) = original.parent() else {
        return false;
    };
    let Ok(parent) = parent.canonicalize() else {
        return false;
    };
    allowed.iter().any(|root| {
        root.canonicalize()
            .map(|root| parent.starts_with(root))
            .unwrap_or(false)
    })
}

fn move_path(from: &Path, to: &Path) -> Result<(), StoreError> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).map_err(|source| StoreError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => {
            copy_tree(from, to)?;
            remove_tree(from).map_err(|source| StoreError::Io {
                path: from.to_path_buf(),
                source,
            })
        }
        Err(source) => Err(StoreError::Io {
            path: from.to_path_buf(),
            source,
        }),
    }
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), StoreError> {
    let meta = fs::symlink_metadata(from).map_err(|source| StoreError::Io {
        path: from.to_path_buf(),
        source,
    })?;
    if meta.file_type().is_symlink() {
        return Err(StoreError::Io {
            path: from.to_path_buf(),
            source: std::io::Error::other("refusing to follow a link into the recycle bin"),
        });
    }
    if meta.is_file() {
        fs::copy(from, to).map_err(|source| StoreError::Io {
            path: to.to_path_buf(),
            source,
        })?;
        return Ok(());
    }
    fs::create_dir_all(to).map_err(|source| StoreError::Io {
        path: to.to_path_buf(),
        source,
    })?;
    for entry in fs::read_dir(from).map_err(|source| StoreError::Io {
        path: from.to_path_buf(),
        source,
    })? {
        let entry = entry.map_err(|source| StoreError::Io {
            path: from.to_path_buf(),
            source,
        })?;
        copy_tree(&entry.path(), &to.join(entry.file_name()))?;
    }
    Ok(())
}

fn remove_tree(path: &Path) -> std::io::Result<()> {
    if fs::symlink_metadata(path)?.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("tntl-trash-{tag}-{stamp}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn put_list_restore_and_purge() {
        let base = dir("roundtrip");
        let file = base.join("mods/a.jar");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, b"jar").unwrap();
        let trash = Trash::new(base.join(".trash"));
        let entry = trash.put(&file, "mod", "A").unwrap();
        assert!(!file.exists());
        assert_eq!(trash.list().len(), 1);
        assert_eq!(trash.list()[0].bytes, 3);
        let restored = trash.restore(&entry.id, std::slice::from_ref(&base)).unwrap();
        assert_eq!(restored, file);
        assert_eq!(fs::read(&file).unwrap(), b"jar");
        assert!(trash.list().is_empty());

        let entry = trash.put(&file, "mod", "A").unwrap();
        trash.purge(&entry.id).unwrap();
        assert!(trash.list().is_empty());
        assert!(!file.exists());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn restore_refuses_a_path_outside_the_instance() {
        let base = dir("escape");
        let file = base.join("a.txt");
        fs::write(&file, b"x").unwrap();
        let trash = Trash::new(base.join(".trash"));
        let entry = trash.put(&file, "world", "A").unwrap();
        let other = dir("other");
        assert!(trash.restore(&entry.id, std::slice::from_ref(&other)).is_err());
        assert!(base.join(".trash").join(&entry.id).exists());
        fs::remove_dir_all(base).unwrap();
        fs::remove_dir_all(other).unwrap();
    }

    #[test]
    fn restore_refuses_parent_components() {
        let base = dir("dotdot");
        let victim = base
            .parent()
            .unwrap()
            .join(format!("tntl-trash-victim-{}", unix_millis()));
        fs::write(&victim, b"x").unwrap();
        let trash = Trash::new(base.join(".trash"));
        let recorded = base.join("..").join(victim.file_name().unwrap());
        let entry = trash.put(&recorded, "world", "W").unwrap();
        assert!(trash.restore(&entry.id, std::slice::from_ref(&base)).is_err());
        fs::remove_dir_all(base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn restore_refuses_a_symlinked_parent() {
        let base = dir("symlink");
        let outside = dir("symlink-out");
        std::os::unix::fs::symlink(&outside, base.join("link")).unwrap();
        let file = base.join("link").join("a.txt");
        fs::write(&file, b"x").unwrap();
        let trash = Trash::new(base.join(".trash"));
        let entry = trash.put(&file, "world", "A").unwrap();
        assert!(trash.restore(&entry.id, std::slice::from_ref(&base)).is_err());
        assert!(!file.exists());
        fs::remove_dir_all(base).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn empty_removes_every_entry() {
        let base = dir("empty");
        fs::write(base.join("a"), b"a").unwrap();
        fs::write(base.join("b"), b"b").unwrap();
        let trash = Trash::new(base.join(".trash"));
        trash.put(&base.join("a"), "mod", "A").unwrap();
        trash.put(&base.join("b"), "mod", "B").unwrap();
        assert_eq!(trash.empty().unwrap(), 2);
        assert!(trash.list().is_empty());
        fs::remove_dir_all(base).unwrap();
    }
}
