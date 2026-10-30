//! Worlds in an instance: metadata from `level.dat`, backups and imports.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, BufWriter, Read};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use zip::write::SimpleFileOptions;

const MAX_NBT_DEPTH: usize = 64;
const MAX_NBT_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum WorldError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid world {0}")]
    Invalid(String),
    #[error("level.dat in {0} is not readable NBT: {1}")]
    Nbt(PathBuf, String),
    #[error("zip error: {0}")]
    Zip(String),
    #[error("archive entry escapes the world directory: {0}")]
    UnsafeEntry(String),
    #[error("archive does not contain a level.dat")]
    NoLevel,
}

fn io_err(path: &Path, source: io::Error) -> WorldError {
    WorldError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// One world directory under `saves/`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldInfo {
    /// Directory name under `saves/`.
    pub dir: String,
    /// `LevelName` from `level.dat`, or the directory name.
    pub name: String,
    pub version: Option<String>,
    /// `survival`, `creative`, `adventure` or `spectator`.
    pub game_mode: Option<String>,
    pub hardcore: bool,
    /// `RandomSeed` from `level.dat`. Serialised as a string: a JSON number
    /// would be rounded to 53 bits by every JavaScript client.
    #[serde(serialize_with = "seed_as_string")]
    pub seed: Option<i64>,
    /// Milliseconds since the epoch, as written by the game.
    pub last_played_ms: Option<i64>,
    pub size: u64,
}

fn seed_as_string<S: serde::Serializer>(seed: &Option<i64>, serializer: S) -> Result<S::Ok, S::Error> {
    match seed {
        Some(seed) => serializer.serialize_str(&seed.to_string()),
        None => serializer.serialize_none(),
    }
}

pub fn saves_dir(game_dir: &Path) -> PathBuf {
    game_dir.join("saves")
}

/// Every directory in `saves/` that has a `level.dat`, newest first.
pub fn list(game_dir: &Path) -> Result<Vec<WorldInfo>, WorldError> {
    let dir = saves_dir(game_dir);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(io_err(&dir, error)),
    };
    let mut worlds = Vec::new();
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if !valid_name(&name) || !entry.path().join("level.dat").is_file() {
            continue;
        }
        // One unreadable world (Bedrock copy, truncated file) must not hide the
        // rest of the list.
        match read_world(&entry.path(), &name) {
            Ok(world) => worlds.push(world),
            Err(_) => worlds.push(WorldInfo {
                dir: name.clone(),
                name,
                version: None,
                game_mode: None,
                hardcore: false,
                seed: None,
                last_played_ms: fs::metadata(entry.path())
                    .and_then(|meta| meta.modified())
                    .ok()
                    .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                    .map(|duration| duration.as_millis() as i64),
                size: directory_size(&entry.path()),
            }),
        }
    }
    worlds.sort_by(|left, right| {
        right
            .last_played_ms
            .unwrap_or_default()
            .cmp(&left.last_played_ms.unwrap_or_default())
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(worlds)
}

fn read_world(path: &Path, dir: &str) -> Result<WorldInfo, WorldError> {
    let level = read_level(path)?;
    let modified_ms = fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as i64);
    Ok(WorldInfo {
        dir: dir.to_string(),
        name: level
            .get("LevelName")
            .and_then(Nbt::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| dir.to_string()),
        version: level
            .get("Version")
            .and_then(Nbt::as_compound)
            .and_then(|version| version.get("Name"))
            .and_then(Nbt::as_str)
            .map(str::to_string),
        game_mode: level
            .get("GameType")
            .and_then(Nbt::as_int)
            .and_then(|kind| match kind {
                0 => Some("survival"),
                1 => Some("creative"),
                2 => Some("adventure"),
                3 => Some("spectator"),
                _ => None,
            })
            .map(str::to_string),
        hardcore: level
            .get("hardcore")
            .and_then(Nbt::as_byte)
            .is_some_and(|value| value != 0),
        seed: level.get("RandomSeed").and_then(Nbt::as_long),
        last_played_ms: level
            .get("LastPlayed")
            .and_then(Nbt::as_long)
            .filter(|value| *value > 0)
            .or(modified_ms),
        size: directory_size(path),
    })
}

