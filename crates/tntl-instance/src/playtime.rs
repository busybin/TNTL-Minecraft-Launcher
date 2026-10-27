//! Accumulated play time per instance.
//!
//! Every finished launch adds its duration to `playtime.json` in the instance
//! directory. The numbers drive the library card and the editor header.

use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::InstanceError;

pub const PLAY_FILE: &str = "playtime.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Playtime {
    /// Total seconds the game ran for this instance.
    pub seconds: u64,
    /// Finished launches, including ones that ended in a crash.
    pub launches: u64,
    /// Unix timestamp of the last finished launch.
    pub last_played: Option<i64>,
}

impl Playtime {
    pub fn load(instance_dir: &Path) -> Self {
        fs::read(instance_dir.join(PLAY_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Add one finished run and write the file back.
    pub fn record(instance_dir: &Path, duration_ms: u128) -> Result<Self, InstanceError> {
        let mut playtime = Self::load(instance_dir);
        playtime.seconds += (duration_ms / 1000).min(u128::from(u64::MAX)) as u64;
        playtime.launches += 1;
        playtime.last_played = Some(now());
        fs::create_dir_all(instance_dir).map_err(|source| InstanceError::Io {
            path: instance_dir.to_path_buf(),
            source,
        })?;
        let path = instance_dir.join(PLAY_FILE);
        let bytes = serde_json::to_vec_pretty(&playtime)
            .map_err(|source| InstanceError::Invalid(format!("cannot encode play time: {source}")))?;
        tntl_store::atomic_write(&path, &bytes)?;
        Ok(playtime)
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulates_runs() {
        let dir = std::env::temp_dir().join(format!("tntl-playtime-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(Playtime::load(&dir).seconds, 0);

        let first = Playtime::record(&dir, 90_500).unwrap();
        assert_eq!(first.seconds, 90);
        assert_eq!(first.launches, 1);
        assert!(first.last_played.is_some());

        let second = Playtime::record(&dir, 30_000).unwrap();
        assert_eq!(second.seconds, 120);
        assert_eq!(second.launches, 2);
        assert_eq!(Playtime::load(&dir).seconds, 120);
        fs::remove_dir_all(&dir).unwrap();
    }
}
