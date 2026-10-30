//! Crash reports written by the game into `crash-reports/`.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_REPORT_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum CrashError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid report name: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrashReport {
    pub file: String,
    pub bytes: u64,
    pub modified_ms: Option<i64>,
}

pub fn reports_dir(game_dir: &Path) -> PathBuf {
    game_dir.join("crash-reports")
}

/// Reports, newest first. Missing directory means none.
pub fn list(game_dir: &Path) -> Result<Vec<CrashReport>, CrashError> {
    let dir = reports_dir(game_dir);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(CrashError::Io {
                path: dir,
                source: error,
            });
        }
    };
    let mut reports = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if !(name.ends_with(".txt") || name.ends_with(".log")) {
            continue;
        }
        let meta = entry.metadata().ok();
        reports.push(CrashReport {
            file: name,
            bytes: meta.as_ref().map(|meta| meta.len()).unwrap_or_default(),
            modified_ms: meta
                .and_then(|meta| meta.modified().ok())
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_millis() as i64),
        });
    }
    reports.sort_by_key(|report| std::cmp::Reverse(report.modified_ms));
    Ok(reports)
}

/// Read one report. Only a file name is accepted, never a path.
pub fn read(game_dir: &Path, file: &str) -> Result<String, CrashError> {
    if !tntl_store::valid_file_name(file) {
        return Err(CrashError::Invalid(file.to_string()));
    }
    let path = reports_dir(game_dir).join(file);
    let mut bytes = Vec::new();
    fs::File::open(&path)
        .map_err(|error| CrashError::Io {
            path: path.clone(),
            source: error,
        })?
        .take(MAX_REPORT_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|error| CrashError::Io {
            path: path.clone(),
            source: error,
        })?;
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

/// Delete one report.
pub fn delete(game_dir: &Path, file: &str) -> Result<(), CrashError> {
    if !tntl_store::valid_file_name(file) {
        return Err(CrashError::Invalid(file.to_string()));
    }
    let path = reports_dir(game_dir).join(file);
    fs::remove_file(&path).map_err(|error| CrashError::Io { path, source: error })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_and_reads_reports() {
        let dir = std::env::temp_dir().join(format!("tntl-crash-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("crash-reports")).unwrap();
        fs::write(dir.join("crash-reports/crash-2026-01-01.txt"), "boom").unwrap();
        let reports = list(&dir).unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].file, "crash-2026-01-01.txt");
        assert_eq!(read(&dir, &reports[0].file).unwrap(), "boom");
        assert!(read(&dir, "../instance.json").is_err());
        delete(&dir, &reports[0].file).unwrap();
        assert!(list(&dir).unwrap().is_empty());
        fs::remove_dir_all(dir).unwrap();
    }
}