/// The `Data` compound of `level.dat`.
fn read_level(world: &Path) -> Result<BTreeMap<String, Nbt>, WorldError> {
    let path = world.join("level.dat");
    let file = File::open(&path).map_err(|error| io_err(&path, error))?;
    let mut decoder = GzDecoder::new(file);
    let mut bytes = Vec::new();
    (&mut decoder)
        .take(MAX_NBT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| WorldError::Nbt(path.clone(), error.to_string()))?;
    if bytes.len() as u64 > MAX_NBT_BYTES {
        return Err(WorldError::Nbt(path, "level.dat is too large".to_string()));
    }
    let root = parse_nbt(&bytes).map_err(|error| WorldError::Nbt(path, error))?;
    let data = root
        .as_compound()
        .and_then(|root| root.get("Data"))
        .and_then(Nbt::as_compound)
        .ok_or_else(|| WorldError::Invalid(format!("{} has no Data compound", world.display())))?;
    Ok(data.clone())
}

fn directory_size(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| {
            let path = entry.path();
            match fs::symlink_metadata(&path) {
                Ok(meta) if meta.is_dir() => directory_size(&path),
                Ok(meta) if meta.is_file() => meta.len(),
                _ => 0,
            }
        })
        .sum()
}

/// Zip a world into `backup_dir` and return the archive path.
pub fn backup(game_dir: &Path, world: &str, backup_dir: &Path) -> Result<PathBuf, WorldError> {
    let source = world_path(game_dir, world)?;
    if !source.join("level.dat").is_file() {
        return Err(WorldError::Invalid(format!("{world} has no level.dat")));
    }
    fs::create_dir_all(backup_dir).map_err(|error| io_err(backup_dir, error))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let stem = sanitize(world);
    let mut target = backup_dir.join(format!("{stem}-{stamp}.zip"));
    let mut unique = 1;
    while target.exists() {
        target = backup_dir.join(format!("{stem}-{stamp}-{unique}.zip"));
        unique += 1;
    }
    let temporary = backup_dir.join(format!(".{stem}-{stamp}-{unique}.zip.part"));
    let result = write_archive(&source, world, &temporary);
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    fs::rename(&temporary, &target).map_err(|error| io_err(&target, error))?;
    Ok(target)
}

fn write_archive(source: &Path, world: &str, target: &Path) -> Result<(), WorldError> {
    let file = File::create(target).map_err(|error| io_err(target, error))?;
    let mut zip = zip::ZipWriter::new(BufWriter::new(file));
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut stack = vec![source.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = fs::read_dir(&dir).map_err(|error| io_err(&dir, error))?;
        for entry in entries.flatten() {
            let path = entry.path();
            let meta = fs::symlink_metadata(&path).map_err(|error| io_err(&path, error))?;
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                stack.push(path);
                continue;
            }
            let relative = path
                .strip_prefix(source)
                .map_err(|_| WorldError::Invalid(path.display().to_string()))?;
            let name = format!("{world}/{}", slash(relative));
            zip.start_file(name, options)
                .map_err(|error| WorldError::Zip(error.to_string()))?;
            let mut input = File::open(&path).map_err(|error| io_err(&path, error))?;
            io::copy(&mut input, &mut zip).map_err(|error| io_err(&path, error))?;
        }
    }
    zip.finish().map_err(|error| WorldError::Zip(error.to_string()))?;
    Ok(())
}

/// Delete a world directory. Refuses a directory that is a link (the target
/// belongs to whoever it points at) and anything without `level.dat`, so a
/// hand-made folder under `saves/` cannot be swept away by a typo.
pub fn delete(game_dir: &Path, world: &str) -> Result<(), WorldError> {
    let path = world_path(game_dir, world)?;
    if tntl_store::is_link(&path) {
        return Err(WorldError::Invalid(format!("refusing to delete {world}: it is a link")));
    }
    if !path.join("level.dat").is_file() {
        return Err(WorldError::Invalid(format!(
            "{world} has no level.dat, so it is not a world"
        )));
    }
    fs::remove_dir_all(&path).map_err(|error| io_err(&path, error))
}

