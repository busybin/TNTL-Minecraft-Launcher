//! Microsoft sign-in through the device code flow, then Xbox Live, XSTS and
//! the Minecraft services API.
//!
//! No secret is stored here: the launcher only needs a public client ID that
//! has the device code flow enabled.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::store::SkinModel;
use crate::{AuthError, io_error, now_secs};

const DEVICE_CODE_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode";
const TOKEN_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/token";
const XBL_URL: &str = "https://user.auth.xboxlive.com/user/authenticate";
const XSTS_URL: &str = "https://xsts.auth.xboxlive.com/xsts/authorize";
const MC_LOGIN_URL: &str = "https://api.minecraftservices.com/authentication/login_with_xbox";
const MC_PROFILE_URL: &str = "https://api.minecraftservices.com/minecraft/profile";
const MC_SKIN_URL: &str = "https://api.minecraftservices.com/minecraft/profile/skins";
const MC_ACTIVE_CAPE_URL: &str = "https://api.minecraftservices.com/minecraft/profile/capes/active";

const SCOPE: &str = "XboxLive.signin offline_access";

/// One device code the user has to type on the Microsoft page.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    /// Seconds to wait between polls.
    pub interval: u64,
    /// Ready-to-show instructions from Microsoft.
    pub message: String,
}

