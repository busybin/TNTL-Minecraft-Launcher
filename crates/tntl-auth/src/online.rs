//! Public skin lookups: Mojang first, ely.by as a fallback, plus a browsable
//! gallery of recently uploaded textures.
//!
//! Mojang is the authoritative source for premium names and answers with a
//! signed profile property; ely.by covers its own accounts and usually proxies
//! premium names too, but it rate limits, so it is only the fallback.
//! See <https://docs.ely.by/en/skins-system.html> and
//! <https://wiki.vg/Mojang_API>.

use serde::Deserialize;
use tntl_source::{Http, SourceError};

use crate::{AuthError, prepare_skin, valid_username};

const MOJANG_PROFILE: &str = "https://api.mojang.com/users/profiles/minecraft";
const MOJANG_SESSION: &str = "https://sessionserver.mojang.com/session/minecraft/profile";
const ELY_TEXTURES: &str = "https://skinsystem.ely.by/textures";
const MINESKIN_LIST: &str = "https://api.mineskin.org/get/list";

/// Which service the texture came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkinSource {
    /// Mojang first, ely.by when Mojang has no such name.
    Auto,
    Mojang,
    Ely,
}

impl SkinSource {
    pub fn from_key(key: &str) -> Option<Self> {
        match key.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => Some(Self::Auto),
            "mojang" => Some(Self::Mojang),
            "ely" | "ely.by" => Some(Self::Ely),
            _ => None,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Mojang => "mojang",
            Self::Ely => "ely",
        }
    }
}

#[derive(Debug, Clone)]
pub struct OnlineSkin {
    pub nickname: String,
    /// The name Minecraft uses, which can differ in case from the query.
    pub name: String,
    pub skin: Vec<u8>,
    /// `slim` when the source publishes `metadata.model = slim`.
    pub slim: bool,
    /// Human readable origin, e.g. `Mojang` or `ely.by`.
    pub source: String,
}

/// One texture from the gallery.
#[derive(Debug, Clone)]
pub struct GallerySkin {
    pub id: i64,
    pub skin: Vec<u8>,
    /// Upload time, seconds since the Unix epoch.
    pub time: u64,
    /// Arm model the upload was tagged with, when the list reports it.
    pub slim: Option<bool>,
}

/// One page of the gallery plus the cursor that continues it.
#[derive(Debug, Clone)]
pub struct GalleryPage {
    pub skins: Vec<GallerySkin>,
    /// Pass back as `after` to get the next page; `None` on the last page.
    pub next: Option<String>,
}

/// The lists MineSkin exposes. Only `Latest` is documented; the rest are the
/// `filter` values its own front end uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GalleryFilter {
    Latest,
    Top,
    Staff,
    Slim,
    Hd,
    Random,
}

impl GalleryFilter {
    pub fn from_key(key: &str) -> Option<Self> {
        match key.trim().to_ascii_lowercase().as_str() {
            "" | "latest" | "new" => Some(Self::Latest),
            "top" | "popular" => Some(Self::Top),
            "staff" | "featured" => Some(Self::Staff),
            "slim" => Some(Self::Slim),
            "hd" => Some(Self::Hd),
            "random" => Some(Self::Random),
            _ => None,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Latest => "latest",
            Self::Top => "top",
            Self::Staff => "staff",
            Self::Slim => "slim",
            Self::Hd => "hd",
            Self::Random => "random",
        }
    }

    fn query(self) -> Option<&'static str> {
        match self {
            Self::Latest => None,
            other => Some(other.key()),
        }
    }
}

/// Look a name up with the requested source, trying Mojang first by default.
pub fn online_skin(http: &Http, nickname: &str, source: SkinSource) -> Result<OnlineSkin, AuthError> {
    let nickname = nickname.trim();
    if !valid_username(nickname) {
        return Err(AuthError::InvalidName(nickname.to_string()));
    }
    match source {
        SkinSource::Mojang => mojang_skin(http, nickname),
        SkinSource::Ely => ely_skin(http, nickname),
        SkinSource::Auto => match mojang_skin(http, nickname) {
            Ok(found) => Ok(found),
            // A real Mojang account without a skin is still a match; asking ely
            // would only hide it behind "no skin published".
            Err(AuthError::NoOnlineSkin(_)) => ely_skin(http, nickname),
            Err(error) => Err(error),
        },
    }
}