/// Delete one world backup archive.
pub fn delete_backup(backup_dir: &Path, file: &str) -> Result<(), WorldError> {
    if !valid_name(file) || !file.ends_with(".zip") {
        return Err(WorldError::Invalid(format!("invalid backup {file}")));
    }
    let path = backup_dir.join(file);
    if !path.is_file() {
        return Err(WorldError::Invalid(format!("backup {file} does not exist")));
    }
    fs::remove_file(&path).map_err(|error| io_err(&path, error))
}

/// Copy a world under a new directory name. The copy keeps its `level.dat`, so
/// the game lists it under the same display name with a `-2` suffix.
pub fn duplicate(game_dir: &Path, world: &str, new_dir: &str) -> Result<WorldInfo, WorldError> {
    let from = world_path(game_dir, world)?;
    if !valid_name(new_dir) || new_dir.is_empty() {
        return Err(WorldError::Invalid(format!("invalid world name {new_dir}")));
    }
    let to = unique_dir(&saves_dir(game_dir), new_dir);
    copy_dir(&from, &to)?;
    let dir = to
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| new_dir.to_string());
    read_world(&to, &dir)
}

/// Import a world archive (`zip` with `level.dat` at the root or inside one
/// top-level directory). Returns the new world.
pub fn import(game_dir: &Path, archive: &Path) -> Result<WorldInfo, WorldError> {
    let file = File::open(archive).map_err(|error| io_err(archive, error))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|error| WorldError::Zip(error.to_string()))?;
    let mut level_prefix: Option<String> = None;
    for index in 0..zip.len() {
        let entry = zip
            .by_index(index)
            .map_err(|error| WorldError::Zip(error.to_string()))?;
        let name = entry.name().to_string();
        let normalized = name.trim_end_matches('/');
        if normalized == "level.dat" {
            level_prefix = Some(String::new());
            break;
        }
        if let Some(prefix) = normalized.strip_suffix("/level.dat") {
            if !prefix.contains('/') {
                level_prefix = Some(format!("{prefix}/"));
                break;
            }
            return Err(WorldError::Invalid(format!("level.dat is nested too deep in {name}")));
        }
    }
    let prefix = level_prefix.ok_or(WorldError::NoLevel)?;
    let stem = archive
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_else(|| "world".to_string());
    let name = if prefix.is_empty() {
        sanitize(&stem)
    } else {
        sanitize(prefix.trim_end_matches('/'))
    };
    let saves = saves_dir(game_dir);
    fs::create_dir_all(&saves).map_err(|error| io_err(&saves, error))?;
    let destination = unique_dir(&saves, &name);
    fs::create_dir_all(&destination).map_err(|error| io_err(&destination, error))?;

    let result = extract(&mut zip, &prefix, &destination);
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&destination);
        return Err(error);
    }
    let dir = destination
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or(name);
    match read_world(&destination, &dir) {
        Ok(world) => Ok(world),
        Err(error) => {
            // Do not leave a world the game cannot open in `saves/`.
            let _ = fs::remove_dir_all(&destination);
            Err(error)
        }
    }
}

fn extract(zip: &mut zip::ZipArchive<File>, prefix: &str, destination: &Path) -> Result<(), WorldError> {
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|error| WorldError::Zip(error.to_string()))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        let Some(relative) = name.strip_prefix(prefix) else {
            continue;
        };
        let path = PathBuf::from(relative);
        if path.as_os_str().is_empty() || path.components().any(|part| !matches!(part, Component::Normal(_))) {
            return Err(WorldError::UnsafeEntry(name));
        }
        let target = destination.join(&path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|error| io_err(parent, error))?;
        }
        let mut output = File::create(&target).map_err(|error| io_err(&target, error))?;
        io::copy(&mut entry, &mut output).map_err(|error| io_err(&target, error))?;
    }
    Ok(())
}

