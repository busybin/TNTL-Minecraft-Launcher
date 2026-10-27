//! On-disk layout, hashing, atomic writes and safe archive extraction.

#![forbid(unsafe_code)]

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use sha1::{Digest, Sha1};
use sha2::Sha512;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("unsafe archive entry: {0}")]
    UnsafeEntry(String),
    #[error("invalid archive: {0}")]
    Zip(String),
    #[error("invalid asset index: {0}")]
    Index(String),
}

fn io_err(path: &Path, source: io::Error) -> StoreError {
    StoreError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Standard `.minecraft` layout.
#[derive(Debug, Clone)]
pub struct RootLayout {
    pub root: PathBuf,
}

impl RootLayout {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn versions_dir(&self) -> PathBuf {
        self.root.join("versions")
    }

    pub fn version_dir(&self, id: &str) -> PathBuf {
        self.versions_dir().join(id)
    }

    pub fn version_json(&self, id: &str) -> PathBuf {
        self.version_dir(id).join(format!("{id}.json"))
    }

    pub fn client_jar(&self, id: &str) -> PathBuf {
        self.version_dir(id).join(format!("{id}.jar"))
    }

    pub fn libraries_dir(&self) -> PathBuf {
        self.root.join("libraries")
    }

    pub fn library(&self, relative: &str) -> PathBuf {
        join_relative(&self.libraries_dir(), relative)
    }

    pub fn assets_dir(&self) -> PathBuf {
        self.root.join("assets")
    }

    pub fn asset_index(&self, id: &str) -> PathBuf {
        join_relative(&self.assets_dir().join("indexes"), &format!("{id}.json"))
    }

    /// Pre-1.7 indexes are virtual: the game reads a plain tree of asset names
    /// instead of the hashed object store.
    pub fn assets_virtual_dir(&self, id: &str) -> PathBuf {
        join_relative(&self.assets_dir().join("virtual"), id)
    }

    pub fn asset_object(&self, hash: &str) -> PathBuf {
        if hash.len() == 40 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return self.assets_dir().join("objects").join(&hash[..2]).join(hash);
        }
        // Not a SHA-1: keep the value from escaping the object store.
        let safe: String = hash.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        let safe = if safe.is_empty() { "invalid".to_string() } else { safe };
        self.assets_dir().join("objects").join("invalid").join(safe)
    }

    pub fn asset_object_url(hash: &str) -> String {
        let prefix = hash.get(..2).unwrap_or("00");
        format!("{prefix}/{hash}")
    }

    pub fn natives_dir(&self, version: &str) -> PathBuf {
        self.version_dir(version).join("natives")
    }

    pub fn log_config(&self, id: &str) -> PathBuf {
        join_relative(&self.assets_dir().join("log_configs"), id)
    }
}

/// Join a path taken from version JSON onto a root directory. Components that
/// would escape (absolute paths, `..`, drive letters) are dropped so a crafted
/// version file cannot make the launcher write outside its root.
fn join_relative(base: &Path, relative: &str) -> PathBuf {
    let mut out = base.to_path_buf();
    for part in relative.split(['/', '\\']) {
        if part.is_empty() || part == "." || part == ".." || part.contains(':') {
            continue;
        }
        out.push(part);
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verify {
    Ok,
    Missing,
    SizeMismatch { actual: u64, expected: u64 },
    Sha1Mismatch { actual: String },
}

impl Verify {
    pub fn is_ok(&self) -> bool {
        matches!(self, Verify::Ok)
    }
}

pub fn sha1_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha1::digest(bytes))
}

pub fn sha1_file(path: &Path) -> io::Result<String> {
    hash_file(Sha1::new(), path)
}

/// SHA-512 of a file, used by `.mrpack` indexes that publish no SHA-1.
pub fn sha512_file(path: &Path) -> io::Result<String> {
    hash_file(Sha512::new(), path)
}

