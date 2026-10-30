//! Instances: a named version plus the settings that make it launch.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tntl_store::{RootLayout, StoreError};

pub mod archive;
pub mod content;
pub mod crashes;
pub mod datapacks;
pub mod mods;
pub mod nbt;
pub mod playtime;
pub mod screenshots;
pub mod servers;
pub mod worlds;

pub const INSTANCE_FILE: &str = "instance.json";

#[derive(Debug, Error)]
pub enum InstanceError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid instance file at {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("invalid instance: {0}")]
    Invalid(String),
    #[error(transparent)]
    Store(#[from] StoreError),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Instance {
    pub name: String,
    pub version_id: String,
    /// Shared root that holds `versions/`, `libraries/` and `assets/`.
    /// `None` means the launcher root. Imported instances point elsewhere.
    pub root: Option<PathBuf>,
    /// Working directory. `None` means `<instance>/minecraft`.
    pub game_dir: Option<PathBuf>,
    pub java: Option<PathBuf>,
    pub min_memory_mb: Option<u32>,
    pub max_memory_mb: u32,
    pub jvm_args: Vec<String>,
    pub game_args: Vec<String>,
    /// Extra environment variables for the game process.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Join this server right after the world loads.
    pub server: Option<ServerAddress>,
    pub username: String,
    /// Start the trial world (`--demo`) instead of the full game.
    #[serde(default)]
    pub demo: bool,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub created_at: Option<String>,
    /// Library group used for filtering.
    #[serde(default)]
    pub group: Option<String>,
    /// Free-form note for the editor.
    #[serde(default)]
    pub notes: String,
    /// Icon file stored in the instance directory, e.g. `icon.png`.
    #[serde(default)]
    pub icon: Option<PathBuf>,
}

/// A server address to join on launch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerAddress {
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

