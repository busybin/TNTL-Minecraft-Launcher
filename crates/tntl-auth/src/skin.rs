//! Skin image checks and local skin files.

use std::io::Cursor;
use std::path::Path;

use png::{BitDepth, ColorType, Decoder, Encoder, Transformations};

use crate::AuthError;
use crate::store::{cape_path, skin_path, skins_dir};

/// Width and height from a PNG header.
pub fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if bytes.len() < 24 || bytes[..8] != SIGNATURE || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    (width > 0 && height > 0).then_some((width, height))
}

/// A Minecraft skin is 64x64 (or an HD multiple) and may be the legacy
/// 64x32 layout.
pub fn validate_skin(bytes: &[u8]) -> Result<(u32, u32), AuthError> {
    let (width, height) =
        png_dimensions(bytes).ok_or_else(|| AuthError::InvalidSkin("the file is not a PNG image".to_string()))?;
    if width % 64 != 0 || width > 1024 {
        return Err(AuthError::InvalidSkin(format!(
            "width {width} is not a multiple of 64 (64, 128, 256 ... 1024)"
        )));
    }
    if height != width && height.checked_mul(2) != Some(width) {
        return Err(AuthError::InvalidSkin(format!(
            "{width}x{height} is neither square nor the legacy 64x32 layout"
        )));
    }
    Ok((width, height))
}

/// A skin in the modern square layout plus the arm model its pixels imply.
pub struct PreparedSkin {
    pub png: Vec<u8>,
    /// `None` when the arms are not painted in either layout.
    pub slim: Option<bool>,
}

/// Alpha at or above this level counts as painted. The model preview cuts
/// below the same value, so anything less is a hole there.
const PAINTED: u8 = 100;

/// Decode the texture, turn a legacy 64x32 skin into the square layout and read
/// the arm model the pixels are drawn for.
pub fn prepare_skin(bytes: &[u8]) -> Result<PreparedSkin, AuthError> {
    let (width, height) = validate_skin(bytes)?;
    let pixels = decode_png(bytes, width, height)?;
    if height * 2 != width {
        return Ok(PreparedSkin {
            slim: detect_slim(&pixels, width),
            png: bytes.to_vec(),
        });
    }
    let pixels = mirror_legacy(&pixels, width);
    let slim = detect_slim(&pixels, width);
    Ok(PreparedSkin {
        png: encode_png(&pixels, width, width)?,
        slim,
    })
}

/// The arm blocks of a 64-grid skin: right arm, right sleeve, left arm, left
/// sleeve. Legs and the body use the same width in both models.
const ARM_BLOCKS: [(u32, u32); 4] = [(40, 16), (40, 32), (32, 48), (48, 48)];

/// The two textures an offline resource pack needs: one whose arm UVs match the
/// wide model and one for slim. `model_slim` is the fallback when the skin does
/// not paint its arms, so the pixels cannot say which layout was drawn.
///
/// The game picks one of the nine default profiles by UUID; `alex` is slim and
/// the rest are wide. Writing the same wide image to every path made one skin
/// in nine render on the 3px arm model, with a transparent strip on the back.
pub fn arm_variants(png: &[u8], model_slim: bool) -> Result<(Vec<u8>, Vec<u8>), AuthError> {
    let prepared = prepare_skin(png)?;
    let source_slim = prepared.slim.unwrap_or(model_slim);
    if !source_slim {
        let slim = repack_arms(&prepared.png, false)?;
        return Ok((prepared.png, slim));
    }
    let wide = repack_arms(&prepared.png, true)?;
    Ok((wide, prepared.png))
}

/// Copy every arm block into the other model's layout. The front and back faces
/// are 4px wide on the wide model and 3px on slim, so their columns are
/// resampled; the two side faces keep all four columns.
fn repack_arms(png: &[u8], source_slim: bool) -> Result<Vec<u8>, AuthError> {
    let (width, height) = validate_skin(png)?;
    if width != height {
        return Err(AuthError::InvalidSkin("the skin is not square".to_string()));
    }
    let step = width / 64;
    let mut pixels = decode_png(png, width, height)?;
    for (x, y) in ARM_BLOCKS {
        let block = arm_block(&pixels, width, step, x, y);
        clear_block(&mut pixels, width, step, x, y);
        if source_slim {
            slim_to_wide(&block, &mut pixels, width, step, x, y);
        } else {
            wide_to_slim(&block, &mut pixels, width, step, x, y);
        }
    }
    encode_png(&pixels, width, height)
}