fn hash_file<D: Digest>(mut hasher: D, path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Check a file against an optional size and SHA-1. Size is checked first
/// because it is much cheaper.
pub fn verify_file(path: &Path, sha1: Option<&str>, size: Option<u64>) -> Verify {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => return Verify::Missing,
    };
    if let Some(expected) = size
        && metadata.len() != expected
    {
        return Verify::SizeMismatch {
            actual: metadata.len(),
            expected,
        };
    }
    if let Some(expected) = sha1 {
        match sha1_file(path) {
            Ok(actual) if actual.eq_ignore_ascii_case(expected) => Verify::Ok,
            Ok(actual) => Verify::Sha1Mismatch { actual },
            Err(_) => Verify::Missing,
        }
    } else {
        Verify::Ok
    }
}

/// Write `bytes` to `path` via a sibling temp file and a rename.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| io_err(parent, error))?;
    }
    let temp = temp_path(path);
    fs::write(&temp, bytes).map_err(|error| io_err(&temp, error))?;
    fs::rename(&temp, path).map_err(|error| io_err(path, error))?;
    Ok(())
}

/// Unique sibling path for the write-then-rename dance. A pid alone is not
/// enough: several commands in one process write the same file.
fn temp_path(path: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "tmp".to_string());
    path.with_file_name(format!(".{name}.{}.{seq}.tmp", std::process::id()))
}

/// Write JSON with stable pretty formatting.
pub fn atomic_write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), StoreError> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|error| StoreError::Zip(format!("json: {error}")))?;
    bytes.push(b'\n');
    atomic_write(path, &bytes)
}

/// Write a file only the current user may read (0600 on Unix).
pub fn atomic_write_private(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| io_err(parent, error))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
        }
    }
    let temp = temp_path(path);
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temp)
            .map_err(|error| io_err(&temp, error))?;
        file.write_all(bytes).map_err(|error| io_err(&temp, error))?;
    }
    #[cfg(not(unix))]
    fs::write(&temp, bytes).map_err(|error| io_err(&temp, error))?;
    fs::rename(&temp, path).map_err(|error| io_err(path, error))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Write private JSON with stable pretty formatting.
pub fn atomic_write_private_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), StoreError> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|error| StoreError::Zip(format!("json: {error}")))?;
    bytes.push(b'\n');
    atomic_write_private(path, &bytes)
}

/// Reject absolute paths, `..`, prefixes and drive letters.
pub fn safe_relative(name: &str) -> Option<PathBuf> {
    let path = Path::new(name);
    if path.is_absolute() {
        return None;
    }
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if out.as_os_str().is_empty() { None } else { Some(out) }
}

/// Where a file should be written when the user may have switched it off.
///
/// `mods/example.jar.disabled` wins over `mods/example.jar` as long as the
/// enabled file is missing, so downloads and pack updates keep the toggle.
pub fn target_respecting_disabled(path: &Path) -> PathBuf {
    if path.is_file() {
        return path.to_path_buf();
    }
    let mut name = path.as_os_str().to_os_string();
    name.push(".disabled");
    let disabled = PathBuf::from(name);
    if disabled.is_file() {
        disabled
    } else {
        path.to_path_buf()
    }
}

/// Marker file inside a materialized asset tree, holding the index stamp.
const TREE_STAMP: &str = ".tntl-tree";

/// Materialize an asset index as a plain tree of names.
///
/// Two Mojang formats need this: pre-1.6 indexes (`map_to_resources`) are read
/// by the game from `resources/`, and pre-1.7 "virtual" indexes need the same
/// kind of tree under `assets/virtual/<id>`. Objects are hard linked, so a
/// second tree costs no disk space; filesystems without hard links fall back to
/// copying.
///
/// `stamp` identifies the index (its SHA-1). A matching marker means the tree
/// is complete and the call returns without touching anything. `force` relinks
/// every entry, which a repair pass needs after re-downloading a corrupt
/// object: an existing hard link still points at the old inode.
pub fn materialize_asset_tree(
    roots: &RootLayout,
    index: &tntl_model::AssetIndex,
    dest: &Path,
    stamp: &str,
    force: bool,
) -> Result<usize, StoreError> {
    let marker = dest.join(TREE_STAMP);
    if !force && fs::read_to_string(&marker).is_ok_and(|text| text.trim() == stamp) {
        return Ok(0);
    }
    let mut written = 0usize;
    for (name, object) in &index.objects {
        let Some(relative) = safe_relative(name) else {
            continue;
        };
        let target = dest.join(relative);
        if !force && target.metadata().is_ok_and(|meta| meta.len() == object.size) {
            continue;
        }
        let source = roots.asset_object(&object.hash);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|error| io_err(parent, error))?;
        }
        if target.is_file() {
            fs::remove_file(&target).map_err(|error| io_err(&target, error))?;
        }
        if fs::hard_link(&source, &target).is_err() {
            fs::copy(&source, &target).map_err(|error| io_err(&target, error))?;
        }
        written += 1;
    }
    atomic_write(&marker, stamp.as_bytes())?;
    Ok(written)
}