impl ServerAddress {
    /// Parse `host`, `host:port` or `[v6]:port`.
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        if value.is_empty() {
            return None;
        }
        if let Some(rest) = value.strip_prefix('[') {
            let (host, tail) = rest.split_once(']')?;
            let port = match tail.strip_prefix(':') {
                Some(port) => port.parse().ok()?,
                None if tail.is_empty() => default_port(),
                None => return None,
            };
            return (!host.is_empty()).then(|| Self {
                host: host.to_string(),
                port,
            });
        }
        let (host, port) = match value.rsplit_once(':') {
            Some((host, port)) if !host.contains(':') => (host, port.parse().ok()?),
            Some(_) => (value, default_port()),
            None => (value, default_port()),
        };
        (!host.is_empty()).then(|| Self {
            host: host.to_string(),
            port,
        })
    }

    pub fn display(&self) -> String {
        if self.port == default_port() {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

fn default_port() -> u16 {
    25565
}

impl Default for Instance {
    fn default() -> Self {
        Self {
            name: String::new(),
            version_id: String::new(),
            root: None,
            game_dir: None,
            java: None,
            min_memory_mb: None,
            max_memory_mb: 4096,
            jvm_args: Vec::new(),
            game_args: Vec::new(),
            env: BTreeMap::new(),
            server: None,
            username: "Player".to_string(),
            demo: false,
            width: None,
            height: None,
            created_at: None,
            group: None,
            notes: String::new(),
            icon: None,
        }
    }
}

/// Only the file names [`crate::Instance`]'s icon commands write. Nothing with
/// a directory component or an absolute path is accepted.
fn valid_icon(icon: &Path) -> bool {
    let mut components = icon.components();
    let Some(std::path::Component::Normal(name)) = components.next() else {
        return false;
    };
    if components.next().is_some() {
        return false;
    }
    name.to_str()
        .is_some_and(|name| matches!(name, "icon.png" | "icon.jpg" | "icon.jpeg" | "icon.webp" | "icon.gif"))
}

fn valid_instance_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', ':'])
        && !name.chars().any(char::is_control)
}

impl Instance {
    pub fn new(name: &str, version_id: &str) -> Self {
        Self {
            name: name.to_string(),
            version_id: version_id.to_string(),
            created_at: Some(now()),
            ..Self::default()
        }
    }

    pub fn load(dir: &Path) -> Result<Self, InstanceError> {
        let path = dir.join(INSTANCE_FILE);
        let bytes = fs::read(&path).map_err(|error| InstanceError::Io {
            path: path.clone(),
            source: error,
        })?;
        let mut instance: Self = serde_json::from_slice(&bytes).map_err(|source| InstanceError::Json {
            path: path.clone(),
            source,
        })?;
        if instance.version_id.is_empty() {
            return Err(InstanceError::Invalid("version_id is empty".to_string()));
        }
        // The id becomes `versions/<id>/<id>.json`; a value such as `../../x`
        // from a crafted archive must not be able to read outside the root.
        if !tntl_store::valid_component(&instance.version_id) {
            return Err(InstanceError::Invalid(format!(
                "unusable version id {:?}",
                instance.version_id
            )));
        }
        // Both paths are resolved against the process working directory
        // otherwise, which silently changes when the launcher is started from
        // another folder. They are always written absolute; older builds wrote
        // the caller's argument verbatim, so resolve those against the
        // instance directory and persist the result.
        let mut migrated = false;
        for path in [&mut instance.root, &mut instance.game_dir] {
            if let Some(value) = path.as_mut()
                && !value.is_absolute()
            {
                *value = dir.join(&*value);
                migrated = true;
            }
        }
        // Older builds stored Windows verbatim paths (`\\?\D:\...`). They work
        // for IO but leak into the UI and break string comparisons.
        instance.root = instance.root.as_deref().map(tntl_store::simplify_path);
        instance.game_dir = instance.game_dir.as_deref().map(tntl_store::simplify_path);
        // Every consumer derives the instance directory from `name`, so a
        // hand-edited name must not be able to point anywhere else.
        if !valid_instance_name(&instance.name) {
            return Err(InstanceError::Invalid(format!(
                "unusable instance name {:?}",
                instance.name
            )));
        }
        // `icon` is joined onto the instance directory by the icon commands.
        // A crafted archive used to be able to name `/home/u/file` or
        // `../../x` and have it read or deleted from the UI.
        if instance.icon.as_deref().is_some_and(|icon| !valid_icon(icon)) {
            instance.icon = None;
            migrated = true;
        }
        // A `java` path is executed by the launcher. An archive must not be
        // able to point it at an arbitrary binary; drop paths that are not a
        // real file.
        if instance
            .java
            .as_deref()
            .is_some_and(|java| !java.is_absolute() || !java.is_file())
        {
            instance.java = None;
            migrated = true;
        }
        if migrated {
            let _ = instance.save(dir);
        }
        Ok(instance)
    }

    pub fn save(&self, dir: &Path) -> Result<(), InstanceError> {
        fs::create_dir_all(dir).map_err(|error| InstanceError::Io {
            path: dir.to_path_buf(),
            source: error,
        })?;
        tntl_store::atomic_write_json(&dir.join(INSTANCE_FILE), self)?;
        Ok(())
    }

    /// Shared root for this instance, given the launcher root.
    pub fn shared_root(&self, launcher_root: &Path) -> PathBuf {
        match &self.root {
            Some(root) => root.clone(),
            None => launcher_root.to_path_buf(),
        }
    }

    pub fn game_dir(&self, instance_dir: &Path) -> PathBuf {
        match &self.game_dir {
            Some(dir) => dir.clone(),
            None => instance_dir.join("minecraft"),
        }
    }

    pub fn natives_dir(&self, instance_dir: &Path) -> PathBuf {
        instance_dir.join("natives")
    }

    pub fn layout(&self, launcher_root: &Path) -> RootLayout {
        RootLayout::new(self.shared_root(launcher_root))
    }
}

fn now() -> String {
    // Seconds since the epoch keeps this dependency-free and machine readable.
    use std::time::{SystemTime, UNIX_EPOCH};
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    seconds.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_instance_names_that_escape_the_directory() {
        let dir = std::env::temp_dir().join(format!("tntl-name-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let instance = Instance::new("ok", "1.20.1");
        instance.save(&dir).unwrap();
        for bad in ["../evil", "a/b", "..", ""] {
            let mut broken = Instance::new("ok", "1.20.1");
            broken.name = bad.to_string();
            broken.save(&dir).unwrap();
            assert!(Instance::load(&dir).is_err(), "{bad:?} was accepted");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn round_trips() {
        let dir = std::env::temp_dir().join(format!("tntl-instance-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut instance = Instance::new("test", "1.20.1");
        instance.max_memory_mb = 8192;
        instance.game_args.push("--demo".to_string());
        instance.save(&dir).unwrap();
        let loaded = Instance::load(&dir).unwrap();
        assert_eq!(loaded.version_id, "1.20.1");
        assert_eq!(loaded.max_memory_mb, 8192);
        assert_eq!(loaded.game_args, vec!["--demo"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn resolves_paths() {
        let instance = Instance::new("a", "1.20.1");
        assert_eq!(instance.shared_root(Path::new("/launcher")), PathBuf::from("/launcher"));
        assert_eq!(
            instance.game_dir(Path::new("/launcher/instances/a")),
            PathBuf::from("/launcher/instances/a/minecraft")
        );
    }

    #[test]
    fn parses_server_addresses() {
        assert_eq!(
            ServerAddress::parse("mc.example.com").unwrap(),
            ServerAddress {
                host: "mc.example.com".to_string(),
                port: 25565
            }
        );
        assert_eq!(
            ServerAddress::parse("mc.example.com:25566").unwrap(),
            ServerAddress {
                host: "mc.example.com".to_string(),
                port: 25566
            }
        );
        assert_eq!(
            ServerAddress::parse("[2001:db8::1]:25567").unwrap(),
            ServerAddress {
                host: "2001:db8::1".to_string(),
                port: 25567
            }
        );
        assert!(ServerAddress::parse("  ").is_none());
        assert!(ServerAddress::parse("host:notaport").is_none());
    }
}
