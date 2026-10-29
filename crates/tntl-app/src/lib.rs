//! Service layer shared by the command line and the desktop shell.
//!
//! Everything here is synchronous and UI agnostic: callers pass callbacks for
//! progress and log lines, and decide themselves whether to run on a worker
//! thread.

#![forbid(unsafe_code)]

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tntl_auth::Account;
pub use tntl_auth::{AccountKind, DeviceCode, MsaProfile, SkinModel, StoredAccount};
use tntl_auth::{AccountStore, MsaSession, cape_path, read_skin, skin_path};
use tntl_curseforge::{ContentSources, CurseForge, SourceEntry, strip_disabled};
use tntl_instance::Instance;
pub use tntl_instance::datapacks::DatapackInfo;
use tntl_instance::mods::ModLoader;
use tntl_instance::worlds::WorldInfo;
use tntl_java::JavaInstall;
use tntl_model::{Environment, MavenName, VersionJson};
pub use tntl_modpack::{ExportReport, PackEvent, PackReport};
use tntl_modpack::{Modpack, PackIndex};
use tntl_modrinth::{Filter, LocalMod, Modrinth, SearchHit};
pub use tntl_modrinth::{InstallEvent as ModInstallEvent, InstalledMod, ModUpdate, ProjectKind, SearchIndex};
use tntl_resolve::{
    InstanceOptions, LaunchPlan, MissingKind, MissingObject, ResolveInput, VerifyMode, plan as build_plan,
};
pub use tntl_runtime::{InstalledRuntime, RuntimeEvent, RuntimeOffer};
use tntl_source::endpoints;
use tntl_source::{
    DownloadOutcome, DownloadRequest, InstallEvent, InstallOptions, InstallReport, Installer, LoaderKind, LoaderVersion,
};
use tntl_store::RootLayout;

pub use tntl_source::Http;

/// System total memory read on Linux; the desktop shell overrides Windows.
pub mod memory;

/// Launchable band sizes the memory slider is drawn from.
pub use memory::{MemoryAdvice, advice, advice_from_total, clamp_mb, system_memory_mb};

/// Whether `pid` is a live process.
fn process_alive(pid: u32) -> bool {
    // No game can run as pid 0, and on Windows the system reports the idle
    // process for it, which would let a broken marker block an instance for
    // ever.
    if pid == 0 {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        use sysinfo::{Pid, ProcessesToUpdate, System};
        let pid = Pid::from_u32(pid);
        let mut system = System::new();
        system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
        system.process(pid).is_some()
    }
}

/// Start time the OS reports for `pid`. Comparing it with the value stored
/// next to the pid tells a recycled pid apart from the game that wrote the
/// marker: after a reboot a stale marker can otherwise match an unrelated
/// process and the instance looks like it is still running forever.
fn process_start_time(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        // Field 22 of /proc/<pid>/stat: start time in clock ticks since boot.
        // The command name is in parentheses and may contain spaces, so the
        // fields are counted from the last ')'.
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let rest = stat.rsplit_once(')')?.1;
        rest.split_whitespace().nth(19)?.parse().ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        use sysinfo::{Pid, ProcessesToUpdate, System};
        let pid = Pid::from_u32(pid);
        let mut system = System::new();
        system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
        system.process(pid).map(|process| process.start_time())
    }
}

/// A launcher data directory: `instances/`, `versions/`, `libraries/`, `assets/`.
#[derive(Clone)]
pub struct Launcher {
    pub root: PathBuf,
}

/// Copy of the pack index an instance was installed from.
pub const PACK_FILE: &str = "modpack.json";

/// Marker holding the pid of a running game, inside the instance directory.
pub const RUNNING_FILE: &str = ".running";

/// Which Modrinth project a pack instance came from, so it can be updated.
pub const PACK_SOURCE_FILE: &str = "modpack.source.json";

/// Deletes a downloaded archive when the install finishes, including on error:
/// CurseForge packs are hundreds of megabytes and used to stay in `root/tmp`.
struct RemoveFileOnDrop(PathBuf);

impl Drop for RemoveFileOnDrop {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// FTB pack an instance was installed from, with the file list of that build.
pub const FTB_FILE: &str = "ftb.json";

/// CurseForge pack an instance was installed from.
pub const CURSEFORGE_PACK_FILE: &str = "curseforge.pack.json";

/// `<instance>/modrinth.content.json`: the Modrinth project each resource pack
/// or shader came from. Modrinth files are otherwise recognised by sha1, but
/// search results carry no hashes, so the browser needs this to mark them.
pub const MODRINTH_CONTENT_FILE: &str = "modrinth.content.json";

/// Launcher-wide settings kept next to `instances/`.
pub const SETTINGS_FILE: &str = "launcher.json";

/// Where content comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentSource {
    Modrinth,
    CurseForge,
}

impl ContentSource {
    pub fn key(self) -> &'static str {
        match self {
            Self::Modrinth => "modrinth",
            Self::CurseForge => "curseforge",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key.trim().to_ascii_lowercase().as_str() {
            "modrinth" | "mr" => Some(Self::Modrinth),
            "curseforge" | "cf" => Some(Self::CurseForge),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LauncherSettings {
    /// Parallel downloads for installs and runtime downloads.
    pub concurrency: usize,
    /// Hide the launcher window while the game runs.
    pub hide_on_launch: bool,
    /// Quit the launcher once the game has started.
    pub quit_on_launch: bool,
    /// `dark`, `light` or `system`.
    pub theme: String,
    /// Background image drawn behind the interface, if any.
    pub background: Option<String>,
    /// `none`, `system`, `http` or `socks5`.
    pub proxy_mode: String,
    /// Proxy URL for the `http` and `socks5` modes.
    pub proxy_url: Option<String>,
    /// Optional proxy account for an authenticated HTTP or SOCKS5 proxy.
    pub proxy_username: Option<String>,
    #[serde(default)]
    pub proxy_password: Option<String>,
    /// Cleared after the first-run guide has been shown.
    pub onboarded: bool,
    /// Public Microsoft Entra client id with the device code flow enabled.
    /// Without it Microsoft sign-in stays disabled; `TNTL_MSA_CLIENT_ID` also
    /// works.
    pub msa_client_id: Option<String>,
    /// CurseForge API key. The launcher does not ship one, so CurseForge stays
    /// disabled until the user pastes a key; `TNTL_CURSEFORGE_API_KEY` also
    /// works.
    pub curseforge_api_key: Option<String>,
}

impl Default for LauncherSettings {
    fn default() -> Self {
        Self {
            concurrency: 8,
            hide_on_launch: true,
            quit_on_launch: false,
            theme: "dark".to_string(),
            background: None,
            proxy_mode: "system".to_string(),
            proxy_url: None,
            proxy_username: None,
            proxy_password: None,
            onboarded: false,
            msa_client_id: None,
            curseforge_api_key: None,
        }
    }
}

/// Account entry for user interfaces. Session tokens never leave the disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountView {
    pub id: String,
    /// `offline` or `microsoft`.
    pub kind: String,
    pub name: String,
    pub uuid: String,
    pub active: bool,
    pub skin_model: String,
    pub has_skin: bool,
    pub has_cape: bool,
    /// Offline skin is painted into the game through a resource pack.
    pub apply_skin: bool,
    pub cape: Option<String>,
    pub capes: Vec<CapeView>,
    pub expires_at: Option<i64>,
    pub microsoft: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapeView {
    pub id: String,
    pub alias: String,
    pub active: bool,
}

/// Skin found online, before the user decides to wear it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnlineSkinPreview {
    /// Name as typed.
    pub nickname: String,
    /// Name the source reports, which can differ in case.
    pub name: String,
    pub slim: bool,
    pub skin: Vec<u8>,
    /// `Mojang` or `ely.by`, for the UI to show.
    pub source: String,
}

/// One skin from the browsable gallery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GalleryPreview {
    pub id: i64,
    pub time: u64,
    pub skin: Vec<u8>,
    /// Arm model the upload was tagged with, when the list reports it.
    #[serde(default)]
    pub slim: Option<bool>,
}

/// One page of the gallery plus the cursor that continues it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GalleryPage {
    pub skins: Vec<GalleryPreview>,
    /// Pass back as `after` to fetch the next page; `None` on the last page.
    #[serde(default)]
    pub next: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountsResponse {
    pub current: Option<String>,
    pub accounts: Vec<AccountView>,
    /// A Microsoft client id is configured, so sign-in can be offered.
    pub client_id: bool,
}

/// Result of one device-code poll.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum MsaPoll {
    Pending,
    /// Microsoft asked the client to poll less often.
    SlowDown,
    SignedIn {
        account: AccountView,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissingDepsReport {
    pub installed: Vec<InstalledMod>,
    /// Dependency ids Modrinth has no exact project for.
    pub unresolved: Vec<String>,
}

/// One dependency replaced because the installed build sat outside a range the
/// game enforces.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyUpdate {
    /// Dependency mod id.
    pub dep: String,
    /// Name of the jar that supplied the wrong build.
    pub name: String,
    pub file: String,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DependencyFixReport {
    pub updated: Vec<DependencyUpdate>,
    /// Dependency ids no matching build was found for.
    pub unresolved: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackSummary {
    pub project_id: String,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub icon_url: Option<String>,
    pub downloads: u64,
    pub author: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackPage {
    pub hits: Vec<PackSummary>,
    pub total_hits: u32,
    pub offset: u32,
    pub limit: u32,
}

/// One build of a CurseForge modpack as shown in the browser.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CursePackFileView {
    pub file_id: u64,
    pub name: String,
    /// CurseForge display name, falling back to the file name.
    pub version: String,
    pub game_versions: Vec<String>,
    /// 1 release, 2 beta, 3 alpha.
    pub release_type: u32,
    pub file_date: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CursePackView {
    pub project_id: u64,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub icon_url: Option<String>,
    pub downloads: u64,
    pub author: String,
    /// Builds the search result already carried; the newest comes last.
    pub files: Vec<CursePackFileView>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CursePackPage {
    pub hits: Vec<CursePackView>,
    pub total_hits: u32,
    pub offset: u32,
    pub limit: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackSource {
    pub project_id: String,
    /// Version number written in the pack index, e.g. `5.4.1`.
    pub version_number: String,
}

/// One newer build of the pack a Modrinth project publishes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackTarget {
    pub version_id: String,
    pub version_number: String,
    /// `release`, `beta` or `alpha`.
    pub version_type: String,
    pub game_version: String,
    pub loader: Option<String>,
    /// Same game version as the instance, so no new game install is needed.
    pub in_place: bool,
    pub url: String,
    pub sha1: Option<String>,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackUpdate {
    pub instance: String,
    pub name: String,
    pub project_id: String,
    pub current_version: String,
    pub current_game_version: String,
    pub targets: Vec<PackTarget>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackUpdateReport {
    pub instance: String,
    pub name: String,
    pub version_number: String,
    pub version_id: String,
    pub game_version: String,
    pub loader: Option<String>,
    /// Pack files the old build shipped and the new one dropped.
    pub removed: usize,
    pub files: PackReport,
}

/// What an integrity pass over an instance found and fetched again.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RepairReport {
    /// Files of the game version, its libraries and assets that had to be fetched.
    pub game_downloaded: usize,
    /// Files of the same set that already passed their hash.
    pub game_skipped: usize,
    /// Pack files that had to be fetched.
    pub pack_downloaded: usize,
    pub pack_skipped: usize,
    /// Bytes downloaded by the whole pass.
    pub bytes: u64,
    /// Pack name when the instance came from a modpack.
    pub pack: Option<String>,
    /// True when the loader had to be installed again because its patched
    /// jars no longer matched the record written at install time.
    #[serde(default)]
    pub loader_rebuilt: bool,
}

/// Result of the offline instance check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticReport {
    /// Seconds since the Unix epoch.
    pub checked_at: u64,
    pub game_version: String,
    pub loader: Option<String>,
    pub loader_version: Option<String>,
    pub mods: usize,
    pub enabled_mods: usize,
    pub memory_mb: u32,
    pub java_major: Option<u32>,
    pub findings: Vec<tntl_doctor::Finding>,
}

/// Instance view for user interfaces.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceView {
    pub name: String,
    pub version_id: String,
    pub username: String,
    pub max_memory_mb: u32,
    pub java: Option<String>,
    pub imported: bool,
    pub dir: PathBuf,
    pub game_dir: PathBuf,
    pub installed: bool,
    pub last_log: Option<PathBuf>,
    /// Name of the Modrinth modpack the instance came from, if any.
    pub modpack: Option<String>,
    /// Library group, used as a filter chip.
    pub group: Option<String>,
    pub notes: String,
    /// Absolute path of the custom icon, when one is set.
    pub icon: Option<PathBuf>,
    /// Name of the account that will actually be used.
    pub account_name: Option<String>,
    /// Total seconds played across finished launches.
    pub playtime_seconds: u64,
    pub launches: u64,
    /// Unix timestamp of the last finished launch.
    pub last_played: Option<i64>,
}

/// Version entry for the install picker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionView {
    pub id: String,
    pub version_type: String,
    pub release_time: String,
    pub installed: bool,
}

/// Java runtime view for settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JavaView {
    pub path: PathBuf,
    pub major: u32,
    pub version: String,
    pub vendor: Option<String>,
}

/// Game version and mod loader an instance is built on.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionEnvironment {
    pub game_version: String,
    /// `fabric`, `quilt`, `forge`, `neoforge`, or `None` for vanilla.
    pub loader: Option<String>,
}

/// Mod loader an instance currently runs, derived from its version chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoaderStatus {
    /// Vanilla version the loader sits on.
    pub game_version: String,
    /// `fabric`, `quilt`, `legacyfabric`, `forge`, `neoforge`, or `None` for vanilla.
    pub kind: Option<String>,
    /// Loader build in the form `loader_versions` returns it.
    pub version: Option<String>,
    /// Version id the instance launches.
    pub version_id: String,
}

/// Newest loader build published for an instance's game version.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoaderUpdate {
    pub kind: String,
    pub current: String,
    pub latest: String,
    /// True when a newer build than `current` is published.
    pub newer: bool,
}

/// One page of Modrinth search results, scoped to an instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModSearchPage {
    pub game_version: String,
    pub loader: String,
    pub hits: Vec<SearchHit>,
    pub total_hits: u32,
    pub offset: u32,
    pub limit: u32,
}

/// What the install command needs to know. A loader install writes the
/// loader's profile JSON first and then installs that version like any other,
/// so the vanilla parent comes down through `inheritsFrom`.
#[derive(Debug, Clone, Default)]
pub struct InstallRequest {
    /// Game version, e.g. `1.20.1`.
    pub version: String,
    pub loader: Option<LoaderKind>,
    /// Loader build; the newest stable one when omitted.
    pub loader_version: Option<String>,
    pub assets: bool,
    /// Register the result as an instance under this name.
    pub instance: Option<String>,
}

impl InstallRequest {
    pub fn new(version: impl Into<String>) -> Self {
        Self {
            version: version.into(),
            assets: true,
            ..Self::default()
        }
    }

    pub fn with_loader(mut self, kind: LoaderKind, version: Option<String>) -> Self {
        self.loader = Some(kind);
        self.loader_version = version;
        self
    }

    /// Default instance name: `1.20.1` for vanilla, `1.20.1-fabric` otherwise.
    pub fn instance_name(&self) -> String {
        match self.loader {
            Some(kind) => sanitize_name(&format!("{}-{}", self.version, kind.key())),
            None => sanitize_name(&self.version),
        }
    }
}

/// Result of installing a Modrinth modpack.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModpackReport {
    /// Instance the pack was registered as.
    pub instance: String,
    /// Pack name as written in its index.
    pub name: String,
    pub game_version: String,
    /// `fabric`, `quilt`, `forge`, `neoforge`, or `None` for a vanilla pack.
    pub loader: Option<String>,
    pub version_id: String,
    pub files: PackReport,
}

/// One FTB pack version in a browse list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FtbVersionView {
    pub id: u64,
    pub name: String,
    /// `release`, `beta` or `alpha`.
    pub kind: String,
    pub game_version: String,
    pub loader: Option<String>,
    pub loader_version: Option<String>,
    pub java: Option<String>,
    pub recommended_memory_mb: Option<u32>,
    pub files: usize,
    pub bytes: u64,
}

/// FTB pack as shown in the modpack browser.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FtbPackView {
    pub id: u64,
    pub name: String,
    pub synopsis: String,
    pub description: String,
    pub icon_url: Option<String>,
    pub installs: u64,
    pub plays: u64,
    pub tags: Vec<String>,
    pub versions: Vec<FtbVersionView>,
}

/// One file an FTB build wrote, kept so updates can drop stale ones safely.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FtbFileEntry {
    pub path: String,
    #[serde(default)]
    pub sha1: Option<String>,
}

/// FTB pack an instance was installed from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FtbSource {
    pub pack_id: u64,
    pub pack_name: String,
    pub version_id: u64,
    pub version_name: String,
    #[serde(default)]
    pub version_kind: String,
    pub updated: i64,
    pub game_version: String,
    pub loader: Option<String>,
    /// Files this build wrote into the game directory.
    #[serde(default)]
    pub files: Vec<FtbFileEntry>,
}

/// One file a CurseForge pack wrote, kept so repair and updates can find it
/// without re-reading the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CursePackFileEntry {
    pub path: String,
    pub project_id: u64,
    pub file_id: u64,
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(default)]
    pub size: u64,
}

/// CurseForge pack an instance was installed from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CursePackSource {
    pub project_id: u64,
    pub pack_name: String,
    /// CurseForge file id of the installed build.
    pub version_id: u64,
    pub version_name: String,
    pub file_date: String,
    #[serde(default)]
    pub release_type: u32,
    pub game_version: String,
    pub loader: Option<String>,
    pub loader_version: Option<String>,
    #[serde(default)]
    pub files: Vec<CursePackFileEntry>,
}

/// Download one CurseForge file into the game directory.
#[derive(Debug, Clone)]
struct CursePackDownload {
    path: String,
    project_id: u64,
    file_id: u64,
    url: String,
    sha1: Option<String>,
    size: u64,
}

/// What a `.mrpack` asks for, readable before anything is downloaded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModpackInfo {
    pub name: String,
    pub summary: String,
    pub game_version: String,
    pub loader: Option<String>,
    pub loader_version: Option<String>,
    /// Number of files the client needs.
    pub files: usize,
    pub bytes: u64,
}

/// Read a pack's index without installing it.
pub fn modpack_info(archive: &Path) -> Result<ModpackInfo> {
    let pack = Modpack::open(archive)?;
    let (loader, loader_version) = match pack.loader() {
        Some((loader, version)) => (Some(loader.key().to_string()), version),
        None => (None, None),
    };
    Ok(ModpackInfo {
        name: pack.name(),
        summary: pack.index.summary.clone(),
        game_version: pack.game_version()?,
        loader,
        loader_version,
        files: pack.client_files().count(),
        bytes: pack.client_files().map(|file| file.file_size).sum(),
    })
}

impl Launcher {
    pub fn from_root(root: Option<PathBuf>) -> Self {
        let root = root.unwrap_or_else(|| dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("tntl"));
        let launcher = Self { root };
        launcher.apply_network();
        launcher
    }

    /// Push the saved proxy settings into the shared HTTP stack.
    pub fn apply_network(&self) {
        let settings = self.settings();
        tntl_source::set_network_config(tntl_source::NetworkConfig::from_parts_auth(
            &settings.proxy_mode,
            settings.proxy_url.as_deref(),
            settings.proxy_username.as_deref(),
            settings.proxy_password.as_deref(),
        ));
    }

    pub fn layout(&self) -> RootLayout {
        RootLayout::new(&self.root)
    }

    /// Microsoft client id from settings, falling back to the environment.
    /// The launcher ships none: the user registers a public Entra app.
    pub fn msa_client_id(&self) -> Option<String> {
        [self.settings().msa_client_id, std::env::var("TNTL_MSA_CLIENT_ID").ok()]
            .into_iter()
            .flatten()
            .map(|id| id.trim().to_string())
            .find(|id| !id.is_empty())
    }

    pub fn accounts(&self) -> Result<AccountsResponse> {
        let store = AccountStore::load(&self.root)?;
        let current = store.current().map(|account| account.id.clone());
        Ok(AccountsResponse {
            accounts: store
                .accounts()
                .iter()
                .map(|account| self.account_view(account, current.as_deref()))
                .collect(),
            current,
            client_id: self.msa_client_id().is_some(),
        })
    }

    fn account_view(&self, account: &StoredAccount, current: Option<&str>) -> AccountView {
        AccountView {
            id: account.id.clone(),
            kind: account.kind.key().to_string(),
            name: account.name.clone(),
            uuid: account.uuid.clone(),
            active: current == Some(account.id.as_str()),
            skin_model: account.skin_model.key().to_string(),
            has_skin: skin_path(&self.root, &account.id).is_file(),
            has_cape: tntl_auth::cape_path(&self.root, &account.id).is_file(),
            apply_skin: account.apply_skin,
            cape: account.cape.clone(),
            capes: account
                .capes
                .iter()
                .map(|cape| CapeView {
                    id: cape.id.clone(),
                    alias: cape.alias.clone(),
                    active: Some(&cape.id) == account.cape.as_ref(),
                })
                .collect(),
            expires_at: account.expires_at,
            microsoft: account.is_microsoft(),
        }
    }

    fn view_of(&self, id: &str) -> Result<AccountView> {
        let store = AccountStore::load(&self.root)?;
        let current = store.current().map(|account| account.id.clone());
        let account = store.get(id).with_context(|| format!("no account {id}"))?;
        Ok(self.account_view(account, current.as_deref()))
    }

    /// Add an offline account and make it the active one. A fresh account wears
    /// a random built-in skin so its avatar and in-game player are not blank.
    pub fn add_offline_account(&self, name: &str) -> Result<AccountView> {
        let mut store = AccountStore::load(&self.root)?;
        let id = store.upsert(StoredAccount::offline(name)?)?;
        store.set_current(&id)?;
        if let Err(error) = self.set_default_skin(&id, tntl_auth::random_skin_key()) {
            // A skin that cannot be written must not cost the account itself.
            eprintln!("cannot give the new offline account a skin: {error}");
        }
        self.view_of(&id)
    }

    /// Rename an offline account. The UUID follows the name (vanilla's offline
    /// rule), so it changes too; the skin and the active-account pick move with
    /// it. Microsoft names come from Mojang and are read-only here.
    pub fn rename_offline_account(&self, id: &str, name: &str) -> Result<AccountView> {
        let name = name.trim();
        let mut store = AccountStore::load(&self.root)?;
        let account = store.get(id).cloned().with_context(|| format!("no account {id}"))?;
        if account.kind != AccountKind::Offline {
            bail!("only offline accounts can be renamed; Microsoft names come from the Mojang profile");
        }
        if account.name == name {
            return self.view_of(id);
        }
        let fresh = StoredAccount::offline(name)?;
        let new_id = fresh.id.clone();
        if new_id != id && store.get(&new_id).is_some() {
            bail!("an offline account named {name} already exists");
        }

        let was_current = store.data().current.as_deref() == Some(id);
        let mut updated = account;
        updated.id = new_id.clone();
        updated.name = fresh.name;
        updated.uuid = fresh.uuid;

        // Move the cached skin files before the store write. A rename on the
        // same filesystem cannot half-copy, and the old id is only dropped
        // once the account record points at the new one.
        let moved = if new_id == id {
            Vec::new()
        } else {
            let mut moved = Vec::new();
            for (from, to) in [
                (skin_path(&self.root, id), skin_path(&self.root, &new_id)),
                (cape_path(&self.root, id), cape_path(&self.root, &new_id)),
            ] {
                if from.exists() {
                    fs::rename(&from, &to)
                        .with_context(|| format!("cannot move {} to {}", from.display(), to.display()))?;
                    moved.push((from, to));
                }
            }
            moved
        };

        let result = (|| -> Result<()> {
            if new_id != id {
                store.remove(id)?;
            }
            store.upsert(updated)?;
            if was_current {
                store.set_current(&new_id)?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            for (from, to) in moved.iter().rev() {
                let _ = fs::rename(to, from);
            }
            return Err(error);
        }
        self.view_of(&new_id)
    }

    /// Built-in skins an offline account can pick from.
    pub fn default_skins(&self) -> Vec<&'static str> {
        tntl_auth::DEFAULT_SKINS.to_vec()
    }

    pub fn default_skin_image(&self, key: &str) -> Result<Vec<u8>> {
        tntl_auth::default_skin_image(key)
            .map(|bytes| bytes.to_vec())
            .with_context(|| format!("unknown built-in skin {key}"))
    }

    /// Wear one of the built-in skins.
    pub fn set_default_skin(&self, id: &str, key: &str) -> Result<AccountView> {
        let png = tntl_auth::default_skin_image(key).with_context(|| format!("unknown built-in skin {key}"))?;
        self.set_account_skin(id, png, None)
    }

    /// Download a skin from ely.by without applying it.
    /// Look a nickname up online, Mojang first and ely.by as the fallback.
    pub fn preview_online_skin(&self, nickname: &str, source: &str) -> Result<OnlineSkinPreview> {
        let http = Http::new()?;
        let source =
            tntl_auth::SkinSource::from_key(source).with_context(|| format!("unknown skin source {source}"))?;
        let found = tntl_auth::online_skin(&http, nickname, source)?;
        // Previews render the stored (square) layout; old accounts still
        // publish 64x32 skins, which would lose a limb in the 3-D model.
        let prepared = tntl_auth::prepare_skin(&found.skin).unwrap_or_else(|_| tntl_auth::PreparedSkin {
            png: found.skin.clone(),
            slim: Some(found.slim),
        });
        Ok(OnlineSkinPreview {
            nickname: found.nickname,
            name: found.name,
            slim: prepared.slim.unwrap_or(found.slim),
            skin: prepared.png,
            source: found.source,
        })
    }

    /// Wear the skin a nickname publishes online.
    pub fn apply_online_skin(&self, id: &str, nickname: &str, source: &str) -> Result<AccountView> {
        let http = Http::new()?;
        let source =
            tntl_auth::SkinSource::from_key(source).with_context(|| format!("unknown skin source {source}"))?;
        let found = tntl_auth::online_skin(&http, nickname, source)?;
        self.set_account_skin(id, &found.skin, Some(if found.slim { "slim" } else { "wide" }))
    }

    /// Wear a gallery texture directly (no nickname attached).
    pub fn apply_gallery_skin(&self, id: &str, skin: &[u8], slim: bool) -> Result<AccountView> {
        let skin = tntl_auth::prepare_skin(skin)?;
        // The texture itself is a better witness than the upload's tag.
        let slim = skin.slim.unwrap_or(slim);
        self.set_account_skin(id, &skin.png, Some(if slim { "slim" } else { "wide" }))
    }

    /// One page of the public gallery; `after` is the cursor from the previous
    /// page (`None` for the first).
    pub fn skin_gallery(&self, filter: &str, after: Option<&str>, count: usize) -> Result<GalleryPage> {
        let filter =
            tntl_auth::GalleryFilter::from_key(filter).with_context(|| format!("unknown gallery category {filter}"))?;
        let http = Http::new()?;
        let page = tntl_auth::gallery(&http, filter, after, count.clamp(1, 24))?;
        Ok(GalleryPage {
            skins: page
                .skins
                .into_iter()
                .map(|entry| {
                    let prepared = tntl_auth::prepare_skin(&entry.skin).unwrap_or_else(|_| tntl_auth::PreparedSkin {
                        png: entry.skin.clone(),
                        slim: entry.slim,
                    });
                    GalleryPreview {
                        id: entry.id,
                        time: entry.time,
                        skin: prepared.png,
                        slim: prepared.slim.or(entry.slim),
                    }
                })
                .collect(),
            next: page.next,
        })
    }

    pub fn remove_account(&self, id: &str) -> Result<()> {
        let mut store = AccountStore::load(&self.root)?;
        if !store.remove(id)? {
            bail!("no account {id}");
        }
        tntl_auth::remove_skin(&self.root, id);
        let _ = fs::remove_file(tntl_auth::cape_path(&self.root, id));
        Ok(())
    }

    pub fn set_current_account(&self, id: &str) -> Result<AccountView> {
        let mut store = AccountStore::load(&self.root)?;
        store.set_current(id)?;
        self.view_of(id)
    }

    /// Change the arm model or the offline in-game toggle.
    pub fn update_account(&self, id: &str, skin_model: Option<&str>, apply_skin: Option<bool>) -> Result<AccountView> {
        let mut store = AccountStore::load(&self.root)?;
        let account = store.get_mut(id).with_context(|| format!("no account {id}"))?;
        if let Some(model) = skin_model {
            account.skin_model = SkinModel::from_key(model).with_context(|| format!("unknown skin model {model}"))?;
        }
        if let Some(apply) = apply_skin {
            account.apply_skin = apply;
        }
        store.save()?;
        self.view_of(id)
    }

    /// Store and, for Microsoft accounts, upload a skin.
    pub fn set_account_skin(&self, id: &str, png: &[u8], skin_model: Option<&str>) -> Result<AccountView> {
        // Legacy 64x32 textures are expanded here so the game, the generated
        // resource pack and the preview all get the square layout.
        let skin = tntl_auth::prepare_skin(png)?;
        let png = skin.png.as_slice();
        let mut store = AccountStore::load(&self.root)?;
        let mut account = store.get(id).cloned().with_context(|| format!("no account {id}"))?;
        if let Some(model) = skin_model {
            account.skin_model = SkinModel::from_key(model).with_context(|| format!("unknown skin model {model}"))?;
        }
        if account.is_microsoft() {
            let session = self.refresh_msa(&mut store, id)?;
            let profile = tntl_auth::upload_skin(&session.access_token, png, account.skin_model)?;
            let mut account = store.get(id).cloned().unwrap_or(account);
            apply_profile(&mut account, &profile);
            tntl_auth::write_skin(&self.root, id, png)?;
            self.cache_profile_images(id, &profile);
            store.upsert(account)?;
        } else {
            tntl_auth::write_skin(&self.root, id, png)?;
            account.apply_skin = true;
            store.upsert(account)?;
        }
        self.view_of(id)
    }

    pub fn clear_account_skin(&self, id: &str) -> Result<AccountView> {
        let mut store = AccountStore::load(&self.root)?;
        let account = store.get(id).cloned().with_context(|| format!("no account {id}"))?;
        tntl_auth::remove_skin(&self.root, id);
        if account.is_microsoft() {
            let session = self.refresh_msa(&mut store, id)?;
            let profile = tntl_auth::reset_skin(&session.access_token)?;
            let mut account = store.get(id).cloned().unwrap_or(account);
            apply_profile(&mut account, &profile);
            store.upsert(account)?;
            self.cache_profile_images(id, &profile);
        }
        self.view_of(id)
    }

    pub fn account_skin(&self, id: &str) -> Result<Option<Vec<u8>>> {
        Ok(read_skin(&self.root, id))
    }

    pub fn account_cape(&self, id: &str) -> Result<Option<Vec<u8>>> {
        Ok(tntl_auth::read_cape(&self.root, id))
    }

    /// Pick one of the account's capes, or `None` to hide it.
    pub fn set_account_cape(&self, id: &str, cape: Option<&str>) -> Result<AccountView> {
        let mut store = AccountStore::load(&self.root)?;
        let session = self.refresh_msa(&mut store, id)?;
        let profile = match cape.filter(|cape| !cape.is_empty()) {
            Some(cape) => tntl_auth::set_cape(&session.access_token, cape)?,
            None => tntl_auth::clear_cape(&session.access_token)?,
        };
        let mut account = store.get(id).cloned().with_context(|| format!("no account {id}"))?;
        apply_profile(&mut account, &profile);
        store.upsert(account)?;
        self.cache_profile_images(id, &profile);
        self.view_of(id)
    }

    /// Query the Mojang profile again and refresh the cached skin and cape.
    pub fn refresh_account(&self, id: &str) -> Result<AccountView> {
        let session = AccountStore::update(&self.root, |store| {
            let convert = |error: anyhow::Error| tntl_auth::AuthError::Api(format!("{error:#}"));
            let session = self.refresh_msa(store, id).map_err(convert)?;
            let mut account = store
                .get(id)
                .cloned()
                .with_context(|| format!("no account {id}"))
                .map_err(convert)?;
            apply_profile(&mut account, &session.profile);
            store.upsert(account)?;
            Ok(session)
        })?;
        self.cache_profile_images(id, &session.profile);
        self.view_of(id)
    }

    /// Ask Microsoft for a device code; the caller shows it and polls.
    pub fn start_msa_login(&self) -> Result<DeviceCode> {
        self.start_msa_login_with(None)
    }

    pub fn start_msa_login_with(&self, client_id: Option<&str>) -> Result<DeviceCode> {
        let client = client_id
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty())
            .or_else(|| self.msa_client_id())
            .context("Microsoft sign-in needs a client ID; add one in Settings")?;
        Ok(tntl_auth::request_device_code(&client)?)
    }