/// Copy one 16x16 arm texture out of the image so the destination can be
/// cleared before the faces are written back.
fn arm_block(pixels: &[u8], width: u32, step: u32, x: u32, y: u32) -> Vec<u8> {
    let size = 16 * step;
    let mut block = vec![0u8; (size * size * 4) as usize];
    let row_bytes = (size * 4) as usize;
    for row in 0..size {
        let from = (((y * step + row) * width) + x * step) as usize * 4;
        let to = row as usize * row_bytes;
        block[to..to + row_bytes].copy_from_slice(&pixels[from..from + row_bytes]);
    }
    block
}

fn clear_block(pixels: &mut [u8], width: u32, step: u32, x: u32, y: u32) {
    let size = 16 * step;
    let row_bytes = (size * 4) as usize;
    for row in 0..size {
        let at = (((y * step + row) * width) + x * step) as usize * 4;
        pixels[at..at + row_bytes].fill(0);
    }
}

/// Wide: top `(4,0,4x4)`, bottom `(8,0,4x4)`, right/front/left/back `4x12` from
/// `x = 0,4,8,12`. Slim is the same but the front and back strips are 3 wide,
/// so the back starts at `x = 10` instead of `12`.
fn wide_to_slim(src: &[u8], dst: &mut [u8], width: u32, step: u32, x: u32, y: u32) {
    let block = 16 * step;
    copy_face(src, dst, block, width, step, x, y, (4, 0, 4, 4), (4, 0, 3, 4));
    copy_face(src, dst, block, width, step, x, y, (8, 0, 4, 4), (7, 0, 3, 4));
    copy_face(src, dst, block, width, step, x, y, (0, 4, 4, 12), (0, 4, 4, 12));
    copy_face(src, dst, block, width, step, x, y, (4, 4, 4, 12), (4, 4, 3, 12));
    copy_face(src, dst, block, width, step, x, y, (8, 4, 4, 12), (7, 4, 4, 12));
    copy_face(src, dst, block, width, step, x, y, (12, 4, 4, 12), (10, 4, 3, 12));
}

fn slim_to_wide(src: &[u8], dst: &mut [u8], width: u32, step: u32, x: u32, y: u32) {
    let block = 16 * step;
    copy_face(src, dst, block, width, step, x, y, (4, 0, 3, 4), (4, 0, 4, 4));
    copy_face(src, dst, block, width, step, x, y, (7, 0, 3, 4), (8, 0, 4, 4));
    copy_face(src, dst, block, width, step, x, y, (0, 4, 4, 12), (0, 4, 4, 12));
    copy_face(src, dst, block, width, step, x, y, (4, 4, 3, 12), (4, 4, 4, 12));
    copy_face(src, dst, block, width, step, x, y, (7, 4, 4, 12), (8, 4, 4, 12));
    copy_face(src, dst, block, width, step, x, y, (10, 4, 3, 12), (12, 4, 4, 12));
}

/// Copy a face from the isolated block into the image, stretching the columns
/// when the width changes. Rows keep their height.
#[allow(clippy::too_many_arguments)]
fn copy_face(
    src: &[u8],
    dst: &mut [u8],
    block_width: u32,
    image_width: u32,
    step: u32,
    bx: u32,
    by: u32,
    source: (u32, u32, u32, u32),
    target: (u32, u32, u32, u32),
) {
    let (sx, sy, sw, _) = source;
    let (dx, dy, dw, dh) = target;
    let source_width = sw * step;
    let target_width = dw * step;
    for row in 0..dh * step {
        for column in 0..target_width {
            let from_column = if source_width == target_width {
                column
            } else {
                column * (source_width - 1) / (target_width - 1)
            };
            let from = (((sy * step + row) * block_width) + sx * step + from_column) as usize * 4;
            let to = ((((by + dy) * step + row) * image_width) + (bx + dx) * step + column) as usize * 4;
            dst[to..to + 4].copy_from_slice(&src[from..from + 4]);
        }
    }
}