fn world_path(game_dir: &Path, world: &str) -> Result<PathBuf, WorldError> {
    if !valid_name(world) {
        return Err(WorldError::Invalid(format!("invalid world name {world}")));
    }
    let path = saves_dir(game_dir).join(world);
    if !path.is_dir() {
        return Err(WorldError::Invalid(format!("world {world} does not exist")));
    }
    Ok(path)
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\'])
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), WorldError> {
    fs::create_dir_all(to).map_err(|error| io_err(to, error))?;
    for entry in fs::read_dir(from).map_err(|error| io_err(from, error))?.flatten() {
        let path = entry.path();
        let target = to.join(entry.file_name());
        // Follow no symlinks: a link to an ancestor would recurse forever, and
        // backups skip them too.
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            copy_dir(&path, &target)?;
        } else if meta.is_file() {
            fs::copy(&path, &target).map_err(|error| io_err(&target, error))?;
        }
    }
    Ok(())
}

fn unique_dir(parent: &Path, name: &str) -> PathBuf {
    let candidate = parent.join(name);
    if !candidate.exists() {
        return candidate;
    }
    for index in 2..1000 {
        let candidate = parent.join(format!("{name}-{index}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    parent.join(format!(
        "{name}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default()
    ))
}

fn sanitize(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.');
    if trimmed.is_empty() {
        "world".to_string()
    } else {
        trimmed.to_string()
    }
}

fn slash(path: &Path) -> String {
    path.components()
        .filter_map(|part| match part {
            Component::Normal(value) => Some(value.to_string_lossy().to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

// --- Minimal NBT reader ------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Nbt {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    String(String),
    List(Vec<Nbt>),
    Compound(BTreeMap<String, Nbt>),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
    ByteArray(Vec<u8>),
}

impl Nbt {
    fn as_byte(&self) -> Option<i8> {
        match self {
            Self::Byte(value) => Some(*value),
            _ => None,
        }
    }

    fn as_int(&self) -> Option<i32> {
        match self {
            Self::Int(value) => Some(*value),
            _ => None,
        }
    }

    fn as_long(&self) -> Option<i64> {
        match self {
            Self::Long(value) => Some(*value),
            _ => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    fn as_compound(&self) -> Option<&BTreeMap<String, Nbt>> {
        match self {
            Self::Compound(value) => Some(value),
            _ => None,
        }
    }
}

fn parse_nbt(bytes: &[u8]) -> Result<Nbt, String> {
    let mut cursor = Cursor { bytes, pos: 0 };
    let kind = cursor.u8()?;
    cursor.string()?;
    cursor.payload(kind, 0)
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Cursor<'_> {
    fn take(&mut self, count: usize) -> Result<&[u8], String> {
        let end = self.pos.checked_add(count).ok_or("NBT length overflow")?;
        if end > self.bytes.len() {
            return Err("NBT ended early".to_string());
        }
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn i16(&mut self) -> Result<i16, String> {
        Ok(i16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn i32(&mut self) -> Result<i32, String> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn i64(&mut self) -> Result<i64, String> {
        Ok(i64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn string(&mut self) -> Result<String, String> {
        let len = self.i16()?;
        if len < 0 {
            return Err("negative NBT string length".to_string());
        }
        let bytes = self.take(len as usize)?;
        // `level.dat` strings are Java modified UTF-8, not plain UTF-8.
        Ok(crate::nbt::decode_modified_utf8(bytes).unwrap_or_else(|| String::from_utf8_lossy(bytes).to_string()))
    }

    fn payload(&mut self, kind: u8, depth: usize) -> Result<Nbt, String> {
        if depth > MAX_NBT_DEPTH {
            return Err("NBT is nested too deeply".to_string());
        }
        match kind {
            1 => Ok(Nbt::Byte(self.u8()? as i8)),
            2 => Ok(Nbt::Short(self.i16()?)),
            3 => Ok(Nbt::Int(self.i32()?)),
            4 => Ok(Nbt::Long(self.i64()?)),
            5 => Ok(Nbt::Float(f32::from_be_bytes(self.take(4)?.try_into().unwrap()))),
            6 => Ok(Nbt::Double(f64::from_be_bytes(self.take(8)?.try_into().unwrap()))),
            7 => {
                let len = self.i32()?;
                if len < 0 || len as usize > self.bytes.len() {
                    return Err("invalid NBT byte array length".to_string());
                }
                Ok(Nbt::ByteArray(self.take(len as usize)?.to_vec()))
            }
            8 => Ok(Nbt::String(self.string()?)),
            9 => {
                let item = self.u8()?;
                let len = self.i32()?;
                if len < 0 || len as usize > self.bytes.len() {
                    return Err("invalid NBT list length".to_string());
                }
                // `len` is only bounded by the file size; reserving that many
                // 32-byte nodes would let a small crafted file ask for gigabytes.
                let mut items = Vec::with_capacity((len as usize).min(4096));
                for _ in 0..len {
                    items.push(self.payload(item, depth + 1)?);
                }
                Ok(Nbt::List(items))
            }
            10 => {
                let mut entries = BTreeMap::new();
                loop {
                    let item = self.u8()?;
                    if item == 0 {
                        break;
                    }
                    let name = self.string()?;
                    entries.insert(name, self.payload(item, depth + 1)?);
                }
                Ok(Nbt::Compound(entries))
            }
            11 => {
                let len = self.i32()?;
                if len < 0 || len as usize > self.bytes.len() / 4 {
                    return Err("invalid NBT int array length".to_string());
                }
                let mut items = Vec::with_capacity(len as usize);
                for _ in 0..len {
                    items.push(self.i32()?);
                }
                Ok(Nbt::IntArray(items))
            }
            12 => {
                let len = self.i32()?;
                if len < 0 || len as usize > self.bytes.len() / 8 {
                    return Err("invalid NBT long array length".to_string());
                }
                let mut items = Vec::with_capacity(len as usize);
                for _ in 0..len {
                    items.push(self.i64()?);
                }
                Ok(Nbt::LongArray(items))
            }
            other => Err(format!("unknown NBT tag {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tntl-worlds-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn nbt_level(name: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.push(10); // root compound
        bytes.extend_from_slice(&0i16.to_be_bytes());
        bytes.push(10); // Data
        bytes.extend_from_slice(&4i16.to_be_bytes());
        bytes.extend_from_slice(b"Data");

        bytes.push(8); // LevelName
        bytes.extend_from_slice(&9i16.to_be_bytes());
        bytes.extend_from_slice(b"LevelName");
        bytes.extend_from_slice(&(name.len() as i16).to_be_bytes());
        bytes.extend_from_slice(name.as_bytes());
        bytes.push(3); // GameType
        bytes.extend_from_slice(&8i16.to_be_bytes());
        bytes.extend_from_slice(b"GameType");
        bytes.extend_from_slice(&1i32.to_be_bytes());

        bytes.push(1); // hardcore
        bytes.extend_from_slice(&8i16.to_be_bytes());
        bytes.extend_from_slice(b"hardcore");
        bytes.push(0);

        bytes.push(4); // LastPlayed
        bytes.extend_from_slice(&10i16.to_be_bytes());
        bytes.extend_from_slice(b"LastPlayed");
        bytes.extend_from_slice(&1_700_000_000_000i64.to_be_bytes());

        bytes.push(0); // end Data
        bytes.push(0); // end root
        bytes
    }

    fn write_world(game: &Path, dir: &str, name: &str) {
        let world = saves_dir(game).join(dir);
        fs::create_dir_all(world.join("region")).unwrap();
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&nbt_level(name)).unwrap();
        fs::write(world.join("level.dat"), encoder.finish().unwrap()).unwrap();
        fs::write(world.join("region/r.0.0.mca"), vec![0u8; 32]).unwrap();
    }

    #[test]
    fn lists_worlds_with_metadata() {
        let game = temp_dir("list");
        write_world(&game, "Survival", "My World");
        fs::create_dir_all(saves_dir(&game).join("broken")).unwrap();
        let worlds = list(&game).unwrap();
        assert_eq!(worlds.len(), 1);
        assert_eq!(worlds[0].dir, "Survival");
        assert_eq!(worlds[0].name, "My World");
        assert_eq!(worlds[0].game_mode.as_deref(), Some("creative"));
        assert!(!worlds[0].hardcore);
        assert_eq!(worlds[0].last_played_ms, Some(1_700_000_000_000));
        assert!(worlds[0].size >= 32);
        fs::remove_dir_all(&game).unwrap();
    }

    #[test]
    fn backs_up_and_restores_a_world() {
        let game = temp_dir("backup");
        write_world(&game, "Survival", "My World");
        let backups = game.join("backups/worlds");
        let archive = backup(&game, "Survival", &backups).unwrap();
        assert!(archive.exists());

        delete(&game, "Survival").unwrap();
        assert!(list(&game).unwrap().is_empty());

        let restored = import(&game, &archive).unwrap();
        assert_eq!(restored.name, "My World");
        assert!(saves_dir(&game).join("Survival/region/r.0.0.mca").exists());
        fs::remove_dir_all(&game).unwrap();
    }

    #[test]
    fn deletes_one_backup_and_rejects_escapes() {
        let game = temp_dir("dropbackup");
        write_world(&game, "Survival", "My World");
        let backups = game.join("backups/worlds");
        let archive = backup(&game, "Survival", &backups).unwrap();
        let name = archive.file_name().unwrap().to_string_lossy().to_string();

        assert!(delete_backup(&backups, "../worlds.zip").is_err());
        assert!(delete_backup(&backups, "missing.zip").is_err());
        delete_backup(&backups, &name).unwrap();
        assert!(!archive.exists());
        assert!(saves_dir(&game).join("Survival/level.dat").is_file());
        fs::remove_dir_all(&game).unwrap();
    }

    #[test]
    fn imports_archives_with_a_prefix_and_avoids_overwrite() {
        let game = temp_dir("import");
        let source = temp_dir("pack");
        let world = source.join("MyWorld");
        fs::create_dir_all(&world).unwrap();
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&nbt_level("Imported")).unwrap();
        fs::write(world.join("level.dat"), encoder.finish().unwrap()).unwrap();
        let archive = source.join("pack.zip");
        {
            let file = File::create(&archive).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            zip.start_file("MyWorld/level.dat", SimpleFileOptions::default())
                .unwrap();
            zip.write_all(&fs::read(world.join("level.dat")).unwrap()).unwrap();
            zip.finish().unwrap();
        }
        let first = import(&game, &archive).unwrap();
        assert_eq!(first.dir, "MyWorld");
        assert_eq!(first.name, "Imported");
        let second = import(&game, &archive).unwrap();
        assert_eq!(second.dir, "MyWorld-2");
        fs::remove_dir_all(&game).unwrap();
        fs::remove_dir_all(&source).unwrap();
    }

    #[test]
    fn rejects_path_traversal() {
        let game = temp_dir("traversal");
        let source = temp_dir("evil");
        let archive = source.join("evil.zip");
        {
            let file = File::create(&archive).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            zip.start_file("w/level.dat", SimpleFileOptions::default()).unwrap();
            zip.write_all(&nbt_level("evil")).unwrap();
            zip.start_file("w/../../escape.txt", SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"x").unwrap();
            zip.finish().unwrap();
        }
        assert!(matches!(import(&game, &archive), Err(WorldError::UnsafeEntry(_))));
        assert!(!game.join("escape.txt").exists());
        fs::remove_dir_all(&game).unwrap();
        fs::remove_dir_all(&source).unwrap();
    }
}