    /// `Pending` while the user has not finished; the new account otherwise.
    pub fn poll_msa_login(&self, device_code: &str) -> Result<MsaPoll> {
        self.poll_msa_login_with(None, device_code)
    }

    pub fn poll_msa_login_with(&self, client_id: Option<&str>, device_code: &str) -> Result<MsaPoll> {
        let client = client_id
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty())
            .or_else(|| self.msa_client_id())
            .context("Microsoft sign-in needs a client ID; add one in Settings")?;
        let session = match tntl_auth::poll_device_code(&client, device_code)? {
            tntl_auth::DevicePoll::Pending => return Ok(MsaPoll::Pending),
            tntl_auth::DevicePoll::SlowDown => return Ok(MsaPoll::SlowDown),
            tntl_auth::DevicePoll::Done(session) => *session,
        };
        let mut account = StoredAccount {
            id: format!("msa-{}", session.profile.id),
            kind: AccountKind::Microsoft,
            name: session.profile.name.clone(),
            uuid: session.profile.id.clone(),
            ..StoredAccount::default()
        };
        account.access_token = Some(session.access_token.clone());
        account.refresh_token = Some(session.refresh_token.clone());
        account.expires_at = Some(session.expires_at);
        apply_profile(&mut account, &session.profile);
        let id = AccountStore::update(&self.root, |store| {
            let id = store.upsert(account)?;
            store.set_current(&id)?;
            Ok(id)
        })?;
        self.cache_profile_images(&id, &session.profile);
        Ok(MsaPoll::SignedIn {
            account: self.view_of(&id)?,
        })
    }

    /// Fresh Minecraft session for a Microsoft account, stored back on disk.
    fn refresh_msa(&self, store: &mut AccountStore, id: &str) -> Result<MsaSession> {
        let account = store.get(id).cloned().with_context(|| format!("no account {id}"))?;
        if !account.is_microsoft() {
            bail!("{id} is an offline account");
        }
        if !account.needs_refresh()
            && let Some(token) = account.access_token.clone()
        {
            match tntl_auth::fetch_profile(&token) {
                Ok(profile) => {
                    return Ok(MsaSession {
                        access_token: token,
                        refresh_token: account.refresh_token.clone().unwrap_or_default(),
                        expires_at: account.expires_at.unwrap_or_default(),
                        profile,
                    });
                }
                // The server rejected a token we believed valid (clock change,
                // revoked session). Refresh once instead of failing the launch.
                Err(tntl_auth::AuthError::Unauthorized) => {}
                Err(error) => return Err(error.into()),
            }
        }
        let client = self
            .msa_client_id()
            .context("Microsoft sign-in needs a client ID; add one in Settings")?;
        let session = tntl_auth::refresh_session(&client, account.refresh_token.as_deref().unwrap_or_default())?;
        let mut updated = account;
        updated.access_token = Some(session.access_token.clone());
        updated.refresh_token = Some(session.refresh_token.clone());
        updated.expires_at = Some(session.expires_at);
        apply_profile(&mut updated, &session.profile);
        store.upsert(updated)?;
        Ok(session)
    }

    fn cache_profile_images(&self, id: &str, profile: &MsaProfile) {
        match profile
            .skins
            .iter()
            .find(|skin| skin.state.eq_ignore_ascii_case("active"))
        {
            Some(skin) => {
                let _ = tntl_auth::save_skin_image(&self.root, id, &skin.url);
            }
            None => tntl_auth::remove_skin(&self.root, id),
        }
        match profile
            .capes
            .iter()
            .find(|cape| cape.state.eq_ignore_ascii_case("active"))
        {
            Some(cape) => {
                let _ = tntl_auth::save_cape_image(&self.root, id, &cape.url);
            }
            None => {
                let _ = fs::remove_file(tntl_auth::cape_path(&self.root, id));
            }
        }
    }

    /// Account the game launches with: always the launcher's current account.
    /// Instances do not pin their own account; the rail avatar and the account
    /// page are the single place to switch. There is no implicit fallback: the
    /// game cannot start without an account.
    pub fn effective_account(&self, _instance: &Instance) -> Result<tntl_auth::Account> {
        let mut store = AccountStore::load(&self.root)?;
        let Some(account) = store.current().cloned() else {
            bail!("add an account first: no offline or Microsoft account is selected");
        };
        if account.is_microsoft() && account.needs_refresh() {
            self.refresh_msa(&mut store, &account.id)?;
            let updated = store.get(&account.id).cloned().unwrap_or(account);
            return Ok(updated.session());
        }
        Ok(account.session())
    }

    /// Paint an offline skin into the instance before the game starts, and
    /// take the pack out again when the account does not want it.
    pub fn prepare_skin(&self, name: &str) -> Result<()> {
        let instance = self.load_instance(name)?;
        let game_dir = instance.game_dir(&self.instance_dir(name)?);
        let store = AccountStore::load(&self.root)?;
        let account = store.current();
        let apply = account.is_some_and(|account| !account.is_microsoft() && account.apply_skin);
        if !apply {
            tntl_instance::content::remove_default_skin(&game_dir)?;
            return Ok(());
        }
        let account = account.expect("checked above");
        match read_skin(&self.root, &account.id) {
            Some(png) => {
                let variants = tntl_auth::arm_variants(&png, account.skin_model == SkinModel::Slim);
                match variants {
                    Ok((wide, slim)) => {
                        if let Err(error) =
                            tntl_instance::content::apply_default_skin(&game_dir, &instance.version_id, &wide, &slim)
                        {
                            eprintln!("cannot apply the offline skin: {error}");
                        }
                    }
                    Err(error) => eprintln!("cannot prepare the offline skin: {error}"),
                }
            }
            None => tntl_instance::content::remove_default_skin(&game_dir)?,
        }
        Ok(())
    }

    /// Status of one server from its list entry.
    pub fn server_status(&self, host: &str, port: u16) -> Result<tntl_status::ServerStatus> {
        Ok(tntl_status::ping(host, port, std::time::Duration::from_millis(4000))?)
    }

    /// Modrinth modpacks, newest and most downloaded first.
    pub fn search_modpacks(&self, query: &str, sort: &str, limit: u32, offset: u32) -> Result<PackPage> {
        let http = Http::new()?;
        let page = Modrinth::new(&http).search_packs(query, parse_index(sort)?, limit, offset)?;
        Ok(PackPage {
            hits: page
                .hits
                .into_iter()
                .map(|hit| PackSummary {
                    project_id: hit.project_id,
                    slug: hit.slug,
                    title: hit.title,
                    description: hit.description,
                    icon_url: hit.icon_url,
                    downloads: hit.downloads,
                    author: hit.author,
                })
                .collect(),
            total_hits: page.total_hits,
            offset: page.offset,
            limit: page.limit,
        })
    }

    /// CurseForge modpacks. Needs the user's own API key, like every other
    /// CurseForge call.
    pub fn search_curseforge_packs(&self, query: &str, sort: &str, limit: u32, offset: u32) -> Result<CursePackPage> {
        let http = Http::new()?;
        let curseforge = self.curseforge(&http)?;
        let page = curseforge.search_packs(query, parse_index(sort)?, limit, offset)?;
        Ok(CursePackPage {
            hits: page.mods.into_iter().map(curse_pack_view).collect(),
            total_hits: page.total,
            offset: page.offset,
            limit: page.limit,
        })
    }

    /// Download and install a Modrinth modpack by project id.
    pub fn install_modpack_project(
        &self,
        project: &str,
        instance: Option<&str>,
        version_progress: &(dyn Fn(InstallEvent) + Send + Sync),
        pack_progress: &(dyn Fn(PackEvent) + Send + Sync),
    ) -> Result<ModpackReport> {
        let http = Http::new()?;
        let modrinth = Modrinth::new(&http);
        let versions = modrinth.project_versions(project)?;
        // Same channel rule as update checks: a brand-new alpha must not win
        // over the newest stable release.
        let version = versions
            .iter()
            .find(|version| version.version_type == "release")
            .or_else(|| versions.first())
            .with_context(|| format!("{project} has no versions"))?
            .clone();
        let file = version
            .files
            .iter()
            .find(|file| file.filename.ends_with(".mrpack"))
            .with_context(|| format!("{} ships no .mrpack file", version.version_number))?;
        let dir = tempfile::tempdir()?;
        let archive = dir.path().join(&file.filename);
        let mut request = DownloadRequest::new(&file.url, &archive);
        if let Some(sha1) = &file.hashes.sha1 {
            request = request.with_sha1(sha1);
        }
        let total = (file.size > 0).then_some(file.size);
        http.download(&request, &mut |done| pack_progress(PackEvent::Archive { done, total }))?;
        let report = self.install_modpack(&archive, instance, version_progress, pack_progress)?;
        let _ = fs::remove_dir_all(dir.path());
        Ok(report)
    }

    /// FTB packs from the official FTB catalogue, newest first for an empty
    /// query.
    pub fn search_ftb_packs(&self, query: &str, limit: u32) -> Result<Vec<FtbPackView>> {
        let http = Http::new()?;
        let ftb = tntl_ftb::Ftb::new(&http);
        Ok(ftb.search(query, limit)?.into_iter().map(ftb_summary_view).collect())
    }

    /// One FTB pack with its version history.
    pub fn ftb_pack(&self, id: u64) -> Result<FtbPackView> {
        let http = Http::new()?;
        let ftb = tntl_ftb::Ftb::new(&http);
        Ok(ftb_pack_view(ftb.pack(id)?))
    }

    /// Install an FTB pack: the game version, its loader, then every file the
    /// build ships. The instance records the pack and its file list so later
    /// builds can replace it.
    pub fn install_ftb_pack(
        &self,
        pack_id: u64,
        version_id: Option<u64>,
        instance: Option<&str>,
        version_progress: &(dyn Fn(InstallEvent) + Send + Sync),
        pack_progress: &(dyn Fn(PackEvent) + Send + Sync),
    ) -> Result<ModpackReport> {
        let http = Http::new()?;
        let ftb = tntl_ftb::Ftb::new(&http);
        let pack = ftb.pack(pack_id)?;
        let wanted = match version_id {
            Some(id) => pack
                .versions
                .iter()
                .find(|version| version.id == id)
                .with_context(|| format!("FTB pack {pack_id} has no version {id}"))?,
            None => pack
                .latest()
                .with_context(|| format!("FTB pack {pack_id} has no versions"))?,
        };
        let version = ftb.version(pack_id, wanted.id)?;
        let name = match instance {
            Some(name) => {
                validate_name(name)?;
                name.to_string()
            }
            None => sanitize_name(&pack.name),
        };
        let mut request = InstallRequest::new(&version.game_version);
        request.instance = Some(name.clone());
        if let Some(loader) = &version.loader {
            request.loader = Some(
                LoaderKind::from_key(loader.kind.key())
                    .with_context(|| format!("unsupported pack loader {}", loader.kind))?,
            );
            request.loader_version = Some(loader.version.clone());
        }
        let _report = self.install(&request, version_progress)?;

        let instance_dir = self.instance_dir(&name)?;
        // Same rule as the .mrpack path: a failure after the instance was
        // registered leaves a half installed instance that cannot be retried
        // under the same name, so drop it.
        let result = (|| -> Result<ModpackReport> {
            let mut instance = self.load_instance(&name)?;
            let game_dir = instance.game_dir(&instance_dir);
            let files = ftb.install(&version, &game_dir, self.settings().concurrency, pack_progress)?;
            // Large FTB packs rarely fit in the 4 GiB default.
            if let Some(recommended) = version.recommended_memory_mb {
                let recommended = recommended.clamp(1024, 65_536);
                if recommended > instance.max_memory_mb {
                    instance.max_memory_mb = recommended;
                    self.save_instance(&instance)?;
                }
            }
            let source = FtbSource {
                pack_id,
                pack_name: pack.name.clone(),
                version_id: version.id,
                version_name: version.name.clone(),
                version_kind: version.kind.clone(),
                updated: version.updated,
                game_version: version.game_version.clone(),
                loader: version.loader_key(),
                files: version
                    .client_files()
                    .map(|file| FtbFileEntry {
                        path: file.path.clone(),
                        sha1: file.sha1.clone(),
                    })
                    .collect(),
            };
            tntl_store::atomic_write_json(&instance_dir.join(FTB_FILE), &source)?;
            Ok(ModpackReport {
                instance: name.clone(),
                name: pack.name.clone(),
                game_version: version.game_version.clone(),
                loader: request.loader.map(|kind| kind.key().to_string()),
                version_id: version.name.clone(),
                files,
            })
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&instance_dir);
        }
        result
    }

    /// Install a CurseForge modpack by project and file id. `file` omitted
    /// picks the newest release the API lists for the project.
    pub fn install_curseforge_pack(
        &self,
        project_id: u64,
        file_id: Option<u64>,
        instance: Option<&str>,
        version_progress: &(dyn Fn(InstallEvent) + Send + Sync),
        pack_progress: &(dyn Fn(PackEvent) + Send + Sync),
    ) -> Result<ModpackReport> {
        let http = Http::new()?;
        let curseforge = self.curseforge(&http)?;
        let file = self.curseforge_pack_file(&curseforge, project_id, file_id)?;
        let archive = self.download_curseforge_pack(&http, &curseforge, &file, pack_progress)?;
        let _archive = RemoveFileOnDrop(archive.clone());
        let manifest = tntl_curseforge::open_manifest(&archive)?;
        let game = manifest
            .game_version()
            .with_context(|| format!("{} does not name a Minecraft version", manifest.display_name()))?
            .to_string();
        let name = match instance {
            Some(name) => {
                validate_name(name)?;
                name.to_string()
            }
            None => sanitize_name(&manifest.display_name()),
        };
        let mut request = InstallRequest::new(&game);
        request.instance = Some(name.clone());
        self.apply_manifest_loader(&mut request, &manifest)?;
        let _report = self.install(&request, version_progress)?;

        let instance_dir = self.instance_dir(&name)?;
        let result = (|| -> Result<ModpackReport> {
            let instance = self.load_instance(&name)?;
            let game_dir = instance.game_dir(&instance_dir);
            let plans = self.curseforge_plan(&curseforge, &manifest)?;
            let mut files = self.curseforge_download(&http, &game_dir, &plans, pack_progress)?;
            files.overrides =
                tntl_curseforge::extract_overrides(&archive, &manifest, &game_dir, &mut |prefix, entries| {
                    pack_progress(PackEvent::Extracting {
                        prefix: prefix.to_string(),
                        entries,
                    })
                })?;
            let source = CursePackSource {
                project_id,
                pack_name: manifest.display_name(),
                version_id: file.id,
                version_name: curseforge_version(&file),
                file_date: file.file_date.clone(),
                release_type: file.release_type.max(1),
                game_version: game.clone(),
                loader: request.loader.map(|kind| kind.key().to_string()),
                loader_version: request.loader_version.clone(),
                files: plans.iter().map(curse_pack_entry).collect(),
            };
            tntl_store::atomic_write_json(&instance_dir.join(CURSEFORGE_PACK_FILE), &source)?;
            Ok(ModpackReport {
                instance: name.clone(),
                name: manifest.display_name(),
                game_version: game,
                loader: request.loader.map(|kind| kind.key().to_string()),
                version_id: if manifest.version.trim().is_empty() {
                    curseforge_version(&file)
                } else {
                    manifest.version.clone()
                },
                files,
            })
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&instance_dir);
        }
        result
    }

    /// Newest release of a CurseForge modpack, or one specific file.
    fn curseforge_pack_file(
        &self,
        curseforge: &CurseForge<'_>,
        project: u64,
        file: Option<u64>,
    ) -> Result<tntl_curseforge::File> {
        if let Some(file) = file {
            return Ok(curseforge.file(project, file)?);
        }
        let files = curseforge.files(project, &Filter::any_loader(""))?;
        files
            .iter()
            .find(|file| file.release_type == 1)
            .or_else(|| files.first())
            .cloned()
            .with_context(|| format!("CurseForge project {project} publishes no downloadable file"))
    }

    fn download_curseforge_pack(
        &self,
        http: &Http,
        curseforge: &CurseForge<'_>,
        file: &tntl_curseforge::File,
        progress: &(dyn Fn(PackEvent) + Send + Sync),
    ) -> Result<PathBuf> {
        let dir = self.root.join("tmp");
        fs::create_dir_all(&dir)?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or_default();
        let archive = dir.join(format!("{stamp}-{}.zip", file.id));
        let url = curseforge.download_url(file)?;
        let mut request = DownloadRequest::new(&url, &archive);
        if let Some(sha1) = file.hashes.iter().find(|hash| hash.algo == 1) {
            request = request.with_sha1(sha1.value.clone());
        }
        if file.file_length > 0 {
            request = request.with_size(file.file_length);
        }
        let total = (file.file_length > 0).then_some(file.file_length);
        http.download(&request, &mut |done| progress(PackEvent::Archive { done, total }))?;
        Ok(archive)
    }

    fn apply_manifest_loader(
        &self,
        request: &mut InstallRequest,
        manifest: &tntl_curseforge::PackManifest,
    ) -> Result<()> {
        if let Some((kind, version)) = manifest.loader() {
            let loader = LoaderKind::from_key(&kind).with_context(|| format!("unsupported pack loader {kind}"))?;
            request.loader = Some(loader);
            request.loader_version = Some(normalize_loader_version(loader, &request.version, &version));
        }
        Ok(())
    }

    /// Ask the API about every file in the manifest and work out where it goes.
    fn curseforge_plan(
        &self,
        curseforge: &CurseForge<'_>,
        manifest: &tntl_curseforge::PackManifest,
    ) -> Result<Vec<CursePackDownload>> {
        let mut classes: HashMap<u64, u32> = HashMap::new();
        let mut seen = HashSet::new();
        let mut plans = Vec::new();
        for entry in &manifest.files {
            if entry.project_id == 0 || entry.file_id == 0 {
                if entry.required {
                    bail!("the pack manifest names a file without project and file ids");
                }
                continue;
            }
            let file = curseforge.file(entry.project_id, entry.file_id)?;
            if !file.is_available {
                if entry.required {
                    bail!("{} is no longer downloadable on CurseForge", file.file_name);
                }
                continue;
            }
            let class = match classes.get(&entry.project_id) {
                Some(class) => *class,
                None => {
                    let class = curseforge.project(entry.project_id)?.class_id;
                    classes.insert(entry.project_id, class);
                    class
                }
            };
            let Some(dir) = tntl_curseforge::content_dir(class) else {
                if entry.required {
                    bail!(
                        "{} ships {}, which the launcher cannot place",
                        manifest.display_name(),
                        file.file_name
                    );
                }
                continue;
            };
            if !tntl_curseforge::valid_file_name(&file.file_name) {
                bail!("the pack ships a file with an unusable name: {}", file.file_name);
            }
            let path = format!("{dir}/{}", file.file_name);
            if !seen.insert(path.clone()) {
                continue;
            }
            plans.push(CursePackDownload {
                path,
                project_id: entry.project_id,
                file_id: entry.file_id,
                url: curseforge.download_url(&file)?,
                sha1: file
                    .hashes
                    .iter()
                    .find(|hash| hash.algo == 1)
                    .map(|hash| hash.value.clone()),
                size: file.file_length,
            });
        }
        Ok(plans)
    }

    /// The same plan for an archived sidecar, used by repair.
    fn curseforge_source_plan(
        &self,
        curseforge: &CurseForge<'_>,
        source: &CursePackSource,
    ) -> Result<Vec<CursePackDownload>> {
        let mut plans = Vec::new();
        for entry in &source.files {
            let file = curseforge.file(entry.project_id, entry.file_id)?;
            plans.push(CursePackDownload {
                path: entry.path.clone(),
                project_id: entry.project_id,
                file_id: entry.file_id,
                url: curseforge.download_url(&file)?,
                sha1: entry.sha1.clone().or_else(|| {
                    file.hashes
                        .iter()
                        .find(|hash| hash.algo == 1)
                        .map(|hash| hash.value.clone())
                }),
                size: if entry.size > 0 { entry.size } else { file.file_length },
            });
        }
        Ok(plans)
    }

    fn curseforge_download(
        &self,
        http: &Http,
        game_dir: &Path,
        plans: &[CursePackDownload],
        progress: &(dyn Fn(PackEvent) + Send + Sync),
    ) -> Result<PackReport> {
        fs::create_dir_all(game_dir)?;
        let files_total = plans.len();
        let bytes_total: u64 = plans.iter().map(|plan| plan.size).sum();
        let mut report = PackReport::default();
        let mut bytes_done = 0_u64;
        for (files_done, plan) in plans.iter().enumerate() {
            let relative = tntl_store::safe_relative(&plan.path)
                .with_context(|| format!("unsafe pack file path: {}", plan.path))?;
            let dest = tntl_store::target_respecting_disabled(&game_dir.join(&relative));
            if !dest.starts_with(game_dir) {
                bail!("unsafe pack file path: {}", plan.path);
            }
            let mut request = DownloadRequest::new(&plan.url, &dest);
            if let Some(sha1) = &plan.sha1 {
                request = request.with_sha1(sha1.clone());
            }
            if plan.size > 0 {
                request = request.with_size(plan.size);
            }
            let total = (plan.size > 0).then_some(plan.size);
            let base = bytes_done;
            let mut sink = |done: u64| {
                progress(PackEvent::Downloading {
                    path: plan.path.clone(),
                    done,
                    total,
                    files_done,
                    files_total,
                    bytes_done: base + done.min(plan.size),
                    bytes_total,
                });
            };
            let outcome = http.download(&request, &mut sink)?;
            report.files += 1;
            bytes_done += plan.size;
            if matches!(outcome, DownloadOutcome::AlreadyValid) {
                report.skipped += 1;
                progress(PackEvent::Skipped {
                    path: plan.path.clone(),
                });
            } else {
                report.downloaded += 1;
                report.bytes += plan.size;
                progress(PackEvent::Installed {
                    path: plan.path.clone(),
                    bytes: plan.size,
                });
            }
        }
        Ok(report)
    }

    /// Newer CurseForge build for the instance's game version; other versions
    /// are not offered because that would replace the whole game.
    fn check_curseforge_update(&self, instance: &Instance, source: &CursePackSource) -> Result<PackUpdate> {
        let http = Http::new()?;
        let curseforge = self.curseforge(&http)?;
        let files = curseforge.files(source.project_id, &Filter::any_loader(""))?;
        let allow = source.release_type.max(1);
        let newer = files.iter().find(|file| {
            file.file_date > source.file_date
                && file.release_type <= allow
                && file.game_versions.iter().any(|game| game == &source.game_version)
        });
        let targets = newer
            .map(|file| PackTarget {
                version_id: file.id.to_string(),
                version_number: curseforge_version(file),
                version_type: release_kind(file.release_type).to_string(),
                game_version: source.game_version.clone(),
                loader: source.loader.clone(),
                in_place: true,
                url: String::new(),
                sha1: None,
                size: file.file_length,
            })
            .into_iter()
            .collect();
        Ok(PackUpdate {
            instance: instance.name.clone(),
            name: source.pack_name.clone(),
            project_id: format!("curseforge:{}", source.project_id),
            current_version: source.version_name.clone(),
            current_game_version: source.game_version.clone(),
            targets,
        })
    }

    /// Replace a CurseForge pack with another build of the same project.
    fn update_curseforge_pack(
        &self,
        instance: &Instance,
        source: &CursePackSource,
        version_id: &str,
        version_progress: &(dyn Fn(InstallEvent) + Send + Sync),
        pack_progress: &(dyn Fn(PackEvent) + Send + Sync),
    ) -> Result<PackUpdateReport> {
        let file_id: u64 = version_id
            .trim()
            .parse()
            .with_context(|| format!("{version_id} is not a CurseForge file id"))?;
        let http = Http::new()?;
        let curseforge = self.curseforge(&http)?;
        let file = curseforge.file(source.project_id, file_id)?;
        let archive = self.download_curseforge_pack(&http, &curseforge, &file, pack_progress)?;
        let _archive = RemoveFileOnDrop(archive.clone());
        let manifest = tntl_curseforge::open_manifest(&archive)?;
        let game = manifest
            .game_version()
            .with_context(|| format!("{} does not name a Minecraft version", manifest.display_name()))?
            .to_string();
        let mut install = InstallRequest::new(&game);
        self.apply_manifest_loader(&mut install, &manifest)?;
        let report = self.install(&install, version_progress)?;

        let dir = self.instance_dir(&instance.name)?;
        let game_dir = instance.game_dir(&dir);
        let plans = self.curseforge_plan(&curseforge, &manifest)?;
        let mut files = self.curseforge_download(&http, &game_dir, &plans, pack_progress)?;
        // Install first, then drop files the new build no longer ships.
        let keep: HashSet<&str> = plans.iter().map(|plan| plan.path.as_str()).collect();
        let mut removed = 0;
        for entry in &source.files {
            if keep.contains(entry.path.as_str()) {
                continue;
            }
            let Some(relative) = tntl_store::safe_relative(&entry.path) else {
                continue;
            };
            let path = tntl_store::target_respecting_disabled(&game_dir.join(relative));
            if !path.is_file() {
                continue;
            }
            // Leave files the user changed alone.
            if let Some(sha1) = &entry.sha1
                && !tntl_store::sha1_file(&path).is_ok_and(|actual| actual.eq_ignore_ascii_case(sha1))
            {
                continue;
            }
            if fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
        files.overrides =
            tntl_curseforge::extract_overrides(&archive, &manifest, &game_dir, &mut |prefix, entries| {
                pack_progress(PackEvent::Extracting {
                    prefix: prefix.to_string(),
                    entries,
                })
            })?;
        let updated = CursePackSource {
            project_id: source.project_id,
            pack_name: manifest.display_name(),
            version_id: file.id,
            version_name: curseforge_version(&file),
            file_date: file.file_date.clone(),
            release_type: file.release_type.max(1),
            game_version: game.clone(),
            loader: install.loader.map(|kind| kind.key().to_string()),
            loader_version: install.loader_version.clone(),
            files: plans.iter().map(curse_pack_entry).collect(),
        };
        tntl_store::atomic_write_json(&dir.join(CURSEFORGE_PACK_FILE), &updated)?;
        self.set_instance_version(&instance.name, &report.version_id)?;

        Ok(PackUpdateReport {
            instance: instance.name.clone(),
            name: manifest.display_name(),
            version_number: curseforge_version(&file),
            version_id: report.version_id,
            game_version: game,
            loader: install.loader.map(|kind| kind.key().to_string()),
            removed,
            files,
        })
    }

    /// Add one finished game run to the instance's play time.
    pub fn record_playtime(&self, name: &str, duration_ms: u128) -> Result<()> {
        let dir = self.instance_dir(name)?;
        // The instance may have been deleted while the game was running; do not
        // resurrect a directory that no longer exists.
        if !dir.join(tntl_instance::INSTANCE_FILE).exists() {
            return Ok(());
        }
        tntl_instance::playtime::Playtime::record(&dir, duration_ms)?;
        Ok(())
    }

    pub fn instances_dir(&self) -> PathBuf {
        self.root.join("instances")
    }
    pub fn instance_dir(&self, name: &str) -> Result<PathBuf> {
        valid_component(name)?;
        Ok(self.instances_dir().join(name))
    }

    pub fn load_instance(&self, name: &str) -> Result<Instance> {
        let dir = self.instance_dir(name)?;
        Instance::load(&dir).with_context(|| format!("cannot load instance {name} at {}", dir.display()))
    }

    pub fn create_instance(&self, instance: &Instance) -> Result<PathBuf> {
        let dir = self.instance_dir(&instance.name)?;
        if dir.join(tntl_instance::INSTANCE_FILE).exists() {
            bail!("instance {} already exists; pick another name", instance.name);
        }
        instance.save(&dir)?;
        Ok(dir)
    }

    /// Register another instance on an already installed version chain. No
    /// network access: the version, libraries and assets are shared with every
    /// other instance that uses the same chain.
    pub fn create_instance_from_version(&self, version_id: &str, name: &str) -> Result<PathBuf> {
        validate_version_id(version_id)?;
        if !self.version_installed(version_id) {
            bail!("version {version_id} is not installed");
        }
        self.create_instance(&Instance::new(name.trim(), version_id))
    }

    pub fn save_instance(&self, instance: &Instance) -> Result<PathBuf> {
        let dir = self.instance_dir(&instance.name)?;
        instance.save(&dir)?;
        Ok(dir)
    }

    /// Point an instance at another version chain. The file is re-read here so
    /// edits made while a slow install was running (memory, JVM args, notes)
    /// survive instead of being reverted to the snapshot taken before it.
    fn set_instance_version(&self, name: &str, version_id: &str) -> Result<()> {
        let mut instance = self.load_instance(name)?;
        instance.version_id = version_id.to_string();
        self.save_instance(&instance)?;
        Ok(())
    }

    /// Record the pid of the game that is running from this instance. The file
    /// is how a second launcher process (the CLI, another window) knows the
    /// instance is in use; the in-memory flag only covers one process.
    pub fn mark_running(&self, name: &str, pid: u32) -> Result<()> {
        let dir = self.instance_dir(name)?;
        let mut marker = serde_json::json!({ "pid": pid });
        if let Some(started) = process_start_time(pid) {
            marker["start_time"] = serde_json::json!(started);
        }
        tntl_store::atomic_write_json(&dir.join(RUNNING_FILE), &marker)?;
        Ok(())
    }

    /// Drop the marker; a missing file is fine.
    pub fn clear_running(&self, name: &str) {
        if let Ok(dir) = self.instance_dir(name) {
            let _ = fs::remove_file(dir.join(RUNNING_FILE));
        }
    }

    /// Pid of a game still running from this instance, if any.
    pub fn running_pid(&self, name: &str) -> Option<u32> {
        let dir = self.instance_dir(name).ok()?;
        let path = dir.join(RUNNING_FILE);
        let bytes = fs::read(&path).ok()?;
        let value = serde_json::from_slice::<serde_json::Value>(&bytes).ok()?;
        let pid = value
            .get("pid")
            .and_then(serde_json::Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok())?;
        let started = value.get("start_time").and_then(serde_json::Value::as_u64);
        let same_process = match (started, process_start_time(pid)) {
            (Some(expected), Some(actual)) => expected == actual,
            // Older markers carry no start time, and an unreadable one must
            // not make a running game look dead.
            _ => true,
        };
        if process_alive(pid) && same_process {
            return Some(pid);
        }
        // The game (or the launcher) died without cleaning up, or the pid now
        // belongs to another process.
        let _ = fs::remove_file(&path);
        None
    }

    /// Refuse to touch an instance whose game is still running.
    pub fn ensure_not_running(&self, name: &str) -> Result<()> {
        if let Some(pid) = self.running_pid(name) {
            bail!("instance {name} is running (pid {pid}); close the game first");
        }
        Ok(())
    }

    /// Move an instance directory and update the name inside `instance.json`.
    pub fn rename_instance(&self, old: &str, new: &str) -> Result<PathBuf> {
        validate_name(new)?;
        self.ensure_not_running(old)?;
        let old_dir = self.instance_dir(old)?;
        let new_dir = self.instance_dir(new)?;
        if old == new {
            return Ok(old_dir);
        }
        if !old_dir.join(tntl_instance::INSTANCE_FILE).exists() {
            bail!("instance {old} does not exist");
        }
        if new_dir.exists() {
            bail!("instance {new} already exists; pick another name");
        }
        fs::rename(&old_dir, &new_dir).with_context(|| format!("cannot rename {old} to {new}"))?;
        let mut instance = Instance::load(&new_dir)?;
        instance.name = new.to_string();
        instance.save(&new_dir)?;
        Ok(new_dir)
    }

    /// Delete an instance directory (game files inside it included).
    pub fn delete_instance(&self, name: &str) -> Result<()> {
        self.ensure_not_running(name)?;
        let dir = self.instance_dir(name)?;
        if !dir.join(tntl_instance::INSTANCE_FILE).exists() {
            bail!("instance {name} does not exist");
        }
        fs::remove_dir_all(&dir).with_context(|| format!("cannot delete {}", dir.display()))?;
        Ok(())
    }

    /// Delete an instance and, when asked, the version directories only it used.
    /// Returns the version ids that were removed.
    pub fn delete_instance_with_cleanup(&self, name: &str, cleanup: bool) -> Result<Vec<String>> {
        let chain = self.instance_chain(name);
        self.delete_instance(name)?;
        if !cleanup {
            return Ok(Vec::new());
        }
        let mut removed = Vec::new();
        for id in chain {
            if !self.version_installed(&id) || !self.version_users(&id).is_empty() {
                continue;
            }
            let dir = self.versions_dir().join(&id);
            if fs::remove_dir_all(&dir).is_ok() {
                removed.push(id);
            }
        }
        Ok(removed)
    }

    /// Version ids an instance launches through, child first; empty when the
    /// instance or its metadata cannot be read.
    pub fn instance_chain(&self, name: &str) -> Vec<String> {
        let Ok(dir) = self.instance_dir(name) else {
            return Vec::new();
        };
        let Ok(instance) = Instance::load(&dir) else {
            return Vec::new();
        };
        let layout = instance.layout(&self.root);
        match self.load_chain(&layout, &instance.version_id) {
            Ok(chain) => chain.into_iter().map(|json| json.id).collect(),
            Err(_) => vec![instance.version_id.clone()],
        }
    }

    /// Instances that launch through `id`, loader versions included.
    pub fn version_users(&self, id: &str) -> Vec<String> {
        self.instance_chains()
            .into_iter()
            .filter(|(_, chain)| chain.iter().any(|version| version.as_str() == id))
            .map(|(name, _)| name)
            .collect()
    }

    fn instance_chains(&self) -> Vec<(String, Vec<String>)> {
        let Ok(entries) = fs::read_dir(self.instances_dir()) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        names
            .into_iter()
            .map(|name| {
                let chain = self.instance_chain(&name);
                (name, chain)
            })
            .collect()
    }

    pub fn game_dir(&self, name: &str) -> Result<PathBuf> {
        let instance = self.load_instance(name)?;
        Ok(instance.game_dir(&self.instance_dir(name)?))
    }

    /// Launcher-wide settings; missing or broken files fall back to defaults.
    pub fn settings(&self) -> LauncherSettings {
        let path = self.root.join(SETTINGS_FILE);
        let Ok(bytes) = fs::read(&path) else {
            return LauncherSettings::default().normalized();
        };
        match serde_json::from_slice::<LauncherSettings>(&bytes) {
            Ok(settings) => settings.normalized(),
            Err(error) => {
                // Keep the old file: it may still hold the Microsoft client id
                // and the CurseForge key, which cannot be regenerated.
                let stamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_secs())
                    .unwrap_or_default();
                let backup = path.with_extension(format!("broken-{stamp}"));
                eprintln!("cannot read {}: {error}; keeping {}", path.display(), backup.display());
                let _ = fs::rename(&path, &backup);
                LauncherSettings::default().normalized()
            }
        }
    }

    pub fn save_settings(&self, settings: &LauncherSettings) -> Result<LauncherSettings> {
        let settings = settings.clone().normalized();
        // The file carries the CurseForge key and any proxy credentials, so it is 0600.
        tntl_store::atomic_write_private_json(&self.root.join(SETTINGS_FILE), &settings)?;
        self.apply_network();
        Ok(settings)
    }

    /// Copy an image into the instance directory and use it as the icon.
    pub fn set_instance_icon(&self, name: &str, source: &Path) -> Result<String> {
        const MAX_ICON: u64 = 4 * 1024 * 1024;
        const ICON_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp", "gif"];
        let mut instance = self.load_instance(name)?;
        let dir = self.instance_dir(name)?;
        let extension = source
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase())
            .filter(|value| ICON_EXTENSIONS.contains(&value.as_str()))
            .with_context(|| format!("{} is not a PNG, JPEG, WebP or GIF image", source.display()))?;
        let size = fs::metadata(source).map(|meta| meta.len()).unwrap_or_default();
        if size > MAX_ICON {
            bail!("icon is larger than 4 MiB");
        }
        if !source.is_file() {
            bail!("{} is not a file", source.display());
        }
        let file = format!("icon.{extension}");
        fs::copy(source, dir.join(&file)).with_context(|| format!("cannot copy icon into {}", dir.display()))?;
        for other in ICON_EXTENSIONS.iter().filter(|other| **other != extension) {
            let _ = fs::remove_file(dir.join(format!("icon.{other}")));
        }
        instance.icon = Some(PathBuf::from(&file));
        instance.save(&dir)?;
        Ok(file)
    }

    pub fn clear_instance_icon(&self, name: &str) -> Result<()> {
        let mut instance = self.load_instance(name)?;
        let dir = self.instance_dir(name)?;
        if let Some(icon) = instance.icon.take() {
            let _ = fs::remove_file(dir.join(icon));
            instance.save(&dir)?;
        }
        Ok(())
    }

    /// Raw bytes of the instance icon; the shell turns them into a data URL.
    pub fn instance_icon(&self, name: &str) -> Result<Option<Vec<u8>>> {
        const MAX_ICON: u64 = 8 * 1024 * 1024;
        let instance = self.load_instance(name)?;
        let Some(icon) = instance.icon else {
            return Ok(None);
        };
        let path = self.instance_dir(name)?.join(icon);
        match fs::read(&path) {
            Ok(bytes) if bytes.len() as u64 <= MAX_ICON => Ok(Some(bytes)),
            Ok(_) => Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).with_context(|| format!("cannot read {}", path.display())),
        }
    }

    /// Write the instance directory as a portable `.zip` archive.
    pub fn export_archive(&self, name: &str, output: &Path) -> Result<tntl_instance::archive::ArchiveReport> {
        self.ensure_not_running(name)?;
        let dir = self.instance_dir(name)?;
        Ok(tntl_instance::archive::export(&dir, output)?)
    }

    /// Extract a `.zip` archive as a new instance.
    pub fn import_archive(&self, archive: &Path, name: &str) -> Result<Instance> {
        validate_name(name)?;
        let dir = tntl_instance::archive::import(archive, &self.instances_dir(), name)?;
        let mut instance = Instance::load(&dir)?;
        instance.name = name.to_string();
        // Archives are portable: absolute paths from the exporting machine must
        // not leak into the restored instance.
        instance.root = None;
        instance.game_dir = None;
        instance.save(&dir)?;
        Ok(instance)
    }

    pub fn crash_reports(&self, name: &str) -> Result<Vec<tntl_instance::crashes::CrashReport>> {
        Ok(tntl_instance::crashes::list(&self.game_dir(name)?)?)
    }

    pub fn read_crash_report(&self, name: &str, file: &str) -> Result<String> {
        Ok(tntl_instance::crashes::read(&self.game_dir(name)?, file)?)
    }

    pub fn delete_crash_report(&self, name: &str, file: &str) -> Result<()> {
        Ok(tntl_instance::crashes::delete(&self.game_dir(name)?, file)?)
    }

    pub fn screenshots(&self, name: &str) -> Result<Vec<tntl_instance::screenshots::Screenshot>> {
        Ok(tntl_instance::screenshots::list(&self.game_dir(name)?)?)
    }

    pub fn read_screenshot(&self, name: &str, file: &str) -> Result<Vec<u8>> {
        Ok(tntl_instance::screenshots::read(&self.game_dir(name)?, file)?)
    }

    pub fn delete_screenshot(&self, name: &str, file: &str) -> Result<()> {
        Ok(tntl_instance::screenshots::delete(&self.game_dir(name)?, file)?)
    }

    pub fn servers(&self, name: &str) -> Result<Vec<tntl_instance::servers::ServerEntry>> {
        Ok(tntl_instance::servers::list(&self.game_dir(name)?)?)
    }

    pub fn save_servers(
        &self,
        name: &str,
        entries: &[tntl_instance::servers::ServerEntry],
    ) -> Result<Vec<tntl_instance::servers::ServerEntry>> {
        let game_dir = self.game_dir(name)?;
        tntl_instance::servers::save(&game_dir, entries)?;
        Ok(tntl_instance::servers::list(&game_dir)?)
    }

    /// Size of everything this instance occupies, plus the shared directories.
    pub fn disk_usage(&self, name: &str) -> Result<DiskUsage> {
        let instance = self.load_instance(name)?;
        let dir = self.instance_dir(name)?;
        let game_dir = instance.game_dir(&dir);
        let mut entries = vec![
            DiskEntry::new("game_files", "Game files", game_dir.clone()),
            DiskEntry::new("natives", "Natives", dir.join("natives")),
            DiskEntry::new("logs", "Logs", dir.join("logs")),
            DiskEntry::new("crash_reports", "Crash reports", game_dir.join("crash-reports")),
            DiskEntry::new("world_backups", "World backups", dir.join("backups")),
            DiskEntry::new("shared_assets", "Shared assets", self.root.join("assets")),
            DiskEntry::new("shared_libraries", "Shared libraries", self.root.join("libraries")),
            DiskEntry::new("shared_versions", "Shared versions", self.root.join("versions")),
            DiskEntry::new("java_runtimes", "Java runtimes", self.root.join("java")),
        ];
        entries.retain(|entry| entry.bytes > 0 || ["game_files", "shared_assets"].contains(&entry.key.as_str()));
        let total = entries.iter().map(|entry| entry.bytes).sum();
        Ok(DiskUsage { entries, total })
    }

    /// Remove data the game or the launcher can rebuild: logs, crash reports
    /// and extracted natives.
    pub fn cleanup(&self, name: &str) -> Result<CleanupReport> {
        let dir = self.instance_dir(name)?;
        let game_dir = self.game_dir(name)?;
        let targets = [
            ("logs", dir.join("logs")),
            ("crash_reports", game_dir.join("crash-reports")),
            ("natives", dir.join("natives")),
        ];
        let mut report = CleanupReport::default();
        for (label, path) in targets {
            let bytes = dir_size(&path);
            if bytes == 0 && !path.exists() {
                continue;
            }
            fs::remove_dir_all(&path).with_context(|| format!("cannot remove {}", path.display()))?;
            report.removed.push(label.to_string());
            report.bytes += bytes;
        }
        Ok(report)
    }

    /// Versions inside an existing game root, for the import dialog.
    pub fn import_candidates(&self, path: &Path) -> Result<ImportScan> {
        if !path.is_dir() {
            bail!("{} is not a directory", path.display());
        }
        let (root, _) = import_root(path);
        Ok(scan_import_versions(&root))
    }

    /// Register an existing `.minecraft` directory as an instance without
    /// copying anything: the instance keeps a pointer to that root.
    pub fn import_instance(
        &self,
        path: &Path,
        name: Option<&str>,
        version: Option<&str>,
        progress: &(dyn Fn(InstallEvent) + Send + Sync),
    ) -> Result<Instance> {
        if !path.is_dir() {
            bail!("{} is not a directory", path.display());
        }
        let (root, picked) = import_root(path);
        let scan = scan_import_versions(&root);
        if scan.versions.is_empty() {
            if !scan.skipped.is_empty() {
                bail!(
                    "{} has versions but none could be read: {}",
                    root.join("versions").display(),
                    scan.skipped.join(", ")
                );
            }
            // A game directory that only holds mods, saves and configs: the
            // version has to be installed before the instance can launch.
            return self.import_game_dir(&root, name, version.or(picked.as_deref()), progress);
        }
        let chosen = match version.or(picked.as_deref()) {
            Some(wanted) => pick_candidate(&scan, wanted)?,
            None => scan
                .versions
                .first()
                .expect("scan already checked for an empty version list"),
        };
        let instance_name = name
            .map(ToString::to_string)
            .unwrap_or_else(|| sanitize_name(&chosen.id));
        validate_name(&instance_name)?;
        let dir = self.instance_dir(&instance_name)?;
        if dir.join(tntl_instance::INSTANCE_FILE).exists() {
            bail!("instance {instance_name} already exists; pick another name");
        }
        let mut instance = Instance::new(&instance_name, &chosen.id);
        instance.root = Some(root.clone());
        instance.game_dir = Some(root);
        instance.save(&dir)?;
        Ok(instance)
    }

    /// Import a game directory with no `versions/` folder: the version and the
    /// loader are read from the newest log (or from `version`), then installed
    /// into that directory so the instance can launch.
    fn import_game_dir(
        &self,
        path: &Path,
        name: Option<&str>,
        version: Option<&str>,
        progress: &(dyn Fn(InstallEvent) + Send + Sync),
    ) -> Result<Instance> {
        let detected = detect_install(&path.join("logs").join("latest.log"));
        let game = version
            .map(str::to_string)
            .or_else(|| detected.as_ref().map(|found| found.game.clone()))
            .with_context(|| {
                format!(
                    "{} has no versions/ folder and its log names no game version; pass the version (for example 1.20.1) so it can be installed",
                    path.display()
                )
            })?;
        let instance_name = name
            .map(ToString::to_string)
            .or_else(|| path.file_name().map(|name| directory_name(&name.to_string_lossy())))
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| sanitize_name(&game));
        validate_name(&instance_name)?;
        let dir = self.instance_dir(&instance_name)?;
        if dir.join(tntl_instance::INSTANCE_FILE).exists() {
            bail!("instance {instance_name} already exists; pick another name");
        }

        let mut instance = Instance::new(&instance_name, &game);
        instance.root = Some(path.to_path_buf());
        instance.game_dir = Some(path.to_path_buf());
        instance.group = Some("Imported".to_string());
        instance.save(&dir)?;

        let http = Http::new()?;
        let options = InstallOptions {
            assets: true,
            concurrency: self.settings().concurrency,
            verify_assets: false,
        };
        let result = (|| -> Result<Instance> {
            Installer::new(&http, instance.layout(&self.root), Environment::current())
                .install(&game, &options, progress)?;

            if let Some((kind, build)) = detected.and_then(|found| found.loader) {
                self.install_loader(&instance_name, kind, build.as_deref(), progress)?;
                return self.load_instance(&instance_name);
            }
            Ok(instance)
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&dir);
        }
        result
    }

    /// Import a Prism Launcher or MultiMC instance: copy its game directory,
    /// then install the Minecraft version and loader its `mmc-pack.json` lists.
    /// The shared libraries and assets are installed for the new instance
    /// instead of pointing at another launcher's folders.
    pub fn import_prism_instance(
        &self,
        path: &Path,
        name: Option<&str>,
        progress: &(dyn Fn(InstallEvent) + Send + Sync),
    ) -> Result<Instance> {
        let source = prism_instance_dir(path)?;
        let bytes = fs::read(source.join("mmc-pack.json"))
            .with_context(|| format!("cannot read {}", source.join("mmc-pack.json").display()))?;
        let pack: MmcPack =
            serde_json::from_slice(&bytes).with_context(|| format!("invalid mmc-pack.json at {}", source.display()))?;
        let game = pack
            .components
            .iter()
            .find(|component| component.uid == "net.minecraft")
            .map(|component| component.version.clone())
            .with_context(|| format!("{} names no Minecraft version", source.display()))?;
        let mut loader = None;
        for component in &pack.components {
            if component.uid == "net.minecraft" {
                continue;
            }
            if let Some(kind) = prism_loader(&component.uid) {
                loader = Some((kind, component.version.clone()));
                continue;
            }
            if PRISM_UNSUPPORTED.iter().any(|prefix| component.uid.starts_with(prefix)) {
                bail!(
                    "{} uses {}, which this launcher cannot install",
                    source.display(),
                    component.uid
                );
            }
        }
        let settings = prism_settings(&source.join("instance.cfg"));
        let instance_name = match name {
            Some(name) => name.to_string(),
            None => settings
                .name
                .clone()
                .filter(|name| !name.trim().is_empty())
                .or_else(|| source.file_name().map(|name| name.to_string_lossy().to_string()))
                .map(|name| sanitize_name(&name))
                .unwrap_or_else(|| sanitize_name(&game)),
        };
        validate_name(&instance_name)?;
        let dir = self.instance_dir(&instance_name)?;
        if dir.join(tntl_instance::INSTANCE_FILE).exists() {
            bail!("instance {instance_name} already exists; pick another name");
        }

        let mut request = InstallRequest::new(&game);
        request.instance = Some(instance_name.clone());
        if let Some((kind, version)) = &loader {
            request.loader = Some(*kind);
            request.loader_version = Some(version.clone());
        }
        self.install(&request, progress)?;

        let result = (|| -> Result<Instance> {
            let game_source = if source.join(".minecraft").is_dir() {
                source.join(".minecraft")
            } else {
                source.join("minecraft")
            };
            let mut instance = self.load_instance(&instance_name)?;
            let game_dir = instance.game_dir(&dir);
            if game_source.is_dir() && game_source != game_dir {
                copy_game_dir(&game_source, &game_dir)?;
            }
            if let Some(memory) = settings.max_memory_mb {
                instance.max_memory_mb = memory;
            }
            if !settings.notes.trim().is_empty() {
                instance.notes = settings.notes;
            }
            instance.group = Some("Imported".to_string());
            self.save_instance(&instance)?;
            Ok(instance)
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&dir);
        }
        result
    }

    /// Read `id` and its `inheritsFrom` chain from disk.
    pub fn load_chain(&self, layout: &RootLayout, id: &str) -> Result<Vec<VersionJson>> {
        let mut chain = Vec::new();
        let mut current = Some(id.to_string());
        while let Some(version_id) = current {
            if chain.iter().any(|json: &VersionJson| json.id == version_id) {
                break;
            }
            if chain.len() > 16 {
                bail!("version inheritance chain is too deep at {version_id}");
            }
            let path = layout.version_json(&version_id);
            let bytes = fs::read(&path).with_context(|| {
                format!(
                    "version {version_id} is not installed ({}); run `tntl install {version_id}`",
                    path.display()
                )
            })?;
            let json: VersionJson = serde_json::from_slice(&bytes)
                .with_context(|| format!("invalid version json at {}", path.display()))?;
            current = json.inherits_from.clone();
            chain.push(json);
        }
        Ok(chain)
    }

    pub fn required_java(&self, chain: &[VersionJson]) -> u32 {
        chain
            .iter()
            .find_map(|version| version.java_version.as_ref().and_then(|java| java.major_version))
            .unwrap_or(8)
    }

    pub fn resolve_java(&self, chain: &[VersionJson], override_path: Option<&Path>) -> (PathBuf, Option<JavaInstall>) {
        if let Some(path) = override_path {
            return (path.to_path_buf(), None);
        }
        let required = self.required_java(chain);
        let installs = self.java_installs();
        match tntl_java::select(&installs, required) {
            Some(install) => (install.path.clone(), Some(install.clone())),
            None => (PathBuf::from(if cfg!(windows) { "java.exe" } else { "java" }), None),
        }
    }

    /// Directory holding the runtimes this launcher downloaded itself.
    pub fn java_dir(&self) -> PathBuf {
        self.root.join("java")
    }

    /// Managed runtimes first, then whatever the machine already has.
    pub fn java_installs(&self) -> Vec<JavaInstall> {
        let mut installs = tntl_java::discover_in(&self.java_dir());
        installs.extend(tntl_java::discover());
        installs
    }

    pub fn plan_instance(&self, instance: &Instance, java: Option<&Path>, verify: VerifyMode) -> Result<LaunchPlan> {
        let account = self.effective_account(instance)?;
        self.plan_instance_with_account(instance, java, verify, account)
    }

    /// Same plan with a throwaway offline account, for file checks and dry
    /// runs: those never touch the network and must work before the first
    /// account is added. Launching stays on `plan_instance` and still needs a
    /// real account.
    pub fn plan_instance_unattended(
        &self,
        instance: &Instance,
        java: Option<&Path>,
        verify: VerifyMode,
    ) -> Result<LaunchPlan> {
        let account = Account::offline(&instance.username)
            .or_else(|_| Account::offline("Player"))
            .expect("offline name is valid");
        self.plan_instance_with_account(instance, java, verify, account)
    }

    /// Loader files whose content no longer matches the record the Forge or
    /// NeoForge installer wrote. Version JSONs usually do not hash the patched
    /// client jar, so this is the only way a file check can notice a loader
    /// that `repair` would rebuild.
    pub fn broken_loader_outputs(&self, name: &str) -> Result<Vec<PathBuf>> {
        let instance = self.load_instance(name)?;
        let layout = instance.layout(&self.root);
        Ok(tntl_forge::broken_outputs(&layout, &instance.version_id)?)
    }

    /// Same plan, with the account supplied by the caller. The doctor uses an
    /// offline account so checking an instance never refreshes a token or
    /// touches the network.
    fn plan_instance_with_account(
        &self,
        instance: &Instance,
        java: Option<&Path>,
        verify: VerifyMode,
        account: Account,
    ) -> Result<LaunchPlan> {
        let instance_dir = self.instance_dir(&instance.name)?;
        let layout = instance.layout(&self.root);
        let chain = self.load_chain(&layout, &instance.version_id)?;
        let override_path = java.or(instance.java.as_deref());
        let (java_path, _) = self.resolve_java(&chain, override_path);
        // A hand-picked runtime can be older than the version needs; without
        // this the game dies with a class-file error in its own log.
        if override_path.is_some() {
            let required = self.required_java(&chain);
            if let Some(install) = tntl_java::probe(&java_path)
                && install.major < required
            {
                bail!(
                    "the Java at {} is Java {} but this version needs Java {required}; pick another runtime or download it",
                    java_path.display(),
                    install.major
                );
            }
        }
        let mut options = InstanceOptions::new(instance.game_dir(&instance_dir), instance.natives_dir(&instance_dir));
        options.java = java_path;
        options.min_memory_mb = instance.min_memory_mb;
        options.max_memory_mb = instance.max_memory_mb;
        options.extra_jvm_args = instance.jvm_args.clone();
        options.extra_game_args = instance.game_args.clone();
        options.width = instance.width;
        options.height = instance.height;
        options.demo = instance.demo;
        options.env = instance.env.clone();
        if let Some(server) = &instance.server {
            options.server = Some((server.host.clone(), server.port));
            let game = environment_from_chain(&instance.version_id, &chain).game_version;
            options.quick_play = supports_quick_play(&game);
        }
        let mut env = Environment::current();
        if instance.demo {
            // Modern version JSONs only add `--demo` when this feature is on.
            env = env.with_feature("is_demo_user", true);
        }
        if instance.width.is_some() || instance.height.is_some() {
            // The `--width/--height` arguments in modern version JSON are gated
            // on this feature; without it the requested window size is ignored.
            env = env.with_feature("has_custom_resolution", true);
        }
        let input = ResolveInput {
            chain,
            roots: layout,
            env,
            account,
            options,
            verify,
            library_base_url: endpoints::LIBRARIES.to_string(),
            launcher_version: env!("CARGO_PKG_VERSION_MAJOR").to_string(),
        };
        Ok(build_plan(&input)?)
    }

    /// Instances with enough derived data to render a list.
    pub fn instance_views(&self) -> Result<Vec<InstanceView>> {
        let mut views = Vec::new();
        let accounts = AccountStore::load(&self.root).ok();
        for instance in list_instances(self)? {
            let dir = self.instance_dir(&instance.name)?;
            let game_dir = instance.game_dir(&dir);
            let layout = instance.layout(&self.root);
            let playtime = tntl_instance::playtime::Playtime::load(&dir);
            let icon = instance
                .icon
                .as_ref()
                .map(|icon| dir.join(icon))
                .filter(|path| path.is_file());
            views.push(InstanceView {
                installed: layout.version_json(&instance.version_id).exists(),
                last_log: newest_log(&dir),
                name: instance.name,
                version_id: instance.version_id,
                username: accounts
                    .as_ref()
                    .and_then(|store| store.current())
                    .map(|account| account.name.clone())
                    .unwrap_or_default(),
                max_memory_mb: instance.max_memory_mb,
                java: instance.java.map(|path| path.display().to_string()),
                imported: instance.root.is_some(),
                dir: dir.clone(),
                game_dir,
                modpack: modpack_name(&dir),
                group: instance.group,
                notes: instance.notes,
                icon,
                account_name: accounts
                    .as_ref()
                    .and_then(|store| store.current())
                    .map(|account| account.name.clone()),
                playtime_seconds: playtime.seconds,
                launches: playtime.launches,
                last_played: playtime.last_played,
            });
        }
        Ok(views)
    }

    pub fn versions_dir(&self) -> PathBuf {
        self.root.join("versions")
    }

    /// A version directory with its own JSON counts as installed.
    pub fn version_installed(&self, id: &str) -> bool {
        validate_version_id(id).is_ok() && self.layout().version_json(id).exists()
    }

    /// Versions already present in `versions/`.
    pub fn installed_versions(&self) -> Vec<String> {
        let dir = self.versions_dir();
        let mut versions = Vec::new();
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let id = entry.file_name().to_string_lossy().to_string();
                if self.version_installed(&id) {
                    versions.push(id);
                }
            }
        }
        versions.sort();
        versions
    }

    /// Installed versions with their size and the instances that need them.
    pub fn installed_version_details(&self) -> Vec<InstalledVersion> {
        let chains = self.instance_chains();
        self.installed_versions()
            .into_iter()
            .map(|id| {
                let instances = chains
                    .iter()
                    .filter(|(_, chain)| chain.iter().any(|version| version.as_str() == id.as_str()))
                    .map(|(name, _)| name.clone())
                    .collect::<Vec<_>>();
                let bytes = dir_size(&self.versions_dir().join(&id));
                InstalledVersion { id, bytes, instances }
            })
            .collect()
    }

    /// Remove one installed version. Refuses while an instance still uses it;
    /// shared libraries and assets are left alone.
    pub fn delete_version(&self, id: &str) -> Result<()> {
        validate_version_id(id)?;
        if !self.version_installed(id) {
            bail!("version {id} is not installed");
        }
        let users = self.version_users(id);
        if !users.is_empty() {
            bail!(
                "version {id} is used by {}; delete or move those instances first",
                users.join(", ")
            );
        }
        let dir = self.versions_dir().join(id);
        fs::remove_dir_all(&dir).with_context(|| format!("cannot delete {}", dir.display()))
    }

    /// Remove every installed version no instance uses. Returns the removed ids
    /// and the bytes freed.
    pub fn delete_unused_versions(&self) -> Result<(Vec<String>, u64)> {
        let mut removed = Vec::new();
        let mut bytes = 0;
        for version in self.installed_version_details() {
            if !version.instances.is_empty() {
                continue;
            }
            if self.delete_version(&version.id).is_ok() {
                bytes += version.bytes;
                removed.push(version.id);
            }
        }
        Ok((removed, bytes))
    }

    /// Releases only, releases plus snapshots, or everything including
    /// alpha and beta builds.
    pub fn versions(&self, channel: &str) -> Result<Vec<VersionView>> {
        let http = Http::new()?;
        let manifest = http.version_manifest()?;
        let installed = self.installed_versions();
        let channel = channel.trim().to_ascii_lowercase();
        let wanted = |kind: &str| match channel.as_str() {
            "" | "release" | "stable" => kind == "release",
            "snapshot" | "snapshots" => kind == "release" || kind == "snapshot",
            _ => true,
        };
        Ok(manifest
            .versions
            .into_iter()
            .filter(|version| wanted(&version.version_type))
            .map(|version| VersionView {
                installed: installed.contains(&version.id),
                id: version.id,
                version_type: version.version_type,
                release_time: version.release_time.unwrap_or_default(),
            })
            .collect())
    }

    /// Loader builds published for a game version, newest first.
    pub fn loader_versions(&self, game: &str, kind: LoaderKind) -> Result<Vec<LoaderVersion>> {
        let http = Http::new()?;
        if let Some(forge) = tntl_forge::ForgeKind::from_loader(kind) {
            return Ok(tntl_forge::versions(&http, forge, game)?);
        }
        Ok(http.loader_versions(kind, game)?)
    }

    /// Mod loader an instance runs right now; `None` for vanilla.
    pub fn loader_status(&self, name: &str) -> Result<LoaderStatus> {
        let instance = self.load_instance(name)?;
        let layout = instance.layout(&self.root);
        let chain = self.load_chain(&layout, &instance.version_id)?;
        let environment = environment_from_chain(&instance.version_id, &chain);
        let loader = installed_loader(&chain, &environment.game_version);
        Ok(LoaderStatus {
            game_version: environment.game_version,
            kind: loader.as_ref().map(|(kind, _)| kind.key().to_string()),
            version: loader.map(|(_, version)| version),
            version_id: instance.version_id,
        })
    }

    /// Install or switch the mod loader of an existing instance. Version files
    /// land in the instance's own shared root, so imported instances keep
    /// their `.minecraft` self-contained.
    pub fn install_loader(
        &self,
        name: &str,
        kind: LoaderKind,
        version: Option<&str>,
        progress: &(dyn Fn(InstallEvent) + Send + Sync),
    ) -> Result<LoaderStatus> {
        self.ensure_not_running(name)?;
        let instance = self.load_instance(name)?;
        let layout = instance.layout(&self.root);
        let environment = self.environment(name)?;
        let game = environment.game_version;
        let http = Http::new()?;
        let target = match kind {
            kind if kind.is_installer() => {
                let request = InstallRequest {
                    version: game,
                    loader: Some(kind),
                    loader_version: version.map(str::to_string),
                    assets: true,
                    instance: None,
                };
                let id = self.install_forge(&layout, &http, &request, kind, progress)?;
                // Legacy installers keep their libraries in the version JSON,
                // so pull whatever the profile did not download.
                let options = InstallOptions {
                    assets: true,
                    concurrency: self.settings().concurrency,
                    verify_assets: false,
                };
                Installer::new(&http, layout.clone(), Environment::current()).install(&id, &options, progress)?;
                id
            }
            kind => {
                let (id, profile) = self.loader_profile(&http, &game, kind, version)?;
                tntl_store::atomic_write(&layout.version_json(&id), &profile)?;
                let options = InstallOptions {
                    assets: true,
                    concurrency: self.settings().concurrency,
                    verify_assets: false,
                };
                Installer::new(&http, layout.clone(), Environment::current()).install(&id, &options, progress)?;
                id
            }
        };
        self.set_instance_version(name, &target)?;
        self.loader_status(name)
    }

    /// Drop the mod loader and launch the vanilla version underneath it.
    pub fn remove_loader(&self, name: &str) -> Result<LoaderStatus> {
        let instance = self.load_instance(name)?;
        let environment = self.environment(name)?;
        if environment.loader.is_none() {
            bail!("instance {name} has no mod loader");
        }
        let layout = instance.layout(&self.root);
        if !layout.version_json(&environment.game_version).exists() {
            bail!(
                "vanilla {} is not installed in {}",
                environment.game_version,
                layout.root.display()
            );
        }
        self.set_instance_version(name, &environment.game_version)?;
        self.loader_status(name)
    }

    /// Compare the instance's loader with the newest published build.
    pub fn check_loader_update(&self, name: &str) -> Result<LoaderUpdate> {
        let status = self.loader_status(name)?;
        let kind_key = status
            .kind
            .clone()
            .with_context(|| format!("instance {name} has no mod loader"))?;
        let current = status.version.clone().unwrap_or_default();
        let kind = LoaderKind::from_key(&kind_key).with_context(|| format!("unknown loader {kind_key}"))?;
        let versions = self.loader_versions(&status.game_version, kind)?;
        let latest = versions
            .iter()
            .find(|entry| entry.stable)
            .or_else(|| versions.first())
            .with_context(|| format!("no {kind} build is published for {}", status.game_version))?;
        // The lists are newest first, so the current build sitting before the
        // newest stable one means the instance is on a prerelease or a fork.
        let newer = match versions.iter().position(|entry| entry.version == current) {
            Some(index) => {
                let latest_index = versions
                    .iter()
                    .position(|entry| entry.version == latest.version)
                    .unwrap_or(0);
                index > latest_index
            }
            None => latest.version != current,
        };
        Ok(LoaderUpdate {
            kind: kind_key,
            current,
            latest: latest.version.clone(),
            newer,
        })
    }

    /// Download a version (and its loader, if asked) and optionally register it
    /// as an instance.
    pub fn install(
        &self,
        request: &InstallRequest,
        progress: &(dyn Fn(InstallEvent) + Send + Sync),
    ) -> Result<InstallReport> {
        if let Some(name) = request.instance.as_deref() {
            validate_name(name)?;
            let dir = self.instance_dir(name)?;
            if dir.join(tntl_instance::INSTANCE_FILE).exists() {
                bail!("instance {name} already exists; pick another name");
            }
        }

        let http = Http::new()?;
        let target = match request.loader {
            Some(kind) if kind.is_installer() => self.install_forge(&self.layout(), &http, request, kind, progress)?,
            Some(kind) => {
                let (id, profile) =
                    self.loader_profile(&http, &request.version, kind, request.loader_version.as_deref())?;
                tntl_store::atomic_write(&self.layout().version_json(&id), &profile)?;
                id
            }
            None => request.version.clone(),
        };

        let options = InstallOptions {
            assets: request.assets,
            concurrency: self.settings().concurrency,
            verify_assets: false,
        };
        let installer = Installer::new(&http, self.layout(), Environment::current());
        let mut report = installer.install(&target, &options, progress)?;
        if let Some(name) = request.instance.as_deref() {
            let instance = Instance::new(name, &target);
            self.create_instance(&instance)?;
            report.instance = Some(name.to_string());
        }
        Ok(report)
    }

    /// Install a Modrinth modpack: the game version, its loader, then the pack's
    /// files and overrides. The pack is registered as an instance.
    pub fn install_modpack(
        &self,
        archive: &Path,
        instance: Option<&str>,
        version_progress: &(dyn Fn(InstallEvent) + Send + Sync),
        pack_progress: &(dyn Fn(PackEvent) + Send + Sync),
    ) -> Result<ModpackReport> {
        let pack = Modpack::open(archive)?;
        let game = pack.game_version()?;
        let name = match instance {
            Some(name) => {
                validate_name(name)?;
                name.to_string()
            }
            None => sanitize_name(&pack.name()),
        };
        let mut request = InstallRequest::new(&game);
        request.instance = Some(name.clone());
        if let Some((loader, version)) = pack.loader() {
            let kind =
                LoaderKind::from_key(loader.key()).with_context(|| format!("unsupported pack loader {loader}"))?;
            request.loader = Some(kind);
            request.loader_version = version;
        }
        let report = self.install(&request, version_progress)?;

        let instance_dir = self.instance_dir(&name)?;
        // A failure after the instance was registered would leave a half
        // installed, unrepairable instance that cannot be retried under the
        // same name. Drop it instead.
        let result = (|| -> Result<ModpackReport> {
            let instance = self.load_instance(&name)?;
            let game_dir = instance.game_dir(&instance_dir);
            let http = Http::new()?;
            let files = pack.install(&http, &game_dir, &mut |event| pack_progress(event))?;
            tntl_store::atomic_write_json(&instance_dir.join(PACK_FILE), &pack.index)?;
            self.identify_pack(&pack, archive, &http, &instance_dir);
            Ok(ModpackReport {
                instance: name.clone(),
                name: pack.name(),
                game_version: game,
                loader: request.loader.map(|kind| kind.key().to_string()),
                version_id: report.version_id.clone(),
                files,
            })
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&instance_dir);
        }
        result
    }

    /// Remember which Modrinth project a freshly installed pack came from.
    /// The archive hash is the reliable way; the name is the fallback for
    /// re-packed archives. Failures leave the instance installable, just
    /// without update checks.
    fn identify_pack(&self, pack: &Modpack, archive: &Path, http: &Http, dir: &Path) {
        let remember = || -> Result<()> {
            let modrinth = Modrinth::new(http);
            let by_hash = tntl_store::sha1_file(archive)
                .ok()
                .and_then(|sha1| modrinth.version_by_hash(&sha1).ok().flatten())
                .filter(|version| version.files.iter().any(|file| file.filename.ends_with(".mrpack")));
            let source = match by_hash {
                Some(version) => PackSource {
                    project_id: version.project_id,
                    version_number: version.version_number,
                },
                None => match modrinth.find_modpack(&pack.index.name)? {
                    Some(hit) => PackSource {
                        project_id: hit.project_id,
                        version_number: pack.index.version_id.clone(),
                    },
                    None => return Ok(()),
                },
            };
            tntl_store::atomic_write_json(&dir.join(PACK_SOURCE_FILE), &source)?;
            Ok(())
        };
        let _ = remember();
    }

    /// Newer builds of the Modrinth pack an instance was installed from. The
    /// newest build for the instance's own game version comes first; a build
    /// for another Minecraft version is offered as a separate target.
    pub fn check_pack_update(&self, name: &str) -> Result<PackUpdate> {
        let instance = self.load_instance(name)?;
        let dir = self.instance_dir(name)?;
        if let Some(source) = read_ftb_source(&dir) {
            return self.check_ftb_update(&instance, &source);
        }
        if let Some(source) = read_curseforge_source(&dir) {
            return self.check_curseforge_update(&instance, &source);
        }
        let index = read_pack_index(&dir)?;
        let environment = self.environment(name)?;
        let http = Http::new()?;
        let modrinth = Modrinth::new(&http);
        let project_id = pack_project(&modrinth, &dir, &index)?;
        let versions = modrinth.project_versions(&project_id)?;
        // The archive's own `versionId` is author-defined (dates, `v1.0.0`, …),
        // so prefer the Modrinth version number recorded at install time and
        // keep the index value as a fallback.
        let recorded = read_pack_source(&dir)
            .map(|source| source.version_number.trim().to_string())
            .unwrap_or_default();
        let current_version = [recorded.as_str(), index.version_id.trim()]
            .into_iter()
            .find(|candidate| {
                !candidate.is_empty() && versions.iter().any(|version| version.version_number == *candidate)
            })
            .unwrap_or_else(|| index.version_id.trim())
            .to_string();
        let current = versions
            .iter()
            .find(|version| version.version_number == current_version)
            .with_context(|| {
                format!("Modrinth no longer lists version {current_version} of {project_id}; check the pack by hand")
            })?;
        let current_date = current.date_published.clone();
        // Stay on the channel the instance is on: release builds are not
        // bothered with betas, while beta users still see releases and betas.
        let allow_beta = current.version_type != "release";
        let allow_alpha = current.version_type == "alpha";
        let newer: Vec<_> = versions
            .into_iter()
            .filter(|version| version.date_published > current_date)
            .collect();

        let mut targets = Vec::new();
        let same_game: Vec<_> = newer
            .iter()
            .filter(|version| {
                version
                    .game_versions
                    .iter()
                    .any(|game| game == &environment.game_version)
            })
            .collect();
        if let Some(version) =
            pick_pack_version(&same_game, allow_beta, allow_alpha).and_then(|version| pack_target(version, true))
        {
            targets.push(version);
        }
        let all: Vec<_> = newer.iter().collect();
        if let Some(version) =
            pick_pack_version(&all, allow_beta, allow_alpha).and_then(|version| pack_target(version, false))
            && targets
                .first()
                .is_none_or(|target| target.version_id != version.version_id)
        {
            targets.push(version);
        }
        Ok(PackUpdate {
            instance: instance.name,
            name: index.name,
            project_id,
            current_version,
            current_game_version: environment.game_version,
            targets,
        })
    }

    /// Download a newer pack build, install its game and loader, replace the
    /// pack files and point the instance at the new version. Pack files the new
    /// build dropped are removed only when they are still untouched.
    pub fn update_pack(
        &self,
        name: &str,
        version_id: &str,
        version_progress: &(dyn Fn(InstallEvent) + Send + Sync),
        pack_progress: &(dyn Fn(PackEvent) + Send + Sync),
    ) -> Result<PackUpdateReport> {
        self.ensure_not_running(name)?;
        let instance = self.load_instance(name)?;
        let dir = self.instance_dir(name)?;
        if let Some(source) = read_ftb_source(&dir) {
            return self.update_ftb_pack(&instance, &source, version_id, version_progress, pack_progress);
        }
        if let Some(source) = read_curseforge_source(&dir) {
            return self.update_curseforge_pack(&instance, &source, version_id, version_progress, pack_progress);
        }
        let old_index = read_pack_index(&dir)?;
        let http = Http::new()?;
        let version = Modrinth::new(&http).version(version_id)?;
        let file = version
            .files
            .iter()
            .find(|file| file.filename.ends_with(".mrpack"))
            .with_context(|| format!("version {} does not publish a .mrpack file", version.version_number))?;
        let temp_dir = self.root.join("tmp");
        fs::create_dir_all(&temp_dir)?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or_default();
        let archive = temp_dir.join(format!("{stamp}-{}", sanitize_name(&file.filename)));
        let mut request = DownloadRequest::new(&file.url, &archive);
        if let Some(sha1) = &file.hashes.sha1 {
            request = request.with_sha1(sha1.clone());
        }
        if file.size > 0 {
            request = request.with_size(file.size);
        }
        let total = (file.size > 0).then_some(file.size);
        http.download(&request, &mut |done| pack_progress(PackEvent::Archive { done, total }))?;

        let result = (|| -> Result<PackUpdateReport> {
            let pack = Modpack::open(&archive)?;
            let game = pack.game_version()?;
            let mut install = InstallRequest::new(&game);
            if let Some((loader, loader_version)) = pack.loader() {
                let kind =
                    LoaderKind::from_key(loader.key()).with_context(|| format!("unsupported pack loader {loader}"))?;
                install.loader = Some(kind);
                install.loader_version = loader_version;
            }
            let report = self.install(&install, version_progress)?;

            let game_dir = instance.game_dir(&dir);
            // Install first, then drop files the new build no longer ships; a
            // failure halfway must not leave the instance without its mods.
            let files = pack.install(&http, &game_dir, &mut |event| pack_progress(event))?;
            let removed = remove_stale_pack_files(&game_dir, &old_index, &pack.index);
            tntl_store::atomic_write_json(&dir.join(PACK_FILE), &pack.index)?;
            tntl_store::atomic_write_json(
                &dir.join(PACK_SOURCE_FILE),
                &PackSource {
                    project_id: version.project_id.clone(),
                    version_number: version.version_number.clone(),
                },
            )?;
            self.set_instance_version(name, &report.version_id)?;
            Ok(PackUpdateReport {
                instance: name.to_string(),
                name: pack.name(),
                version_number: version.version_number,
                version_id: report.version_id,
                game_version: game,
                loader: install.loader.map(|kind| kind.key().to_string()),
                removed,
                files,
            })
        })();
        let _ = fs::remove_file(&archive);
        result
    }

    /// Re-check every file the launcher installed for an instance and fetch the
    /// ones that are missing or fail their hash. Overrides and files the user
    /// added are left alone.
    pub fn repair_instance(
        &self,
        name: &str,
        version_progress: &(dyn Fn(InstallEvent) + Send + Sync),
        pack_progress: &(dyn Fn(PackEvent) + Send + Sync),
    ) -> Result<RepairReport> {
        self.ensure_not_running(name)?;
        let instance = self.load_instance(name)?;
        let dir = self.instance_dir(name)?;
        let http = Http::new()?;
        let options = InstallOptions {
            assets: true,
            concurrency: self.settings().concurrency,
            verify_assets: true,
        };
        let layout = instance.layout(&self.root);
        let installer = Installer::new(&http, layout.clone(), Environment::current());
        let installed = installer.install(&instance.version_id, &options, version_progress)?;
        let mut report = RepairReport {
            game_downloaded: installed.downloaded,
            game_skipped: installed.skipped,
            bytes: installed.bytes,
            ..RepairReport::default()
        };
        // Patch-based loaders leave jars that the version JSON does not hash.
        // Check the record the installer wrote and rebuild when it disagrees.
        let status = self.loader_status(name)?;
        if let Some(kind) = status.kind.as_deref().and_then(LoaderKind::from_key)
            && tntl_forge::ForgeKind::from_loader(kind).is_some()
            && let Some(version) = status.version.as_deref()
            && !tntl_forge::broken_outputs(&layout, &instance.version_id)?.is_empty()
        {
            self.install_loader(name, kind, Some(version), version_progress)?;
            report.loader_rebuilt = true;
        }
        let game_dir = instance.game_dir(&dir);
        if dir.join(PACK_FILE).exists() {
            let index = read_pack_index(&dir)?;
            let pack = tntl_modpack::repair_index(&http, &game_dir, &index, &mut |event| pack_progress(event))?;
            report.pack_downloaded = pack.downloaded;
            report.pack_skipped = pack.skipped;
            report.bytes += pack.bytes;
            report.pack = Some(index.name);
        } else if let Some(source) = read_ftb_source(&dir) {
            let ftb = tntl_ftb::Ftb::new(&http);
            let version = ftb.version(source.pack_id, source.version_id)?;
            let pack = ftb.install(&version, &game_dir, self.settings().concurrency, pack_progress)?;
            report.pack_downloaded = pack.downloaded;
            report.pack_skipped = pack.skipped;
            report.bytes += pack.bytes;
            report.pack = Some(source.pack_name);
        } else if let Some(source) = read_curseforge_source(&dir) {
            let curseforge = self.curseforge(&http)?;
            let plans = self.curseforge_source_plan(&curseforge, &source)?;
            let pack = self.curseforge_download(&http, &game_dir, &plans, pack_progress)?;
            report.pack_downloaded = pack.downloaded;
            report.pack_skipped = pack.skipped;
            report.bytes += pack.bytes;
            report.pack = Some(source.pack_name);
        }
        Ok(report)
    }

    /// Newer builds of the FTB pack an instance came from. A build for the
    /// instance's own game version wins; the newest release of another version
    /// is offered as a separate target.
    fn check_ftb_update(&self, instance: &Instance, source: &FtbSource) -> Result<PackUpdate> {
        let http = Http::new()?;
        let ftb = tntl_ftb::Ftb::new(&http);
        let pack = ftb.pack(source.pack_id)?;
        // Stay on the channel the instance is on.
        let allow_beta = !source.version_kind.is_empty() && source.version_kind != "release";
        let newer: Vec<&tntl_ftb::VersionSummary> = pack
            .versions_after(source.version_id)
            .into_iter()
            .filter(|version| allow_beta || version.kind == "release")
            .collect();
        let environment = self.environment(&instance.name)?;
        let mut targets = Vec::new();
        if let Some(version) = newer
            .iter()
            .rev()
            .find(|version| version.game_version == environment.game_version)
        {
            targets.push(ftb_target(version, true));
        }
        if let Some(version) = newer.last() {
            let target = ftb_target(version, version.game_version == environment.game_version);
            if targets
                .first()
                .is_none_or(|first| first.version_id != target.version_id)
            {
                targets.push(target);
            }
        }
        Ok(PackUpdate {
            instance: instance.name.clone(),
            name: source.pack_name.clone(),
            project_id: format!("ftb:{}", source.pack_id),
            current_version: source.version_name.clone(),
            current_game_version: environment.game_version,
            targets,
        })
    }

    /// Replace an FTB pack installation with another build: install its game
    /// and loader, drop files the old build shipped and the new one does not,
    /// then download the new file list.
    fn update_ftb_pack(
        &self,
        instance: &Instance,
        source: &FtbSource,
        version_id: &str,
        version_progress: &(dyn Fn(InstallEvent) + Send + Sync),
        pack_progress: &(dyn Fn(PackEvent) + Send + Sync),
    ) -> Result<PackUpdateReport> {
        let wanted: u64 = version_id
            .trim()
            .parse()
            .with_context(|| format!("{version_id} is not an FTB version id"))?;
        let http = Http::new()?;
        let ftb = tntl_ftb::Ftb::new(&http);
        let pack = ftb.pack(source.pack_id)?;
        let version = ftb.version(source.pack_id, wanted)?;
        let mut install = InstallRequest::new(&version.game_version);
        if let Some(loader) = &version.loader {
            install.loader = Some(
                LoaderKind::from_key(loader.kind.key())
                    .with_context(|| format!("unsupported pack loader {}", loader.kind))?,
            );
            install.loader_version = Some(loader.version.clone());
        }
        let report = self.install(&install, version_progress)?;

        let dir = self.instance_dir(&instance.name)?;
        let game_dir = instance.game_dir(&dir);
        let files = ftb.install(&version, &game_dir, self.settings().concurrency, pack_progress)?;
        // Install first, then drop files the new build no longer ships; a
        // failure halfway must not leave the instance without its mods.
        let keep: HashSet<&str> = version.client_files().map(|file| file.path.as_str()).collect();
        let mut removed = 0;
        for file in &source.files {
            if keep.contains(file.path.as_str()) {
                continue;
            }
            let Some(relative) = tntl_store::safe_relative(&file.path) else {
                continue;
            };
            let path = tntl_store::target_respecting_disabled(&game_dir.join(relative));
            if !path.is_file() {
                continue;
            }
            // Leave files the user changed alone.
            if let Some(sha1) = &file.sha1
                && !tntl_store::sha1_file(&path).is_ok_and(|actual| actual.eq_ignore_ascii_case(sha1))
            {
                continue;
            }
            if fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
        let updated_source = FtbSource {
            pack_id: source.pack_id,
            pack_name: pack.name.clone(),
            version_id: version.id,
            version_name: version.name.clone(),
            version_kind: version.kind.clone(),
            updated: version.updated,
            game_version: version.game_version.clone(),
            loader: version.loader_key(),
            files: version
                .client_files()
                .map(|file| FtbFileEntry {
                    path: file.path.clone(),
                    sha1: file.sha1.clone(),
                })
                .collect(),
        };
        tntl_store::atomic_write_json(&dir.join(FTB_FILE), &updated_source)?;
        self.set_instance_version(&instance.name, &report.version_id)?;

        Ok(PackUpdateReport {
            instance: instance.name.clone(),
            name: pack.name,
            version_number: version.name,
            version_id: report.version_id,
            game_version: version.game_version,
            loader: install.loader.map(|kind| kind.key().to_string()),
            removed,
            files,
        })
    }

    /// Forge and NeoForge run their own installer over the vanilla client jar,
    /// so that version has to be installed before the loader is.
    fn install_forge(
        &self,
        layout: &RootLayout,
        http: &Http,
        request: &InstallRequest,
        kind: LoaderKind,
        progress: &(dyn Fn(InstallEvent) + Send + Sync),
    ) -> Result<String> {
        let forge = tntl_forge::ForgeKind::from_loader(kind)
            .with_context(|| format!("{kind} does not install from an installer jar"))?;
        let options = InstallOptions {
            assets: request.assets,
            concurrency: self.settings().concurrency,
            verify_assets: false,
        };
        Installer::new(http, layout.clone(), Environment::current()).install(&request.version, &options, progress)?;

        let java = self.java_for_version(layout, &request.version)?;
        let installer = tntl_forge::ForgeInstaller::new(http, layout.clone(), java);
        let version = match request.loader_version.clone() {
            Some(version) => version,
            None => {
                installer
                    .versions(forge, &request.version)?
                    .into_iter()
                    .next()
                    .with_context(|| format!("no {forge} build is published for {}", request.version))?
                    .version
            }
        };
        let installed = installer.install(forge, &request.version, &version, progress)?;
        Ok(installed.version_id)
    }

    /// Java runtime the game version asks for; the installer processors run on it.
    fn java_for_version(&self, layout: &RootLayout, version_id: &str) -> Result<PathBuf> {
        let path = layout.version_json(version_id);
        let bytes = fs::read(&path).with_context(|| format!("version {version_id} is not installed"))?;
        let json: VersionJson =
            serde_json::from_slice(&bytes).with_context(|| format!("invalid version json at {}", path.display()))?;
        let required = json
            .java_version
            .as_ref()
            .and_then(|java| java.major_version)
            .unwrap_or(8);
        match tntl_java::select(&self.java_installs(), required) {
            Some(install) => Ok(install.path.clone()),
            None => {
                bail!("no Java {required} runtime found; install one from the settings page or point JAVA_HOME at it")
            }
        }
    }

    /// Java runtimes Mojang publishes for this platform.
    pub fn java_offers(&self) -> Result<Vec<RuntimeOffer>> {
        let http = Http::new()?;
        let mut offers = tntl_runtime::offers(&http)?;
        for offer in tntl_runtime::adoptium::offers(&http).unwrap_or_default() {
            offers.push(RuntimeOffer {
                component: offer.component,
                version: offer.version,
                released: None,
            });
        }
        Ok(offers)
    }

    /// Download a runtime into the launcher's own `java/` directory. Mojang
    /// components (`java-runtime-*`) and `adoptium-<major>` both work.
    pub fn install_java_runtime(
        &self,
        component: &str,
        progress: &(dyn Fn(RuntimeEvent) + Send + Sync),
    ) -> Result<InstalledRuntime> {
        let http = Http::new()?;
        let home = self.java_dir().join(sanitize_name(component));
        if let Some(major) = tntl_runtime::adoptium::major_of(component) {
            return Ok(tntl_runtime::adoptium::install(&http, &home, major, &mut |event| {
                progress(event)
            })?);
        }
        Ok(tntl_runtime::install(&http, &home, component, &mut |event| {
            progress(event)
        })?)
    }

    /// Fetch a loader profile and check that it is safe to install.
    fn loader_profile(
        &self,
        http: &Http,
        game: &str,
        kind: LoaderKind,
        wanted: Option<&str>,
    ) -> Result<(String, Vec<u8>)> {
        let versions = http.loader_versions(kind, game)?;
        let chosen = match wanted {
            Some(wanted) => versions
                .iter()
                .find(|entry| entry.version == wanted)
                .with_context(|| format!("{kind} {wanted} is not published for {game}"))?,
            None => versions
                .iter()
                .find(|entry| entry.stable)
                .or_else(|| versions.first())
                .with_context(|| format!("no {kind} build is published for {game}"))?,
        };
        let profile = http.loader_profile(kind, game, &chosen.version)?;
        let id = profile.json.id.clone();
        validate_name(&id)?;
        if profile.json.inherits_from.as_deref() != Some(game) {
            bail!("{kind} profile {id} does not inherit from {game}");
        }
        Ok((id, profile.bytes))
    }

    /// Mods found in the instance's `mods` directory.
    pub fn mods(&self, name: &str) -> Result<Vec<tntl_instance::mods::ModInfo>> {
        let instance = self.load_instance(name)?;
        let dir = tntl_instance::mods::mods_dir(&instance.game_dir(&self.instance_dir(name)?));
        let (loader, game_version, status) = match self.loader_status(name) {
            Ok(status) => (
                status.kind.as_deref().map(loader_from_key),
                status.game_version.clone(),
                Some(status),
            ),
            Err(_) => {
                let environment = self.environment(name).ok();
                (
                    environment
                        .as_ref()
                        .and_then(|environment| environment.loader.as_deref().map(loader_from_key)),
                    environment
                        .map(|environment| environment.game_version)
                        .unwrap_or_default(),
                    None,
                )
            }
        };
        let mut mods = tntl_instance::mods::scan_for(&dir, loader, &game_version)?;
        if let Some(status) = status {
            mark_blocked(&mut mods, &status);
        }
        Ok(mods)
    }

    /// Check an instance without starting it and without touching the network.
    pub fn diagnose_instance(&self, name: &str, total_memory_mb: Option<u64>) -> Result<DiagnosticReport> {
        let instance = self.load_instance(name)?;
        let instance_dir = self.instance_dir(name)?;
        let game_dir = instance.game_dir(&instance_dir);
        let layout = instance.layout(&self.root);
        let mut findings: Vec<tntl_doctor::Finding> = Vec::new();
        let mut game_version = instance.version_id.clone();
        let mut loader = None;
        let mut loader_version = None;
        let mut java_major = None;

        match self.load_chain(&layout, &instance.version_id) {
            Ok(chain) => {
                let environment = environment_from_chain(&instance.version_id, &chain);
                game_version = environment.game_version.clone();
                if let Some((kind, version)) = installed_loader(&chain, &environment.game_version) {
                    loader = Some(kind.key().to_string());
                    loader_version = Some(version);
                }
                let required_java = self.required_java(&chain);

                let (java_path, install) = self.resolve_java(&chain, instance.java.as_deref());
                match install.or_else(|| tntl_java::probe(&java_path)) {
                    Some(install) => {
                        java_major = Some(install.major);
                        if install.major < required_java {
                            findings.push(
                                tntl_doctor::Finding::new(tntl_doctor::Severity::Error, "java_requirement")
                                    .param("required", required_java.to_string())
                                    .param("actual", install.major.to_string()),
                            );
                        }
                    }
                    None => findings.push(
                        tntl_doctor::Finding::new(tntl_doctor::Severity::Error, "java_missing")
                            .subject(java_path.display().to_string()),
                    ),
                }

                // File check with a throwaway offline account: the doctor must
                // stay offline even when the stored Microsoft token is stale.
                let account = Account::offline(&instance.username)
                    .or_else(|_| Account::offline("Player"))
                    .expect("offline name is valid");
                let mut probe = instance.clone();
                probe.java = None;
                match self.plan_instance_with_account(&probe, None, VerifyMode::Exists, account) {
                    Ok(plan) => {
                        let missing: Vec<&MissingObject> = plan
                            .missing
                            .iter()
                            .filter(|object| object.kind != MissingKind::Java)
                            .collect();
                        if !missing.is_empty() {
                            findings.push(
                                tntl_doctor::Finding::new(tntl_doctor::Severity::Error, "files_missing")
                                    .param("files", summarize_missing(&missing, &layout.root))
                                    .evidence(format!("{} file(s) missing or unusable", missing.len())),
                            );
                        }
                    }
                    Err(error) => findings.push(
                        tntl_doctor::Finding::new(tntl_doctor::Severity::Error, "files_missing")
                            .evidence(format!("{error:#}")),
                    ),
                }
            }
            Err(error) => findings.push(
                tntl_doctor::Finding::new(tntl_doctor::Severity::Error, "files_missing")
                    .param("files", instance.version_id.clone())
                    .evidence(format!("{error:#}")),
            ),
        }

        // Patched loader jars are not hashed by the version JSON; without this
        // the check reported a clean instance that could not start.
        if let Ok(broken) = self.broken_loader_outputs(&instance.name)
            && !broken.is_empty()
        {
            let names: Vec<String> = broken
                .iter()
                .map(|path| path.strip_prefix(&layout.root).unwrap_or(path).display().to_string())
                .collect();
            findings.push(
                tntl_doctor::Finding::new(tntl_doctor::Severity::Error, "loader_outputs")
                    .param("files", names.join(", "))
                    .evidence(format!("{} loader file(s) changed after install", broken.len())),
            );
        }

        let mut accounts = AccountStore::load(&self.root).ok();
        if accounts.as_mut().and_then(|store| store.current()).is_none() {
            findings.push(tntl_doctor::Finding::new(tntl_doctor::Severity::Error, "no_account"));
        }

        let mut mods = tntl_instance::mods::scan_for(
            &tntl_instance::mods::mods_dir(&game_dir),
            loader.as_deref().map(loader_from_key),
            &game_version,
        )?;
        mark_blocked(
            &mut mods,
            &LoaderStatus {
                game_version: game_version.clone(),
                kind: loader.clone(),
                version: loader_version.clone(),
                version_id: instance.version_id.clone(),
            },
        );
        let enabled_mods = mods.iter().filter(|module| module.enabled).count();
        let context = tntl_doctor::mods::ModContext {
            loader: loader.as_deref().map(loader_from_key),
            game_version: &game_version,
            loader_version: loader_version.as_deref(),
            java_major,
            memory_mb: instance.max_memory_mb,
            total_memory_mb,
        };
        findings.extend(tntl_doctor::mods::check(&mods, &context));

        Ok(DiagnosticReport {
            checked_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_secs())
                .unwrap_or_default(),
            game_version,
            loader,
            loader_version,
            mods: mods.len(),
            enabled_mods,
            memory_mb: instance.max_memory_mb,
            java_major,
            findings: tntl_doctor::tidy(findings),
        })
    }

    /// Resolve an on-disk mod name. Enabling needs the `.disabled` suffix;
    /// accepting the plain name too saves a confusing failure.
    fn mod_path(&self, name: &str, file: &str) -> Result<PathBuf> {
        let instance = self.load_instance(name)?;
        let dir = tntl_instance::mods::mods_dir(&instance.game_dir(&self.instance_dir(name)?));
        if file.contains('/') || file.contains('\\') || file == ".." {
            bail!("invalid mod file name: {file}");
        }
        let direct = dir.join(file);
        if direct.is_file() {
            return Ok(direct);
        }
        let alternate = match file.strip_suffix(".disabled") {
            Some(stem) => dir.join(stem),
            None => dir.join(format!("{file}.disabled")),
        };
        if alternate.is_file() {
            Ok(alternate)
        } else {
            bail!("mod {file} is not installed in {}", dir.display())
        }
    }

    /// Enable or disable one mod file and return the refreshed list.
    pub fn set_mod_enabled(&self, name: &str, file: &str, enabled: bool) -> Result<Vec<tntl_instance::mods::ModInfo>> {
        self.ensure_not_running(name)?;
        let path = self.mod_path(name, file)?;
        if enabled {
            // Switching on a jar the loader can never satisfy only produces a
            // crash on the next launch, so refuse it here.
            let target = path.file_name().map(|file| file.to_string_lossy().to_string());
            if let Some(module) = self
                .mods(name)?
                .iter()
                .find(|module| Some(&module.file) == target.as_ref())
                && let Some(blocked) = &module.blocked
            {
                bail!(
                    "{} {} needs {} {}, but this instance runs {}. Use a build that matches.",
                    module.name,
                    module.version,
                    blocked.loader,
                    blocked.required.join(" / "),
                    blocked.actual
                );
            }
        }
        tntl_instance::mods::set_enabled(&path, enabled)?;
        self.mods(name)
    }

    /// Delete one mod file and return the refreshed list.
    pub fn remove_mod(&self, name: &str, file: &str) -> Result<Vec<tntl_instance::mods::ModInfo>> {
        self.ensure_not_running(name)?;
        let path = self.mod_path(name, file)?;
        fs::remove_file(&path).with_context(|| format!("failed to delete {}", path.display()))?;
        let mut record = ModrinthContent::load(&self.modrinth_content_path(name)?);
        if record.remove(ProjectKind::Mod, file) {
            record.save(&self.modrinth_content_path(name)?)?;
        }
        self.mods(name)
    }

    /// Game version and mod loader of an instance, derived from its version chain.
    pub fn environment(&self, name: &str) -> Result<VersionEnvironment> {
        let instance = self.load_instance(name)?;
        let layout = instance.layout(&self.root);
        let chain = self.load_chain(&layout, &instance.version_id)?;
        Ok(environment_from_chain(&instance.version_id, &chain))
    }

    /// Search Modrinth for mods that fit the instance.
    pub fn search_mods(&self, name: &str, query: &str, sort: &str, limit: u32, offset: u32) -> Result<ModSearchPage> {
        self.search_projects(
            name,
            ProjectKind::Mod,
            query,
            sort,
            limit,
            offset,
            ContentSource::Modrinth,
            None,
        )
    }

    /// Game builds Modrinth can filter by, newest first.
    pub fn mod_game_versions(&self) -> Result<Vec<String>> {
        let http = Http::new()?;
        Ok(Modrinth::new(&http)
            .game_versions()?
            .into_iter()
            .map(|tag| tag.version)
            .filter(|version| !version.is_empty())
            .collect())
    }

    /// Search for projects of `kind` that fit the instance. Resource packs and
    /// shaders ignore the loader. `source` picks the catalogue; CurseForge
    /// needs the user's own API key. `game_version` overrides the instance's
    /// own build; an empty string searches every game version.
    #[allow(clippy::too_many_arguments)]
    pub fn search_projects(
        &self,
        name: &str,
        kind: ProjectKind,
        query: &str,
        sort: &str,
        limit: u32,
        offset: u32,
        source: ContentSource,
        game_version: Option<&str>,
    ) -> Result<ModSearchPage> {
        let environment = self.environment(name)?;
        // Browsing MUST narrow to the instance's loader: anything looser would
        // put mods of a different loader on the shelf (a Forge build on a
        // NeoForge instance), and a vanilla instance has no mod loader at all.
        let loader = match kind {
            ProjectKind::Mod => environment
                .loader
                .clone()
                .with_context(|| format!("instance {name} has no mod loader; install Fabric or Quilt first"))?,
            ProjectKind::ResourcePack | ProjectKind::Shader => String::new(),
        };
        let game_version = game_version
            .map(str::trim)
            .map(str::to_string)
            .unwrap_or(environment.game_version);
        let filter = if loader.is_empty() {
            Filter::any_loader(&game_version)
        } else {
            mod_filter(&game_version, &loader)
        };
        let http = Http::new()?;
        let mut page = match source {
            ContentSource::Modrinth => {
                Modrinth::new(&http).search(query, &filter, kind, parse_index(sort)?, limit, offset)?
            }
            ContentSource::CurseForge => {
                self.curseforge(&http)?
                    .search(query, &filter, kind, parse_index(sort)?, limit, offset)?
            }
        };
        let mut installed = self.installed_ids(name, kind)?;
        if source == ContentSource::CurseForge {
            installed.extend(
                self.content_sources(name)?
                    .entries(kind)
                    .into_iter()
                    .map(|(_, entry)| entry.project_id.to_string()),
            );
        }
        for hit in &mut page.hits {
            hit.installed = tntl_modrinth::matches_project(&hit.slug, &installed)
                || tntl_modrinth::matches_project(&hit.project_id, &installed);
        }
        Ok(ModSearchPage {
            game_version,
            loader,
            hits: page.hits,
            total_hits: page.total_hits,
            offset: page.offset,
            limit: page.limit,
        })
    }

    /// Search a catalogue without an instance, for callers that already know
    /// the game version and loader. Mods must name a loader: without one the
    /// catalogue would answer across every loader and only ever be wrong.
    #[allow(clippy::too_many_arguments)]
    pub fn search_catalogue(
        &self,
        kind: ProjectKind,
        game_version: &str,
        loader: &str,
        query: &str,
        sort: &str,
        limit: u32,
        offset: u32,
        source: ContentSource,
    ) -> Result<ModSearchPage> {
        if kind == ProjectKind::Mod && loader.trim().is_empty() {
            bail!("a mod search needs a loader; install Fabric or Quilt first");
        }
        let filter = if loader.is_empty() {
            Filter::any_loader(game_version)
        } else {
            mod_filter(game_version, loader)
        };
        let http = Http::new()?;
        let page = match source {
            ContentSource::Modrinth => {
                Modrinth::new(&http).search(query, &filter, kind, parse_index(sort)?, limit, offset)?
            }
            ContentSource::CurseForge => {
                self.curseforge(&http)?
                    .search(query, &filter, kind, parse_index(sort)?, limit, offset)?
            }
        };
        Ok(ModSearchPage {
            game_version: game_version.to_string(),
            loader: loader.to_string(),
            hits: page.hits,
            total_hits: page.total_hits,
            offset: page.offset,
            limit: page.limit,
        })
    }

    /// Metadata ids of the mods already in the instance's `mods` directory.
    fn installed_ids(&self, name: &str, kind: ProjectKind) -> Result<Vec<String>> {
        let instance = self.load_instance(name)?;
        let game_dir = instance.game_dir(&self.instance_dir(name)?);
        if kind == ProjectKind::Mod {
            let dir = tntl_instance::mods::mods_dir(&game_dir);
            let environment = self.environment(name).ok();
            let loader = environment
                .as_ref()
                .and_then(|environment| environment.loader.as_deref().map(loader_from_key));
            let game_version = environment
                .as_ref()
                .map(|environment| environment.game_version.as_str())
                .unwrap_or_default();
            // A switched-off jar still occupies its mod id: offering the same
            // project again would put two jars with one id in `mods/`.
            let mut installed: Vec<String> = tntl_instance::mods::scan_for(&dir, loader, game_version)?
                .into_iter()
                .map(|module| module.id)
                .collect();
            // A re-install is matched by project id when the mod id differs
            // from the project slug (corelib vs its slug).
            let record = ModrinthContent::load(&self.modrinth_content_path(name)?);
            installed.extend(record.projects_present(kind, &dir));
            return Ok(installed);
        }
        let mut installed: Vec<String> = content_files(&game_dir.join(kind.dir()))
            .iter()
            .map(|file| file_stem(file))
            .collect();
        // Search results have no hashes, so packs and shaders installed from
        // Modrinth are recognised through the sidecar written at install time.
        installed.extend(ModrinthContent::load(&self.modrinth_content_path(name)?).projects(kind));
        Ok(installed)
    }

    /// Install projects (and, for mods, their required dependencies) into the
    /// directory that belongs to `kind`. `game_version` overrides the
    /// instance's own build; an empty string installs whatever is newest.
    #[allow(clippy::too_many_arguments)]
    pub fn install_projects(
        &self,
        name: &str,
        kind: ProjectKind,
        projects: &[String],
        source: ContentSource,
        game_version: Option<&str>,
        progress: &(dyn Fn(tntl_modrinth::InstallEvent) + Send + Sync),
    ) -> Result<Vec<InstalledMod>> {
        let instance = self.load_instance(name)?;
        let environment = self.environment(name)?;
        let loader = match kind {
            ProjectKind::Mod => environment
                .loader
                .clone()
                .with_context(|| format!("instance {name} has no mod loader; install Fabric or Quilt first"))?,
            ProjectKind::ResourcePack | ProjectKind::Shader => String::new(),
        };
        let game_dir = instance.game_dir(&self.instance_dir(name)?);
        let target_dir = match kind {
            ProjectKind::Mod => tntl_instance::mods::mods_dir(&game_dir),
            _ => game_dir.join(kind.dir()),
        };
        // A mod jar is tied to one game version; resource packs and shaders
        // are not, so only mods ignore the caller's version override.
        let game_version = match kind {
            ProjectKind::Mod => environment.game_version.clone(),
            ProjectKind::ResourcePack | ProjectKind::Shader => game_version
                .map(str::trim)
                .map(str::to_string)
                .unwrap_or(environment.game_version),
        };
        let filter = if loader.is_empty() {
            Filter::any_loader(&game_version)
        } else {
            mod_filter(&game_version, &loader)
        };
        let http = Http::new()?;
        if source == ContentSource::Modrinth {
            let installed = self.installed_ids(name, kind)?;
            let entries =
                Modrinth::new(&http).install(&target_dir, projects, &installed, &filter, kind, &mut |event| {
                    progress(event)
                })?;
            // Remember where the files came from: the browser marks installed
            // projects from this, and a replacement of the same project is
            // recognised even when its mod id differs from the project slug.
            self.record_modrinth_content(name, kind, &entries)?;
            if kind == ProjectKind::Mod {
                let files: Vec<String> = entries.iter().map(|entry| entry.file.clone()).collect();
                self.reject_unsatisfied_mods(name, &files)?;
            }
            return Ok(entries);
        }

        let mut sources = self.content_sources(name)?;
        let installed: Vec<String> = sources
            .entries(kind)
            .into_iter()
            .map(|(_, entry)| entry.project_id.to_string())
            .collect();
        let entries =
            self.curseforge(&http)?
                .install(&target_dir, projects, &installed, &filter, kind, &mut |event| {
                    progress(event)
                })?;
        for entry in &entries {
            sources.record(kind, &entry.file_name, entry.source());
        }
        if !entries.is_empty() {
            sources.save(&self.content_sources_path(name)?)?;
        }
        let installed: Vec<InstalledMod> = entries.iter().map(tntl_curseforge::installed_mod).collect();
        if kind == ProjectKind::Mod {
            let files: Vec<String> = installed.iter().map(|entry| entry.file.clone()).collect();
            self.reject_unsatisfied_mods(name, &files)?;
        }
        Ok(installed)
    }

    /// Builds of one project the instance can run, newest first.
    pub fn project_versions(
        &self,
        name: &str,
        kind: ProjectKind,
        project: &str,
    ) -> Result<Vec<tntl_modrinth::VersionChoice>> {
        let environment = self.environment(name)?;
        let loader = match kind {
            ProjectKind::Mod => environment
                .loader
                .clone()
                .with_context(|| format!("instance {name} has no mod loader; install Fabric or Quilt first"))?,
            ProjectKind::ResourcePack | ProjectKind::Shader => String::new(),
        };
        let filter = if loader.is_empty() {
            Filter::any_loader(&environment.game_version)
        } else {
            mod_filter(&environment.game_version, &loader)
        };
        let http = Http::new()?;
        let versions = Modrinth::new(&http).project_versions(project)?;
        Ok(versions
            .iter()
            .filter(|version| filter.accepts(version))
            .filter_map(tntl_modrinth::VersionChoice::from_version)
            .collect())
    }

    /// Install one chosen build of a project and drop the copy it replaces, so
    /// `mods/` never holds two jars with the same mod id. Only Modrinth exposes
    /// a build list, so CurseForge projects cannot be pinned this way.
    pub fn install_project_version(
        &self,
        name: &str,
        kind: ProjectKind,
        project: &str,
        version_id: &str,
        source: ContentSource,
        progress: &(dyn Fn(tntl_modrinth::InstallEvent) + Send + Sync),
    ) -> Result<Vec<InstalledMod>> {
        if source != ContentSource::Modrinth {
            bail!("choosing a build is only available for Modrinth projects");
        }
        let instance = self.load_instance(name)?;
        let environment = self.environment(name)?;
        let loader = match kind {
            ProjectKind::Mod => environment
                .loader
                .clone()
                .with_context(|| format!("instance {name} has no mod loader; install Fabric or Quilt first"))?,
            ProjectKind::ResourcePack | ProjectKind::Shader => String::new(),
        };
        let game_dir = instance.game_dir(&self.instance_dir(name)?);
        let target_dir = match kind {
            ProjectKind::Mod => tntl_instance::mods::mods_dir(&game_dir),
            _ => game_dir.join(kind.dir()),
        };
        let filter = if loader.is_empty() {
            Filter::any_loader(&environment.game_version)
        } else {
            mod_filter(&environment.game_version, &loader)
        };
        let installed = self.installed_ids(name, kind)?;
        let http = Http::new()?;
        let entries = Modrinth::new(&http).install_version(
            &target_dir,
            project,
            version_id,
            &installed,
            &filter,
            kind,
            &mut |event| progress(event),
        )?;
        if kind == ProjectKind::Mod {
            self.record_modrinth_content(name, kind, &entries)?;
            let files: Vec<String> = entries.iter().map(|entry| entry.file.clone()).collect();
            self.reject_unsatisfied_mods(name, &files)?;
            self.drop_superseded_mods(name, &entries)?;
        } else {
            self.record_installed_content(name, kind, &entries)?;
        }
        Ok(entries)
    }

    /// Remove the jars a chosen build replaces. A downgrade otherwise leaves
    /// the old jar next to the new one, and two jars with one mod id stop the
    /// loader from starting.
    fn drop_superseded_mods(&self, name: &str, entries: &[InstalledMod]) -> Result<()> {
        let keep: std::collections::HashSet<&str> = entries.iter().map(|entry| entry.file.as_str()).collect();
        let ids: std::collections::HashSet<String> = entries
            .iter()
            .flat_map(|entry| {
                [
                    tntl_modrinth::normalize(&entry.slug),
                    tntl_modrinth::normalize(&entry.project_id),
                ]
            })
            .filter(|id| !id.is_empty())
            .collect();
        let projects: std::collections::HashSet<&str> = entries
            .iter()
            .map(|entry| entry.project_id.as_str())
            .filter(|project| !project.is_empty())
            .collect();
        let dir = tntl_instance::mods::mods_dir(&self.load_instance(name)?.game_dir(&self.instance_dir(name)?));
        let mut record = ModrinthContent::load(&self.modrinth_content_path(name)?);
        for module in self.mods(name)? {
            if keep.contains(module.file.as_str()) {
                continue;
            }
            // The mod id is what the loader sees; the recorded project catches
            // jars whose id does not match the slug (`supermartijn642corelib`
            // vs `supermartijn642s-core-lib`), which the old name check missed.
            let same_id = ids.contains(&tntl_modrinth::normalize(&module.id));
            let same_project = record
                .project_of(ProjectKind::Mod, &module.file)
                .is_some_and(|project| projects.contains(project));
            if !same_id && !same_project {
                continue;
            }
            let path = dir.join(&module.file);
            fs::remove_file(&path).with_context(|| format!("failed to delete {}", path.display()))?;
            record.remove(ProjectKind::Mod, &module.file);
        }
        record.save(&self.modrinth_content_path(name)?)
    }

    /// Remember which project published each freshly installed file.
    fn record_modrinth_content(&self, name: &str, kind: ProjectKind, entries: &[InstalledMod]) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let mut record = ModrinthContent::load(&self.modrinth_content_path(name)?);
        for entry in entries {
            record.record(kind, &entry.file, &entry.project_id);
        }
        record.save(&self.modrinth_content_path(name)?)
    }

    /// Record packs and shaders installed from Modrinth and delete the file of
    /// the same project they replace; a different build has a different name,
    /// so both copies would otherwise sit in the folder.
    fn record_installed_content(&self, name: &str, kind: ProjectKind, entries: &[InstalledMod]) -> Result<()> {
        let mut record = ModrinthContent::load(&self.modrinth_content_path(name)?);
        let dir = self
            .load_instance(name)?
            .game_dir(&self.instance_dir(name)?)
            .join(kind.dir());
        for entry in entries {
            let prefix = format!("{}/", kind.dir());
            let stale: Vec<String> = record
                .files
                .iter()
                .filter(|(key, project)| key.starts_with(&prefix) && *project == &entry.project_id)
                .filter_map(|(key, _)| key.rsplit_once('/').map(|(_, file)| file.to_string()))
                .filter(|file| file != &entry.file)
                .collect();
            for file in stale {
                let _ = self.set_content_enabled(name, kind, &file, false);
                let _ = tntl_instance::content::remove(&dir, &file);
                record.remove(kind, &file);
            }
            record.record(kind, &entry.file, &entry.project_id);
        }
        record.save(&self.modrinth_content_path(name)?)
    }

    /// Forge-family mods declare the loader's own build number (`[47.4.0,)`),
    /// so a jar published for 1.20.1 can still refuse NeoForge 1.20.1, which
    /// stops at 47.1.106. Such a file stops the game at load time: keep the
    /// download but switch it off, and say which one it was.
    fn reject_unsatisfied_mods(&self, name: &str, installed: &[String]) -> Result<()> {
        if installed.is_empty() {
            return Ok(());
        }
        let status = self.loader_status(name)?;
        let files: std::collections::BTreeSet<&str> = installed.iter().map(String::as_str).collect();
        let rejected: Vec<tntl_instance::mods::ModInfo> = self
            .mods(name)?
            .into_iter()
            .filter(|module| files.contains(module.file.as_str()) && module.blocked.is_some())
            .collect();
        if rejected.is_empty() {
            return Ok(());
        }
        let dir = tntl_instance::mods::mods_dir(&self.load_instance(name)?.game_dir(&self.instance_dir(name)?));
        for module in &rejected {
            tntl_instance::mods::set_enabled(&dir.join(&module.file), false)?;
        }
        let detail = rejected
            .iter()
            .filter_map(|module| module.blocked.as_ref().map(|blocked| (module, blocked)))
            .map(|(module, blocked)| {
                format!(
                    "{} {} needs {} {}",
                    module.name,
                    module.version,
                    blocked.loader,
                    blocked.required.join(" / ")
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        let label = loader_label(status.kind.as_deref().unwrap_or_default());
        let runs = match (status.kind.as_deref(), status.version.as_deref()) {
            (Some(_), Some(version)) if !label.is_empty() => format!(
                "{label} {}",
                tntl_doctor::mods::loader_build_number(version, &status.game_version)
            ),
            _ => format!("Minecraft {}", status.game_version),
        };
        bail!(
            "{detail}, but this instance runs {runs}. The files are switched off in mods/; install a build that matches."
        )
    }

    /// Path of the CurseForge source sidecar inside the instance directory.
    fn content_sources_path(&self, name: &str) -> Result<PathBuf> {
        Ok(self.instance_dir(name)?.join(tntl_curseforge::SOURCES_FILE))
    }

    /// Which CurseForge files an instance holds. Modrinth files are recognised
    /// by hash, so only CurseForge needs this record.
    pub fn content_sources(&self, name: &str) -> Result<ContentSources> {
        Ok(ContentSources::load(&self.content_sources_path(name)?))
    }

    fn modrinth_content_path(&self, name: &str) -> Result<PathBuf> {
        Ok(self.instance_dir(name)?.join(MODRINTH_CONTENT_FILE))
    }

    /// CurseForge client with the key from the settings or the environment.
    fn curseforge<'a>(&self, http: &'a Http) -> Result<CurseForge<'a>> {
        let key = self
            .settings()
            .curseforge_api_key
            .or_else(|| std::env::var("TNTL_CURSEFORGE_API_KEY").ok())
            .unwrap_or_default();
        Ok(CurseForge::new(http, &key)?)
    }

    /// Install missing mod dependencies from Modrinth. A dependency is only
    /// installed when its id matches a project slug exactly, so a wrong mod is
    /// never pulled in; the rest come back as `unresolved`.
    pub fn install_missing_dependencies(
        &self,
        name: &str,
        progress: &(dyn Fn(ModInstallEvent) + Send + Sync),
    ) -> Result<MissingDepsReport> {
        let mut missing: Vec<String> = self
            .mods(name)?
            .into_iter()
            .flat_map(|entry| entry.missing_dependencies)
            .collect();
        missing.sort();
        missing.dedup();
        if missing.is_empty() {
            return Ok(MissingDepsReport {
                installed: Vec::new(),
                unresolved: Vec::new(),
            });
        }
        let environment = self.environment(name)?;
        let loader = environment
            .loader
            .clone()
            .with_context(|| format!("instance {name} has no mod loader"))?;
        let filter = mod_filter(&environment.game_version, &loader);
        let http = Http::new()?;
        let modrinth = Modrinth::new(&http);
        let game_dir = self.load_instance(name)?.game_dir(&self.instance_dir(name)?);
        let target_dir = tntl_instance::mods::mods_dir(&game_dir);
        let mut installed_ids = self.installed_ids(name, ProjectKind::Mod)?;
        let ranges = self.dependency_ranges(&self.mods(name)?, Some(loader_from_key(&loader)));
        let mut projects = Vec::new();
        let mut unresolved = Vec::new();
        for id in missing {
            // Mod ids are usually Modrinth slugs, but the search index is
            // filtered by loader and omits projects whose files for this game
            // version are tagged for another loader. Resolve the slug first.
            let found = modrinth
                .project(&id)
                .ok()
                .filter(|project| project.project_type == "mod")
                .map(|project| project.id)
                .or_else(|| {
                    modrinth
                        .search(&id, &filter, ProjectKind::Mod, SearchIndex::Relevance, 5, 0)
                        .ok()
                        .and_then(|page| {
                            page.hits
                                .into_iter()
                                .find(|hit| {
                                    hit.slug.eq_ignore_ascii_case(&id) || hit.project_id.eq_ignore_ascii_case(&id)
                                })
                                .map(|hit| hit.project_id)
                        })
                })
                .or_else(|| {
                    modrinth
                        .search(
                            &id,
                            &Filter::any_loader(&environment.game_version),
                            ProjectKind::Mod,
                            SearchIndex::Relevance,
                            5,
                            0,
                        )
                        .ok()
                        .and_then(|page| {
                            page.hits
                                .into_iter()
                                .find(|hit| hit.slug.eq_ignore_ascii_case(&id))
                                .map(|hit| hit.project_id)
                        })
                });
            match found {
                Some(project) => projects.push((id, project)),
                None => unresolved.push(id),
            }
        }
        // Install each project on its own so one unavailable dependency does
        // not block the rest. A build that satisfies the ranges other jars
        // declare for this dependency wins over the newest one.
        let mut installed = Vec::new();
        for (id, project) in projects {
            let predicates = ranges.get(&id.to_ascii_lowercase()).cloned().unwrap_or_default();
            let chosen = self
                .compatible_build(&modrinth, &project, &filter, &predicates)
                .unwrap_or_default();
            let outcome = match chosen {
                Some(version_id) => modrinth.install_version(
                    &target_dir,
                    &project,
                    &version_id,
                    &installed_ids,
                    &filter,
                    ProjectKind::Mod,
                    &mut |event| progress(event),
                ),
                None => modrinth.install(
                    &target_dir,
                    &[project],
                    &installed_ids,
                    &filter,
                    ProjectKind::Mod,
                    &mut |event| progress(event),
                ),
            };
            match outcome {
                Ok(entries) => {
                    installed_ids.extend(entries.iter().map(|entry| entry.project_id.clone()));
                    installed.extend(entries);
                }
                Err(_) => unresolved.push(id),
            }
        }
        self.record_modrinth_content(name, ProjectKind::Mod, &installed)?;
        Ok(MissingDepsReport { installed, unresolved })
    }

    /// Version ranges every enabled jar places on each dependency id, using the
    /// metadata the instance's loader actually reads.
    fn dependency_ranges(
        &self,
        mods: &[tntl_instance::mods::ModInfo],
        loader: Option<ModLoader>,
    ) -> BTreeMap<String, Vec<Vec<String>>> {
        let mut ranges: BTreeMap<String, Vec<Vec<String>>> = BTreeMap::new();
        for module in mods.iter().filter(|module| module.enabled) {
            for (dependency, predicates) in tntl_doctor::mods::dependency_requirements(module, loader) {
                if predicates.is_empty() {
                    continue;
                }
                ranges
                    .entry(dependency.to_ascii_lowercase())
                    .or_default()
                    .push(predicates);
            }
        }
        ranges
    }

    /// Newest build of `project` that satisfies every dependent's range. A
    /// predicate that cannot be parsed never rejects a build.
    fn compatible_build(
        &self,
        modrinth: &Modrinth,
        project: &str,
        filter: &Filter,
        predicates: &[Vec<String>],
    ) -> Result<Option<String>> {
        if predicates.is_empty() {
            return Ok(None);
        }
        let mut versions = modrinth.project_versions(project)?;
        versions.sort_by(|left, right| right.date_published.cmp(&left.date_published));
        Ok(versions
            .iter()
            .filter(|version| filter.accepts(version))
            .find(|version| ranges_match(&version.version_number, predicates))
            .map(|version| version.id.clone()))
    }

    /// Modrinth project that published a jar, looked up by its hash. Jar mod
    /// ids differ from project slugs more often than not (`supermartijn642corelib`
    /// vs `supermartijn642s-core-lib`), so the hash is the reliable link.
    fn modrinth_project_of(&self, modrinth: &Modrinth, path: &Path) -> Result<Option<String>> {
        let Ok(sha1) = tntl_store::sha1_file(path) else {
            return Ok(None);
        };
        Ok(modrinth
            .version_by_hash(&sha1)?
            .map(|version| version.project_id)
            .filter(|project| !project.is_empty()))
    }

    /// Replace dependency builds that sit outside a range another jar enforces.
    /// The replacement is installed through the normal Modrinth path (guard and
    /// superseded-file cleanup included), then the out-of-range jar is removed
    /// so `mods/` never holds two copies of one mod id.
    pub fn fix_dependency_versions(
        &self,
        name: &str,
        progress: &(dyn Fn(tntl_modrinth::InstallEvent) + Send + Sync),
    ) -> Result<DependencyFixReport> {
        self.ensure_not_running(name)?;
        let environment = self.environment(name)?;
        let loader_key = environment
            .loader
            .clone()
            .with_context(|| format!("instance {name} has no mod loader"))?;
        let loader = loader_from_key(&loader_key);
        let filter = mod_filter(&environment.game_version, &loader_key);
        let mods = self.mods(name)?;
        let ranges = self.dependency_ranges(&mods, Some(loader));
        let mut report = DependencyFixReport::default();
        if ranges.is_empty() {
            return Ok(report);
        }
        let nested: HashSet<String> = mods
            .iter()
            .flat_map(|module| module.nested.iter().map(|id| id.to_ascii_lowercase()))
            .collect();
        let mut providers: BTreeMap<String, usize> = BTreeMap::new();
        for (index, module) in mods.iter().enumerate().filter(|(_, module)| module.enabled) {
            for id in std::iter::once(&module.id).chain(module.provides.iter()) {
                providers.entry(id.to_ascii_lowercase()).or_insert(index);
            }
        }
        let game_dir = self.load_instance(name)?.game_dir(&self.instance_dir(name)?);
        let dir = tntl_instance::mods::mods_dir(&game_dir);
        let http = Http::new()?;
        let modrinth = Modrinth::new(&http);
        for (dependency, predicates) in ranges {
            if nested.contains(&dependency) {
                continue;
            }
            let Some(index) = providers.get(&dependency).copied() else {
                continue;
            };
            if ranges_match(&mods[index].version, &predicates) {
                continue;
            }
            let id = mods[index].id.clone();
            let from = mods[index].version.clone();
            let file = mods[index].file.clone();
            let Some(project) = self.modrinth_project_of(&modrinth, &dir.join(&file))? else {
                report.unresolved.push(dependency);
                continue;
            };
            let Some(version_id) = self.compatible_build(&modrinth, &project, &filter, &predicates)? else {
                report.unresolved.push(dependency);
                continue;
            };
            let installed = self.install_project_version(
                name,
                ProjectKind::Mod,
                &project,
                &version_id,
                ContentSource::Modrinth,
                progress,
            );
            if installed.is_err() {
                report.unresolved.push(dependency);
                continue;
            }
            let entries = installed.unwrap_or_default();
            if entries.iter().all(|entry| entry.file != file) {
                let path = dir.join(&file);
                if path.is_file() {
                    fs::remove_file(&path).with_context(|| format!("failed to delete {}", path.display()))?;
                }
            }
            match self
                .mods(name)?
                .into_iter()
                .find(|module| module.id.eq_ignore_ascii_case(&id))
            {
                Some(module) if ranges_match(&module.version, &predicates) => {
                    report.updated.push(DependencyUpdate {
                        dep: dependency,
                        name: module.name,
                        file: module.file,
                        from,
                        to: module.version,
                    });
                }
                _ => report.unresolved.push(dependency),
            }
        }
        Ok(report)
    }

    /// Install Modrinth mods and their required dependencies.
    pub fn install_mods(
        &self,
        name: &str,
        projects: &[String],
        game_version: Option<&str>,
        progress: &(dyn Fn(tntl_modrinth::InstallEvent) + Send + Sync),
    ) -> Result<Vec<InstalledMod>> {
        self.install_projects(
            name,
            ProjectKind::Mod,
            projects,
            ContentSource::Modrinth,
            game_version,
            progress,
        )
    }

    /// Files of a content kind with their sha1, plus the filter updates must
    /// match. Mods use their jar metadata; packs and shaders use the file name.
    fn local_content(&self, name: &str, kind: ProjectKind) -> Result<(PathBuf, Vec<LocalMod>, Filter)> {
        let instance = self.load_instance(name)?;
        let environment = self.environment(name)?;
        let loader = match kind {
            ProjectKind::Mod => environment
                .loader
                .clone()
                .with_context(|| format!("instance {name} has no mod loader; install Fabric or Quilt first"))?,
            ProjectKind::ResourcePack | ProjectKind::Shader => String::new(),
        };
        let game_dir = instance.game_dir(&self.instance_dir(name)?);
        let dir = match kind {
            ProjectKind::Mod => tntl_instance::mods::mods_dir(&game_dir),
            _ => game_dir.join(kind.dir()),
        };
        let mut locals = Vec::new();
        if kind == ProjectKind::Mod {
            for entry in tntl_instance::mods::scan(&dir)? {
                let Ok(sha1) = tntl_store::sha1_file(&dir.join(&entry.file)) else {
                    continue;
                };
                locals.push(LocalMod {
                    file: entry.file,
                    sha1,
                    name: entry.name,
                    version: entry.version,
                });
            }
        } else {
            for file in tntl_instance::content::list(&dir)? {
                // Unzipped packs are directories and have no hash to look up.
                if !dir.join(&file).is_file() {
                    continue;
                }
                let Ok(sha1) = tntl_store::sha1_file(&dir.join(&file)) else {
                    continue;
                };
                let stem = file.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(&file).to_string();
                locals.push(LocalMod {
                    file,
                    sha1,
                    name: stem.clone(),
                    version: stem,
                });
            }
        }
        let filter = match kind {
            ProjectKind::Mod => mod_filter(&environment.game_version, &loader),
            _ => Filter::any_loader(environment.game_version),
        };
        Ok((dir, locals, filter))
    }

    /// Newer builds of the instance's mods, resource packs or shaders. Files
    /// dropped in by hand and files no catalogue published are simply not
    /// reported. CurseForge files are matched through the source sidecar, so
    /// without a key the Modrinth results are still returned.
    pub fn check_updates(&self, name: &str, kind: ProjectKind) -> Result<Vec<ModUpdate>> {
        self.updated_files(name, kind).map(|(updates, _)| updates)
    }

    /// The update list plus, for mods, the instance's dependency set. The
    /// frontend filters the updates down to dependencies to offer the
    /// "upgrade dependencies" button without a second catalogue round trip.
    pub fn updated_files(&self, name: &str, kind: ProjectKind) -> Result<(Vec<ModUpdate>, HashSet<String>)> {
        let deps = if kind == ProjectKind::Mod {
            self.mods(name)?
                .into_iter()
                .filter(|module| module.enabled)
                .flat_map(|module| module.dependency)
                .collect::<HashSet<String>>()
        } else {
            HashSet::new()
        };
        let (_, locals, filter) = self.local_content(name, kind)?;
        let http = Http::new()?;
        let mut updates = if locals.is_empty() {
            Vec::new()
        } else {
            Modrinth::new(&http).updates(&locals, &filter, kind)?
        };
        let mut seen: HashSet<String> = updates.iter().map(|update| update.file.clone()).collect();
        // Only ask CurseForge about files it installed, but then surface its
        // errors: a swallowed bad key or rate limit looks like "up to date".
        if !self.content_sources(name)?.entries(kind).is_empty() {
            updates.extend(
                self.curseforge_updates(name, kind)?
                    .into_iter()
                    .map(|(update, _)| update)
                    .filter(|update| seen.insert(update.file.clone())),
            );
        }
        Ok((updates, deps))
    }

    /// Updates that apply to the instance's dependencies: the jars required by
    /// enabled mods that Modrinth or CurseForge published newer builds for.
    /// The button downloads the newest build of each dependency; the mods that
    /// need them stay untouched otherwise.
    pub fn check_dependency_updates(&self, name: &str) -> Result<Vec<ModUpdate>> {
        let environment = self.environment(name)?;
        if environment.loader.is_none() {
            bail!("instance {name} has no mod loader; install Fabric or Quilt first");
        }
        let (updates, deps) = self.updated_files(name, ProjectKind::Mod)?;
        let dep_ids: HashSet<String> = deps.iter().map(|id| tntl_modrinth::normalize(id)).collect();
        let mods = self.mods(name)?;
        Ok(updates
            .into_iter()
            .filter(|update| {
                mods.iter()
                    .any(|module| module.file == update.file && dep_ids.contains(&tntl_modrinth::normalize(&module.id)))
            })
            .collect())
    }

    /// CurseForge updates for the files the sidecar knows, with the source
    /// entry that has to replace the recorded one after applying.
    fn curseforge_updates(&self, name: &str, kind: ProjectKind) -> Result<Vec<(ModUpdate, SourceEntry)>> {
        let sources = self.content_sources(name)?;
        let entries = sources.entries(kind);
        if entries.is_empty() {
            return Ok(Vec::new());
        }
        let (dir, _, filter) = self.local_content(name, kind)?;
        let locals: Vec<(String, SourceEntry)> = entries
            .into_iter()
            .filter_map(|(base, entry)| {
                let disabled = format!("{base}.disabled");
                // A file the user deleted needs no update.
                if dir.join(&disabled).is_file() {
                    Some((disabled, entry))
                } else if dir.join(&base).is_file() {
                    Some((base, entry))
                } else {
                    None
                }
            })
            .collect();
        if locals.is_empty() {
            return Ok(Vec::new());
        }
        let http = Http::new()?;
        Ok(self.curseforge(&http)?.updates(&locals, &filter, kind)?)
    }

    /// Replace outdated files of one kind, all of them unless `files` names
    /// specific ones.
    pub fn update_content(
        &self,
        name: &str,
        kind: ProjectKind,
        files: &[String],
        progress: &(dyn Fn(tntl_modrinth::InstallEvent) + Send + Sync),
    ) -> Result<Vec<InstalledMod>> {
        self.ensure_not_running(name)?;
        let (dir, _, _) = self.local_content(name, kind)?;
        let updates = self.check_updates(name, kind)?;
        if !files.is_empty() {
            let installed = content_files(&dir);
            let unknown: Vec<&str> = files
                .iter()
                .map(String::as_str)
                .filter(|file| {
                    !updates.iter().any(|update| update.file == *file) && !installed.iter().any(|name| name == file)
                })
                .collect();
            if !unknown.is_empty() {
                bail!("{name} has no {} called {}", kind.dir(), unknown.join(", "));
            }
        }
        let selected: Vec<ModUpdate> = updates
            .into_iter()
            .filter(|update| files.is_empty() || files.iter().any(|file| file == &update.file))
            .collect();
        if selected.is_empty() {
            return Ok(Vec::new());
        }
        let http = Http::new()?;
        let modrinth = Modrinth::new(&http);
        let curseforge = self.curseforge_updates(name, kind).unwrap_or_default();
        let mut sources = self.content_sources(name)?;
        let mut modrinth_record = ModrinthContent::load(&self.modrinth_content_path(name)?);
        let mut record_touched = false;
        // Resource packs and shaders are switched on by name: replacing the
        // file with a newer build under a new name would silently switch the
        // pack off, so remember what was on and carry it over.
        let switched_on: std::collections::HashSet<String> = if kind == ProjectKind::Mod {
            std::collections::HashSet::new()
        } else {
            self.content(name, kind)?
                .files
                .into_iter()
                .filter(|entry| entry.enabled)
                .map(|entry| entry.file)
                .collect()
        };
        let mut touched = false;
        let mut applied = Vec::new();
        for update in &selected {
            let keep_on = switched_on.contains(&update.file);
            if keep_on {
                // Clear the old name before the file disappears.
                self.set_content_enabled(name, kind, &update.file, false)?;
            }
            let installed = modrinth.apply_update(&dir, update, &mut |event| progress(event))?;
            if keep_on {
                self.set_content_enabled(name, kind, &installed.file, true)?;
            }
            if let Some((_, entry)) = curseforge.iter().find(|(candidate, _)| candidate.file == update.file) {
                sources.remove(kind, &update.file);
                sources.record(kind, &installed.file, entry.clone());
                touched = true;
            }
            if kind != ProjectKind::Mod {
                modrinth_record.rename(kind, &update.file, &installed.file);
                record_touched = true;
            }
            applied.push(installed);
        }
        if touched {
            sources.save(&self.content_sources_path(name)?)?;
        }
        if record_touched {
            modrinth_record.save(&self.modrinth_content_path(name)?)?;
        }
        Ok(applied)
    }

    pub fn java_list(&self) -> Vec<JavaView> {
        self.java_installs()
            .into_iter()
            .map(|install| JavaView {
                path: install.path,
                major: install.major,
                version: install.version,
                vendor: install.vendor,
            })
            .collect()
    }

    /// Files of one content kind an instance has installed, with the ones the
    /// game has switched on.
    pub fn content(&self, name: &str, kind: ProjectKind) -> Result<ContentListing> {
        let instance = self.load_instance(name)?;
        let game_dir = instance.game_dir(&self.instance_dir(name)?);
        let dir = game_dir.join(kind.dir());
        let (enabled, toggle_supported) = match kind {
            ProjectKind::ResourcePack => (tntl_instance::content::enabled_resource_packs(&game_dir)?, true),
            ProjectKind::Shader => (
                tntl_instance::content::active_shader(&game_dir)?.into_iter().collect(),
                tntl_instance::content::shaders_supported(&game_dir),
            ),
            ProjectKind::Mod => (
                tntl_instance::mods::scan(&dir)?
                    .into_iter()
                    .filter(|entry| entry.enabled)
                    .map(|entry| entry.file)
                    .collect(),
                true,
            ),
        };
        let expected_format = if kind == ProjectKind::ResourcePack {
            self.environment(name)
                .ok()
                .and_then(|environment| tntl_instance::content::expected_pack_format(&environment.game_version))
        } else {
            None
        };
        let files = content_files(&dir)
            .into_iter()
            .map(|file| {
                let path = dir.join(&file);
                let meta = fs::metadata(&path).ok();
                let bytes = meta.as_ref().map(|meta| meta.len()).unwrap_or(0);
                let modified = meta
                    .as_ref()
                    .and_then(|meta| meta.modified().ok())
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|elapsed| elapsed.as_secs())
                    .unwrap_or_default();
                // Shader packs have no pack.mcmeta, so only resource packs are read.
                let pack = if kind == ProjectKind::ResourcePack {
                    tntl_instance::content::pack_meta(&path)
                } else {
                    tntl_instance::content::PackMeta::default()
                };
                ContentFile {
                    enabled: enabled.iter().any(|entry| entry == &file),
                    file,
                    bytes,
                    modified,
                    description: pack.description,
                    format: pack.format,
                    min_format: pack.min_format,
                    max_format: pack.max_format,
                }
            })
            .collect();
        let files = match kind {
            ProjectKind::ResourcePack => order_resource_packs(files, &enabled),
            _ => files,
        };
        Ok(ContentListing {
            files,
            toggle_supported,
            expected_format,
        })
    }

    /// Switch one resource pack or shader pack on or off. The game rewrites
    /// these settings on exit, so it must not be running.
    pub fn set_content_enabled(
        &self,
        name: &str,
        kind: ProjectKind,
        file: &str,
        enabled: bool,
    ) -> Result<ContentListing> {
        if kind == ProjectKind::Mod {
            bail!("mods are switched through the mods list");
        }
        if file.is_empty() || file.contains('/') || file.contains('\\') || file == ".." {
            bail!("invalid content file name: {file}");
        }
        self.ensure_not_running(name)?;
        let instance = self.load_instance(name)?;
        let game_dir = instance.game_dir(&self.instance_dir(name)?);
        if !game_dir.join(kind.dir()).join(file).exists() {
            bail!("{file} is not installed in {}", game_dir.join(kind.dir()).display());
        }
        match kind {
            ProjectKind::ResourcePack => tntl_instance::content::set_resource_pack(&game_dir, file, enabled)?,
            ProjectKind::Shader => tntl_instance::content::set_shader(&game_dir, file, enabled)?,
            ProjectKind::Mod => bail!("mods are enabled and disabled by the mods commands"),
        }
        self.content(name, kind)
    }

    /// Move an enabled resource pack up or down in the stacking order; the last
    /// pack in `options.txt` wins.
    pub fn move_resource_pack(&self, name: &str, file: &str, up: bool) -> Result<ContentListing> {
        self.ensure_not_running(name)?;
        let instance = self.load_instance(name)?;
        let game_dir = instance.game_dir(&self.instance_dir(name)?);
        tntl_instance::content::move_resource_pack(&game_dir, file, up)?;
        self.content(name, ProjectKind::ResourcePack)
    }

    /// `pack.png` of one resource pack, for the icon next to its name.
    pub fn content_icon(&self, name: &str, kind: ProjectKind, file: &str) -> Result<Vec<u8>> {
        if file.is_empty() || file.contains('/') || file.contains('\\') || file == ".." {
            bail!("invalid content file name: {file}");
        }
        let instance = self.load_instance(name)?;
        let game_dir = instance.game_dir(&self.instance_dir(name)?);
        let path = game_dir.join(kind.dir()).join(file);
        tntl_instance::content::pack_icon(&path).with_context(|| format!("{file} has no pack.png"))
    }

    /// Delete one resource pack or shader pack. A pack that is switched on is
    /// switched off first, so `options.txt` and the shader mod's config do not
    /// keep pointing at a file that no longer exists.
    pub fn remove_content(&self, name: &str, kind: ProjectKind, file: &str) -> Result<ContentListing> {
        if kind == ProjectKind::Mod {
            bail!("use remove_mod for mods");
        }
        self.ensure_not_running(name)?;
        let listing = self.content(name, kind)?;
        if listing.files.iter().any(|entry| entry.file == file && entry.enabled) {
            self.set_content_enabled(name, kind, file, false)?;
        }
        let instance = self.load_instance(name)?;
        let game_dir = instance.game_dir(&self.instance_dir(name)?);
        tntl_instance::content::remove(&game_dir.join(kind.dir()), file)?;
        let mut sources = self.content_sources(name)?;
        if sources.entries(kind).iter().any(|(entry, _)| entry == file) {
            sources.remove(kind, file);
            sources.save(&self.content_sources_path(name)?)?;
        }
        let mut record = ModrinthContent::load(&self.modrinth_content_path(name)?);
        if record.remove(kind, file) {
            record.save(&self.modrinth_content_path(name)?)?;
        }
        self.content(name, kind)
    }

    /// Worlds saved in this instance's game directory.
    pub fn worlds(&self, name: &str) -> Result<Vec<WorldInfo>> {
        let instance = self.load_instance(name)?;
        let game_dir = instance.game_dir(&self.instance_dir(name)?);
        Ok(tntl_instance::worlds::list(&game_dir)?)
    }

    /// Zip one world into `<instance>/backups/worlds/`.
    pub fn backup_world(&self, name: &str, world: &str) -> Result<PathBuf> {
        let dir = self.instance_dir(name)?;
        let instance = self.load_instance(name)?;
        let game_dir = instance.game_dir(&dir);
        Ok(tntl_instance::worlds::backup(
            &game_dir,
            world,
            &dir.join("backups").join("worlds"),
        )?)
    }

    /// Backups kept for this instance, newest first.
    pub fn world_backups(&self, name: &str) -> Result<Vec<WorldBackup>> {
        let dir = self.instance_dir(name)?.join("backups").join("worlds");
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error).with_context(|| format!("cannot read {}", dir.display())),
        };
        let mut backups = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() || path.extension().is_none_or(|ext| ext != "zip") {
                continue;
            }
            let meta = entry.metadata().ok();
            backups.push(WorldBackup {
                file: entry.file_name().to_string_lossy().to_string(),
                path: path.clone(),
                bytes: meta.as_ref().map(|meta| meta.len()).unwrap_or_default(),
                created_ms: meta
                    .and_then(|meta| meta.modified().ok())
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|duration| duration.as_millis() as i64),
            });
        }
        backups.sort_by_key(|entry| std::cmp::Reverse(entry.created_ms));
        Ok(backups)
    }

    /// Delete one archive under `<instance>/backups/worlds/`.
    pub fn delete_world_backup(&self, name: &str, file: &str) -> Result<Vec<WorldBackup>> {
        let dir = self.instance_dir(name)?.join("backups").join("worlds");
        tntl_instance::worlds::delete_backup(&dir, file)?;
        self.world_backups(name)
    }

    pub fn delete_world(&self, name: &str, world: &str) -> Result<()> {
        let instance = self.load_instance(name)?;
        let game_dir = instance.game_dir(&self.instance_dir(name)?);
        tntl_instance::worlds::delete(&game_dir, world)?;
        tntl_instance::datapacks::remove_world(&game_dir, world)?;
        Ok(())
    }

    /// Copy a world inside the same instance.
    pub fn duplicate_world(&self, name: &str, world: &str) -> Result<WorldInfo> {
        let instance = self.load_instance(name)?;
        let game_dir = instance.game_dir(&self.instance_dir(name)?);
        Ok(tntl_instance::worlds::duplicate(
            &game_dir,
            world,
            &format!("{}-copy", world.trim()),
        )?)
    }

    /// Import a world zip; also restores one of this instance's backups.
    pub fn import_world(&self, name: &str, archive: &Path) -> Result<WorldInfo> {
        let instance = self.load_instance(name)?;
        let game_dir = instance.game_dir(&self.instance_dir(name)?);
        Ok(tntl_instance::worlds::import(&game_dir, archive)?)
    }

    /// Datapacks of one world, the enabled ones first.
    pub fn datapacks(&self, name: &str, world: &str) -> Result<Vec<DatapackInfo>> {
        let instance = self.load_instance(name)?;
        let game_dir = instance.game_dir(&self.instance_dir(name)?);
        Ok(tntl_instance::datapacks::list(&game_dir, world)?)
    }

    /// Switch one world datapack on or off. The game must not be running.
    pub fn set_datapack_enabled(
        &self,
        name: &str,
        world: &str,
        file: &str,
        enabled: bool,
    ) -> Result<Vec<DatapackInfo>> {
        let instance = self.load_instance(name)?;
        let game_dir = instance.game_dir(&self.instance_dir(name)?);
        tntl_instance::datapacks::set_enabled(&game_dir, world, file, enabled)?;
        Ok(tntl_instance::datapacks::list(&game_dir, world)?)
    }

    /// Copy a datapack zip into a world.
    pub fn add_datapack(&self, name: &str, world: &str, path: &Path) -> Result<Vec<DatapackInfo>> {
        let instance = self.load_instance(name)?;
        let game_dir = instance.game_dir(&self.instance_dir(name)?);
        tntl_instance::datapacks::add(&game_dir, world, path)?;
        Ok(tntl_instance::datapacks::list(&game_dir, world)?)
    }

    pub fn delete_datapack(&self, name: &str, world: &str, file: &str) -> Result<Vec<DatapackInfo>> {
        let instance = self.load_instance(name)?;
        let game_dir = instance.game_dir(&self.instance_dir(name)?);
        tntl_instance::datapacks::delete(&game_dir, world, file)?;
        Ok(tntl_instance::datapacks::list(&game_dir, world)?)
    }

    /// Copy a jar into `mods/` and return the new list.
    pub fn add_local_mod(&self, name: &str, path: &Path) -> Result<Vec<tntl_instance::mods::ModInfo>> {
        self.ensure_not_running(name)?;
        let instance = self.load_instance(name)?;
        let game_dir = instance.game_dir(&self.instance_dir(name)?);
        copy_content(&game_dir, ProjectKind::Mod, path)?;
        let file = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        self.reject_unsatisfied_mods(name, &[file])?;
        self.mods(name)
    }

    /// Copy a resource pack or shader archive into the game directory.
    pub fn add_local_content(&self, name: &str, kind: ProjectKind, path: &Path) -> Result<ContentListing> {
        if kind == ProjectKind::Mod {
            bail!("use add_local_mod for mods");
        }
        self.ensure_not_running(name)?;
        let instance = self.load_instance(name)?;
        let game_dir = instance.game_dir(&self.instance_dir(name)?);
        copy_content(&game_dir, kind, path)?;
        self.content(name, kind)
    }

    /// Copy an instance directory under a new name.
    pub fn duplicate_instance(&self, name: &str, new_name: &str) -> Result<PathBuf> {
        validate_name(new_name)?;
        let source = self.instance_dir(name)?;
        if !source.join(tntl_instance::INSTANCE_FILE).exists() {
            bail!("instance {name} does not exist");
        }
        let target = self.instance_dir(new_name)?;
        if target.exists() {
            bail!("instance {new_name} already exists; pick another name");
        }
        let source_instance = self.load_instance(name)?;
        copy_dir(&source, &target).with_context(|| format!("cannot copy {name} to {new_name}"))?;
        let mut instance = Instance::load(&target)?;
        instance.name = new_name.to_string();
        let source_game = source_instance.game_dir(&source);
        // An imported instance keeps its game directory outside the launcher.
        // Copy it into the duplicate so the two instances stop sharing mods,
        // saves and options.
        if !source_game.starts_with(&source) {
            let copy = copy_dir(&source_game, &target.join("minecraft"))
                .with_context(|| format!("cannot copy the game directory of {name}"));
            if let Err(error) = copy {
                let _ = fs::remove_dir_all(&target);
                return Err(error);
            }
            instance.game_dir = None;
        }
        instance.save(&target)?;
        Ok(target)
    }

    /// Write the instance's game directory as a portable `.mrpack`.
    pub fn export_instance(
        &self,
        name: &str,
        output: &Path,
        pack_name: Option<&str>,
        pack_version: &str,
    ) -> Result<ExportReport> {
        self.ensure_not_running(name)?;
        let dir = self.instance_dir(name)?;
        let instance = self.load_instance(name)?;
        let game_dir = instance.game_dir(&dir);
        let chain = self.load_chain(&instance.layout(&self.root), &instance.version_id)?;
        let environment = environment_from_chain(&instance.version_id, &chain);
        let loader = loader_from_chain(&chain, &environment.game_version);
        let options = tntl_modpack::ExportOptions {
            name: pack_name.unwrap_or(&instance.name),
            version: pack_version,
            game: &environment.game_version,
            loader,
            summary: "Exported by TNTL",
        };
        Ok(tntl_modpack::export(&game_dir, output, &options)?)
    }
}

