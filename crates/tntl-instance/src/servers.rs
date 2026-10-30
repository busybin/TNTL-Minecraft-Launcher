//! `servers.dat`: the multiplayer server list kept inside the game directory.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::nbt::{self, NbtError, Tag};

#[derive(Debug, Error)]
pub enum ServerError {
    #[error(transparent)]
    Nbt(#[from] NbtError),
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerEntry {
    pub name: String,
    pub ip: String,
    #[serde(default = "default_true")]
    pub accept_textures: bool,
    /// Row in the existing file, so unknown fields such as the icon survive.
    /// `None` for a server that is being added.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<usize>,
}

fn default_true() -> bool {
    true
}

pub fn servers_file(game_dir: &Path) -> PathBuf {
    game_dir.join("servers.dat")
}

/// Servers in the order the game shows them. A missing file is an empty list.
pub fn list(game_dir: &Path) -> Result<Vec<ServerEntry>, ServerError> {
    let path = servers_file(game_dir);
    let root = match nbt::read_file(&path) {
        Ok(root) => root,
        Err(NbtError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let Some(Tag::List(entries)) = root.get("servers") else {
        return Ok(Vec::new());
    };
    let mut servers = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let Tag::Compound(_) = entry else {
            continue;
        };
        servers.push(ServerEntry {
            name: entry
                .get("name")
                .and_then(Tag::as_str)
                .unwrap_or("Minecraft Server")
                .to_string(),
            ip: entry.get("ip").and_then(Tag::as_str).unwrap_or_default().to_string(),
            accept_textures: entry.get("acceptTextures").and_then(Tag::as_bool).unwrap_or(true),
            index: Some(index),
        });
    }
    Ok(servers)
}

/// Replace the list with `entries`, keeping unknown fields of rows that already
/// exist. Rows without an index are appended.
pub fn save(game_dir: &Path, entries: &[ServerEntry]) -> Result<(), ServerError> {
    let path = servers_file(game_dir);
    let mut root = match nbt::read_file(&path) {
        Ok(root) => root,
        Err(NbtError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => Tag::Compound(Vec::new()),
        // Never overwrite a file that exists but cannot be decoded: the user's
        // server list would be gone for good.
        Err(error) => return Err(error.into()),
    };
    if !matches!(root, Tag::Compound(_)) {
        root = Tag::Compound(Vec::new());
    }
    let existing = match root.get("servers") {
        Some(Tag::List(rows)) => rows.clone(),
        _ => Vec::new(),
    };
    let mut rows = Vec::with_capacity(entries.len());
    for entry in entries {
        let mut row = match entry.index.and_then(|index| existing.get(index)) {
            Some(Tag::Compound(fields)) => Tag::Compound(fields.clone()),
            _ => Tag::Compound(Vec::new()),
        };
        row.set("name", Tag::String(entry.name.clone()));
        row.set("ip", Tag::String(entry.ip.clone()));
        row.set("acceptTextures", Tag::Byte(i8::from(entry.accept_textures)));
        rows.push(row);
    }
    root.set("servers", Tag::List(rows));
    fs::create_dir_all(game_dir).map_err(|error| ServerError::Io {
        path: game_dir.to_path_buf(),
        source: error,
    })?;
    // First edit of the user's list keeps a copy; the NBT rewrite drops
    // unknown-shaped rows it cannot parse back.
    crate::content::backup_once(&path).map_err(|error| ServerError::Io {
        path: path.clone(),
        source: std::io::Error::other(error.to_string()),
    })?;
    nbt::write_file(&path, &root)?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tntl-servers-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_file_is_empty() {
        let dir = temp_dir("empty");
        assert!(list(&dir).unwrap().is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn round_trips_and_keeps_unknown_fields() {
        let dir = temp_dir("round");
        let root = Tag::Compound(vec![(
            "servers".to_string(),
            Tag::List(vec![
                Tag::Compound(vec![
                    ("name".to_string(), Tag::String("One".to_string())),
                    ("ip".to_string(), Tag::String("one.example.com".to_string())),
                    ("icon".to_string(), Tag::String("keep-me".to_string())),
                    ("acceptTextures".to_string(), Tag::Byte(0)),
                ]),
                Tag::Compound(vec![
                    ("name".to_string(), Tag::String("Two".to_string())),
                    ("ip".to_string(), Tag::String("two.example.com:25566".to_string())),
                ]),
            ]),
        )]);
        nbt::write_file(&servers_file(&dir), &root).unwrap();

        let mut entries = list(&dir).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "One");
        assert!(!entries[0].accept_textures);
        assert_eq!(entries[1].ip, "two.example.com:25566");

        entries[0].name = "Renamed".to_string();
        entries.remove(1);
        entries.push(ServerEntry {
            name: "New".to_string(),
            ip: "new.example.com".to_string(),
            accept_textures: true,
            index: None,
        });
        save(&dir, &entries).unwrap();

        let reread = list(&dir).unwrap();
        assert_eq!(reread.len(), 2);
        assert_eq!(reread[0].name, "Renamed");
        assert_eq!(reread[1].name, "New");
        let raw = nbt::read_file(&servers_file(&dir)).unwrap();
        let Some(Tag::List(rows)) = raw.get("servers") else {
            panic!("no server list");
        };
        assert_eq!(rows[0].get("icon").and_then(Tag::as_str), Some("keep-me"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn keeps_names_with_supplementary_characters() {
        let dir = temp_dir("emoji");
        let entries = vec![ServerEntry {
            name: "Fun \u{1f600}".to_string(),
            ip: "mc.example.com".to_string(),
            accept_textures: true,
            index: None,
        }];
        save(&dir, &entries).unwrap();
        let reread = list(&dir).unwrap();
        assert_eq!(reread[0].name, "Fun \u{1f600}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn refuses_to_overwrite_a_broken_file() {
        let dir = temp_dir("broken");
        fs::write(servers_file(&dir), b"not nbt").unwrap();
        let entries = vec![ServerEntry {
            name: "New".to_string(),
            ip: "new.example.com".to_string(),
            accept_textures: true,
            index: None,
        }];
        assert!(save(&dir, &entries).is_err());
        assert_eq!(fs::read(servers_file(&dir)).unwrap(), b"not nbt");
        fs::remove_dir_all(dir).unwrap();
    }
}