/// Stored skins are always square, so the game and the previews agree.
pub fn read_skin(root: &Path, id: &str) -> Option<Vec<u8>> {
    let bytes = std::fs::read(skin_path(root, id)).ok()?;
    Some(prepare_skin(&bytes).map(|skin| skin.png).unwrap_or(bytes))
}

pub fn write_skin(root: &Path, id: &str, bytes: &[u8]) -> Result<std::path::PathBuf, AuthError> {
    let skin = prepare_skin(bytes)?;
    let dir = skins_dir(root);
    std::fs::create_dir_all(&dir).map_err(|error| crate::io_error(&dir, error))?;
    let path = skin_path(root, id);
    // This file is the only copy of an offline account's skin; never truncate
    // it in place.
    tntl_store::atomic_write(&path, &skin.png).map_err(|error| AuthError::InvalidSkin(error.to_string()))?;
    Ok(path)
}

pub fn remove_skin(root: &Path, id: &str) {
    let _ = std::fs::remove_file(skin_path(root, id));
}

pub fn read_cape(root: &Path, id: &str) -> Option<Vec<u8>> {
    std::fs::read(cape_path(root, id)).ok()
}

pub fn write_cape(root: &Path, id: &str, bytes: &[u8]) -> Result<std::path::PathBuf, AuthError> {
    let dir = skins_dir(root);
    std::fs::create_dir_all(&dir).map_err(|error| crate::io_error(&dir, error))?;
    let path = cape_path(root, id);
    tntl_store::atomic_write(&path, bytes).map_err(|error| AuthError::InvalidSkin(error.to_string()))?;
    Ok(path)
}

fn invalid_skin(error: impl std::fmt::Display) -> AuthError {
    AuthError::InvalidSkin(format!("cannot read the skin: {error}"))
}

fn decode_png(bytes: &[u8], width: u32, height: u32) -> Result<Vec<u8>, AuthError> {
    let mut decoder = Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(Transformations::normalize_to_color8() | Transformations::ALPHA);
    let mut reader = decoder.read_info().map_err(invalid_skin)?;
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| invalid_skin("the image exceeds the decoder limits"))?;
    let mut buffer = vec![0; size];
    let info = reader.next_frame(&mut buffer).map_err(invalid_skin)?;
    if info.width != width || info.height != height {
        return Err(AuthError::InvalidSkin(
            "the PNG header and the image disagree".to_string(),
        ));
    }
    let data = &buffer[..info.buffer_size()];
    Ok(match info.color_type {
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
        // `ALPHA` expands palettes, so this cannot come out of the decoder.
        ColorType::Indexed => return Err(invalid_skin("unexpanded palette")),
    })
}