/// A world zip kept under `<instance>/backups/worlds/`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldBackup {
    pub file: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub created_ms: Option<i64>,
}

/// A file in `resourcepacks/` or `shaderpacks/`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentFile {
    pub file: String,
    pub bytes: u64,
    /// Seconds since the Unix epoch when the file was last changed, 0 if unknown.
    #[serde(default)]
    pub modified: u64,
    /// Whether the game currently has it switched on.
    pub enabled: bool,
    /// `pack.description` from `pack.mcmeta`, if the pack has one.
    #[serde(default)]
    pub description: Option<String>,
    /// `pack.pack_format`.
    #[serde(default)]
    pub format: Option<u32>,
    /// `supported_formats` bounds, from packs built for 1.20.2 and newer.
    #[serde(default)]
    pub min_format: Option<u32>,
    #[serde(default)]
    pub max_format: Option<u32>,
}

/// Sidecar of [`MODRINTH_CONTENT_FILE`]: `resourcepacks/x.zip` -> project id.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModrinthContent {
    #[serde(default)]
    pub files: std::collections::BTreeMap<String, String>,
}

impl ModrinthContent {
    fn load(path: &Path) -> Self {
        fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self, path: &Path) -> Result<()> {
        tntl_store::atomic_write_json(path, self)?;
        Ok(())
    }