/// Read an asset index from disk and materialize its name tree.
pub fn materialize_asset_index(
    roots: &RootLayout,
    index_path: &Path,
    dest: &Path,
    force: bool,
) -> Result<usize, StoreError> {
    let bytes = fs::read(index_path).map_err(|error| io_err(index_path, error))?;
    let index: tntl_model::AssetIndex = serde_json::from_slice(&bytes)
        .map_err(|error| StoreError::Index(format!("{}: {error}", index_path.display())))?;
    materialize_asset_tree(roots, &index, dest, &sha1_bytes(&bytes), force)
}

/// Extract a natives jar into `dest`, skipping excluded prefixes.
const MAX_NATIVE_BYTES: u64 = 256 * 1024 * 1024;

pub fn extract_natives(jar: &Path, dest: &Path, excludes: &[String]) -> Result<usize, StoreError> {
    let file = File::open(jar).map_err(|error| io_err(jar, error))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| StoreError::Zip(error.to_string()))?;
    fs::create_dir_all(dest).map_err(|error| io_err(dest, error))?;
    let mut written = 0;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| StoreError::Zip(error.to_string()))?;
        let name = entry.name().to_string();
        if entry.is_dir() {
            continue;
        }
        if excludes.iter().any(|prefix| name.starts_with(prefix)) {
            continue;
        }
        let Some(relative) = safe_relative(&name) else {
            return Err(StoreError::UnsafeEntry(name));
        };
        let target = dest.join(&relative);
        if !target.starts_with(dest) {
            return Err(StoreError::UnsafeEntry(name));
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|error| io_err(parent, error))?;
        }
        let mut out = File::create(&target).map_err(|error| io_err(&target, error))?;
        // Natives jars are a few MB; a hostile one must not fill the disk.
        let copied =
            io::copy(&mut entry.by_ref().take(MAX_NATIVE_BYTES), &mut out).map_err(|error| io_err(&target, error))?;
        if copied >= MAX_NATIVE_BYTES {
            return Err(StoreError::Zip(format!(
                "{} is too large to be a natives jar",
                target.display()
            )));
        }
        written += 1;
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn verifies_size_and_hash() {
        let dir = std::env::temp_dir().join(format!("tntl-store-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        fs::write(&path, b"hello").unwrap();
        let hash = sha1_bytes(b"hello");
        assert!(verify_file(&path, Some(&hash), Some(5)).is_ok());
        assert!(matches!(
            verify_file(&path, Some(&hash), Some(6)),
            Verify::SizeMismatch { .. }
        ));
        assert!(matches!(
            verify_file(&path, Some("00"), None),
            Verify::Sha1Mismatch { .. }
        ));
        assert_eq!(verify_file(&dir.join("missing"), None, None), Verify::Missing);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn keeps_json_paths_inside_the_root() {
        let roots = RootLayout::new("/tmp/tntl-root");
        assert_eq!(
            roots.library("net/minecraft/a.jar"),
            std::path::PathBuf::from("/tmp/tntl-root/libraries/net/minecraft/a.jar")
        );
        assert_eq!(
            roots.library("../../etc/evil.jar"),
            std::path::PathBuf::from("/tmp/tntl-root/libraries/etc/evil.jar")
        );
        assert_eq!(
            roots.asset_object("../../x"),
            std::path::PathBuf::from("/tmp/tntl-root/assets/objects/invalid/x")
        );
        assert!(roots.asset_object(&"a".repeat(40)).ends_with("a".repeat(40)));
    }

    #[test]
    fn concurrent_writes_leave_one_complete_file() {
        let dir = std::env::temp_dir().join(format!("tntl-atomic-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("same.txt");
        let payloads: Vec<String> = (0..8).map(|index| format!("payload-{index}").repeat(2048)).collect();
        std::thread::scope(|scope| {
            for payload in &payloads {
                let path = path.clone();
                scope.spawn(move || atomic_write(&path, payload.as_bytes()).unwrap());
            }
        });
        let written = fs::read_to_string(&path).unwrap();
        assert!(payloads.contains(&written), "file is a mix of two writes");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn hashes_with_sha512() {
        let dir = std::env::temp_dir().join(format!("tntl-sha512-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        fs::write(&path, b"hello").unwrap();
        assert_eq!(
            sha512_file(&path).unwrap(),
            "9b71d224bd62f3785d96d46ad3ea3d73319bfbc2890caadae2dff72519673ca72323c3d99ba5c11d7c7acc6e14b8c5da0c4663475c2e5c3adef46f73bcdec043"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn keeps_a_disabled_file_as_the_target() {
        let dir = std::env::temp_dir().join(format!("tntl-disabled-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("mods")).unwrap();
        let enabled = dir.join("mods/a.jar");
        assert_eq!(target_respecting_disabled(&enabled), enabled);
        fs::write(dir.join("mods/a.jar.disabled"), b"off").unwrap();
        assert_eq!(target_respecting_disabled(&enabled), dir.join("mods/a.jar.disabled"));
        fs::write(&enabled, b"on").unwrap();
        assert_eq!(target_respecting_disabled(&enabled), enabled);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_unsafe_archive_paths() {
        assert!(safe_relative("../escape").is_none());
        assert!(safe_relative("/etc/passwd").is_none());
        assert_eq!(
            safe_relative("META-INF/MANIFEST.MF"),
            Some(PathBuf::from("META-INF/MANIFEST.MF"))
        );
    }

    #[test]
    fn materializes_an_asset_tree_once_and_relinks_it_on_repair() {
        let dir = std::env::temp_dir().join(format!("tntl-tree-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let roots = RootLayout::new(&dir);
        let object = roots.asset_object("aa11");
        fs::create_dir_all(object.parent().unwrap()).unwrap();
        fs::write(&object, b"sound").unwrap();
        let index = tntl_model::AssetIndex {
            objects: [(
                "sound/one.ogg".to_string(),
                tntl_model::AssetObject {
                    hash: "aa11".to_string(),
                    size: 5,
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let dest = dir.join("resources");
        assert_eq!(
            materialize_asset_tree(&roots, &index, &dest, "stamp-1", false).unwrap(),
            1
        );
        assert_eq!(fs::read(dest.join("sound/one.ogg")).unwrap(), b"sound");
        // Same stamp: nothing to do, even when a file was deleted behind our back.
        fs::remove_file(dest.join("sound/one.ogg")).unwrap();
        assert_eq!(
            materialize_asset_tree(&roots, &index, &dest, "stamp-1", false).unwrap(),
            0
        );
        // A repair pass rewrites every entry, so a replaced object is visible.
        fs::write(&object, b"fixd").unwrap();
        assert!(materialize_asset_tree(&roots, &index, &dest, "stamp-1", true).unwrap() >= 1);
        assert_eq!(fs::read(dest.join("sound/one.ogg")).unwrap(), b"fixd");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn extracts_natives_and_honours_excludes() {
        let dir = std::env::temp_dir().join(format!("tntl-natives-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let jar = dir.join("n.jar");
        {
            let file = File::create(&jar).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            zip.start_file("libfoo.so", zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"elf").unwrap();
            zip.start_file("META-INF/MANIFEST.MF", zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"manifest").unwrap();
            zip.finish().unwrap();
        }
        let out = dir.join("out");
        let count = extract_natives(&jar, &out, &["META-INF/".to_string()]).unwrap();
        assert_eq!(count, 1);
        assert!(out.join("libfoo.so").exists());
        assert!(!out.join("META-INF").exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