#[derive(Deserialize)]
struct MojangProfile {
    id: String,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
struct SessionProfile {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    properties: Vec<Property>,
}

#[derive(Deserialize)]
struct Property {
    name: String,
    value: String,
}

#[derive(Deserialize)]
struct TexturesProperty {
    #[serde(default)]
    textures: Textures,
}

/// Mojang's profile property: `{"textures":{"SKIN":…}}`.
#[derive(Default, Deserialize)]
struct Textures {
    #[serde(rename = "SKIN", default)]
    skin: Option<TextureRef>,
}

/// ely.by answers flat: `{"SKIN":…}`.
#[derive(Deserialize)]
struct FlatTextures {
    #[serde(rename = "SKIN")]
    skin: Option<TextureRef>,
}

#[derive(Deserialize)]
struct TextureRef {
    url: String,
    #[serde(default)]
    metadata: Option<TextureModel>,
}

#[derive(Deserialize)]
struct TextureModel {
    model: Option<String>,
}

/// Mojang's own APIs: name → UUID → signed profile → texture.
pub fn mojang_skin(http: &Http, nickname: &str) -> Result<OnlineSkin, AuthError> {
    if !valid_username(nickname) {
        return Err(AuthError::InvalidName(nickname.to_string()));
    }
    let profile: MojangProfile = mojang_get(http, &format!("{MOJANG_PROFILE}/{nickname}"), nickname)?;
    let session: SessionProfile = mojang_get(http, &format!("{MOJANG_SESSION}/{}", profile.id), nickname)?;
    let name = session.name.or(profile.name).unwrap_or_else(|| nickname.to_string());
    let property = session
        .properties
        .into_iter()
        .find(|entry| entry.name == "textures")
        .ok_or_else(|| AuthError::NoOnlineSkin(nickname.to_string()))?;
    let decoded = decode_base64(&property.value)
        .ok_or_else(|| AuthError::Api("cannot decode the Mojang texture property".to_string()))?;
    let property: TexturesProperty = serde_json::from_slice(&decoded)
        .map_err(|error| AuthError::Api(format!("invalid Mojang texture property: {error}")))?;
    let texture = property
        .textures
        .skin
        .ok_or_else(|| AuthError::NoOnlineSkin(nickname.to_string()))?;
    let png = http.get_bytes(&https(&texture.url), 2 * 1024 * 1024)?;
    let skin = prepare_skin(&png)?;
    Ok(OnlineSkin {
        nickname: nickname.to_string(),
        name,
        skin: skin.png,
        slim: metadata_model(&texture).or(skin.slim).unwrap_or(false),
        source: "Mojang".to_string(),
    })
}

fn mojang_get<T: serde::de::DeserializeOwned>(http: &Http, url: &str, nickname: &str) -> Result<T, AuthError> {
    match http.get_json::<T>(url) {
        Ok(value) => Ok(value),
        Err(SourceError::Status { status: 204 | 404, .. }) => Err(AuthError::NoOnlineSkin(nickname.to_string())),
        Err(error) => Err(error.into()),
    }
}

/// ely.by's textures endpoint, which also proxies most premium names.
pub fn ely_skin(http: &Http, nickname: &str) -> Result<OnlineSkin, AuthError> {
    if !valid_username(nickname) {
        return Err(AuthError::InvalidName(nickname.to_string()));
    }
    let url = format!("{ELY_TEXTURES}/{nickname}");
    let body = match http.get_bytes(&url, 64 * 1024) {
        Ok(body) => body,
        // 204 means "nothing published", which the docs use instead of 404.
        Err(SourceError::Status { status: 204 | 404, .. }) => {
            return Err(AuthError::NoOnlineSkin(nickname.to_string()));
        }
        Err(error) => return Err(error.into()),
    };
    if body.is_empty() {
        return Err(AuthError::NoOnlineSkin(nickname.to_string()));
    }
    let textures: FlatTextures =
        serde_json::from_slice(&body).map_err(|error| AuthError::Api(format!("invalid ely.by response: {error}")))?;
    let texture = textures
        .skin
        .ok_or_else(|| AuthError::NoOnlineSkin(nickname.to_string()))?;
    let png = http.get_bytes(&https(&texture.url), 2 * 1024 * 1024)?;
    let skin = prepare_skin(&png)?;
    Ok(OnlineSkin {
        nickname: nickname.to_string(),
        name: nickname.to_string(),
        skin: skin.png,
        slim: metadata_model(&texture).or(skin.slim).unwrap_or(false),
        source: "ely.by".to_string(),
    })
}

/// One page of the public gallery.
///
/// MineSkin dropped `?page=N` (it answers the first page forever), so paging
/// walks a cursor: `start` for the first page, then the uuid of the last entry.
/// See <https://api.mineskin.org/openapi.yml>.
pub fn gallery(
    http: &Http,
    filter: GalleryFilter,
    after: Option<&str>,
    count: usize,
) -> Result<GalleryPage, AuthError> {
    #[derive(Deserialize)]
    struct List {
        #[serde(default)]
        skins: Vec<Entry>,
    }
    #[derive(Deserialize)]
    struct Entry {
        id: i64,
        #[serde(default)]
        url: String,
        #[serde(default)]
        uuid: String,
        #[serde(default)]
        time: f64,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        variant: Option<String>,
    }
    let cursor = after
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("start");
    let mut url = format!("{MINESKIN_LIST}/{cursor}");
    if let Some(query) = filter.query() {
        url.push_str("?filter=");
        url.push_str(query);
    }
    let list: List = http.get_json(&url)?;

    let mut skins = Vec::new();
    let mut next = None;
    let last = list.skins.len();
    for (index, entry) in list.skins.into_iter().enumerate() {
        // The cursor follows the last entry we looked at, kept or not, so a
        // page never repeats.
        if index + 1 == last {
            let uuid = entry.uuid.trim();
            if !uuid.is_empty() {
                next = Some(uuid.to_string());
            }
        }
        if skins.len() >= count {
            continue;
        }
        let Ok(png) = http.get_bytes(&https(&entry.url), 2 * 1024 * 1024) else {
            continue;
        };
        let Ok(skin) = prepare_skin(&png) else {
            continue;
        };
        // Most uploads come back as `unknown`; the pixels decide then.
        let slim = model_flag(entry.variant.as_deref().or(entry.model.as_deref())).or(skin.slim);
        skins.push(GallerySkin {
            id: entry.id,
            skin: skin.png,
            time: entry.time.max(0.0) as u64,
            slim,
        });
    }
    Ok(GalleryPage { skins, next })
}

/// The arm model a source reports, or `None` when it does not really know.
fn model_flag(value: Option<&str>) -> Option<bool> {
    let value = value?.trim();
    if value.eq_ignore_ascii_case("slim") || value.eq_ignore_ascii_case("alex") {
        Some(true)
    } else if value.eq_ignore_ascii_case("steve")
        || value.eq_ignore_ascii_case("classic")
        || value.eq_ignore_ascii_case("wide")
    {
        Some(false)
    } else {
        None
    }
}

fn metadata_model(texture: &TextureRef) -> Option<bool> {
    model_flag(texture.metadata.as_ref()?.model.as_deref())
}

/// Texture URLs come back as plain `http://`, which some networks block.
fn https(url: &str) -> String {
    match url.strip_prefix("http://") {
        Some(rest) => format!("https://{rest}"),
        None => url.to_string(),
    }
}

/// Standard base64 with padding; the texture property has no line breaks.
fn decode_base64(text: &str) -> Option<Vec<u8>> {
    fn value(byte: u8) -> Option<u32> {
        Some(match byte {
            b'A'..=b'Z' => (byte - b'A') as u32,
            b'a'..=b'z' => (byte - b'a') as u32 + 26,
            b'0'..=b'9' => (byte - b'0') as u32 + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    }
    let mut bytes = Vec::with_capacity(text.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for byte in text.bytes() {
        if byte == b'=' {
            break;
        }
        if byte.is_ascii_whitespace() {
            continue;
        }
        buffer = (buffer << 6) | value(byte)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push((buffer >> bits) as u8);
        }
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upgrades_plain_http_texture_urls() {
        assert_eq!(https("http://example.com/a.png"), "https://example.com/a.png");
        assert_eq!(https("https://example.com/a.png"), "https://example.com/a.png");
    }

    #[test]
    fn reads_slim_from_texture_metadata() {
        let body = br#"{"SKIN":{"url":"http://x/a.png","metadata":{"model":"slim"}}}"#;
        let textures: FlatTextures = serde_json::from_slice(body).unwrap();
        assert_eq!(metadata_model(&textures.skin.unwrap()), Some(true));
    }

    #[test]
    fn ignores_unknown_arm_models() {
        assert_eq!(model_flag(Some("slim")), Some(true));
        assert_eq!(model_flag(Some("alex")), Some(true));
        assert_eq!(model_flag(Some("classic")), Some(false));
        assert_eq!(model_flag(Some("steve")), Some(false));
        assert_eq!(model_flag(Some("unknown")), None);
        assert_eq!(model_flag(None), None);
    }

    #[test]
    fn decodes_the_mojang_texture_property() {
        let encoded = "eyJ0ZXh0dXJlcyI6eyJTS0lOIjp7InVybCI6Imh0dHA6Ly94L2EucG5nIn19fQ==";
        let decoded = decode_base64(encoded).unwrap();
        assert_eq!(
            std::str::from_utf8(&decoded).unwrap(),
            r#"{"textures":{"SKIN":{"url":"http://x/a.png"}}}"#
        );
    }

    #[test]
    fn parses_the_texture_property() {
        let property: TexturesProperty =
            serde_json::from_slice(br#"{"textures":{"SKIN":{"url":"http://x/a.png"}}}"#).unwrap();
        assert_eq!(property.textures.skin.unwrap().url, "http://x/a.png");
    }

    #[test]
    fn skin_sources_round_trip() {
        for key in ["", "auto", "mojang", "ely", "ely.by"] {
            let source = SkinSource::from_key(key).unwrap();
            assert_eq!(SkinSource::from_key(source.key()), Some(source));
        }
        assert!(SkinSource::from_key("namemc").is_none());
    }
}