    fn key(kind: ProjectKind, file: &str) -> String {
        format!("{}/{}", kind.dir(), file)
    }

    fn record(&mut self, kind: ProjectKind, file: &str, project: &str) {
        if !project.is_empty() {
            // Keys ignore the `.disabled` suffix so a toggled mod keeps its
            // source: the file is back under its plain name when enabled.
            self.files
                .insert(Self::key(kind, strip_disabled(file)), project.to_string());
        }
    }

    /// Project that published a file, by its on-disk name.
    fn project_of(&self, kind: ProjectKind, file: &str) -> Option<&str> {
        self.files
            .get(&Self::key(kind, strip_disabled(file)))
            .map(String::as_str)
    }

    /// Forget one file; returns whether anything was recorded for it.
    fn remove(&mut self, kind: ProjectKind, file: &str) -> bool {
        self.files.remove(&Self::key(kind, file)).is_some()
    }

    fn rename(&mut self, kind: ProjectKind, from: &str, to: &str) {
        if let Some(project) = self.files.remove(&Self::key(kind, from)) {
            self.files.insert(Self::key(kind, to), project);
        }
    }

    /// Project ids of one kind whose file is still on disk.
    fn projects_present(&self, kind: ProjectKind, dir: &Path) -> Vec<String> {
        let prefix = format!("{}/", kind.dir());
        self.files
            .iter()
            .filter(|(key, _)| key.starts_with(&prefix))
            .filter(|(key, _)| {
                let file = &key[prefix.len()..];
                dir.join(file).is_file()
                    || dir
                        .join(format!("{file}{}", tntl_instance::mods::DISABLED_SUFFIX))
                        .is_file()
            })
            .map(|(_, project)| project.clone())
            .collect()
    }

