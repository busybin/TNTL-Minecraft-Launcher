//! Screenshots written by the game into `screenshots/`.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum ScreenshotError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid screenshot name: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Screenshot {
    pub file: String,
    pub bytes: u64,
    pub modified_ms: Option<i64>,
}

pub fn screenshots_dir(game_dir: &Path) -> PathBuf {
    game_dir.join("screenshots")
}

/// Screenshots, newest first. Missing directory means none.
pub fn list(game_dir: &Path) -> Result<Vec<Screenshot>, ScreenshotError> {
    let dir = screenshots_dir(game_dir);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(ScreenshotError::Io {
                path: dir,
                source: error,
            });
        }
    };
    let mut shots = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.to_ascii_lowercase().ends_with(".png") {
            continue;
        }
        let meta = entry.metadata().ok();
        shots.push(Screenshot {
            file: name,
            bytes: meta.as_ref().map(|meta| meta.len()).unwrap_or_default(),
            modified_ms: meta
                .and_then(|meta| meta.modified().ok())
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_millis() as i64),
        });
    }
    shots.sort_by_key(|shot| std::cmp::Reverse(shot.modified_ms));
    Ok(shots)
}

fn checked_path(game_dir: &Path, file: &str) -> Result<PathBuf, ScreenshotError> {
    if file.is_empty() || file.contains('/') || file.contains('\\') || file == ".." {
        return Err(ScreenshotError::Invalid(file.to_string()));
    }
    Ok(screenshots_dir(game_dir).join(file))
}

/// Read one PNG. The shell turns the bytes into a data URL.
pub fn read(game_dir: &Path, file: &str) -> Result<Vec<u8>, ScreenshotError> {
    let path = checked_path(game_dir, file)?;
    let size = fs::metadata(&path)
        .map_err(|error| ScreenshotError::Io {
            path: path.clone(),
            source: error,
        })?
        .len();
    if size > MAX_IMAGE_BYTES {
        return Err(ScreenshotError::Invalid(format!("{file} is too large")));
    }
    fs::read(&path).map_err(|error| ScreenshotError::Io { path, source: error })
}

pub fn delete(game_dir: &Path, file: &str) -> Result<(), ScreenshotError> {
    let path = checked_path(game_dir, file)?;
    fs::remove_file(&path).map_err(|error| ScreenshotError::Io { path, source: error })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_reads_and_deletes() {
        let dir = std::env::temp_dir().join(format!("tntl-shots-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("screenshots")).unwrap();
        fs::write(dir.join("screenshots/a.png"), b"png").unwrap();
        let shots = list(&dir).unwrap();
        assert_eq!(shots.len(), 1);
        assert_eq!(read(&dir, "a.png").unwrap(), b"png");
        assert!(read(&dir, "../a.png").is_err());
        delete(&dir, "a.png").unwrap();
        assert!(list(&dir).unwrap().is_empty());
        fs::remove_dir_all(dir).unwrap();
    }
}
