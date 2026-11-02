//! Small crash-aware file transaction for destructive install/update steps.
//!
//! The pattern is: record the move, move the old file into a transaction
//! folder, apply the new state, then either commit (delete the folder) or roll
//! back (move the old file back). The journal is written before every move so
//! a crash between two steps can be recovered on the next start.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

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
    #[serde(default)]
    committed: bool,
}

impl Entry {
    /// Journal entries are data, not commands: only paths inside the instance
    /// (or the transaction folder) may be replayed. A journal planted by an
    /// imported archive must not delete or move files elsewhere.
    fn contained(&self, root: &Path, allowed: &[PathBuf]) -> bool {
        match self {
            Entry::Moved { original, backup } => {
                within(original, allowed) && within(backup, std::slice::from_ref(&root.to_path_buf()))
            }
            Entry::Created { path } => within(path, allowed),
        }
    }
}

pub struct FileTransaction {
    root: PathBuf,
    journal_path: PathBuf,
    entries: Vec<Entry>,
    committed: bool,
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
            committed: false,
            finished: false,
        };
        transaction.save_journal()?;
        Ok(transaction)
    }

    /// Move an existing file into the transaction folder. The journal records
    /// the move before it happens, so a crash or a failed journal write never
    /// loses the file: recovery sees a missing backup and leaves the original
    /// alone.
    pub fn trash(&mut self, path: &Path) -> Result<bool, StoreError> {
        if !path.exists() {
            return Ok(false);
        }
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_string());
        let backup = self.root.join(format!("{}-{name}", self.entries.len()));
        self.entries.push(Entry::Moved {
            original: path.to_path_buf(),
            backup: backup.clone(),
        });
        if let Err(error) = self.save_journal() {
            self.entries.pop();
            return Err(error);
        }
        if let Err(error) = move_file(path, &backup) {
            self.entries.pop();
            let _ = self.save_journal();
            return Err(error);
        }
        Ok(true)
    }

    /// Back up an existing file and mark its replacement as new. Use this
    /// before overwriting a file so a failed update restores the old bytes.
    pub fn prepare(&mut self, path: &Path) -> Result<(), StoreError> {
        self.trash(path)?;
        self.track_created(path.to_path_buf())
    }

    /// Remember a file produced by the new state; rollback removes it.
    pub fn track_created(&mut self, path: impl Into<PathBuf>) -> Result<(), StoreError> {
        self.entries.push(Entry::Created { path: path.into() });
        self.save_journal()
    }

    pub fn commit(mut self) -> Result<(), StoreError> {
        // Mark the journal committed before deleting the backups. A crash
        // between the two must not make recovery restore files that the new
        // state already replaced.
        self.committed = true;
        self.save_journal()?;
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
        // Do not delete the whole folder: a failed journal write can leave a
        // backup that no entry points at, and that file is the only copy.
        let _ = fs::remove_file(&self.journal_path);
        let _ = fs::remove_dir(&self.root);
        Ok(())
    }

    fn save_journal(&self) -> Result<(), StoreError> {
        atomic_write_json(
            &self.journal_path,
            &Journal {
                entries: self.entries.clone(),
                committed: self.committed,
            },
        )
    }
}

/// A `FileTransaction` shared with download workers. Every write goes through
/// `prepare`, so the whole pack update can be rolled back as one unit.
pub struct SharedTransaction {
    inner: Mutex<FileTransaction>,
}

impl SharedTransaction {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, StoreError> {
        Ok(Self {
            inner: Mutex::new(FileTransaction::new(root)?),
        })
    }

    fn lock(&self) -> MutexGuard<'_, FileTransaction> {
        self.inner.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    /// Back up the file at `path` and register its replacement.
    pub fn prepare(&self, path: &Path) -> Result<(), StoreError> {
        self.lock().prepare(path)
    }

    pub fn trash(&self, path: &Path) -> Result<bool, StoreError> {
        self.lock().trash(path)
    }

    pub fn track_created(&self, path: impl Into<PathBuf>) -> Result<(), StoreError> {
        self.lock().track_created(path)
    }

    pub fn commit(self) -> Result<(), StoreError> {
        let transaction = self.inner.into_inner().unwrap_or_else(|poison| poison.into_inner());
        transaction.commit()
    }