    /// Project ids of one kind, for the "installed" marks in the browser.
    fn projects(&self, kind: ProjectKind) -> Vec<String> {
        let prefix = format!("{}/", kind.dir());
        self.files
            .iter()
            .filter(|(key, _)| key.starts_with(&prefix))
            .map(|(_, project)| project.clone())
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentListing {
    pub files: Vec<ContentFile>,
    /// Shaders can only be switched when Iris or OptiFine is installed.
    pub toggle_supported: bool,
    /// Pack format the instance's game version expects, for flagging packs
    /// built for another release.
    #[serde(default)]
    pub expected_format: Option<u32>,
}

/// Copy the fields a fresh profile brings into a stored account.
fn apply_profile(account: &mut StoredAccount, profile: &MsaProfile) {
    account.name = profile.name.clone();
    account.uuid = profile.id.clone();
    if let Some(skin) = profile
        .skins
        .iter()
        .find(|skin| skin.state.eq_ignore_ascii_case("active"))
    {
        account.skin_model = if skin.variant.eq_ignore_ascii_case("slim") {
            SkinModel::Slim
        } else {
            SkinModel::Wide
        };
    }
    account.cape = profile
        .capes
        .iter()
        .find(|cape| cape.state.eq_ignore_ascii_case("active"))
        .map(|cape| cape.id.clone());
    account.capes = profile.capes.clone();
}

/// Name of the pack an instance was installed from, read from its copy of the
/// index.
fn modpack_name(instance_dir: &Path) -> Option<String> {
    if let Some(source) = read_ftb_source(instance_dir) {
        return Some(source.pack_name);
    }
    let bytes = fs::read(instance_dir.join(PACK_FILE)).ok()?;
    let index: PackIndex = serde_json::from_slice(&bytes).ok()?;
    let name = index.name.trim();
    if !name.is_empty() {
        return Some(name.to_string());
    }
    let version_id = index.version_id.trim();
    (!version_id.is_empty()).then(|| version_id.to_string())
}

/// FTB marker of an instance, when it was installed from the FTB catalogue.
fn curse_pack_view(entry: tntl_curseforge::Mod) -> CursePackView {
    let mut files: Vec<CursePackFileView> = entry
        .latest_files
        .into_iter()
        .filter(|file| file.is_available)
        .map(|file| CursePackFileView {
            file_id: file.id,
            name: file.file_name.clone(),
            version: curseforge_version(&file),
            game_versions: file.game_versions.clone(),
            release_type: file.release_type,
            file_date: file.file_date.clone(),
            size: file.file_length,
        })
        .collect();
    files.sort_by(|left, right| left.file_date.cmp(&right.file_date));
    CursePackView {
        project_id: entry.id,
        slug: entry.slug,
        title: entry.name,
        description: entry.summary,
        icon_url: entry.logo.map(|logo| logo.url).filter(|url| !url.is_empty()),
        downloads: entry.download_count,
        author: entry
            .authors
            .first()
            .map(|author| author.name.clone())
            .unwrap_or_default(),
        files,
    }
}

fn curseforge_version(file: &tntl_curseforge::File) -> String {
    if file.display_name.trim().is_empty() {
        file.file_name.clone()
    } else {
        file.display_name.clone()
    }
}

fn release_kind(release_type: u32) -> &'static str {
    match release_type {
        2 => "beta",
        3 => "alpha",
        _ => "release",
    }
}

fn curse_pack_entry(plan: &CursePackDownload) -> CursePackFileEntry {
    CursePackFileEntry {
        path: plan.path.clone(),
        project_id: plan.project_id,
        file_id: plan.file_id,
        sha1: plan.sha1.clone(),
        size: plan.size,
    }
}

/// Forge and NeoForge 1.20.1 builds carry the game version in their maven
/// version; CurseForge manifests leave it out.
fn normalize_loader_version(loader: LoaderKind, game: &str, version: &str) -> String {
    tntl_modpack::PackLoader::from_key(loader.key())
        .map(|kind| kind.install_version(game, version))
        .unwrap_or_else(|| version.to_string())
}

fn read_curseforge_source(instance_dir: &Path) -> Option<CursePackSource> {
    let bytes = fs::read(instance_dir.join(CURSEFORGE_PACK_FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn read_ftb_source(instance_dir: &Path) -> Option<FtbSource> {
    let bytes = fs::read(instance_dir.join(FTB_FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn ftb_summary_view(summary: tntl_ftb::PackSummary) -> FtbPackView {
    FtbPackView {
        id: summary.id,
        name: summary.name,
        synopsis: summary.synopsis,
        description: summary.description,
        icon_url: summary.icon_url,
        installs: summary.installs,
        plays: summary.plays,
        tags: summary.tags,
        versions: summary.versions.into_iter().map(ftb_version_view).collect(),
    }
}

fn ftb_version_view(version: tntl_ftb::VersionSummary) -> FtbVersionView {
    let loader = version.loader_key();
    let loader_version = version.loader.map(|loader| loader.version);
    FtbVersionView {
        id: version.id,
        name: version.name,
        kind: version.kind,
        game_version: version.game_version,
        loader,
        loader_version,
        java: version.java,
        recommended_memory_mb: version.recommended_memory_mb,
        files: 0,
        bytes: 0,
    }
}

fn ftb_pack_view(pack: tntl_ftb::Pack) -> FtbPackView {
    FtbPackView {
        id: pack.id,
        name: pack.name,
        synopsis: pack.synopsis,
        description: pack.description,
        icon_url: pack.icon_url,
        installs: pack.installs,
        plays: pack.plays,
        tags: pack.tags,
        versions: pack.versions.into_iter().map(ftb_version_view).collect(),
    }
}

/// The copy of a pack index an instance keeps.
/// Modrinth sort order from a user-facing key.
fn parse_index(sort: &str) -> Result<SearchIndex> {
    SearchIndex::from_key(sort)
        .with_context(|| format!("unknown sort order {sort}; use relevance, downloads, follows, newest or updated"))
}

/// Instance loader as catalogue filter. NeoForge 1.20.1 is a Forge fork and
/// runs Forge mods, which Modrinth tags `forge` only; without the fallback the
/// newest compatible build would be an old one tagged `neoforge`.
/// True when `version` satisfies every dependent's set of predicates. A set
/// whose predicates are not understood does not reject the version.
fn ranges_match(version: &str, predicates: &[Vec<String>]) -> bool {
    predicates
        .iter()
        .all(|ranges| tntl_doctor::version::any_matches(version, ranges) != Some(false))
}

fn mod_filter(game_version: &str, loader: &str) -> Filter {
    let filter = Filter::new(game_version, loader);
    if filter.loader == "neoforge" && filter.game_version.trim() == "1.20.1" {
        filter.accepting("forge")
    } else {
        filter
    }
}

fn read_pack_index(instance_dir: &Path) -> Result<PackIndex> {
    let path = instance_dir.join(PACK_FILE);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            bail!(
                "{} is not a modpack instance; install a .mrpack to get pack updates",
                instance_dir.display()
            );
        }
        Err(error) => return Err(error).with_context(|| format!("cannot read {}", path.display())),
    };
    serde_json::from_slice(&bytes).with_context(|| {
        format!(
            "{} is corrupt; reinstall the pack to restore update checks",
            path.display()
        )
    })
}

fn read_pack_source(instance_dir: &Path) -> Option<PackSource> {
    let bytes = fs::read(instance_dir.join(PACK_SOURCE_FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Modrinth project a pack instance belongs to. Checked from the sidecar first,
/// then guessed from the pack name and remembered for next time.
fn pack_project(modrinth: &Modrinth, instance_dir: &Path, index: &PackIndex) -> Result<String> {
    let path = instance_dir.join(PACK_SOURCE_FILE);
    if let Some(source) = read_pack_source(instance_dir)
        && !source.project_id.is_empty()
    {
        return Ok(source.project_id);
    }
    let hit = modrinth.find_modpack(&index.name)?.with_context(|| {
        format!(
            "cannot tell which Modrinth project {} came from; reinstall it from modrinth.com to get updates",
            index.name
        )
    })?;
    let source = PackSource {
        project_id: hit.project_id.clone(),
        version_number: index.version_id.clone(),
    };
    let _ = tntl_store::atomic_write_json(&path, &source);
    Ok(hit.project_id)
}

/// Newest build of the channels the instance follows.
fn pick_pack_version<'a>(
    candidates: &[&'a tntl_modrinth::Version],
    allow_beta: bool,
    allow_alpha: bool,
) -> Option<&'a tntl_modrinth::Version> {
    let channels: &[&str] = if allow_alpha {
        &["release", "beta", "alpha"]
    } else if allow_beta {
        &["release", "beta"]
    } else {
        &["release"]
    };
    channels.iter().find_map(|channel| {
        candidates
            .iter()
            .copied()
            .find(|version| version.version_type == *channel)
    })
}

fn ftb_target(version: &tntl_ftb::VersionSummary, in_place: bool) -> PackTarget {
    PackTarget {
        version_id: version.id.to_string(),
        version_number: version.name.clone(),
        version_type: version.kind.clone(),
        game_version: version.game_version.clone(),
        loader: version.loader_key(),
        in_place,
        url: String::new(),
        sha1: None,
        size: 0,
    }
}

fn pack_target(version: &tntl_modrinth::Version, in_place: bool) -> Option<PackTarget> {
    let file = version
        .files
        .iter()
        .find(|file| file.filename.ends_with(".mrpack"))
        .or_else(|| version.files.first())?;
    Some(PackTarget {
        version_id: version.id.clone(),
        version_number: version.version_number.clone(),
        version_type: version.version_type.clone(),
        game_version: version.game_versions.first().cloned().unwrap_or_default(),
        loader: version.loaders.first().cloned(),
        in_place,
        url: file.url.clone(),
        sha1: file.hashes.sha1.clone(),
        size: file.size,
    })
}

/// Remove files the previous pack build shipped and the new one dropped. Files
/// the user changed are left alone.
fn remove_stale_pack_files(game_dir: &Path, old: &PackIndex, new: &PackIndex) -> usize {
    let keep: HashSet<&str> = new
        .files
        .iter()
        .filter(|file| file.client_side())
        .map(|file| file.path.as_str())
        .collect();
    let mut removed = 0;
    for file in old.files.iter().filter(|file| file.client_side()) {
        if keep.contains(file.path.as_str()) {
            continue;
        }
        let Some(relative) = tntl_store::safe_relative(&file.path) else {
            continue;
        };
        let path = tntl_store::target_respecting_disabled(&game_dir.join(relative));
        if !path.is_file() {
            continue;
        }
        if let Some(sha1) = &file.hashes.sha1
            && !tntl_store::sha1_file(&path).is_ok_and(|actual| actual.eq_ignore_ascii_case(sha1))
        {
            continue;
        }
        if fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Components Prism and MultiMC write into `mmc-pack.json`.
#[derive(Debug, Default, Deserialize)]
struct MmcPack {
    #[serde(default)]
    components: Vec<MmcComponent>,
}

#[derive(Debug, Default, Deserialize)]
struct MmcComponent {
    #[serde(default)]
    uid: String,
    #[serde(default)]
    version: String,
}

/// Prism components that need a loader this launcher does not install.
const PRISM_UNSUPPORTED: &[&str] = &[
    "org.multimc.onesix.OneSixLauncher",
    "net.minecraft.launchwrapper",
    "com.mumfrey.liteloader",
    "optifine",
    "net.optifine",
];

fn prism_loader(uid: &str) -> Option<LoaderKind> {
    match uid {
        "net.fabricmc.fabric-loader" => Some(LoaderKind::Fabric),
        "org.quiltmc.quilt-loader" => Some(LoaderKind::Quilt),
        "net.minecraftforge" => Some(LoaderKind::Forge),
        "net.neoforged" | "net.neoforged.neoforge" => Some(LoaderKind::NeoForge),
        _ => None,
    }
}

/// Instance directory inside a Prism or MultiMC data folder.
fn prism_instance_dir(path: &Path) -> Result<PathBuf> {
    if path.join("mmc-pack.json").is_file() {
        return Ok(path.to_path_buf());
    }
    if path.join("instances").is_dir() {
        bail!(
            "{} is a launcher data folder; pick one instance inside its instances/ folder",
            path.display()
        );
    }
    bail!(
        "{} is not a Prism or MultiMC instance (no mmc-pack.json)",
        path.display()
    )
}

/// Values this launcher copies from `instance.cfg`.
#[derive(Debug, Default)]
struct PrismSettings {
    name: Option<String>,
    notes: String,
    max_memory_mb: Option<u32>,
}

fn prism_settings(path: &Path) -> PrismSettings {
    let Ok(text) = fs::read_to_string(path) else {
        return PrismSettings::default();
    };
    let mut settings = PrismSettings::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "name" => settings.name = Some(value.trim().to_string()),
            "notes" => settings.notes = value.trim().replace("\\n", "\n"),
            "MaxMemAlloc" => {
                settings.max_memory_mb = value.trim().parse::<u32>().ok().map(|mb| mb.clamp(512, 65536));
            }
            _ => {}
        }
    }
    settings
}

/// Copy a game directory, leaving logs and crash reports behind.
fn copy_game_dir(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    let mut stack = vec![from.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)?.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let relative = path.strip_prefix(from).unwrap_or(&path);
            if relative.components().count() == 1 && matches!(name.as_str(), "logs" | "crash-reports" | "natives") {
                continue;
            }
            let meta = fs::symlink_metadata(&path)?;
            if meta.file_type().is_symlink() {
                continue;
            }
            let target = to.join(relative);
            if meta.is_dir() {
                fs::create_dir_all(&target)?;
                stack.push(path);
            } else if meta.is_file() {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::copy(&path, &target)?;
            }
        }
    }
    Ok(())
}

/// Quick play arguments replaced `--server/--port` in 1.20.
pub fn supports_quick_play(game_version: &str) -> bool {
    let mut parts = game_version.split('.');
    let Some(major) = parts.next().and_then(|part| part.parse::<u32>().ok()) else {
        return false;
    };
    if major != 1 {
        return major > 1;
    }
    parts
        .next()
        .and_then(|part| part.parse::<u32>().ok())
        .is_some_and(|minor| minor >= 20)
}

/// Loader build an instance chain carries, in the version form
/// `loader_versions` reports (Forge keeps its `<game>-<build>` text).
fn installed_loader(chain: &[VersionJson], game: &str) -> Option<(LoaderKind, String)> {
    // Legacy Fabric publishes the same `fabric-loader` artifact as Fabric, so
    // the intermediary and LWJGL libraries are what tell the two apart.
    let legacy = chain
        .iter()
        .flat_map(|version| version.libraries.iter())
        .any(|library| {
            library.name.starts_with("net.legacyfabric:")
                || library.url.as_deref().is_some_and(|url| url.contains("legacyfabric"))
        });
    if let Some(found) = chain
        .iter()
        .flat_map(|version| version.libraries.iter())
        .find_map(|library| {
            let name = MavenName::parse(&library.name)?;
            let kind = match (name.group.as_str(), name.artifact.as_str()) {
                ("net.fabricmc", "fabric-loader") if legacy => LoaderKind::LegacyFabric,
                ("net.fabricmc", "fabric-loader") => LoaderKind::Fabric,
                ("org.quiltmc", "quilt-loader") => LoaderKind::Quilt,
                ("net.minecraftforge", "forge") => LoaderKind::Forge,
                ("net.neoforged", "neoforge") | ("net.neoforged", "forge") => LoaderKind::NeoForge,
                _ => return None,
            };
            Some((kind, name.version.clone()))
        })
    {
        return Some(found);
    }

    // Current Forge and NeoForge builds name no matching artifact, but their
    // version JSON carries the build in the FML launch arguments.
    let neoforge = chain
        .iter()
        .flat_map(|version| version.libraries.iter())
        .any(|library| library.name.starts_with("net.neoforged"));
    for version in chain {
        let args: Vec<&str> = version
            .arguments
            .as_ref()
            .map(|arguments| arguments.game.as_slice())
            .unwrap_or_default()
            .iter()
            .filter_map(|argument| match argument {
                tntl_model::Argument::Plain(value) => Some(value.as_str()),
                tntl_model::Argument::Conditional { .. } => None,
            })
            .collect();
        let after = |flag: &str| {
            args.iter()
                .position(|argument| *argument == flag)
                .and_then(|index| args.get(index + 1))
                .map(|value| value.to_string())
        };
        if let Some(build) = after("--fml.neoForgeVersion") {
            return Some((LoaderKind::NeoForge, build));
        }
        if let Some(build) = after("--fml.forgeVersion") {
            let kind = if neoforge {
                LoaderKind::NeoForge
            } else {
                LoaderKind::Forge
            };
            return Some((kind, format!("{game}-{build}")));
        }
    }
    // Composite profiles name the loader in `patches` instead of libraries.
    for patch in chain.iter().flat_map(|version| version.patches.iter()) {
        let kind = match patch.id.trim().to_ascii_lowercase().as_str() {
            "fabric" => LoaderKind::Fabric,
            "quilt" => LoaderKind::Quilt,
            "forge" => LoaderKind::Forge,
            "neoforge" => LoaderKind::NeoForge,
            _ => continue,
        };
        let build = patch.version.trim();
        if build.is_empty() {
            continue;
        }
        // Forge and NeoForge publish `<game>-<build>`; Fabric and Quilt publish
        // bare builds, so prefixing them would never match the update list.
        let version = match kind {
            LoaderKind::Fabric | LoaderKind::Quilt => build.to_string(),
            _ if build.starts_with(&format!("{game}-")) => build.to_string(),
            _ => format!("{game}-{build}"),
        };
        return Some((kind, version));
    }
    None
}

/// Loader kind and its maven version, as a `.mrpack` dependency needs them.
fn loader_from_chain(chain: &[VersionJson], game_version: &str) -> Option<(tntl_modpack::PackLoader, String)> {
    use tntl_modpack::PackLoader;
    let (kind, version) = installed_loader(chain, game_version)?;
    let loader = match kind {
        LoaderKind::Fabric => PackLoader::Fabric,
        LoaderKind::Quilt => PackLoader::Quilt,
        // `.mrpack` has no dependency key for Legacy Fabric.
        LoaderKind::LegacyFabric => return None,
        LoaderKind::Forge => PackLoader::Forge,
        LoaderKind::NeoForge => PackLoader::NeoForge,
    };
    // Pack indexes name the loader build without the game prefix.
    let version = version
        .strip_prefix(&format!("{game_version}-"))
        .unwrap_or(&version)
        .to_string();
    Some((loader, version))
}

fn loader_from_key(key: &str) -> ModLoader {
    match key {
        "fabric" | "legacyfabric" => ModLoader::Fabric,
        "quilt" => ModLoader::Quilt,
        "forge" => ModLoader::Forge,
        "neoforge" => ModLoader::NeoForge,
        _ => ModLoader::Unknown,
    }
}

fn loader_label(key: &str) -> &'static str {
    match key {
        "fabric" | "legacyfabric" => "Fabric",
        "quilt" => "Quilt",
        "forge" => "Forge",
        "neoforge" => "NeoForge",
        _ => "",
    }
}

/// Mark every jar the instance cannot satisfy. The mod list, the install
/// guard and the doctor all read this field, so the three never disagree.
fn mark_blocked(mods: &mut [tntl_instance::mods::ModInfo], status: &LoaderStatus) {
    let kind = status.kind.as_deref().unwrap_or_default();
    // Forge-family mods declare the loader's own build number, e.g.
    // `[47.4.0,)` against `1.20.1-47.1.106`.
    let forge_family = matches!(kind, "forge" | "neoforge");
    let label = loader_label(kind);
    let build =
        tntl_doctor::mods::loader_build_number(status.version.as_deref().unwrap_or_default(), &status.game_version);
    for module in mods {
        module.blocked =
            if forge_family && tntl_doctor::version::any_matches(build, &module.loader_versions) == Some(false) {
                Some(tntl_instance::mods::BlockedReason {
                    kind: "loader".to_string(),
                    loader: label.to_string(),
                    required: module.loader_versions.clone(),
                    actual: build.to_string(),
                })
            } else if tntl_doctor::version::any_matches(&status.game_version, &module.game_versions) == Some(false) {
                Some(tntl_instance::mods::BlockedReason {
                    kind: "game".to_string(),
                    loader: "Minecraft".to_string(),
                    required: module.game_versions.clone(),
                    actual: status.game_version.clone(),
                })
            } else {
                None
            };
    }
}

/// A short list of missing paths for the diagnostic report.
fn summarize_missing(missing: &[&MissingObject], root: &Path) -> String {
    let mut paths = Vec::new();
    for object in missing.iter().take(4) {
        let relative = object.path.strip_prefix(root).unwrap_or(&object.path);
        paths.push(relative.display().to_string());
    }
    let mut text = paths.join(", ");
    if missing.len() > paths.len() {
        text.push_str(&format!(" (+{})", missing.len() - paths.len()));
    }
    text
}

/// Derive the game version and loader from a resolved version chain (child
/// first, vanilla last).
pub fn environment_from_chain(version_id: &str, chain: &[VersionJson]) -> VersionEnvironment {
    let game_version = real_game_version(version_id, chain);
    let loader = installed_loader(chain, &game_version).map(|(kind, _)| kind.key().to_string());
    VersionEnvironment { game_version, loader }
}

/// Game build an instance runs. A real `inheritsFrom` chain ends at the vanilla
/// version; composite profiles (PCL2/HMCL `root: true` with `patches`) instead
/// carry it as the `game` patch, because their own `id` is the pack version.
fn real_game_version(version_id: &str, chain: &[VersionJson]) -> String {
    if chain.len() > 1
        && let Some(id) = chain
            .last()
            .map(|version| version.id.trim())
            .filter(|id| !id.is_empty())
    {
        return id.to_string();
    }
    chain
        .iter()
        .flat_map(|version| version.patches.iter())
        .find(|patch| patch.id.eq_ignore_ascii_case("game"))
        .map(|patch| patch.version.trim())
        .filter(|version| !version.is_empty())
        .map(str::to_string)
        .or_else(|| {
            chain
                .last()
                .map(|version| version.id.trim().to_string())
                .filter(|id| !id.is_empty())
        })
        .unwrap_or_else(|| version_id.to_string())
}

/// Files in a content directory such as `resourcepacks/`. Unzipped packs are
/// directories and count as content too.
fn content_files(dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<String> = entries
        .flatten()
        .filter(|entry| {
            let path = entry.path();
            path.is_file() || path.is_dir()
        })
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        // `.DS_Store`, `.index` and other hidden bookkeeping files are not packs.
        .filter(|name| !name.starts_with('.'))
        .collect();
    files.sort();
    files
}

/// Enabled resource packs in loading order with the most important one first:
/// `options.txt` lists them bottom-to-top, but the UI shows the stack top-down.
/// Disabled packs follow by name.
fn order_resource_packs(files: Vec<ContentFile>, enabled: &[String]) -> Vec<ContentFile> {
    let priority: HashMap<&str, usize> = enabled
        .iter()
        .rev()
        .enumerate()
        .map(|(index, name)| (name.as_str(), index))
        .collect();
    let mut files = files;
    files.sort_by(
        |left, right| match (priority.get(left.file.as_str()), priority.get(right.file.as_str())) {
            (Some(left), Some(right)) => left.cmp(right),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => left.file.cmp(&right.file),
        },
    );
    files
}

/// File name without its extensions, comparable with a Modrinth slug.
fn file_stem(file: &str) -> String {
    let name = file.strip_suffix(".disabled").unwrap_or(file);
    name.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(name).to_string()
}

/// Copy a local archive into the instance's content directory.
fn copy_content(game_dir: &Path, kind: ProjectKind, source: &Path) -> Result<PathBuf> {
    if !source.is_file() {
        bail!("{} is not a file", source.display());
    }
    let file = source
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .ok_or_else(|| anyhow::anyhow!("{} has no file name", source.display()))?;
    let lower = file.to_ascii_lowercase();
    let accepted = match kind {
        ProjectKind::Mod => lower.ends_with(".jar"),
        ProjectKind::ResourcePack | ProjectKind::Shader => lower.ends_with(".zip") || lower.ends_with(".jar"),
    };
    if !accepted {
        let expected = if kind == ProjectKind::Mod { ".jar" } else { ".zip" };
        bail!("{file} cannot be installed as a {kind}; expected a {expected} file");
    }
    let dir = game_dir.join(kind.dir());
    fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let target = dir.join(&file);
    // A disabled twin counts as installed too; creating both would leave a
    // file that can never be toggled.
    if target.exists() || tntl_store::target_respecting_disabled(&target) != target {
        bail!("{file} is already installed in {}", dir.display());
    }
    fs::copy(source, &target).with_context(|| format!("cannot copy {file} to {}", dir.display()))?;
    Ok(target)
}

/// Recursive copy used by instance duplication. Natives are regenerated on
/// every launch, so they are skipped.
fn copy_dir(source: &Path, target: &Path) -> std::io::Result<()> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == "natives" {
            continue;
        }
        let from = entry.path();
        let to = target.join(&name);
        let meta = fs::symlink_metadata(&from)?;
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            copy_dir(&from, &to)?;
        } else if meta.is_file() {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// One version directory inside an existing game root.
#[derive(Debug, Clone, Serialize)]
pub struct ImportCandidate {
    /// Directory name: the id everything else has to use.
    pub id: String,
    /// `id` inside the JSON, which launchers do not always keep in sync.
    pub json_id: String,
    pub game_version: String,
    pub loader: Option<String>,
    pub loader_version: Option<String>,
    pub release_time: String,
}

/// What the import dialog shows for a chosen path.
#[derive(Debug, Clone, Serialize)]
pub struct ImportScan {
    pub root: PathBuf,
    /// Newest first, so the default pick is the most recently installed one.
    pub versions: Vec<ImportCandidate>,
    /// Directories under `versions/` with a missing or unreadable JSON.
    pub skipped: Vec<String>,
}

/// Directory an import path points at. The picker hands over whatever the user
/// clicked, which may be the game root, its `versions/` folder, or one version
/// inside it.
fn import_root(path: &Path) -> (PathBuf, Option<String>) {
    if path.join("versions").is_dir() {
        return (path.to_path_buf(), None);
    }
    let name = path.file_name().map(|name| name.to_string_lossy().to_string());
    if name.as_deref() == Some("versions")
        && let Some(parent) = path.parent()
    {
        return (parent.to_path_buf(), None);
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| parent.file_name().is_some_and(|parent| parent == "versions"))
        && let Some(root) = parent.parent()
    {
        return (root.to_path_buf(), name);
    }
    (path.to_path_buf(), None)
}

fn scan_import_versions(root: &Path) -> ImportScan {
    let mut versions = Vec::new();
    let mut skipped = Vec::new();
    if let Ok(entries) = fs::read_dir(root.join("versions")) {
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().to_string();
            let Ok(bytes) = fs::read(entry.path().join(format!("{id}.json"))) else {
                skipped.push(id);
                continue;
            };
            let Ok(json) = serde_json::from_slice::<VersionJson>(&bytes) else {
                skipped.push(id);
                continue;
            };
            // Launchers rename a patch directory without rewriting the `id`
            // inside its JSON (HMCL keeps `neoforge-21.1.264` in
            // `versions/1.21.1-Neoforge`), so the directory name is the id.
            let game_version = json
                .inherits_from
                .clone()
                .or_else(|| {
                    json.patches
                        .iter()
                        .find(|patch| patch.id.eq_ignore_ascii_case("game"))
                        .map(|patch| patch.version.clone())
                })
                .map(|game| game.trim().to_string())
                .filter(|game| !game.is_empty())
                .unwrap_or_else(|| json.id.clone());
            let loader = installed_loader(std::slice::from_ref(&json), &game_version).or_else(|| {
                json.patches
                    .iter()
                    .find_map(|patch| LoaderKind::from_key(&patch.id).map(|kind| (kind, patch.version.clone())))
            });
            versions.push(ImportCandidate {
                id,
                json_id: json.id.clone(),
                game_version,
                loader: loader.as_ref().map(|(kind, _)| kind.key().to_string()),
                loader_version: loader.map(|(_, version)| version),
                release_time: json.release_time.unwrap_or_default(),
            });
        }
    }
    versions.sort_by(|left, right| {
        (right.release_time.as_str(), right.id.as_str()).cmp(&(left.release_time.as_str(), left.id.as_str()))
    });
    ImportScan {
        root: root.to_path_buf(),
        versions,
        skipped,
    }
}

/// Resolve the version the user named: directory, JSON id, or a game version
/// when only one version matches it.
fn pick_candidate<'a>(scan: &'a ImportScan, wanted: &str) -> Result<&'a ImportCandidate> {
    let wanted = wanted.trim();
    if let Some(found) = scan
        .versions
        .iter()
        .find(|candidate| candidate.id == wanted || candidate.json_id == wanted)
    {
        return Ok(found);
    }
    let by_game: Vec<&ImportCandidate> = scan
        .versions
        .iter()
        .filter(|candidate| candidate.game_version == wanted)
        .collect();
    let names = |list: &[&ImportCandidate]| {
        list.iter()
            .map(|candidate| candidate.id.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    match by_game.len() {
        1 => Ok(by_game[0]),
        0 => bail!(
            "version {wanted} not found in {}; available: {}",
            scan.root.join("versions").display(),
            names(&scan.versions.iter().collect::<Vec<_>>())
        ),
        _ => bail!(
            "{} has more than one version for {wanted}; pass the directory name: {}",
            scan.root.display(),
            names(&by_game)
        ),
    }
}

/// Game version and loader read from a launcher log.
struct DetectedInstall {
    game: String,
    loader: Option<(LoaderKind, Option<String>)>,
}

/// Scan `logs/latest.log` for the version and loader a game directory runs.
/// Only the two lines every loader prints are used; anything else is ignored,
/// which is why the import dialog still offers a version field.
fn detect_install(log: &Path) -> Option<DetectedInstall> {
    let bytes = fs::read(log).ok()?;
    let head = &bytes[..bytes.len().min(8 * 1024 * 1024)];
    let text = String::from_utf8_lossy(head);
    for line in text.lines() {
        if let Some(index) = line.find("Loading Minecraft ") {
            let rest = &line[index + "Loading Minecraft ".len()..];
            let mut parts = rest.split(" with ");
            let Some(game) = parts.next().map(str::trim).filter(|game| looks_like_version(game)) else {
                continue;
            };
            let loader = parts.next().and_then(|tail| {
                let (name, build) = tail.trim().rsplit_once(' ')?;
                let kind = match name {
                    "Fabric" | "Fabric Loader" => LoaderKind::Fabric,
                    "Quilt" | "Quilt Loader" => LoaderKind::Quilt,
                    "Forge" => LoaderKind::Forge,
                    "NeoForge" => LoaderKind::NeoForge,
                    _ => return None,
                };
                Some((kind, Some(build.trim().to_string())))
            });
            return Some(DetectedInstall {
                game: game.to_string(),
                loader,
            });
        }
        // Forge 1.7 to 1.12, including "Forge Mod Loader version 7.10.0.1150
        // for Minecraft 1.7.10 loading". The build there is FML's own version,
        // not the maven one, so only the game version is used.
        if let Some(index) = line.find("Forge Mod Loader version ") {
            let rest = &line[index + "Forge Mod Loader version ".len()..];
            let Some((_, after)) = rest.split_once(" for Minecraft ") else {
                continue;
            };
            let game = after.split_whitespace().next().unwrap_or_default();
            if !looks_like_version(game) {
                continue;
            }
            return Some(DetectedInstall {
                game: game.to_string(),
                loader: Some((LoaderKind::Forge, None)),
            });
        }
    }
    None
}

fn looks_like_version(value: &str) -> bool {
    value.starts_with(|ch: char| ch.is_ascii_digit()) && value.contains('.')
}

/// Directory name usable as an instance name: `.minecraft` becomes `minecraft`.
fn directory_name(value: &str) -> String {
    sanitize_name(value.trim_matches('.'))
}

fn validate_version_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id == "."
        || id == ".."
        || id.contains('/')
        || id.contains('\\')
        || id.chars().any(char::is_control)
    {
        bail!("invalid version id: {id}");
    }
    Ok(())
}

/// Reject names that cannot be one directory entry. Every path built from an
/// instance name goes through this, including instances that were created
/// before the portability rules in [`validate_name`] existed.
fn valid_component(name: &str) -> Result<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.chars().any(|ch| ch.is_control())
    {
        bail!("invalid instance name: {name}");
    }
    Ok(())
}

pub fn validate_name(name: &str) -> Result<()> {
    // Windows folds `foo.` and `foo ` onto `foo`, so two instances could share
    // one folder, and `CON`/`NUL` name devices. Rejecting them everywhere keeps
    // a data directory portable.
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1",
        "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    valid_component(name)?;
    let stem = name.split('.').next().unwrap_or(name).trim().to_ascii_uppercase();
    #[cfg(windows)]
    let bad_character = name
        .chars()
        .any(|ch| matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*'));
    #[cfg(not(windows))]
    let bad_character = false;
    if name.ends_with('.') || name.ends_with(' ') || RESERVED.contains(&stem.as_str()) || bad_character {
        bail!("invalid instance name: {name}");
    }
    Ok(())
}

pub fn sanitize_name(value: &str) -> String {
    let name: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    // Pack titles can end with a dot or spell a Windows device; both are
    // rejected as instance names, and the install would fail on a name the
    // user never typed.
    let name = name.trim_end_matches(['.', ' ']);
    if name.is_empty() {
        return "instance".to_string();
    }
    if validate_name(name).is_ok() {
        return name.to_string();
    }
    // A Windows device name reserves the stem, so `CON` becomes `CON-` and
    // `NUL.txt` becomes `NUL-.txt`.
    match name.split_once('.') {
        Some((stem, rest)) => format!("{stem}-.{rest}"),
        None => format!("{name}-"),
    }
}

pub fn list_instances(launcher: &Launcher) -> Result<Vec<Instance>> {
    let dir = launcher.instances_dir();
    let mut instances = Vec::new();
    let Ok(entries) = fs::read_dir(&dir) else {
        return Ok(instances);
    };
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        if let Ok(instance) = Instance::load(&entry.path()) {
            instances.push(instance);
        }
    }
    instances.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(instances)
}

/// Newest file in `<instance>/logs`, so the UI can offer "last log".
pub fn newest_log(instance_dir: &Path) -> Option<PathBuf> {
    let dir = instance_dir.join("logs");
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        let Ok(modified) = entry.metadata().and_then(|meta| meta.modified()) else {
            continue;
        };
        if newest.as_ref().is_none_or(|(time, _)| modified > *time) {
            newest = Some((modified, path));
        }
    }
    newest.map(|(_, path)| path)
}

