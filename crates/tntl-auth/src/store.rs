//! The account list persisted in `<root>/accounts.json`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Account, AuthError, now_secs, offline_uuid, valid_offline_name};

pub const ACCOUNTS_FILE: &str = "accounts.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AccountKind {
    #[default]
    Offline,
    Microsoft,
}

impl AccountKind {
    pub fn key(self) -> &'static str {
        match self {
            Self::Offline => "offline",
            Self::Microsoft => "microsoft",
        }
    }
}

/// Which arm width the skin uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SkinModel {
    #[default]
    Wide,
    Slim,
}

impl SkinModel {
    /// The name the Microsoft skin API expects.
    pub fn variant(self) -> &'static str {
        match self {
            Self::Wide => "classic",
            Self::Slim => "slim",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Wide => "wide",
            Self::Slim => "slim",
        }
    }

    pub fn from_key(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "wide" | "classic" => Some(Self::Wide),
            "slim" | "alex" => Some(Self::Slim),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StoredAccount {
    /// Stable key, `<kind>-<uuid>`.
    pub id: String,
    pub kind: AccountKind,
    pub name: String,
    /// Undashed UUID.
    pub uuid: String,
    /// Minecraft session token (Microsoft accounts only).
    pub access_token: Option<String>,
    /// Microsoft refresh token (Microsoft accounts only).
    pub refresh_token: Option<String>,
    /// Unix seconds at which `access_token` stops working.
    pub expires_at: Option<i64>,
    pub skin_model: SkinModel,
    /// Offline accounts may paint their skin into single-player through a
    /// generated resource pack.
    pub apply_skin: bool,
    /// Active cape id (Microsoft accounts only).
    pub cape: Option<String>,
    /// Capes the account owns, as last reported by Mojang.
    #[serde(default)]
    pub capes: Vec<crate::msa::CapeEntry>,
    pub added_at: i64,
}

impl Default for StoredAccount {
    fn default() -> Self {
        Self {
            id: String::new(),
            kind: AccountKind::Offline,
            name: String::new(),
            uuid: String::new(),
            access_token: None,
            refresh_token: None,
            expires_at: None,
            skin_model: SkinModel::Wide,
            apply_skin: false,
            cape: None,
            capes: Vec::new(),
            added_at: now_secs(),
        }
    }
}

impl StoredAccount {
    pub fn offline(name: &str) -> Result<Self, AuthError> {
        if !valid_offline_name(name) {
            return Err(AuthError::InvalidName(name.to_string()));
        }
        let uuid = offline_uuid(name);
        Ok(Self {
            id: format!("offline-{uuid}"),
            kind: AccountKind::Offline,
            name: name.to_string(),
            uuid,
            ..Self::default()
        })
    }

    /// The session the game expects on its command line.
    pub fn session(&self) -> Account {
        Account {
            username: self.name.clone(),
            uuid: self.uuid.clone(),
            access_token: self.access_token.clone().unwrap_or_else(|| "0".to_string()),
            user_type: match self.kind {
                AccountKind::Offline => "legacy".to_string(),
                AccountKind::Microsoft => "msa".to_string(),
            },
        }
    }

    pub fn is_microsoft(&self) -> bool {
        self.kind == AccountKind::Microsoft
    }

    /// True when the Minecraft token is missing or about to expire.
    pub fn needs_refresh(&self) -> bool {
        match (self.kind, self.expires_at) {
            (AccountKind::Microsoft, Some(expires_at)) => expires_at - 300 <= now_secs(),
            (AccountKind::Microsoft, None) => true,
            _ => false,
        }
    }
}

/// Contents of `accounts.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AccountViews {
    pub current: Option<String>,
    pub accounts: Vec<StoredAccount>,
}

#[derive(Debug, Clone)]
pub struct AccountStore {
    path: PathBuf,
    data: AccountViews,
}

impl AccountStore {
    pub fn new(root: &Path) -> Self {
        Self {
            path: root.join(ACCOUNTS_FILE),
            data: AccountViews::default(),
        }
    }

    pub fn load(root: &Path) -> Result<Self, AuthError> {
        let mut store = Self::new(root);
        let bytes = match std::fs::read(&store.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(store),
            Err(error) => return Err(crate::io_error(&store.path, error)),
        };
        store.data = match serde_json::from_slice(&bytes) {
            Ok(data) => data,
            Err(error) => {
                // Keep the tokens in a backup instead of blocking the whole
                // launcher on one bad file.
                let stamp = crate::now_secs();
                let backup = store.path.with_extension(format!("broken-{stamp}"));
                eprintln!(
                    "cannot read {}: {error}; keeping {}",
                    store.path.display(),
                    backup.display()
                );
                let _ = std::fs::rename(&store.path, &backup);
                AccountViews::default()
            }
        };
        Ok(store)
    }

