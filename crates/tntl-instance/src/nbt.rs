//! Minimal NBT reader and writer, used for `servers.dat`.
//!
//! Only the tags Minecraft writes into that file are supported. The reader is
//! bounded so a corrupt file cannot exhaust memory or the stack.

use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use thiserror::Error;

const MAX_DEPTH: usize = 64;
const MAX_BYTES: u64 = 64 * 1024 * 1024;
const MAX_LIST_LEN: usize = 1_000_000;

#[derive(Debug, Error)]
pub enum NbtError {
    #[error("io error at {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid NBT: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Tag {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    ByteArray(Vec<u8>),
    String(String),
    List(Vec<Tag>),
    Compound(Vec<(String, Tag)>),
}

impl Tag {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Tag::String(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Tag::Byte(value) => Some(*value != 0),
            _ => None,
        }
    }

    pub fn get(&self, key: &str) -> Option<&Tag> {
        match self {
            Tag::Compound(entries) => entries.iter().find(|(name, _)| name == key).map(|(_, value)| value),
            _ => None,
        }
    }

    pub fn set(&mut self, key: &str, value: Tag) {
        if let Tag::Compound(entries) = self {
            match entries.iter_mut().find(|(name, _)| name == key) {
                Some((_, existing)) => *existing = value,
                None => entries.push((key.to_string(), value)),
            }
        }
    }
}

/// Read a file that may or may not be gzip compressed. Returns the root tag.
pub fn read_file(path: &Path) -> Result<Tag, NbtError> {
    let bytes = fs::read(path).map_err(|error| NbtError::Io {
        path: path.to_path_buf(),
        source: error,
    })?;
    let raw = if bytes.starts_with(&[0x1f, 0x8b]) {
        let decoder = GzDecoder::new(&bytes[..]);
        let mut decoded = Vec::new();
        decoder
            .take(MAX_BYTES + 1)
            .read_to_end(&mut decoded)
            .map_err(|error| NbtError::Io {
                path: path.to_path_buf(),
                source: error,
            })?;
        if decoded.len() as u64 > MAX_BYTES {
            return Err(NbtError::Invalid("file is too large".to_string()));
        }
        decoded
    } else {
        bytes
    };
    from_bytes(&raw)
}

/// Write the root compound as gzip compressed NBT.
pub fn write_file(path: &Path, root: &Tag) -> Result<(), NbtError> {
    let bytes = to_bytes(root)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| NbtError::Io {
            path: parent.to_path_buf(),
            source: error,
        })?;
    }
    let temporary = {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "nbt".to_string());
        path.with_file_name(format!(".{name}.{}.{seq}.part", std::process::id()))
    };
    let file = fs::File::create(&temporary).map_err(|error| NbtError::Io {
        path: temporary.clone(),
        source: error,
    })?;
    let mut encoder = GzEncoder::new(file, Compression::default());
    encoder.write_all(&bytes).map_err(|error| NbtError::Io {
        path: temporary.clone(),
        source: error,
    })?;
    encoder.finish().map_err(|error| NbtError::Io {
        path: temporary.clone(),
        source: error,
    })?;
    fs::rename(&temporary, path).map_err(|error| NbtError::Io {
        path: path.to_path_buf(),
        source: error,
    })?;
    Ok(())
}

pub fn from_bytes(bytes: &[u8]) -> Result<Tag, NbtError> {
    let mut cursor = io::Cursor::new(bytes);
    let tag_id = read_u8(&mut cursor)?;
    if tag_id == 0 {
        return Ok(Tag::Compound(Vec::new()));
    }
    read_string(&mut cursor)?;
    let tag = read_tag(tag_id, &mut cursor, 0)?;
    Ok(tag)
}

pub fn to_bytes(root: &Tag) -> Result<Vec<u8>, NbtError> {
    if !matches!(root, Tag::Compound(_)) {
        return Err(NbtError::Invalid("root tag must be a compound".to_string()));
    }
    let mut out = Vec::new();
    out.push(tag_id(root));
    out.extend_from_slice(&0u16.to_be_bytes());
    write_payload(root, &mut out)?;
    Ok(out)
}

fn tag_id(tag: &Tag) -> u8 {
    match tag {
        Tag::Byte(_) => 1,
        Tag::Short(_) => 2,
        Tag::Int(_) => 3,
        Tag::Long(_) => 4,
        Tag::Float(_) => 5,
        Tag::Double(_) => 6,
        Tag::ByteArray(_) => 7,
        Tag::String(_) => 8,
        Tag::List(_) => 9,
        Tag::Compound(_) => 10,
    }
}

