//! Small crash-aware file transaction for destructive install/update steps.
//!
//! The pattern is: move the old file into a transaction folder, apply the new
//! state, then either commit (delete the folder) or roll back (move the old
//! file back). A journal is written after every move so a launcher crash
//! between two steps can be recovered on the next start.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{StoreError, atomic_write_json};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Entry {
    Moved { original: PathBuf, backup: PathBuf },
    Created { path: PathBuf },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Journal {
    entries: Vec<Entry>,
}

pub struct FileTransaction {
    root: PathBuf,
    journal_path: PathBuf,
    entries: Vec<Entry>,
    finished: bool,
}

impl FileTransaction {
    /// `root` must be a new, empty directory on the same filesystem as the
    /// files it will move when possible.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(|source| StoreError::Io {
            path: root.clone(),
            source,
        })?;
        let journal_path = root.join("journal.json");
        let transaction = Self {
            root,
            journal_path,
            entries: Vec::new(),
            finished: false,
        };
        transaction.save_journal()?;
        Ok(transaction)
    }

    /// Move an existing file into the transaction folder.
    pub fn trash(&mut self, path: &Path) -> Result<bool, StoreError> {
        if !path.exists() {
            return Ok(false);
        }
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_string());
        let backup = self.root.join(format!("{}-{name}", self.entries.len()));
        move_file(path, &backup)?;
        self.entries.push(Entry::Moved {
            original: path.to_path_buf(),
            backup,
        });
        self.save_journal()?;
        Ok(true)
    }

    /// Remember a file produced by the new state; rollback removes it.
    pub fn track_created(&mut self, path: impl Into<PathBuf>) -> Result<(), StoreError> {
        self.entries.push(Entry::Created { path: path.into() });
        self.save_journal()
    }

    pub fn commit(mut self) -> Result<(), StoreError> {
        self.finished = true;
        fs::remove_dir_all(&self.root).map_err(|source| StoreError::Io {
            path: self.root.clone(),
            source,
        })
    }

    pub fn rollback(mut self) -> Result<(), StoreError> {
        self.restore()?;
        self.finished = true;
        Ok(())
    }

    fn restore(&mut self) -> Result<(), StoreError> {
        // New files go first, then the old ones come back in reverse order.
        for entry in self.entries.iter().rev() {
            if let Entry::Created { path } = entry {
                match fs::remove_file(path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(source) => {
                        return Err(StoreError::Io {
                            path: path.clone(),
                            source,
                        });
                    }
                }
            }
        }
        for entry in self.entries.iter().rev() {
            if let Entry::Moved { original, backup } = entry {
                if !backup.exists() {
                    continue;
                }
                if original.exists() {
                    let _ = fs::remove_file(original);
                }
                move_file(backup, original)?;
            }
        }
        if self.root.exists() {
            fs::remove_dir_all(&self.root).map_err(|source| StoreError::Io {
                path: self.root.clone(),
                source,
            })?;
        }
        Ok(())
    }

    fn save_journal(&self) -> Result<(), StoreError> {
        atomic_write_json(
            &self.journal_path,
            &Journal {
                entries: self.entries.clone(),
            },
        )
    }
}

impl Drop for FileTransaction {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.restore();
        }
    }
}

/// Restore a transaction that did not reach commit (launcher crash, power
/// loss). Called for every journal found under `root`.
pub fn recover(root: &Path) -> Vec<String> {
    let mut problems = Vec::new();
    let Ok(entries) = fs::read_dir(root) else {
        return problems;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let journal_path = path.join("journal.json");
        let Ok(bytes) = fs::read(&journal_path) else {
            continue;
        };
        let Ok(journal): Result<Journal, _> = serde_json::from_slice(&bytes) else {
            problems.push(format!("{} has an unreadable journal", path.display()));
            continue;
        };
        let mut transaction = FileTransaction {
            root: path.clone(),
            journal_path: journal_path.clone(),
            entries: journal.entries,
            finished: false,
        };
        if let Err(error) = transaction.restore() {
            problems.push(format!("{}: {error}", path.display()));
        }
    }
    problems
}

fn move_file(from: &Path, to: &Path) -> Result<(), StoreError> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).map_err(|source| StoreError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => {
            fs::copy(from, to).map_err(|source| StoreError::Io {
                path: to.to_path_buf(),
                source,
            })?;
            fs::remove_file(from).map_err(|source| StoreError::Io {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn dir(tag: &str) -> PathBuf {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("tntl-txn-{tag}-{stamp}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn rollback_restores_the_old_file_and_removes_the_new_one() {
        let base = dir("rollback");
        let old = base.join("mods/a.jar");
        fs::create_dir_all(old.parent().unwrap()).unwrap();
        fs::write(&old, b"old").unwrap();
        let mut transaction = FileTransaction::new(base.join("txn")).unwrap();
        transaction.trash(&old).unwrap();
        assert!(!old.exists());
        let new = base.join("mods/b.jar");
        fs::write(&new, b"new").unwrap();
        transaction.track_created(&new).unwrap();
        transaction.rollback().unwrap();
        assert_eq!(fs::read(&old).unwrap(), b"old");
        assert!(!new.exists());
        assert!(!base.join("txn").exists());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn commit_keeps_the_new_state() {
        let base = dir("commit");
        let old = base.join("a.jar");
        fs::write(&old, b"old").unwrap();
        let mut transaction = FileTransaction::new(base.join("txn")).unwrap();
        transaction.trash(&old).unwrap();
        fs::write(base.join("b.jar"), b"new").unwrap();
        transaction.commit().unwrap();
        assert!(!old.exists());
        assert!(!base.join("txn").exists());
        assert_eq!(fs::read(base.join("b.jar")).unwrap(), b"new");
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn recover_restores_a_journal_left_behind() {
        let base = dir("recover");
        let old = base.join("a.jar");
        fs::write(&old, b"old").unwrap();
        let txn_root = base.join("txn");
        {
            let mut transaction = FileTransaction::new(txn_root.join("one")).unwrap();
            transaction.trash(&old).unwrap();
            std::mem::forget(transaction);
        }
        assert!(!old.exists());
        let problems = recover(&txn_root);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(fs::read(&old).unwrap(), b"old");
        fs::remove_dir_all(base).unwrap();
    }
}