fn encode_png(pixels: &[u8], width: u32, height: u32) -> Result<Vec<u8>, AuthError> {
    let mut out = Vec::new();
    {
        let mut encoder = Encoder::new(Cursor::new(&mut out), width, height);
        encoder.set_color(ColorType::Rgba);
        encoder.set_depth(BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(invalid_skin)?;
        writer.write_image_data(pixels).map_err(invalid_skin)?;
    }
    Ok(out)
}

/// Legacy skins are half as tall: the left leg and the left arm have no texture
/// of their own and Minecraft mirrors the right ones into the empty blocks.
fn mirror_legacy(pixels: &[u8], width: u32) -> Vec<u8> {
    let mut square = vec![0u8; (width * width * 4) as usize];
    let used = (width * width / 2 * 4) as usize;
    square[..used].copy_from_slice(&pixels[..used]);
    // (source x, source y, destination x, destination y) in 64-grid pixels.
    mirror_block(pixels, &mut square, width, 0, 16, 16, 48);
    mirror_block(pixels, &mut square, width, 40, 16, 32, 48);
    square
}

fn mirror_block(source: &[u8], target: &mut [u8], width: u32, sx: u32, sy: u32, dx: u32, dy: u32) {
    let step = width / 64;
    let size = 16 * step;
    for row in 0..size {
        for column in 0..size {
            let from = (((sy * step + row) * width) + sx * step + column) as usize * 4;
            let to = (((dy * step + row) * width) + dx * step + (size - 1 - column)) as usize * 4;
            target[to..to + 4].copy_from_slice(&source[from..from + 4]);
        }
    }
}

/// The wide layout paints two more pixel columns per arm (the outer half of the
/// back face); a slim texture leaves them empty, which is why rendering one as
/// wide shows a transparent strip. `None` means neither arm is painted.
fn detect_slim(pixels: &[u8], width: u32) -> Option<bool> {
    let step = width / 64;
    let mut painted = 0;
    let mut edge = 0;
    // The base block of each arm, rows 4..16 of it are the front/side faces.
    for (x, y) in [(40u32, 16u32), (32, 48)] {
        for row in 4..16 {
            for column in 0..16 {
                let alpha = pixels[(((y + row) * step * width) + (x + column) * step) as usize * 4 + 3];
                if alpha < PAINTED {
                    continue;
                }
                painted += 1;
                if column >= 14 {
                    edge += 1;
                }
            }
        }
    }
    if painted == 0 {
        return None;
    }
    // 2 arms x 12 rows x 2 columns are the wide-only area.
    Some(edge * 4 < 48)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13];
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes
    }

    /// A real skin: `paint` returns the colour of a 64-grid pixel, or nothing.
    fn skin(width: u32, height: u32, paint: impl Fn(u32, u32) -> Option<[u8; 4]>) -> Vec<u8> {
        let step = width / 64;
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        for y in 0..64 {
            for x in 0..64 {
                let Some(colour) = paint(x, y) else {
                    continue;
                };
                for sy in 0..step {
                    for sx in 0..step {
                        let at = ((((y * step + sy) * width) + x * step + sx) as usize) * 4;
                        pixels[at..at + 4].copy_from_slice(&colour);
                    }
                }
            }
        }
        encode_png(&pixels, width, height).unwrap()
    }

    fn rect(x: u32, y: u32, w: u32, h: u32) -> impl Fn(u32, u32) -> bool {
        move |px, py| px >= x && px < x + w && py >= y && py < y + h
    }

    /// All six base faces of an arm, 16 wide starting at `x`, 16 tall at `y`.
    fn wide_arm(x: u32, y: u32) -> impl Fn(u32, u32) -> bool {
        let top = rect(x + 4, y, 4, 4);
        let bottom = rect(x + 8, y, 4, 4);
        let sides = rect(x, y + 4, 16, 12);
        move |px, py| top(px, py) || bottom(px, py) || sides(px, py)
    }

    fn slim_arm(x: u32, y: u32) -> impl Fn(u32, u32) -> bool {
        let top = rect(x + 4, y, 3, 4);
        let bottom = rect(x + 7, y, 3, 4);
        let sides = rect(x, y + 4, 12, 12);
        move |px, py| top(px, py) || bottom(px, py) || sides(px, py)
    }

    #[test]
    fn accepts_square_and_legacy_skins() {
        assert_eq!(validate_skin(&png(64, 64)).unwrap(), (64, 64));
        assert_eq!(validate_skin(&png(64, 32)).unwrap(), (64, 32));
        assert_eq!(validate_skin(&png(128, 128)).unwrap(), (128, 128));
    }

    #[test]
    fn rejects_bad_images() {
        assert!(validate_skin(b"not a png").is_err());
        assert!(validate_skin(&png(80, 80)).is_err());
        assert!(validate_skin(&png(64, 20)).is_err());
        assert!(prepare_skin(b"not a png").is_err());
    }

    #[test]
    fn mirrors_the_missing_left_limbs_of_legacy_skins() {
        let arm = wide_arm(40, 16);
        let leg = wide_arm(0, 16);
        let bytes = skin(64, 32, |x, y| {
            if arm(x, y) {
                Some([200, 40, 40, 255])
            } else if leg(x, y) {
                Some([40, 40, 200, 255])
            } else {
                None
            }
        });
        let prepared = prepare_skin(&bytes).unwrap();
        assert_eq!(png_dimensions(&prepared.png), Some((64, 64)));
        assert_eq!(prepared.slim, Some(false));

        let (width, height) = (64u32, 64u32);
        let pixels = decode_png(&prepared.png, width, height).unwrap();
        let at = |x: u32, y: u32| {
            let i = ((y * width + x) as usize) * 4;
            [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
        };
        // Left arm block (32, 48) is the right arm block (40, 16) mirrored.
        assert_eq!(at(46, 52), [200, 40, 40, 255]);
        assert_eq!(at(47, 63), [200, 40, 40, 255]);
        assert_eq!(at(32, 48), [0, 0, 0, 0]);
        // Left leg block (16, 48) is the right leg block (0, 16) mirrored.
        assert_eq!(at(31, 52), [40, 40, 200, 255]);
        assert_eq!(at(16, 63), [40, 40, 200, 255]);
        // The overlay rows stay empty: legacy skins have no sleeves or pants.
        assert_eq!(at(40, 36), [0, 0, 0, 0]);
    }

    #[test]
    fn expands_hd_legacy_skins() {
        let arm = wide_arm(40, 16);
        let bytes = skin(128, 64, |x, y| arm(x, y).then_some([10, 20, 30, 255]));
        let prepared = prepare_skin(&bytes).unwrap();
        assert_eq!(png_dimensions(&prepared.png), Some((128, 128)));
        let pixels = decode_png(&prepared.png, 128, 128).unwrap();
        // The mirrored block starts at (64, 96); (64, 108) is the right edge of
        // the source block (111, 44), which the painter filled in.
        let at = |x: u32, y: u32| pixels[((y * 128 + x) as usize) * 4 + 3];
        assert_eq!(at(64, 108), 255);
        assert_eq!(at(63, 108), 0);
    }

    #[test]
    fn keeps_modern_skins_untouched() {
        let bytes = skin(64, 64, |x, y| wide_arm(40, 16)(x, y).then_some([1, 2, 3, 255]));
        let prepared = prepare_skin(&bytes).unwrap();
        assert_eq!(prepared.png, bytes);
        assert_eq!(prepared.slim, Some(false));
    }

    #[test]
    fn repacks_arms_for_the_other_model() {
        let alpha = |png: &[u8], x: u32, y: u32| decode_png(png, 64, 64).unwrap()[((y * 64 + x) as usize) * 4 + 3];

        // Wide front face (44..48) painted; left face stays empty.
        let wide = skin(64, 64, |x, y| {
            (rect(44, 20, 4, 12)(x, y) || rect(52, 20, 4, 12)(x, y)).then_some([200, 10, 10, 255])
        });
        let (wide_png, slim_png) = arm_variants(&wide, false).unwrap();
        assert_eq!(wide_png, wide);
        assert_eq!(alpha(&slim_png, 46, 20), 255);
        assert_eq!(alpha(&slim_png, 47, 20), 0);

        // Slim front face (44..47) painted; the wide layout gets a fourth column.
        let slim = skin(64, 64, |x, y| rect(44, 20, 3, 12)(x, y).then_some([10, 10, 200, 255]));
        let (wide_png, slim_png) = arm_variants(&slim, true).unwrap();
        assert_eq!(slim_png, slim);
        assert_eq!(alpha(&wide_png, 47, 20), 255);
        assert_eq!(alpha(&wide_png, 48, 20), 0);
    }

    #[test]
    fn reads_the_arm_model_from_the_texture() {
        let wide = skin(64, 64, |x, y| {
            (wide_arm(40, 16)(x, y) || wide_arm(32, 48)(x, y)).then_some([1, 2, 3, 255])
        });
        assert_eq!(prepare_skin(&wide).unwrap().slim, Some(false));

        let slim = skin(64, 64, |x, y| {
            (slim_arm(40, 16)(x, y) || slim_arm(32, 48)(x, y)).then_some([1, 2, 3, 255])
        });
        assert_eq!(prepare_skin(&slim).unwrap().slim, Some(true));

        let bare = skin(64, 64, |_, _| None);
        assert_eq!(prepare_skin(&bare).unwrap().slim, None);
    }
}