fn write_payload(tag: &Tag, out: &mut Vec<u8>) -> Result<(), NbtError> {
    match tag {
        Tag::Byte(value) => out.push(*value as u8),
        Tag::Short(value) => out.extend_from_slice(&value.to_be_bytes()),
        Tag::Int(value) => out.extend_from_slice(&value.to_be_bytes()),
        Tag::Long(value) => out.extend_from_slice(&value.to_be_bytes()),
        Tag::Float(value) => out.extend_from_slice(&value.to_be_bytes()),
        Tag::Double(value) => out.extend_from_slice(&value.to_be_bytes()),
        Tag::ByteArray(bytes) => {
            out.extend_from_slice(&(bytes.len() as i32).to_be_bytes());
            out.extend_from_slice(bytes);
        }
        Tag::String(value) => write_string(value, out)?,
        Tag::List(entries) => {
            let id = entries.first().map(tag_id).unwrap_or(0);
            out.push(id);
            out.extend_from_slice(&(entries.len() as i32).to_be_bytes());
            for entry in entries {
                write_payload(entry, out)?;
            }
        }
        Tag::Compound(entries) => {
            for (name, value) in entries {
                out.push(tag_id(value));
                write_string(name, out)?;
                write_payload(value, out)?;
            }
            out.push(0);
        }
    }
    Ok(())
}

/// Java's `DataOutput.writeUTF`: modified UTF-8, bounded by an unsigned 16-bit
/// byte count. Supplementary characters become a surrogate pair of 3-byte
/// sequences, exactly like the game writes them.
fn write_string(value: &str, out: &mut Vec<u8>) -> Result<(), NbtError> {
    let mut bytes = Vec::with_capacity(value.len() + 2);
    for unit in value.encode_utf16() {
        match unit {
            0 => bytes.extend_from_slice(&[0xc0, 0x80]),
            0x0001..=0x007f => bytes.push(unit as u8),
            0x0080..=0x07ff => {
                bytes.push(0xc0 | (unit >> 6) as u8);
                bytes.push(0x80 | (unit & 0x3f) as u8);
            }
            _ => {
                bytes.push(0xe0 | (unit >> 12) as u8);
                bytes.push(0x80 | ((unit >> 6) & 0x3f) as u8);
                bytes.push(0x80 | (unit & 0x3f) as u8);
            }
        }
    }
    let length = u16::try_from(bytes.len()).map_err(|_| NbtError::Invalid("string is too long".to_string()))?;
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(&bytes);
    Ok(())
}

fn read_tag(id: u8, cursor: &mut io::Cursor<&[u8]>, depth: usize) -> Result<Tag, NbtError> {
    if depth > MAX_DEPTH {
        return Err(NbtError::Invalid("nesting is too deep".to_string()));
    }
    Ok(match id {
        1 => Tag::Byte(read_i8(cursor)?),
        2 => Tag::Short(read_i16(cursor)?),
        3 => Tag::Int(read_i32(cursor)?),
        4 => Tag::Long(read_i64(cursor)?),
        5 => Tag::Float(f32::from_bits(read_i32(cursor)? as u32)),
        6 => Tag::Double(f64::from_bits(read_i64(cursor)? as u64)),
        7 => {
            let length = read_length(cursor)?;
            let mut bytes = vec![0u8; length];
            cursor.read_exact(&mut bytes).map_err(read_error)?;
            Tag::ByteArray(bytes)
        }
        8 => Tag::String(read_string(cursor)?),
        9 => {
            let element = read_u8(cursor)?;
            let length = read_length(cursor)?;
            if element == 0 && length > 0 {
                return Err(NbtError::Invalid("list has no element type".to_string()));
            }
            let mut entries = Vec::with_capacity(length.min(4096));
            for _ in 0..length {
                entries.push(read_tag(element, cursor, depth + 1)?);
            }
            Tag::List(entries)
        }
        10 => {
            let mut entries = Vec::new();
            loop {
                let id = read_u8(cursor)?;
                if id == 0 {
                    break;
                }
                let name = read_string(cursor)?;
                entries.push((name, read_tag(id, cursor, depth + 1)?));
            }
            Tag::Compound(entries)
        }
        other => return Err(NbtError::Invalid(format!("unknown tag id {other}"))),
    })
}

fn read_length(cursor: &mut io::Cursor<&[u8]>) -> Result<usize, NbtError> {
    let length = read_i32(cursor)?;
    if length < 0 {
        return Err(NbtError::Invalid("negative length".to_string()));
    }
    let length = length as usize;
    if length > MAX_LIST_LEN {
        return Err(NbtError::Invalid("length is too large".to_string()));
    }
    Ok(length)
}

/// Java's `DataInput.readUTF`: modified UTF-8 (CESU-8, `C0 80` for NUL).
fn read_string(cursor: &mut io::Cursor<&[u8]>) -> Result<String, NbtError> {
    let length = read_u16(cursor)? as usize;
    let mut bytes = vec![0u8; length];
    cursor.read_exact(&mut bytes).map_err(read_error)?;
    decode_modified_utf8(&bytes).ok_or_else(|| NbtError::Invalid("string is not modified UTF-8".to_string()))
}

