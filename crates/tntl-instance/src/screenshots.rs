//! Screenshots written by the game into `screenshots/`.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use png::{BitDepth, ColorType, Decoder, Encoder, Transformations};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;
/// The compressed size says nothing about the decompressed one: a tiny PNG can
/// declare 20000x20000. Cap the pixels before allocating the frame buffer.
const MAX_IMAGE_PIXELS: u64 = 24 * 1024 * 1024;
/// Thumbnails are small enough to keep a whole album in the webview.
const THUMBNAIL_WIDTH: u32 = 480;

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
    #[error("cannot read {file}: {message}")]
    Image { file: String, message: String },
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
    if !tntl_store::valid_file_name(file) {
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

fn image_error(file: &str, error: impl std::fmt::Display) -> ScreenshotError {
    ScreenshotError::Image {
        file: file.to_string(),
        message: error.to_string(),
    }
}

/// Decode a screenshot and scale it down to `max_width`. The album never needs
/// the 4K original, and shipping every full PNG over IPC stalls the webview.
pub fn default_thumbnail_width() -> u32 {
    THUMBNAIL_WIDTH
}

pub fn thumbnail(game_dir: &Path, file: &str, max_width: u32) -> Result<Vec<u8>, ScreenshotError> {
    let bytes = read(game_dir, file)?;
    let mut decoder = Decoder::new(Cursor::new(&bytes));
    decoder.set_transformations(Transformations::normalize_to_color8() | Transformations::ALPHA);
    let mut reader = decoder.read_info().map_err(|error| image_error(file, error))?;
    let declared = reader.info();
    if declared.width == 0
        || declared.height == 0
        || u64::from(declared.width) * u64::from(declared.height) > MAX_IMAGE_PIXELS
    {
        return Err(image_error(file, "the image is too large"));
    }
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| image_error(file, "the image exceeds the decoder limits"))?;
    let mut buffer = vec![0; size];
    let info = reader
        .next_frame(&mut buffer)
        .map_err(|error| image_error(file, error))?;
    let (width, height) = (info.width, info.height);
    if width == 0 || height == 0 {
        return Err(image_error(file, "the image has no pixels"));
    }

    let limit = max_width.clamp(32, 1280);
    if width <= limit {
        return Ok(bytes);
    }

    let pixels = rgba8(&buffer[..info.buffer_size()], info.color_type)
        .ok_or_else(|| image_error(file, "unexpected color type"))?;
    let target_width = limit;
    let target_height = ((height as u64 * target_width as u64 + width as u64 / 2) / width as u64).max(1) as u32;
    let scaled = box_resize(&pixels, width, height, target_width, target_height);
    encode_png(&scaled, target_width, target_height).map_err(|error| image_error(file, error))
}

fn rgba8(data: &[u8], color: ColorType) -> Option<Vec<u8>> {
    Some(match color {
        ColorType::Rgba => data.to_vec(),
        ColorType::Rgb => data
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 0xff])
            .collect(),
        ColorType::GrayscaleAlpha => data
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        ColorType::Grayscale => data.iter().flat_map(|&v| [v, v, v, 0xff]).collect(),
        ColorType::Indexed => return None,
    })
}

/// Average every source block into one destination pixel: cheap and avoids the
/// shimmer of nearest-neighbour on 1080p screenshots.
fn box_resize(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let mut out = vec![0u8; (dw as usize) * (dh as usize) * 4];
    for y in 0..dh {
        let y0 = (y as u64 * sh as u64 / dh as u64) as u32;
        let y1 = ((y as u64 + 1) * sh as u64).div_ceil(dh as u64).max(y0 as u64 + 1) as u32;
        for x in 0..dw {
            let x0 = (x as u64 * sw as u64 / dw as u64) as u32;
            let x1 = ((x as u64 + 1) * sw as u64).div_ceil(dw as u64).max(x0 as u64 + 1) as u32;
            let mut sum = [0u32; 4];
            let mut count = 0u32;
            for sy in y0..y1.min(sh) {
                for sx in x0..x1.min(sw) {
                    let offset = ((sy as usize * sw as usize) + sx as usize) * 4;
                    for channel in 0..4 {
                        sum[channel] += u32::from(src[offset + channel]);
                    }
                    count += 1;
                }
            }
            let offset = ((y as usize * dw as usize) + x as usize) * 4;
            for channel in 0..4 {
                out[offset + channel] = (sum[channel] / count.max(1)) as u8;
            }
        }
    }
    out
}

