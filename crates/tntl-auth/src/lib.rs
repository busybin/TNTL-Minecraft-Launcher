//! Accounts: offline identities, Microsoft device-code sign-in, local skins.

#![forbid(unsafe_code)]

use std::path::PathBuf;

use md5::Digest as _;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::{Builder, Variant, Version};

mod default_skins;
mod msa;
mod online;
mod skin;
mod store;

pub use default_skins::{DEFAULT_SKINS, default_skin_image, random_skin_key};
pub use msa::{
    CapeEntry, DeviceCode, DevicePoll, MsaProfile, MsaSession, SkinEntry, capes, clear_cape, fetch_profile,
    poll_device_code, refresh_session, request_device_code, reset_skin, save_cape_image, save_skin_image, set_cape,
    upload_skin,
};
pub use online::{
    GalleryFilter, GalleryPage, GallerySkin, OnlineSkin, SkinSource, ely_skin, gallery, mojang_skin, online_skin,
};
pub use skin::{
    PreparedSkin, arm_variants, png_dimensions, prepare_skin, read_cape, read_skin, remove_skin, validate_skin,
    write_cape, write_skin,
};
pub use store::{
    ACCOUNTS_FILE, AccountKind, AccountStore, AccountViews, SkinModel, StoredAccount, cape_path, skin_path, skins_dir,
};

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("invalid player name: {0}")]
    InvalidName(String),
    #[error("not implemented yet: {0}")]
    NotImplemented(&'static str),
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid account file at {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("invalid skin: {0}")]
    InvalidSkin(String),
    #[error("Microsoft sign-in needs a client ID; add one in Settings")]
    MissingClientId,
    #[error(transparent)]
    Source(#[from] tntl_source::SourceError),
    #[error(transparent)]
    Store(#[from] tntl_store::StoreError),
    #[error("{0}")]
    Api(String),
    #[error("no account named {0}")]
    UnknownAccount(String),
    #[error("no skin published for {0}")]
    NoOnlineSkin(String),
    #[error("the stored sign-in token is no longer valid")]
    Unauthorized,
}

/// What the game needs to start: name, UUID and a session token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub username: String,
    /// Undashed UUID.
    pub uuid: String,
    pub access_token: String,
    /// `msa` or `legacy`.
    pub user_type: String,
}

impl Account {
    /// Offline account. The UUID follows the vanilla launcher's
    /// `UUID.nameUUIDFromBytes("OfflinePlayer:<name>")` (MD5, version 3).
    pub fn offline(username: &str) -> Result<Self, AuthError> {
        if !valid_offline_name(username) {
            return Err(AuthError::InvalidName(username.to_string()));
        }
        Ok(Self {
            username: username.to_string(),
            uuid: offline_uuid(username),
            access_token: "0".to_string(),
            user_type: "legacy".to_string(),
        })
    }
}

pub fn offline_uuid(username: &str) -> String {
    // Vanilla hashes the raw bytes with MD5 and sets version 3, variant RFC
    // 4122. There is no namespace, unlike a regular v3 UUID.
    let digest = md5::Md5::digest(format!("OfflinePlayer:{username}").as_bytes());
    Builder::from_slice(&digest)
        .expect("MD5 is 16 bytes")
        .with_version(Version::Md5)
        .with_variant(Variant::RFC4122)
        .into_uuid()
        .simple()
        .to_string()
}

/// Vanilla rule: 3-16 characters, ASCII letters, digits and underscore.
pub fn valid_username(username: &str) -> bool {
    (3..=16).contains(&username.chars().count()) && username.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

/// Offline names never reach Mojang, so the 3-character minimum does not
/// apply. The game accepts the name as-is; only keep it shell- and
/// filesystem-safe.
pub fn valid_offline_name(username: &str) -> bool {
    (1..=16).contains(&username.chars().count()) && username.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

pub(crate) fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

pub(crate) fn io_error(path: &std::path::Path, source: std::io::Error) -> AuthError {
    AuthError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_accounts_are_stable_and_validated() {
        let first = Account::offline("Player").unwrap();
        let second = Account::offline("Player").unwrap();
        assert_eq!(first.uuid, second.uuid);
        assert_eq!(first.uuid.len(), 32);
        // Vanilla vector: nameUUIDFromBytes("OfflinePlayer:Notch").
        assert_eq!(offline_uuid("Notch"), "b50ad385829d3141a2167e7d7539ba7f");
        assert!(Account::offline("ab").is_ok());
        assert!(Account::offline("hi").is_ok());
        assert!(Account::offline("").is_err());
        assert!(Account::offline("bad name").is_err());
        assert!(Account::offline("ok_name123").is_ok());
        assert!(!valid_username("hi"));
        assert!(valid_username("hi_3"));
    }
}