/// Decode Java's modified UTF-8. `None` when the bytes are malformed.
pub fn decode_modified_utf8(bytes: &[u8]) -> Option<String> {
    let mut units = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let first = bytes[index];
        match first {
            0x00..=0x7f => {
                units.push(first as u16);
                index += 1;
            }
            0xc0..=0xdf => {
                let second = *bytes.get(index + 1)?;
                if second & 0xc0 != 0x80 {
                    return None;
                }
                units.push((((first & 0x1f) as u16) << 6) | (second & 0x3f) as u16);
                index += 2;
            }
            0xe0..=0xef => {
                let second = *bytes.get(index + 1)?;
                let third = *bytes.get(index + 2)?;
                if second & 0xc0 != 0x80 || third & 0xc0 != 0x80 {
                    return None;
                }
                units.push((((first & 0x0f) as u16) << 12) | (((second & 0x3f) as u16) << 6) | (third & 0x3f) as u16);
                index += 3;
            }
            _ => return None,
        }
    }
    // Unpaired surrogates cannot exist in a Rust string; the game never writes
    // them in practice, and losing one is recoverable where a hard error is not.
    Some(String::from_utf16_lossy(&units))
}

fn read_u8(cursor: &mut io::Cursor<&[u8]>) -> Result<u8, NbtError> {
    let mut byte = [0u8; 1];
    cursor.read_exact(&mut byte).map_err(read_error)?;
    Ok(byte[0])
}

fn read_i8(cursor: &mut io::Cursor<&[u8]>) -> Result<i8, NbtError> {
    Ok(read_u8(cursor)? as i8)
}

fn read_u16(cursor: &mut io::Cursor<&[u8]>) -> Result<u16, NbtError> {
    let mut bytes = [0u8; 2];
    cursor.read_exact(&mut bytes).map_err(read_error)?;
    Ok(u16::from_be_bytes(bytes))
}

fn read_i16(cursor: &mut io::Cursor<&[u8]>) -> Result<i16, NbtError> {
    Ok(read_u16(cursor)? as i16)
}

fn read_i32(cursor: &mut io::Cursor<&[u8]>) -> Result<i32, NbtError> {
    let mut bytes = [0u8; 4];
    cursor.read_exact(&mut bytes).map_err(read_error)?;
    Ok(i32::from_be_bytes(bytes))
}

fn read_i64(cursor: &mut io::Cursor<&[u8]>) -> Result<i64, NbtError> {
    let mut bytes = [0u8; 8];
    cursor.read_exact(&mut bytes).map_err(read_error)?;
    Ok(i64::from_be_bytes(bytes))
}

fn read_error(source: io::Error) -> NbtError {
    NbtError::Invalid(format!("truncated file: {source}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_java_surrogate_pairs() {
        // U+1F600 as two 3-byte halves, the way `DataOutput.writeUTF` stores it.
        let bytes = [0xed, 0xa0, 0xbd, 0xed, 0xb8, 0x80];
        assert_eq!(decode_modified_utf8(&bytes).as_deref(), Some("\u{1F600}"));
        assert_eq!(decode_modified_utf8(&[0xc0, 0x80]).as_deref(), Some("\0"));
        assert_eq!(decode_modified_utf8(&[0xff]), None);
    }

    #[test]
    fn round_trips() {
        let root = Tag::Compound(vec![
            ("name".to_string(), Tag::String("hello".to_string())),
            ("flag".to_string(), Tag::Byte(1)),
            (
                "list".to_string(),
                Tag::List(vec![
                    Tag::Compound(vec![("id".to_string(), Tag::Int(7))]),
                    Tag::Compound(vec![("id".to_string(), Tag::Int(8))]),
                ]),
            ),
        ]);
        let bytes = to_bytes(&root).unwrap();
        assert_eq!(from_bytes(&bytes).unwrap(), root);
    }

    #[test]
    fn rejects_truncated_files() {
        assert!(from_bytes(&[10, 0, 0, 8, 0, 4]).is_err());
    }

    #[test]
    fn round_trips_modified_utf8() {
        for value in ["hello", "名前", "emoji \u{1f600}", "nul\0inside"] {
            let root = Tag::Compound(vec![("name".to_string(), Tag::String(value.to_string()))]);
            let bytes = to_bytes(&root).unwrap();
            assert_eq!(from_bytes(&bytes).unwrap(), root);
        }
    }

    #[test]
    fn reads_java_encoded_surrogate_pairs() {
        // "OfflinePlayer:\u{1f600}" as Java writes it: the emoji is two 3-byte
        // surrogate halves, not a 4-byte UTF-8 sequence.
        let name = "a\u{1f600}";
        let mut raw = vec![b'a'];
        for unit in name.encode_utf16().skip(1) {
            raw.push(0xe0 | (unit >> 12) as u8);
            raw.push(0x80 | ((unit >> 6) & 0x3f) as u8);
            raw.push(0x80 | (unit & 0x3f) as u8);
        }
        let mut utf = Vec::new();
        utf.extend_from_slice(&(raw.len() as u16).to_be_bytes());
        utf.extend_from_slice(&raw);
        let mut bytes = vec![8u8, 0, 0];
        bytes.extend_from_slice(&utf);
        assert_eq!(from_bytes(&bytes).unwrap(), Tag::String(name.to_string()));
    }

    #[test]
    fn rejects_strings_longer_than_16_bits() {
        let long = "a".repeat(65_536);
        assert!(to_bytes(&Tag::Compound(vec![("name".to_string(), Tag::String(long))])).is_err());
    }
}