fn encode_png(pixels: &[u8], width: u32, height: u32) -> Result<Vec<u8>, png::EncodingError> {
    let mut out = Vec::new();
    {
        let mut encoder = Encoder::new(Cursor::new(&mut out), width, height);
        encoder.set_color(ColorType::Rgba);
        encoder.set_depth(BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(pixels)?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tntl-shots-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("screenshots")).unwrap();
        dir
    }

    fn write_png(path: &Path, width: u32, height: u32, pixels: &[u8]) {
        let bytes = encode_png(pixels, width, height).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn decode(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
        let mut decoder = Decoder::new(Cursor::new(bytes));
        decoder.set_transformations(Transformations::normalize_to_color8() | Transformations::ALPHA);
        let mut reader = decoder.read_info().unwrap();
        let mut buffer = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buffer).unwrap();
        let pixels = rgba8(&buffer[..info.buffer_size()], info.color_type).unwrap();
        (info.width, info.height, pixels)
    }

    #[test]
    fn lists_reads_and_deletes() {
        let dir = temp_dir("crud");
        fs::write(dir.join("screenshots/a.png"), b"png").unwrap();
        let shots = list(&dir).unwrap();
        assert_eq!(shots.len(), 1);
        assert_eq!(read(&dir, "a.png").unwrap(), b"png");
        assert!(read(&dir, "../a.png").is_err());
        delete(&dir, "a.png").unwrap();
        assert!(list(&dir).unwrap().is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn scales_a_wide_screenshot_down() {
        let dir = temp_dir("thumb");
        // A red/blue checkerboard: every 2x2 source block averages to the same
        // colour, so the assertion checks the box filter rather than sampling.
        let mut pixels = Vec::new();
        for y in 0..4 {
            for x in 0..64 {
                let color = if (x + y) % 2 == 0 {
                    [200, 20, 20, 255]
                } else {
                    [20, 20, 200, 255]
                };
                pixels.extend_from_slice(&color);
            }
        }
        write_png(&dir.join("screenshots/shot.png"), 64, 4, &pixels);
        let thumb = thumbnail(&dir, "shot.png", 32).unwrap();
        let (width, height, scaled) = decode(&thumb);
        assert_eq!((width, height), (32, 2));
        for pixel in scaled.as_chunks::<4>().0 {
            assert_eq!(pixel, &[110, 20, 110, 255]);
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn keeps_small_screenshots_as_is() {
        let dir = temp_dir("small");
        let pixels = vec![7u8; 2 * 2 * 4];
        write_png(&dir.join("screenshots/small.png"), 2, 2, &pixels);
        let original = fs::read(dir.join("screenshots/small.png")).unwrap();
        assert_eq!(thumbnail(&dir, "small.png", 480).unwrap(), original);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rejects_a_png_that_only_claims_to_be_huge() {
        let dir = temp_dir("huge");
        let mut bytes = encode_png(&[0, 0, 0, 255], 1, 1).unwrap();
        // Rewrite the IHDR dimensions and its CRC; no pixel data is needed
        // because the check must happen before the frame buffer is allocated.
        bytes[16..20].copy_from_slice(&20_000u32.to_be_bytes());
        bytes[20..24].copy_from_slice(&20_000u32.to_be_bytes());
        let mut crc = flate2::Crc::new();
        crc.update(&bytes[12..29]);
        bytes[29..33].copy_from_slice(&crc.sum().to_be_bytes());
        fs::write(dir.join("screenshots/huge.png"), bytes).unwrap();
        assert!(matches!(
            thumbnail(&dir, "huge.png", 480),
            Err(ScreenshotError::Image { .. })
        ));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rejects_a_non_png() {
        let dir = temp_dir("broken");
        fs::write(dir.join("screenshots/broken.png"), b"not a png").unwrap();
        assert!(matches!(
            thumbnail(&dir, "broken.png", 480),
            Err(ScreenshotError::Image { .. })
        ));
        fs::remove_dir_all(dir).unwrap();
    }
}