    pub fn rollback(self) -> Result<(), StoreError> {
        let transaction = self.inner.into_inner().unwrap_or_else(|poison| poison.into_inner());
        transaction.rollback()
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
/// loss). Called for every journal found under `root`. `allowed` bounds the
/// paths a journal may touch (the instance and its game directory); a journal
/// with an entry outside them is left alone and reported.
pub fn recover(root: &Path, allowed: &[PathBuf]) -> Vec<String> {
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
        if journal.committed {
            let _ = fs::remove_dir_all(&path);
            continue;
        }
        if let Some(outside) = journal.entries.iter().find(|entry| !entry.contained(&path, allowed)) {
            problems.push(format!(
                "{} points outside the instance and was not replayed: {outside:?}",
                path.display()
            ));
            continue;
        }
        let mut transaction = FileTransaction {
            root: path.clone(),
            journal_path: journal_path.clone(),
            entries: journal.entries,
            committed: false,
            finished: false,
        };
        if let Err(error) = transaction.restore() {
            problems.push(format!("{}: {error}", path.display()));
        }
    }
    problems
}

/// A path may be replayed when it has no `..` and its parent resolves inside
/// one of the roots. Checking the parent (not the file) also covers files that
/// the transaction removed already.
fn within(path: &Path, roots: &[PathBuf]) -> bool {
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return false;
    }
    let Some(parent) = path.parent() else {
        return false;
    };
    let Ok(parent) = parent.canonicalize() else {
        return false;
    };
    roots.iter().any(|root| {
        root.canonicalize()
            .map(|root| parent.starts_with(root))
            .unwrap_or(false)
    })
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
            // Copy to a sibling first: a crash mid-copy must not leave a
            // partial file at the path recovery will move back.
            let partial = to.with_file_name(format!(
                "{}.partial",
                to.file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| "copy".to_string())
            ));
            let _ = fs::remove_file(&partial);
            if let Err(source) = fs::copy(from, &partial) {
                let _ = fs::remove_file(&partial);
                return Err(StoreError::Io {
                    path: to.to_path_buf(),
                    source,
                });
            }
            if let Err(source) = fs::rename(&partial, to) {
                let _ = fs::remove_file(&partial);
                return Err(StoreError::Io {
                    path: to.to_path_buf(),
                    source,
                });
            }
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
    fn prepare_restores_an_overwritten_file_on_rollback() {
        let base = dir("prepare");
        let path = base.join("config/a.txt");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"old").unwrap();
        let transaction = SharedTransaction::new(base.join("txn")).unwrap();
        transaction.prepare(&path).unwrap();
        fs::write(&path, b"new").unwrap();
        transaction.rollback().unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"old");
        assert!(!base.join("txn").exists());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn prepare_removes_a_new_file_on_rollback() {
        let base = dir("prepare-new");
        let path = base.join("mods/new.jar");
        let transaction = SharedTransaction::new(base.join("txn")).unwrap();
        transaction.prepare(&path).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"new").unwrap();
        transaction.rollback().unwrap();
        assert!(!path.exists());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn prepare_keeps_the_new_file_on_commit() {
        let base = dir("prepare-commit");
        let path = base.join("a.txt");
        fs::write(&path, b"old").unwrap();
        let transaction = SharedTransaction::new(base.join("txn")).unwrap();
        transaction.prepare(&path).unwrap();
        fs::write(&path, b"new").unwrap();
        transaction.commit().unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
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
    fn a_committed_journal_is_not_rolled_back() {
        let base = dir("committed");
        let old = base.join("a.jar");
        fs::write(&old, b"old").unwrap();
        let txn_root = base.join("txn");
        {
            let mut transaction = FileTransaction::new(txn_root.join("one")).unwrap();
            transaction.trash(&old).unwrap();
            transaction.committed = true;
            transaction.save_journal().unwrap();
            std::mem::forget(transaction);
        }
        let problems = recover(&txn_root, std::slice::from_ref(&base));
        assert!(problems.is_empty(), "{problems:?}");
        assert!(!old.exists(), "a committed transaction must stay committed");
        assert!(!txn_root.join("one").exists());
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
        let problems = recover(&txn_root, std::slice::from_ref(&base));
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(fs::read(&old).unwrap(), b"old");
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn recover_refuses_a_journal_that_points_outside() {
        let base = dir("outside-journal");
        let outside = dir("outside-target");
        let file = outside.join("a.txt");
        fs::write(&file, b"x").unwrap();
        let txn_root = base.join("txn");
        {
            let mut transaction = FileTransaction::new(txn_root.join("one")).unwrap();
            transaction.track_created(&file).unwrap();
            std::mem::forget(transaction);
        }
        let problems = recover(&txn_root, std::slice::from_ref(&base));
        assert!(!problems.is_empty(), "an outside entry must be reported");
        assert!(file.exists(), "an outside file must not be removed");
        assert!(txn_root.join("one").exists(), "the journal is kept for inspection");
        fs::remove_dir_all(base).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }
}