    /// Read, mutate and write under a process-wide lock. Two commands (a
    /// login poll and a token refresh) must not overwrite each other's edits,
    /// and Microsoft rotates refresh tokens on every use.
    pub fn update<T>(
        root: &Path,
        mutate: impl FnOnce(&mut AccountStore) -> Result<T, AuthError>,
    ) -> Result<T, AuthError> {
        use std::sync::Mutex;
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        let mut store = Self::load(root)?;
        let value = mutate(&mut store)?;
        store.save()?;
        Ok(value)
    }

    pub fn save(&self) -> Result<(), AuthError> {
        tntl_store::atomic_write_private_json(&self.path, &self.data)?;
        Ok(())
    }

    pub fn data(&self) -> &AccountViews {
        &self.data
    }

    pub fn accounts(&self) -> &[StoredAccount] {
        &self.data.accounts
    }

    pub fn get(&self, id: &str) -> Option<&StoredAccount> {
        self.data.accounts.iter().find(|account| account.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut StoredAccount> {
        self.data.accounts.iter_mut().find(|account| account.id == id)
    }

    pub fn current(&self) -> Option<&StoredAccount> {
        let id = self.data.current.as_deref()?;
        self.get(id).or_else(|| self.data.accounts.first())
    }

    pub fn set_current(&mut self, id: &str) -> Result<(), AuthError> {
        if self.get(id).is_none() {
            return Err(AuthError::UnknownAccount(id.to_string()));
        }
        self.data.current = Some(id.to_string());
        self.save()
    }

    /// Insert or replace an account, keeping the active account choice sane.
    /// Returns the account id.
    pub fn upsert(&mut self, account: StoredAccount) -> Result<String, AuthError> {
        let id = account.id.clone();
        match self.get_mut(&id) {
            Some(slot) => *slot = account,
            None => self.data.accounts.push(account),
        }
        if self.data.current.is_none() {
            self.data.current = Some(id.clone());
        }
        self.save()?;
        Ok(id)
    }

    pub fn remove(&mut self, id: &str) -> Result<bool, AuthError> {
        let before = self.data.accounts.len();
        self.data.accounts.retain(|account| account.id != id);
        if self.data.accounts.len() == before {
            return Ok(false);
        }
        if self.data.current.as_deref() == Some(id) {
            self.data.current = self.data.accounts.first().map(|account| account.id.clone());
        }
        self.save()?;
        Ok(true)
    }

    /// The account an instance should use: its own pick, then the launcher's
    /// current account.
    pub fn effective(&self, preferred: Option<&str>) -> Option<&StoredAccount> {
        preferred.and_then(|id| self.get(id)).or_else(|| self.current())
    }
}

/// Directory that holds downloaded and imported skin images.
pub fn skins_dir(root: &Path) -> PathBuf {
    root.join("skins")
}

/// Ids come from the callers, including IPC; only a bare file name may be
/// joined to the skins directory.
fn safe_id(id: &str) -> bool {
    !id.is_empty() && id != "." && id != ".." && !id.contains(['/', '\\', ':']) && !id.chars().any(char::is_control)
}

pub fn skin_path(root: &Path, id: &str) -> PathBuf {
    let id = if safe_id(id) { id } else { "invalid" };
    skins_dir(root).join(format!("{id}.png"))
}

pub fn cape_path(root: &Path, id: &str) -> PathBuf {
    let id = if safe_id(id) { id } else { "invalid" };
    skins_dir(root).join(format!("{id}.cape.png"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_and_reloads_accounts() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = AccountStore::load(dir.path()).unwrap();
        let id = store.upsert(StoredAccount::offline("Steve").unwrap()).unwrap();
        store.set_current(&id).unwrap();
        let reloaded = AccountStore::load(dir.path()).unwrap();
        assert_eq!(reloaded.current().unwrap().name, "Steve");
        assert_eq!(reloaded.effective(None).unwrap().id, id);
    }

    #[test]
    fn removing_the_active_account_picks_another() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = AccountStore::load(dir.path()).unwrap();
        let first = store.upsert(StoredAccount::offline("First").unwrap()).unwrap();
        let second = store.upsert(StoredAccount::offline("Second").unwrap()).unwrap();
        store.set_current(&first).unwrap();
        assert!(store.remove(&first).unwrap());
        assert_eq!(store.current().unwrap().id, second);
    }
}