/// One directory in the disk usage report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskEntry {
    /// Stable key the UI translates; `label` is the English fallback.
    pub key: String,
    pub label: String,
    pub path: PathBuf,
    pub bytes: u64,
}

impl DiskEntry {
    fn new(key: &str, label: &str, path: PathBuf) -> Self {
        let bytes = dir_size(&path);
        Self {
            key: key.to_string(),
            label: label.to_string(),
            path,
            bytes,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskUsage {
    pub entries: Vec<DiskEntry>,
    pub total: u64,
}

/// An installed `versions/<id>/` directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledVersion {
    pub id: String,
    pub bytes: u64,
    /// Instances whose version chain contains this id; empty means unused.
    pub instances: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CleanupReport {
    /// Directories that were removed, by label.
    pub removed: Vec<String>,
    pub bytes: u64,
}

/// Total size of a directory tree, ignoring symlinks.
fn dir_size(path: &Path) -> u64 {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return 0;
    };
    if meta.file_type().is_symlink() {
        return 0;
    }
    if meta.is_file() {
        return meta.len();
    }
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries.flatten().map(|entry| dir_size(&entry.path())).sum()
}

impl LauncherSettings {
    /// Clamp values that came from a hand-edited file.
    pub fn normalized(mut self) -> Self {
        self.concurrency = self.concurrency.clamp(1, 32);
        if !matches!(self.theme.as_str(), "dark" | "light" | "system") {
            self.theme = "dark".to_string();
        }
        if self.background.as_deref().is_some_and(str::is_empty) {
            self.background = None;
        }
        if tntl_source::ProxyMode::from_key(&self.proxy_mode).is_none() {
            self.proxy_mode = "system".to_string();
        } else {
            self.proxy_mode = self.proxy_mode.trim().to_ascii_lowercase();
        }
        self.proxy_url = self
            .proxy_url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .map(str::to_string);
        self.proxy_username = self
            .proxy_username
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string);
        // Passwords keep their spaces; only a missing account drops them.
        self.proxy_password = self
            .proxy_password
            .filter(|_| self.proxy_username.is_some())
            .filter(|password| !password.is_empty());
        self.msa_client_id = self
            .msa_client_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string);
        self.curseforge_api_key = self
            .curseforge_api_key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(str::to_string);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_names_reject_windows_aliases() {
        for name in ["one", "1.20.1", "my pack", "a.b", "Mekanism"] {
            assert!(validate_name(name).is_ok(), "{name}");
        }
        // `foo.` and `foo ` are the same folder on Windows, and `CON`/`NUL`
        // name devices: both would make two instances share one directory.
        for name in [
            "",
            ".",
            "..",
            "a/b",
            "a\\b",
            "trailing.",
            "trailing ",
            "CON",
            "nul",
            "com1",
            "CON.txt",
            "a\tb",
        ] {
            assert!(validate_name(name).is_err(), "{name}");
        }
        // A pack title becomes an instance name without the user typing it, so
        // it must not run into the rules above.
        assert_eq!(sanitize_name("CON"), "CON-");
        assert_eq!(sanitize_name("Foo."), "Foo");
        assert_eq!(sanitize_name("NUL.txt"), "NUL-.txt");
        assert_eq!(sanitize_name("..."), "instance");
        for title in ["CON", "Foo.", "Foo ", "...", "NUL.txt", "Mekanism 10.4"] {
            let name = sanitize_name(title);
            assert!(validate_name(&name).is_ok(), "{title} -> {name}");
        }
    }

    #[test]
    fn instances_created_before_the_name_rules_still_open() {
        // `foo.` was a legal instance name on Linux, and a data directory can
        // be copied from one platform to another; the stricter rules apply to
        // names being created, not to an existing folder.
        let launcher = Launcher::from_root(Some(std::env::temp_dir().join("tntl-legacy-name")));
        assert!(launcher.instance_dir("CON").is_ok());
        assert!(launcher.instance_dir("foo.").is_ok());
        assert!(launcher.instance_dir("../escape").is_err());
    }

    #[test]
    fn import_uses_the_version_directory_not_the_json_id() {
        // HMCL keeps the installer's id inside the json (`neoforge-21.1.264`)
        // while the directory is `1.21.1-Neoforge`; the directory is what the
        // game root can resolve, so the instance has to store that id.
        let root = std::env::temp_dir().join(format!("tntl-import-id-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let game = root.join("game");
        let version = game.join("versions").join("1.21.1-Neoforge");
        fs::create_dir_all(&version).unwrap();
        fs::write(
            version.join("1.21.1-Neoforge.json"),
            br#"{"id":"neoforge-21.1.264","inheritsFrom":"1.21.1","releaseTime":"2024-08-08T00:00:00+00:00","type":"release","libraries":[{"name":"net.neoforged:neoforge:21.1.264"}]}"#,
        )
        .unwrap();
        let launcher = Launcher::from_root(Some(root.clone()));

        let scan = launcher.import_candidates(&game).unwrap();
        assert_eq!(scan.versions.len(), 1);
        assert_eq!(scan.versions[0].id, "1.21.1-Neoforge");
        assert_eq!(scan.versions[0].game_version, "1.21.1");
        assert_eq!(scan.versions[0].loader.as_deref(), Some("neoforge"));
        assert_eq!(scan.versions[0].loader_version.as_deref(), Some("21.1.264"));

        let instance = launcher
            .import_instance(&game, Some("nf"), None, &|_: InstallEvent| {})
            .unwrap();
        assert_eq!(instance.version_id, "1.21.1-Neoforge");

        // Pointing at the version folder itself resolves the same game root.
        let nested = launcher
            .import_instance(&version, Some("nested"), None, &|_: InstallEvent| {})
            .unwrap();
        assert_eq!(nested.version_id, "1.21.1-Neoforge");
        assert_eq!(nested.root.as_deref(), Some(game.as_path()));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn running_marker_ignores_a_recycled_pid() {
        let root = std::env::temp_dir().join(format!("tntl-running-recycled-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let launcher = Launcher::from_root(Some(root.clone()));
        let dir = launcher.instance_dir("demo").unwrap();
        fs::create_dir_all(&dir).unwrap();

        // Marker left behind by a game that is long gone.
        fs::write(dir.join(RUNNING_FILE), br#"{"pid":4294967295}"#).unwrap();
        assert_eq!(launcher.running_pid("demo"), None);
        assert!(!dir.join(RUNNING_FILE).exists());

        // Live pid, but the marker was written by an older process that had
        // the same number; after a reboot this used to lock the instance.
        let me = std::process::id();
        let started = process_start_time(me).unwrap();
        fs::write(
            dir.join(RUNNING_FILE),
            format!(r#"{{"pid":{me},"start_time":{}}}"#, started + 1),
        )
        .unwrap();
        assert_eq!(launcher.running_pid("demo"), None);

        launcher.mark_running("demo", me).unwrap();
        assert_eq!(launcher.running_pid("demo"), Some(me));
        launcher.clear_running("demo");
        assert_eq!(launcher.running_pid("demo"), None);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn range_sets_reject_builds_one_dependent_cannot_load() {
        let pinned = vec![vec!["[1.1.20,1.2.0)".to_string()]];
        assert!(ranges_match("1.1.25a", &pinned));
        assert!(!ranges_match("1.1.12a", &pinned));
        assert!(!ranges_match("1.2.0", &pinned));
        // No predicates, or ones the parser does not know, never reject.
        assert!(ranges_match("0.1", &[]));
        assert!(ranges_match("0.1", &[vec!["nonsense-predicate".to_string()]]));
        // Every dependent has to be satisfied.
        let two = vec![vec!["[1.1.20,1.2.0)".to_string()], vec![">=1.1.24".to_string()]];
        assert!(!ranges_match("1.1.21", &two));
        assert!(ranges_match("1.1.25a", &two));
    }

    #[test]
    fn disabled_mod_files_stay_installed() {
        let dir = std::env::temp_dir().join(format!("tntl-disabled-content-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let mut record = ModrinthContent::default();
        record.record(ProjectKind::Mod, "gone.jar", "gone");
        record.record(ProjectKind::Mod, "off.jar", "off");
        fs::write(dir.join("off.jar.disabled"), b"jar").unwrap();
        // A toggled-off mod still occupies the folder; offering it again in the
        // browser would install a duplicate next to the disabled file.
        assert_eq!(record.projects_present(ProjectKind::Mod, &dir), ["off"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resource_pack_listing_puts_the_top_of_the_stack_first() {
        let file = |name: &str| ContentFile {
            enabled: false,
            file: name.to_string(),
            bytes: 0,
            modified: 0,
            description: None,
            format: None,
            min_format: None,
            max_format: None,
        };
        let files = vec![file("a.zip"), file("b.zip"), file("c.zip"), file("d.zip")];
        // options.txt is bottom-to-top, so `d` has the highest priority.
        let enabled = ["vanilla".to_string(), "b.zip".to_string(), "d.zip".to_string()];
        let sorted = order_resource_packs(files, &enabled);
        let names: Vec<&str> = sorted.iter().map(|entry| entry.file.as_str()).collect();
        assert_eq!(names, ["d.zip", "b.zip", "a.zip", "c.zip"]);
    }

    #[test]
    fn refuses_to_touch_a_running_instance() {
        let root = std::env::temp_dir().join(format!("tntl-running-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let launcher = Launcher::from_root(Some(root.clone()));
        launcher.create_instance(&Instance::new("live", "1.20.1")).unwrap();
        // The test process itself is a live pid on Linux.
        launcher.mark_running("live", std::process::id()).unwrap();
        assert!(launcher.delete_instance("live").is_err());
        launcher.clear_running("live");
        assert_eq!(launcher.running_pid("live"), None);
        launcher.delete_instance("live").unwrap();
        // A marker whose process is gone is cleaned up instead of blocking.
        launcher.create_instance(&Instance::new("stale", "1.20.1")).unwrap();
        launcher.mark_running("stale", 0).unwrap();
        assert_eq!(launcher.running_pid("stale"), None);
        launcher.delete_instance("stale").unwrap();
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn instance_from_installed_version_needs_no_download() {
        let root = std::env::temp_dir().join(format!("tntl-instance-from-version-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let launcher = Launcher::from_root(Some(root.clone()));
        let path = launcher.layout().version_json("1.20.1");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"id":"1.20.1"}"#).unwrap();

        launcher.create_instance_from_version("1.20.1", "second").unwrap();
        let instance = launcher.load_instance("second").unwrap();
        assert_eq!(instance.version_id, "1.20.1");
        assert!(launcher.create_instance_from_version("1.20.1", "second").is_err());
        assert!(launcher.create_instance_from_version("9.9.9", "nope").is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn new_offline_accounts_wear_a_built_in_skin() {
        let root = std::env::temp_dir().join(format!("tntl-offline-skin-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let launcher = Launcher::from_root(Some(root.clone()));

        let account = launcher.add_offline_account("Skinny").unwrap();
        assert!(account.has_skin);
        assert!(account.apply_skin);
        let bytes = launcher.account_skin(&account.id).unwrap().expect("skin file");
        assert!(
            tntl_auth::DEFAULT_SKINS
                .iter()
                .any(|key| tntl_auth::default_skin_image(key) == Some(bytes.as_slice())),
            "the skin is not one of the built-ins"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn renaming_an_offline_account_moves_its_uuid_and_skin() {
        let root = std::env::temp_dir().join(format!("tntl-rename-account-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let launcher = Launcher::from_root(Some(root.clone()));
        let before = launcher.add_offline_account("OldName").unwrap();
        assert_eq!(before.uuid, tntl_auth::offline_uuid("OldName"));
        fs::create_dir_all(tntl_auth::skins_dir(&root)).unwrap();
        fs::write(tntl_auth::skin_path(&root, &before.id), b"png").unwrap();

        let after = launcher.rename_offline_account(&before.id, "NewName").unwrap();
        assert_eq!(after.name, "NewName");
        assert_eq!(after.uuid, tntl_auth::offline_uuid("NewName"));
        assert_ne!(after.id, before.id);
        assert_eq!(launcher.accounts().unwrap().current.as_deref(), Some(after.id.as_str()));
        assert!(!tntl_auth::skin_path(&root, &before.id).exists());
        assert!(tntl_auth::skin_path(&root, &after.id).exists());

        launcher.add_offline_account("Taken").unwrap();
        assert!(launcher.rename_offline_account(&after.id, "Taken").is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn launch_requires_an_account() {
        let root = std::env::temp_dir().join(format!("tntl-account-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let launcher = Launcher::from_root(Some(root.clone()));
        let instance = Instance::new("test", "1.20.1");
        let error = launcher.effective_account(&instance).unwrap_err();
        assert!(error.to_string().contains("add an account first"), "{error}");
        launcher.add_offline_account("Tester").unwrap();
        assert_eq!(launcher.effective_account(&instance).unwrap().username, "Tester");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn detects_version_and_loader_from_logs() {
        let dir = std::env::temp_dir().join(format!("tntl-detect-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let log = dir.join("latest.log");

        fs::write(
            &log,
            "[20:07:52] [main/INFO]: Forge Mod Loader version 14.23.5.2864 for Minecraft 1.12.2 loading\n",
        )
        .unwrap();
        let found = detect_install(&log).unwrap();
        assert_eq!(found.game, "1.12.2");
        let (kind, build) = found.loader.unwrap();
        assert_eq!(kind, LoaderKind::Forge);
        assert_eq!(build, None);

        fs::write(
            &log,
            "[12:00:00] [main/INFO]: Loading Minecraft 1.20.1 with Fabric Loader 0.19.5\n",
        )
        .unwrap();
        let found = detect_install(&log).unwrap();
        assert_eq!(found.game, "1.20.1");
        let (kind, build) = found.loader.unwrap();
        assert_eq!(kind, LoaderKind::Fabric);
        assert_eq!(build.as_deref(), Some("0.19.5"));

        fs::write(&log, "[12:00:00] [main/INFO]: Setting user: Player\n").unwrap();
        assert!(detect_install(&log).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn version_cleanup_respects_instances() {
        let root = std::env::temp_dir().join(format!("tntl-version-cleanup-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let launcher = Launcher::from_root(Some(root.clone()));
        let layout = launcher.layout();
        let write = |id: &str, parent: Option<&str>| {
            let path = layout.version_json(id);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let body = match parent {
                Some(parent) => format!(r#"{{"id":"{id}","inheritsFrom":"{parent}"}}"#),
                None => format!(r#"{{"id":"{id}"}}"#),
            };
            fs::write(path, body).unwrap();
        };
        write("1.20.1", None);
        write("1.20.1-fabric", Some("1.20.1"));
        write("1.19.4", None);

        launcher
            .create_instance(&Instance::new("pack", "1.20.1-fabric"))
            .unwrap();
        launcher.create_instance(&Instance::new("solo", "1.20.1")).unwrap();

        let details = launcher.installed_version_details();
        let fabric = details.iter().find(|entry| entry.id == "1.20.1-fabric").unwrap();
        assert_eq!(fabric.instances, vec!["pack".to_string()]);
        assert!(
            details
                .iter()
                .find(|entry| entry.id == "1.19.4")
                .unwrap()
                .instances
                .is_empty()
        );

        let error = launcher.delete_version("1.20.1").unwrap_err().to_string();
        assert!(error.contains("used by pack, solo"), "{error}");
        launcher.delete_version("1.19.4").unwrap();
        assert!(launcher.delete_version("1.19.4").is_err());

        // Only the chain of the deleted instance goes away; shared versions stay.
        assert_eq!(
            launcher.delete_instance_with_cleanup("pack", true).unwrap(),
            vec!["1.20.1-fabric".to_string()]
        );
        assert_eq!(launcher.installed_versions(), vec!["1.20.1".to_string()]);

        assert_eq!(
            launcher.delete_instance_with_cleanup("solo", true).unwrap(),
            vec!["1.20.1".to_string()]
        );
        assert!(launcher.installed_versions().is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn names_imported_directories() {
        assert_eq!(directory_name(".minecraft"), "minecraft");
        assert_eq!(directory_name("my pack"), "my-pack");
    }

    #[test]
    fn settings_clamp_values() {
        let settings = LauncherSettings {
            concurrency: 0,
            theme: "sepia".to_string(),
            ..LauncherSettings::default()
        }
        .normalized();
        assert_eq!(settings.concurrency, 1);
        assert_eq!(settings.theme, "dark");
    }

    #[test]
    fn settings_normalize_proxy() {
        let settings = LauncherSettings {
            proxy_mode: "SOCKS5".to_string(),
            proxy_url: Some("  127.0.0.1:1080  ".to_string()),
            ..LauncherSettings::default()
        }
        .normalized();
        assert_eq!(settings.proxy_mode, "socks5");
        assert_eq!(settings.proxy_url.as_deref(), Some("127.0.0.1:1080"));

        let settings = LauncherSettings {
            proxy_mode: "bogus".to_string(),
            proxy_url: Some("  ".to_string()),
            ..LauncherSettings::default()
        }
        .normalized();
        assert_eq!(settings.proxy_mode, "system");
        assert!(settings.proxy_url.is_none());

        let settings = LauncherSettings {
            proxy_mode: "http".to_string(),
            proxy_url: Some("127.0.0.1:7890".to_string()),
            proxy_username: Some("  user  ".to_string()),
            proxy_password: Some(" p w ".to_string()),
            ..LauncherSettings::default()
        }
        .normalized();
        assert_eq!(settings.proxy_username.as_deref(), Some("user"));
        assert_eq!(settings.proxy_password.as_deref(), Some(" p w "));

        // A password without a user name is dropped with it.
        let settings = LauncherSettings {
            proxy_password: Some("secret".to_string()),
            ..LauncherSettings::default()
        }
        .normalized();
        assert!(settings.proxy_username.is_none());
        assert!(settings.proxy_password.is_none());
    }

    #[test]
    fn saving_settings_applies_the_proxy() {
        let dir = std::env::temp_dir().join(format!("tntl-proxy-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let launcher = Launcher::from_root(Some(dir.clone()));
        let saved = launcher
            .save_settings(&LauncherSettings {
                proxy_mode: "http".to_string(),
                proxy_url: Some("127.0.0.1:7890".to_string()),
                proxy_username: Some("user".to_string()),
                proxy_password: Some("secret".to_string()),
                ..LauncherSettings::default()
            })
            .unwrap();
        assert_eq!(saved.proxy_mode, "http");
        let config = tntl_source::network_config();
        assert_eq!(config.mode, tntl_source::ProxyMode::Http);
        assert_eq!(config.url.as_deref(), Some("http://127.0.0.1:7890"));
        assert_eq!(config.username.as_deref(), Some("user"));
        assert_eq!(config.password.as_deref(), Some("secret"));
        let loaded = launcher.settings();
        assert_eq!(loaded.proxy_url.as_deref(), Some("127.0.0.1:7890"));
        tntl_source::set_network_config(tntl_source::NetworkConfig::default());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn normalizes_content_settings() {
        let settings = LauncherSettings {
            curseforge_api_key: Some("  $2a$10$abc  ".to_string()),
            msa_client_id: Some("   ".to_string()),
            ..LauncherSettings::default()
        }
        .normalized();
        assert_eq!(settings.curseforge_api_key.as_deref(), Some("$2a$10$abc"));
        assert!(settings.msa_client_id.is_none());
        assert_eq!(ContentSource::from_key("CF"), Some(ContentSource::CurseForge));
        assert_eq!(ContentSource::from_key("modrinth"), Some(ContentSource::Modrinth));
        assert_eq!(ContentSource::from_key("bogus"), None);
    }

    #[test]
    fn quick_play_starts_at_1_20() {
        assert!(!supports_quick_play("1.19.4"));
        assert!(!supports_quick_play("1.7.10"));
        assert!(supports_quick_play("1.20"));
        assert!(supports_quick_play("1.21.1"));
        assert!(supports_quick_play("26.2"));
        assert!(!supports_quick_play("snapshot"));
    }

    #[test]
    fn rejects_directory_traversal() {
        assert!(validate_name("../escape").is_err());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name("").is_err());
        assert!(validate_name("1.20.1").is_ok());
    }

    #[test]
    fn sanitizes_version_ids() {
        assert_eq!(sanitize_name("1.20.1"), "1.20.1");
        assert_eq!(sanitize_name("24w14a (test)"), "24w14a--test-");
    }

    fn version_with_library(id: &str, name: &str) -> VersionJson {
        VersionJson {
            id: id.to_string(),
            libraries: vec![tntl_model::Library {
                name: name.to_string(),
                ..tntl_model::Library::default()
            }],
            ..VersionJson::default()
        }
    }

    #[test]
    fn detects_game_version_and_loader_from_the_chain() {
        let vanilla = version_with_library("1.20.1", "com.mojang:authlib:4.0.43");
        let fabric = version_with_library("fabric-loader-0.19.5-1.20.1", "net.fabricmc:fabric-loader:0.19.5");
        let environment = environment_from_chain("fabric-loader-0.19.5-1.20.1", &[fabric, vanilla]);
        assert_eq!(environment.game_version, "1.20.1");
        assert_eq!(environment.loader.as_deref(), Some("fabric"));

        let vanilla = version_with_library("1.20.1", "com.mojang:authlib:4.0.43");
        let environment = environment_from_chain("1.20.1", &[vanilla]);
        assert_eq!(environment.game_version, "1.20.1");
        assert_eq!(environment.loader, None);
    }

    #[test]
    fn reads_pcl2_composite_profiles() {
        // PCL2 `root: true` profiles keep the pack version as their id and the
        // game build in `patches`, without a top-level inheritsFrom.
        let composite = VersionJson {
            id: "2.0".to_string(),
            patches: vec![
                tntl_model::PatchRef {
                    id: "game".to_string(),
                    version: "1.20.1".to_string(),
                },
                tntl_model::PatchRef {
                    id: "forge".to_string(),
                    version: "47.4.5".to_string(),
                },
            ],
            ..VersionJson::default()
        };
        let environment = environment_from_chain("2.0", std::slice::from_ref(&composite));
        assert_eq!(environment.game_version, "1.20.1");
        assert_eq!(environment.loader.as_deref(), Some("forge"));
        assert_eq!(
            installed_loader(std::slice::from_ref(&composite), "1.20.1"),
            Some((LoaderKind::Forge, "1.20.1-47.4.5".to_string()))
        );
    }

    fn version_with_arguments(id: &str, game_args: &[&str]) -> VersionJson {
        VersionJson {
            id: id.to_string(),
            arguments: Some(tntl_model::Arguments {
                game: game_args
                    .iter()
                    .map(|argument| tntl_model::Argument::Plain(argument.to_string()))
                    .collect(),
                jvm: Vec::new(),
            }),
            ..VersionJson::default()
        }
    }

    #[test]
    fn detects_forge_and_neoforge_from_fml_arguments() {
        let vanilla = version_with_library("1.20.1", "com.mojang:authlib:4.0.43");
        let forge = version_with_arguments(
            "1.20.1-forge-47.4.28",
            &[
                "--launchTarget",
                "forgeclient",
                "--fml.forgeVersion",
                "47.4.28",
                "--fml.mcVersion",
                "1.20.1",
            ],
        );
        assert_eq!(
            installed_loader(&[forge.clone(), vanilla.clone()], "1.20.1"),
            Some((LoaderKind::Forge, "1.20.1-47.4.28".to_string()))
        );
        assert_eq!(
            environment_from_chain("1.20.1-forge-47.4.28", &[forge, vanilla])
                .loader
                .as_deref(),
            Some("forge")
        );

        let vanilla = version_with_library("1.21.1", "com.mojang:authlib:6.0.54");
        let neo = version_with_arguments(
            "neoforge-21.1.264",
            &["--fml.neoForgeVersion", "21.1.264", "--launchTarget", "forgeclient"],
        );
        assert_eq!(
            installed_loader(&[neo, vanilla], "1.21.1"),
            Some((LoaderKind::NeoForge, "21.1.264".to_string()))
        );
    }

    #[test]
    fn tells_legacy_fabric_from_fabric() {
        let vanilla = version_with_library("1.8.9", "com.mojang:authlib:3.5.11");
        let mut legacy = version_with_library("fabric-loader-0.20.1-1.8.9", "net.fabricmc:fabric-loader:0.20.1");
        legacy.libraries.push(tntl_model::Library {
            name: "net.legacyfabric:intermediary:1.8.9".to_string(),
            ..tntl_model::Library::default()
        });
        assert_eq!(
            installed_loader(&[legacy, vanilla], "1.8.9"),
            Some((LoaderKind::LegacyFabric, "0.20.1".to_string()))
        );
    }

    #[test]
    fn loader_instances_are_named_after_the_loader() {
        assert_eq!(InstallRequest::new("1.20.1").instance_name(), "1.20.1");
        assert_eq!(
            InstallRequest::new("1.20.1")
                .with_loader(LoaderKind::Fabric, None)
                .instance_name(),
            "1.20.1-fabric"
        );
        assert_eq!(
            InstallRequest::new("1.20.1")
                .with_loader(LoaderKind::Quilt, Some("0.24.0".to_string()))
                .instance_name(),
            "1.20.1-quilt"
        );
    }

    fn pack_version(id: &str, game: &str, channel: &str) -> tntl_modrinth::Version {
        serde_json::from_value(serde_json::json!({
            "id": id, "project_id": "1KVo5zza", "version_number": id,
            "version_type": channel, "game_versions": [game], "loaders": ["fabric"],
            "date_published": "2026-09-01T00:00:00Z",
            "files": [{ "primary": true, "filename": "pack.mrpack",
                        "url": format!("https://cdn.example/{id}.mrpack"), "size": 10 }]
        }))
        .unwrap()
    }

    #[test]
    fn pack_targets_follow_the_channel_of_the_instance() {
        let beta = pack_version("15.0.0-beta", "26.3", "beta");
        let release = pack_version("14.2.0", "26.2", "release");
        assert_eq!(
            pick_pack_version(&[&beta, &release], false, false).unwrap().id,
            "14.2.0"
        );
        assert!(pick_pack_version(&[&beta], false, false).is_none());
        assert_eq!(pick_pack_version(&[&beta], true, false).unwrap().id, "15.0.0-beta");
    }

    #[test]
    fn pack_targets_carry_the_mrpack_file() {
        let version = pack_version("14.1.1", "26.2", "release");
        let target = pack_target(&version, true).unwrap();
        assert!(target.in_place);
        assert_eq!(target.game_version, "26.2");
        assert_eq!(target.loader.as_deref(), Some("fabric"));
        assert_eq!(target.url, "https://cdn.example/14.1.1.mrpack");
    }

    fn pack_file(path: &str, sha1: &str) -> tntl_modpack::PackFile {
        tntl_modpack::PackFile {
            path: path.to_string(),
            hashes: tntl_modpack::PackHashes {
                sha1: Some(sha1.to_string()),
                sha512: None,
            },
            ..tntl_modpack::PackFile::default()
        }
    }

    #[test]
    fn stale_pack_files_go_only_when_untouched() {
        let dir = std::env::temp_dir().join(format!("tntl-stale-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("mods")).unwrap();
        fs::write(dir.join("mods/old.jar"), b"old").unwrap();
        fs::write(dir.join("mods/edited.jar"), b"edited").unwrap();
        fs::write(dir.join("mods/off.jar.disabled"), b"off").unwrap();
        let old = PackIndex {
            files: vec![
                pack_file("mods/old.jar", &tntl_store::sha1_bytes(b"old")),
                pack_file("mods/edited.jar", "nope"),
                pack_file("mods/off.jar", &tntl_store::sha1_bytes(b"off")),
            ],
            ..PackIndex::default()
        };
        let new = PackIndex {
            files: vec![pack_file("mods/keep.jar", "aa")],
            ..PackIndex::default()
        };
        assert_eq!(remove_stale_pack_files(&dir, &old, &new), 2);
        assert!(!dir.join("mods/old.jar").exists());
        assert!(dir.join("mods/edited.jar").exists());
        assert!(!dir.join("mods/off.jar.disabled").exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