#[derive(Debug)]
pub enum DevicePoll {
    /// The user has not finished yet.
    Pending,
    /// Microsoft asked us to slow down.
    SlowDown,
    Done(Box<MsaSession>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MsaSession {
    /// Minecraft session token.
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds.
    pub expires_at: i64,
    pub profile: MsaProfile,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MsaProfile {
    /// Undashed UUID.
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub skins: Vec<SkinEntry>,
    #[serde(default)]
    pub capes: Vec<CapeEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkinEntry {
    pub id: String,
    pub state: String,
    pub url: String,
    /// `classic` or `slim`.
    #[serde(default)]
    pub variant: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapeEntry {
    pub id: String,
    pub state: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub alias: String,
}

pub fn client() -> Result<reqwest::blocking::Client, AuthError> {
    let builder = reqwest::blocking::Client::builder()
        .user_agent(format!(
            "{}/{}",
            tntl_source::endpoints::LAUNCHER_NAME,
            env!("CARGO_PKG_VERSION_MAJOR")
        ))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(60));
    let builder = tntl_source::network::apply_proxy(builder)?;
    builder
        .build()
        .map_err(|error| AuthError::Api(format!("cannot create an HTTP client: {error}")))
}

fn form_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn describe_failure(label: &str, status: u16, body: &[u8]) -> AuthError {
    let text = String::from_utf8_lossy(body);
    let detail = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|value| {
            ["error_description", "errorMessage", "error", "Message"]
                .iter()
                .find_map(|key| value.get(*key).and_then(Value::as_str).map(str::to_string))
        })
        .unwrap_or_else(|| text.chars().take(300).collect());
    AuthError::Api(format!("{label} failed ({status}): {detail}"))
}

fn xbox_hint(body: &[u8]) -> Option<&'static str> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let code = value.get("XErr").and_then(Value::as_i64)?;
    Some(match code {
        2148916233 => "this Microsoft account has no Xbox profile; sign in once on xbox.com",
        2148916235 => "Xbox Live is not available for this account's region",
        2148916236 | 2148916237 => "this account needs adult verification",
        2148916238 => "this is a child account; an adult has to add it to a family first",
        _ => return None,
    })
}

/// Ask Microsoft for a code the user types at `verification_uri`.
pub fn request_device_code(client_id: &str) -> Result<DeviceCode, AuthError> {
    if client_id.trim().is_empty() {
        return Err(AuthError::MissingClientId);
    }
    let response = client()?
        .post(DEVICE_CODE_URL)
        .header(reqwest::header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(format!(
            "client_id={}&scope={}",
            form_encode(client_id.trim()),
            form_encode(SCOPE)
        ))
        .send()
        .map_err(|error| AuthError::Api(format!("cannot reach Microsoft: {error}")))?;
    let status = response.status();
    let body = response.bytes().unwrap_or_default();
    if !status.is_success() {
        return Err(describe_failure("device code request", status.as_u16(), &body));
    }
    #[derive(Deserialize)]
    struct Raw {
        device_code: String,
        user_code: String,
        verification_uri: String,
        expires_in: u64,
        #[serde(default = "default_interval")]
        interval: u64,
        #[serde(default)]
        message: String,
    }
    fn default_interval() -> u64 {
        5
    }
    let raw: Raw = serde_json::from_slice(&body)
        .map_err(|error| AuthError::Api(format!("unexpected device code response: {error}")))?;
    Ok(DeviceCode {
        device_code: raw.device_code,
        user_code: raw.user_code,
        verification_uri: raw.verification_uri,
        expires_in: raw.expires_in,
        interval: raw.interval.max(1),
        message: raw.message,
    })
}

/// Poll once. `Done` has already walked the Xbox and Minecraft chain.
pub fn poll_device_code(client_id: &str, device_code: &str) -> Result<DevicePoll, AuthError> {
    if client_id.trim().is_empty() {
        return Err(AuthError::MissingClientId);
    }
    let response = client()?
        .post(TOKEN_URL)
        .header(reqwest::header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(format!(
            "client_id={}&grant_type=urn:ietf:params:oauth:grant-type:device_code&device_code={}",
            form_encode(client_id.trim()),
            form_encode(device_code)
        ))
        .send()
        .map_err(|error| AuthError::Api(format!("cannot reach Microsoft: {error}")))?;
    let status = response.status();
    let body = response.bytes().unwrap_or_default();
    if !status.is_success() {
        let error = serde_json::from_slice::<Value>(&body)
            .ok()
            .and_then(|value| value.get("error").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_default();
        return match error.as_str() {
            "authorization_pending" => Ok(DevicePoll::Pending),
            "slow_down" => Ok(DevicePoll::SlowDown),
            "expired_token" => Err(AuthError::Api(
                "the sign-in code expired; start the sign-in again".to_string(),
            )),
            "authorization_declined" => Err(AuthError::Api("the sign-in was declined".to_string())),
            _ => Err(describe_failure("sign-in", status.as_u16(), &body)),
        };
    }
    let token: TokenResponse =
        serde_json::from_slice(&body).map_err(|error| AuthError::Api(format!("unexpected token response: {error}")))?;
    let session = walk_chain(
        &token.access_token,
        token.refresh_token.unwrap_or_default(),
        token.expires_in,
    )?;
    Ok(DevicePoll::Done(Box::new(session)))
}

/// Use a stored refresh token to get a fresh Minecraft session.
pub fn refresh_session(client_id: &str, refresh_token: &str) -> Result<MsaSession, AuthError> {
    if client_id.trim().is_empty() {
        return Err(AuthError::MissingClientId);
    }
    if refresh_token.is_empty() {
        return Err(AuthError::Api(
            "the account has no refresh token; sign in again".to_string(),
        ));
    }
    let response = client()?
        .post(TOKEN_URL)
        .header(reqwest::header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(format!(
            "client_id={}&grant_type=refresh_token&refresh_token={}&scope={}",
            form_encode(client_id.trim()),
            form_encode(refresh_token),
            form_encode(SCOPE)
        ))
        .send()
        .map_err(|error| AuthError::Api(format!("cannot reach Microsoft: {error}")))?;
    let status = response.status();
    let body = response.bytes().unwrap_or_default();
    if !status.is_success() {
        return Err(describe_failure("token refresh", status.as_u16(), &body));
    }
    let token: TokenResponse =
        serde_json::from_slice(&body).map_err(|error| AuthError::Api(format!("unexpected token response: {error}")))?;
    walk_chain(
        &token.access_token,
        token.refresh_token.unwrap_or_else(|| refresh_token.to_string()),
        token.expires_in,
    )
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    #[serde(default = "default_expiry")]
    expires_in: u64,
}

fn default_expiry() -> u64 {
    86_400
}

/// Microsoft token → Xbox Live → XSTS → Minecraft → profile.
fn walk_chain(msa_token: &str, refresh_token: String, expires_in: u64) -> Result<MsaSession, AuthError> {
    let http = client()?;

    let response = http
        .post(XBL_URL)
        .json(&json!({
            "Properties": {
                "AuthMethod": "RPS",
                "SiteName": "user.auth.xboxlive.com",
                "RpsTicket": format!("d={msa_token}"),
            },
            "RelyingParty": "http://auth.xboxlive.com",
            "TokenType": "JWT",
        }))
        .send()
        .map_err(|error| AuthError::Api(format!("cannot reach Xbox Live: {error}")))?;
    let status = response.status();
    let body = response.bytes().unwrap_or_default();
    if !status.is_success() {
        let hint = xbox_hint(&body)
            .map(str::to_string)
            .unwrap_or_else(|| describe_failure("Xbox Live", status.as_u16(), &body).to_string());
        return Err(AuthError::Api(hint));
    }
    let xbl: XblResponse =
        serde_json::from_slice(&body).map_err(|error| AuthError::Api(format!("unexpected Xbox response: {error}")))?;
    let uhs = xbl
        .claims
        .xui
        .first()
        .map(|entry| entry.uhs.clone())
        .ok_or_else(|| AuthError::Api("Xbox Live returned no user hash".to_string()))?;

    let response = http
        .post(XSTS_URL)
        .json(&json!({
            "Properties": {
                "SandboxId": "RETAIL",
                "UserTokens": [xbl.token],
            },
            "RelyingParty": "rp://api.minecraftservices.com/",
            "TokenType": "JWT",
        }))
        .send()
        .map_err(|error| AuthError::Api(format!("cannot reach Xbox Live: {error}")))?;
    let status = response.status();
    let body = response.bytes().unwrap_or_default();
    if !status.is_success() {
        let hint = xbox_hint(&body)
            .map(str::to_string)
            .unwrap_or_else(|| describe_failure("XSTS", status.as_u16(), &body).to_string());
        return Err(AuthError::Api(hint));
    }
    let xsts: XblResponse =
        serde_json::from_slice(&body).map_err(|error| AuthError::Api(format!("unexpected XSTS response: {error}")))?;

    let response = http
        .post(MC_LOGIN_URL)
        .json(&json!({ "identityToken": format!("XBL3.0 x={uhs};{}", xsts.token) }))
        .send()
        .map_err(|error| AuthError::Api(format!("cannot reach Minecraft services: {error}")))?;
    let status = response.status();
    let body = response.bytes().unwrap_or_default();
    if !status.is_success() {
        return Err(describe_failure("Minecraft login", status.as_u16(), &body));
    }
    let minecraft: MinecraftToken = serde_json::from_slice(&body)
        .map_err(|error| AuthError::Api(format!("unexpected Minecraft response: {error}")))?;
    let expires_at = now_secs() + minecraft.expires_in.unwrap_or(expires_in) as i64;

    let profile = fetch_profile(&minecraft.access_token)?;
    Ok(MsaSession {
        access_token: minecraft.access_token,
        refresh_token,
        expires_at,
        profile,
    })
}

#[derive(Deserialize)]
struct XblResponse {
    #[serde(rename = "Token")]
    token: String,
    #[serde(rename = "DisplayClaims")]
    claims: XblClaims,
}

#[derive(Deserialize)]
struct XblClaims {
    #[serde(default)]
    xui: Vec<XblUser>,
}

#[derive(Deserialize)]
struct XblUser {
    uhs: String,
}

#[derive(Deserialize)]
struct MinecraftToken {
    access_token: String,
    #[serde(default)]
    expires_in: Option<u64>,
}

pub fn fetch_profile(token: &str) -> Result<MsaProfile, AuthError> {
    let response = client()?
        .get(MC_PROFILE_URL)
        .bearer_auth(token)
        .send()
        .map_err(|error| AuthError::Api(format!("cannot reach Minecraft services: {error}")))?;
    let status = response.status();
    let body = response.bytes().unwrap_or_default();
    if status.as_u16() == 404 {
        return Err(AuthError::Api(
            "this Microsoft account does not own Minecraft: Java Edition".to_string(),
        ));
    }
    if status.as_u16() == 401 {
        // The stored token is dead even though it has not expired: a clock
        // that moved, or a session revoked elsewhere. Callers refresh and retry.
        return Err(AuthError::Unauthorized);
    }
    if !status.is_success() {
        return Err(describe_failure("profile request", status.as_u16(), &body));
    }
    serde_json::from_slice(&body).map_err(|error| AuthError::Api(format!("unexpected profile response: {error}")))
}

/// Upload a skin to the account that owns `token`.
pub fn upload_skin(token: &str, png: &[u8], model: SkinModel) -> Result<MsaProfile, AuthError> {
    let boundary = format!("----tntl{}", now_secs());
    let mut body = Vec::with_capacity(png.len() + 512);
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"variant\"\r\n\r\n").as_bytes(),
    );
    body.extend_from_slice(model.variant().as_bytes());
    body.extend_from_slice(
        format!(
            "\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"skin.png\"\r\nContent-Type: image/png\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(png);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let response = client()?
        .post(MC_SKIN_URL)
        .bearer_auth(token)
        .header(
            reqwest::header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .send()
        .map_err(|error| AuthError::Api(format!("cannot reach Minecraft services: {error}")))?;
    let status = response.status();
    let body = response.bytes().unwrap_or_default();
    if !status.is_success() {
        return Err(describe_failure("skin upload", status.as_u16(), &body));
    }
    fetch_profile(token)
}

pub fn reset_skin(token: &str) -> Result<MsaProfile, AuthError> {
    let response = client()?
        .delete(MC_SKIN_URL)
        .bearer_auth(token)
        .send()
        .map_err(|error| AuthError::Api(format!("cannot reach Minecraft services: {error}")))?;
    let status = response.status();
    let body = response.bytes().unwrap_or_default();
    if !status.is_success() {
        return Err(describe_failure("skin reset", status.as_u16(), &body));
    }
    fetch_profile(token)
}

/// Capes the account owns.
pub fn capes(token: &str) -> Result<Vec<CapeEntry>, AuthError> {
    Ok(fetch_profile(token)?.capes)
}

pub fn set_cape(token: &str, cape_id: &str) -> Result<MsaProfile, AuthError> {
    let response = client()?
        .put(MC_ACTIVE_CAPE_URL)
        .bearer_auth(token)
        .json(&json!({ "capeId": cape_id }))
        .send()
        .map_err(|error| AuthError::Api(format!("cannot reach Minecraft services: {error}")))?;
    let status = response.status();
    let body = response.bytes().unwrap_or_default();
    if !status.is_success() {
        return Err(describe_failure("cape change", status.as_u16(), &body));
    }
    fetch_profile(token)
}

pub fn clear_cape(token: &str) -> Result<MsaProfile, AuthError> {
    let response = client()?
        .delete(MC_ACTIVE_CAPE_URL)
        .bearer_auth(token)
        .send()
        .map_err(|error| AuthError::Api(format!("cannot reach Minecraft services: {error}")))?;
    let status = response.status();
    let body = response.bytes().unwrap_or_default();
    if !status.is_success() {
        return Err(describe_failure("cape removal", status.as_u16(), &body));
    }
    fetch_profile(token)
}

pub fn save_cape_image(root: &std::path::Path, id: &str, url: &str) -> Result<(), AuthError> {
    let bytes = client()?
        .get(url)
        .send()
        .map_err(|error| AuthError::Api(format!("cannot download the cape: {error}")))?
        .error_for_status()
        .map_err(|error| AuthError::Api(format!("cannot download the cape: {error}")))?
        .bytes()
        .map_err(|error| AuthError::Api(format!("cannot download the cape: {error}")))?;
    let path = crate::store::cape_path(root, id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;
    }
    std::fs::write(&path, &bytes).map_err(|error| io_error(&path, error))
}

/// Download a skin URL into the local cache.
pub fn save_skin_image(root: &std::path::Path, id: &str, url: &str) -> Result<(), AuthError> {
    let bytes = client()?
        .get(url)
        .send()
        .map_err(|error| AuthError::Api(format!("cannot download the skin: {error}")))?
        .error_for_status()
        .map_err(|error| AuthError::Api(format!("cannot download the skin: {error}")))?
        .bytes()
        .map_err(|error| AuthError::Api(format!("cannot download the skin: {error}")))?;
    crate::skin::write_skin(root, id, &bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_encoding_keeps_token_characters() {
        assert_eq!(form_encode("a b"), "a+b");
        assert_eq!(form_encode("a.b-c_d~e"), "a.b-c_d~e");
        assert_eq!(form_encode("a&b=c"), "a%26b%3Dc");
    }

    #[test]
    fn rejects_missing_client_id() {
        assert!(matches!(request_device_code("  "), Err(AuthError::MissingClientId)));
    }
}
