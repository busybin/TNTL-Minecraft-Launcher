//! Desktop shell: Tauri commands over the `tntl-app` service layer.

#![forbid(unsafe_code)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
use tauri::{Emitter, Manager, State};
use tntl_app::{InstalledMod, InstalledVersion, InstanceView, JavaView, Launcher, ModSearchPage, VersionView};
use tntl_instance::Instance;
use tntl_launch::{GameHandle, GameProcess, LaunchOptions};
use tntl_resolve::{MissingKind, VerifyMode};
use tntl_source::InstallEvent;

struct AppState {
    /// The launcher data directory; the header lets the user switch it.
    root: Mutex<PathBuf>,
    game: Mutex<Option<RunningGame>>,
    /// One launch at a time: two Play clicks used to spawn two games.
    launching: std::sync::atomic::AtomicBool,
}

struct RunningGame {
    name: String,
    /// Imported instances can share a game directory, so the guard compares
    /// this instead of the name alone.
    game_dir: PathBuf,
    handle: GameHandle,
}

/// Progress payloads carry the operation that produced them. Two commands can
/// run at once (a repair while a loader installs), and without the tag the
/// install page would show the other one's stage.
fn emit_tagged<T: Serialize>(app: &tauri::AppHandle, channel: &str, job: &str, event: &T) {
    let _ = app.emit(channel, serde_json::json!({ "job": job, "event": event }));
}

#[derive(Serialize)]
struct Bootstrap {
    root: String,
    instances: Vec<InstanceView>,
    installed_versions: Vec<String>,
    java: Vec<JavaView>,
    accounts: tntl_app::AccountsResponse,
}

#[derive(Serialize)]
struct RootsResponse {
    current: String,
    roots: Vec<String>,
}

#[derive(Clone, Serialize)]
struct InstallDone {
    version: String,
    downloaded: usize,
    skipped: usize,
    bytes: u64,
    /// Instance registered with the install, when one was requested.
    instance: Option<String>,
}

#[derive(Clone, Serialize)]
struct GameExit {
    instance: String,
    exit_code: Option<i32>,
    duration_ms: u128,
    crash: Option<CrashInfo>,
}

#[derive(Clone, Serialize)]
struct CrashInfo {
    kind: String,
    summary: String,
    hints: Vec<String>,
}

fn state_launcher(state: &State<'_, AppState>) -> Launcher {
    let root = state.root.lock().unwrap_or_else(|poison| poison.into_inner()).clone();
    Launcher::from_root(Some(root))
}

/// The game rewrites instance files on exit, so edits wait until it is gone.
fn ensure_stopped(state: &State<'_, AppState>, name: &str) -> Result<(), String> {
    let running = state.game.lock().unwrap_or_else(|poison| poison.into_inner());
    let Some(game) = running.as_ref() else {
        return Ok(());
    };
    if game.name == name {
        return Err(format!("instance {name} is running; close the game before editing it"));
    }
    let launcher = state_launcher(state);
    let shared_game_dir = match (launcher.instance_dir(name), launcher.load_instance(name)) {
        (Ok(dir), Ok(instance)) => instance.game_dir(&dir) == game.game_dir,
        _ => false,
    };
    if shared_game_dir {
        return Err(format!(
            "instance {name} shares its game directory with the running instance {}; close the game first",
            game.name
        ));
    }
    Ok(())
}

#[tauri::command]
fn roots(state: State<'_, AppState>) -> RootsResponse {
    let current = state
        .root
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .display()
        .to_string();
    let mut config = load_root_config();
    if !config.roots.contains(&current) {
        config.roots.push(current.clone());
    }
    RootsResponse {
        roots: config.roots,
        current,
    }
}

#[tauri::command]
fn set_root(state: State<'_, AppState>, path: String) -> Result<RootsResponse, String> {
    if state.game.lock().unwrap_or_else(|poison| poison.into_inner()).is_some() {
        return Err("close the running game before switching directories".to_string());
    }
    let path = PathBuf::from(path);
    if !path.is_dir() {
        return Err(format!("{} is not a directory", path.display()));
    }
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    let current = path.display().to_string();
    let previous = state
        .root
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .display()
        .to_string();
    let mut config = load_root_config();
    // Keep the directory we came from; otherwise the default root would
    // vanish as soon as the first new directory is added.
    if !config.roots.contains(&previous) {
        config.roots.push(previous);
    }
    if !config.roots.contains(&current) {
        config.roots.push(current.clone());
    }
    *state.root.lock().unwrap_or_else(|poison| poison.into_inner()) = path;
    config.current = Some(current.clone());
    save_root_config(&config);
    Ok(RootsResponse {
        roots: config.roots,
        current,
    })
}

#[tauri::command]
fn forget_root(state: State<'_, AppState>, path: String) -> RootsResponse {
    let target = std::fs::canonicalize(&path).unwrap_or_else(|_| PathBuf::from(&path));
    let target_text = target.display().to_string();
    let mut config = load_root_config();
    config.roots.retain(|entry| entry != &path && entry != &target_text);
    let mut current = state.root.lock().unwrap_or_else(|poison| poison.into_inner()).clone();
    if current == target {
        current = config
            .roots
            .first()
            .map(PathBuf::from)
            .unwrap_or_else(|| Launcher::from_root(None).root);
        let text = current.display().to_string();
        if !config.roots.contains(&text) {
            config.roots.push(text.clone());
        }
        config.current = Some(text);
        *state.root.lock().unwrap_or_else(|poison| poison.into_inner()) = current.clone();
    }
    save_root_config(&config);
    RootsResponse {
        current: current.display().to_string(),
        roots: config.roots,
    }
}

#[tauri::command]
async fn bootstrap(state: State<'_, AppState>) -> Result<Bootstrap, String> {
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || {
        Ok(Bootstrap {
            root: launcher.root.display().to_string(),
            instances: launcher.instance_views().map_err(message)?,
            installed_versions: launcher.installed_versions(),
            java: launcher.java_list(),
            accounts: launcher.accounts().map_err(message)?,
        })
    })
    .await
    .map_err(message)?
}

#[tauri::command]
async fn list_instances(state: State<'_, AppState>) -> Result<Vec<InstanceView>, String> {
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || launcher.instance_views().map_err(message))
        .await
        .map_err(message)?
}

#[tauri::command]
async fn list_versions(state: State<'_, AppState>, channel: String) -> Result<Vec<VersionView>, String> {
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || launcher.versions(&channel).map_err(message))
        .await
        .map_err(message)?
}

#[tauri::command]
async fn list_loaders(
    state: State<'_, AppState>,
    game: String,
    loader: String,
) -> Result<Vec<tntl_source::LoaderVersion>, String> {
    let launcher = state_launcher(&state);
    let kind = tntl_source::LoaderKind::from_key(&loader).ok_or_else(|| format!("unknown loader {loader}"))?;
    tauri::async_runtime::spawn_blocking(move || launcher.loader_versions(&game, kind).map_err(message))
        .await
        .map_err(message)?
}

#[tauri::command]
async fn loader_status(state: State<'_, AppState>, name: String) -> Result<tntl_app::LoaderStatus, String> {
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || launcher.loader_status(&name).map_err(message))
        .await
        .map_err(message)?
}

#[tauri::command]
async fn install_loader(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    name: String,
    loader: String,
    version: Option<String>,
) -> Result<tntl_app::LoaderStatus, String> {
    let _cancel = tntl_source::cancel::begin();
    ensure_stopped(&state, &name)?;
    let launcher = state_launcher(&state);
    let kind = tntl_source::LoaderKind::from_key(&loader).ok_or_else(|| format!("unknown loader {loader}"))?;
    let job = format!("loader:{name}");
    tauri::async_runtime::spawn_blocking(move || {
        let progress = |event: InstallEvent| {
            emit_tagged(&app, "install-progress", &job, &event);
        };
        launcher
            .install_loader(&name, kind, version.as_deref(), &progress)
            .map_err(message)
    })
    .await
    .map_err(message)?
}

#[tauri::command]
async fn remove_loader(state: State<'_, AppState>, name: String) -> Result<tntl_app::LoaderStatus, String> {
    ensure_stopped(&state, &name)?;
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || launcher.remove_loader(&name).map_err(message))
        .await
        .map_err(message)?
}

#[tauri::command]
async fn check_loader_update(state: State<'_, AppState>, name: String) -> Result<tntl_app::LoaderUpdate, String> {
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || launcher.check_loader_update(&name).map_err(message))
        .await
        .map_err(message)?
}

#[tauri::command]
async fn install_version(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    version: String,
    assets: bool,
    instance_name: Option<String>,
    loader: Option<String>,
    loader_version: Option<String>,
) -> Result<(), String> {
    let _cancel = tntl_source::cancel::begin();
    let launcher = state_launcher(&state);
    let mut request = tntl_app::InstallRequest::new(&version);
    request.assets = assets;
    request.instance = instance_name.clone();
    if let Some(loader) = loader.as_deref() {
        let kind = tntl_source::LoaderKind::from_key(loader).ok_or_else(|| format!("unknown loader {loader}"))?;
        request.loader = Some(kind);
        request.loader_version = loader_version;
    } else if loader_version.is_some() {
        return Err("a loader version was given without a loader".to_string());
    }
    let job = "install".to_string();
    tauri::async_runtime::spawn_blocking(move || {
        let _ = app.emit(
            "install-start",
            serde_json::json!({
                "version": &request.version,
                "loader": request.loader.map(|kind| kind.key()),
                "instance": &request.instance,
            }),
        );
        let progress = |event: InstallEvent| {
            emit_tagged(&app, "install-progress", &job, &event);
        };
        let result = launcher.install(&request, &progress);
        match result {
            Ok(report) => {
                let _ = app.emit(
                    "install-done",
                    InstallDone {
                        version: report.version_id,
                        downloaded: report.downloaded,
                        skipped: report.skipped,
                        bytes: report.bytes,
                        instance: report.instance,
                    },
                );
                Ok(())
            }
            Err(error) => {
                let message = format!("{error:#}");
                let _ = app.emit("install-failed", message.clone());
                // Also fail the promise so the page can offer the matching
                // Java runtime download instead of a bare banner.
                Err(message)
            }
        }
    })
    .await
    .map_err(message)?
}

/// What a zip archive contains: instance, world, modpack, resource pack.
#[tauri::command]
async fn archive_kind(path: String) -> Result<tntl_instance::archive::ArchiveKind, String> {
    tauri::async_runtime::spawn_blocking(move || {
        tntl_instance::archive::kind(std::path::Path::new(&path)).map_err(message)
    })
    .await
    .map_err(message)?
}

#[tauri::command]
async fn modpack_info(path: String) -> Result<tntl_app::ModpackInfo, String> {
    tauri::async_runtime::spawn_blocking(move || tntl_app::modpack_info(std::path::Path::new(&path)).map_err(message))
        .await
        .map_err(message)?
}

#[derive(Clone, Serialize)]
struct RepairDone {
    checked: usize,
    fetched: usize,
    bytes: u64,
    pack: Option<String>,
    /// Set when the loader had to be installed again.
    loader_rebuilt: bool,
}

/// Stop a running download. The install command then fails with "cancelled".
#[tauri::command]
fn cancel_download() {
    tntl_source::cancel::request();
}

#[tauri::command]
async fn repair_instance(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    name: String,
) -> Result<RepairDone, String> {
    let _cancel = tntl_source::cancel::begin();
    ensure_stopped(&state, &name)?;
    let launcher = state_launcher(&state);
    let job = format!("repair:{name}");
    tauri::async_runtime::spawn_blocking(move || {
        let report = launcher
            .repair_instance(
                &name,
                &|event| {
                    emit_tagged(&app, "install-progress", &job, &event);
                },
                &|event| {
                    emit_tagged(&app, "pack-progress", &job, &event);
                },
            )
            .map_err(message)?;
        Ok(RepairDone {
            checked: report.game_downloaded + report.game_skipped + report.pack_downloaded + report.pack_skipped,
            fetched: report.game_downloaded + report.pack_downloaded,
            bytes: report.bytes,
            pack: report.pack,
            loader_rebuilt: report.loader_rebuilt,
        })
    })
    .await
    .map_err(message)?
}

#[tauri::command]
async fn install_modpack(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    path: String,
    instance_name: Option<String>,
) -> Result<tntl_app::ModpackReport, String> {
    let _cancel = tntl_source::cancel::begin();
    let launcher = state_launcher(&state);
    let job = "modpack".to_string();
    tauri::async_runtime::spawn_blocking(move || {
        launcher
            .install_modpack(
                std::path::Path::new(&path),
                instance_name.as_deref(),
                &|event| {
                    emit_tagged(&app, "install-progress", &job, &event);
                },
                &|event| {
                    emit_tagged(&app, "pack-progress", &job, &event);
                },
            )
            .map_err(message)
    })
    .await
    .map_err(message)?
}

#[tauri::command]
async fn java_offers(state: State<'_, AppState>) -> Result<Vec<tntl_app::RuntimeOffer>, String> {
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || launcher.java_offers().map_err(message))
        .await
        .map_err(message)?
}

#[tauri::command]
async fn install_java(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    component: String,
) -> Result<tntl_app::InstalledRuntime, String> {
    let _cancel = tntl_source::cancel::begin();
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || {
        launcher
            .install_java_runtime(&component, &|event| {
                let _ = app.emit("java-progress", &event);
            })
            .map_err(message)
    })
    .await
    .map_err(message)?
}

#[tauri::command]
fn create_instance(
    state: State<'_, AppState>,
    name: String,
    version_id: String,
    username: Option<String>,
    max_memory_mb: Option<u32>,
) -> Result<(), String> {
    let launcher = state_launcher(&state);
    let mut instance = Instance::new(&name, &version_id);
    if let Some(username) = username.filter(|name| !name.trim().is_empty()) {
        instance.username = username;
    }
    if let Some(memory) = max_memory_mb {
        instance.max_memory_mb = memory.clamp(512, 65536);
    }
    launcher.create_instance(&instance).map_err(message)?;
    Ok(())
}

#[tauri::command]
fn save_instance(state: State<'_, AppState>, instance: Instance) -> Result<(), String> {
    ensure_stopped(&state, &instance.name)?;
    state_launcher(&state).save_instance(&instance).map_err(message)?;
    Ok(())
}

/// Drop the instance's Java override so the launcher picks a matching runtime.
#[tauri::command]
fn clear_instance_java(state: State<'_, AppState>, name: String) -> Result<(), String> {
    ensure_stopped(&state, &name)?;
    let launcher = state_launcher(&state);
    let mut instance = launcher.load_instance(&name).map_err(message)?;
    instance.java = None;
    launcher.save_instance(&instance).map(|_| ()).map_err(message)
}

#[tauri::command]
fn load_instance(state: State<'_, AppState>, name: String) -> Result<Instance, String> {
    state_launcher(&state).load_instance(&name).map_err(message)
}

#[tauri::command]
fn delete_instance(state: State<'_, AppState>, name: String, cleanup: Option<bool>) -> Result<Vec<String>, String> {
    ensure_stopped(&state, &name)?;
    state_launcher(&state)
        .delete_instance_with_cleanup(&name, cleanup.unwrap_or(false))
        .map_err(message)
}

#[tauri::command]
fn installed_versions(state: State<'_, AppState>) -> Vec<InstalledVersion> {
    state_launcher(&state).installed_version_details()
}

#[tauri::command]
fn delete_version(state: State<'_, AppState>, id: String) -> Result<(), String> {
    state_launcher(&state).delete_version(&id).map_err(message)
}

/// Register a second instance on an installed version without downloading it.
#[tauri::command]
fn create_instance_from_version(
    state: State<'_, AppState>,
    version_id: String,
    name: String,
) -> Result<String, String> {
    state_launcher(&state)
        .create_instance_from_version(&version_id, &name)
        .map(|dir| dir.display().to_string())
        .map_err(message)
}

#[tauri::command]
fn rename_instance(state: State<'_, AppState>, name: String, new_name: String) -> Result<String, String> {
    ensure_stopped(&state, &name)?;
    let launcher = state_launcher(&state);
    launcher
        .rename_instance(&name, &new_name)
        .map(|dir| dir.display().to_string())
        .map_err(message)
}

#[tauri::command]
async fn diagnose_instance(state: State<'_, AppState>, name: String) -> Result<tntl_app::DiagnosticReport, String> {
    let launcher = state_launcher(&state);
    let total = total_memory_mb();
    tauri::async_runtime::spawn_blocking(move || launcher.diagnose_instance(&name, total).map_err(message))
        .await
        .map_err(message)?
}

#[tauri::command]
async fn analyze_log(text: String) -> Result<tntl_doctor::log::LogAnalysis, String> {
    tauri::async_runtime::spawn_blocking(move || tntl_doctor::log::analyze(&text))
        .await
        .map_err(message)
}

#[tauri::command]
async fn list_mods(state: State<'_, AppState>, name: String) -> Result<Vec<tntl_instance::mods::ModInfo>, String> {
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || launcher.mods(&name).map_err(message))
        .await
        .map_err(message)?
}

#[tauri::command]
fn set_mod_enabled(
    state: State<'_, AppState>,
    name: String,
    file: String,
    enabled: bool,
) -> Result<Vec<tntl_instance::mods::ModInfo>, String> {
    ensure_stopped(&state, &name)?;
    state_launcher(&state)
        .set_mod_enabled(&name, &file, enabled)
        .map_err(message)
}

#[tauri::command]
fn remove_mod(
    state: State<'_, AppState>,
    name: String,
    file: String,
) -> Result<Vec<tntl_instance::mods::ModInfo>, String> {
    ensure_stopped(&state, &name)?;
    state_launcher(&state).remove_mod(&name, &file).map_err(message)
}

#[tauri::command]
fn import_candidates(state: State<'_, AppState>, path: String) -> Result<tntl_app::ImportScan, String> {
    state_launcher(&state)
        .import_candidates(&PathBuf::from(path))
        .map_err(message)
}

#[tauri::command]
async fn import_instance(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    path: String,
    name: Option<String>,
    version: Option<String>,
) -> Result<String, String> {
    let launcher = state_launcher(&state);
    let path = PathBuf::from(path);
    let job = "import".to_string();
    tauri::async_runtime::spawn_blocking(move || {
        let _cancel = tntl_source::cancel::begin();
        if path.join("mmc-pack.json").is_file() || path.join("instances").is_dir() {
            // Prism/MultiMC instances describe their loader, so the version and
            // the loader have to be installed rather than just registered.
            launcher
                .import_prism_instance(&path, name.as_deref(), &|event| {
                    emit_tagged(&app, "install-progress", &job, &event);
                })
                .map(|instance| instance.name)
        } else {
            launcher
                .import_instance(&path, name.as_deref(), version.as_deref(), &|event| {
                    emit_tagged(&app, "install-progress", &job, &event);
                })
                .map(|instance| instance.name)
        }
        .map_err(message)
    })
    .await
    .map_err(message)?
}

#[tauri::command]
async fn search_mods(
    state: State<'_, AppState>,
    instance: String,
    query: String,
    sort: String,
    limit: u32,
    offset: u32,
) -> Result<ModSearchPage, String> {
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || {
        launcher
            .search_mods(&instance, &query, &sort, limit, offset)
            .map_err(message)
    })
    .await
    .map_err(message)?
}

#[tauri::command]
async fn install_mods(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    instance: String,
    projects: Vec<String>,
) -> Result<Vec<InstalledMod>, String> {
    let _cancel = tntl_source::cancel::begin();
    ensure_stopped(&state, &instance)?;
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || {
        launcher
            .install_mods(&instance, &projects, None, &|event| {
                let _ = app.emit("mod-progress", event);
            })
            .map_err(message)
    })
    .await
    .map_err(message)?
}

#[tauri::command]
async fn install_missing_deps(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    instance: String,
) -> Result<tntl_app::MissingDepsReport, String> {
    let _cancel = tntl_source::cancel::begin();
    ensure_stopped(&state, &instance)?;
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || {
        launcher
            .install_missing_dependencies(&instance, &|event| {
                let _ = app.emit("mod-progress", event);
            })
            .map_err(message)
    })
    .await
    .map_err(message)?
}

#[tauri::command]
async fn fix_dependency_versions(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    instance: String,
) -> Result<tntl_app::DependencyFixReport, String> {
    let _cancel = tntl_source::cancel::begin();
    ensure_stopped(&state, &instance)?;
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || {
        launcher
            .fix_dependency_versions(&instance, &|event| {
                let _ = app.emit("mod-progress", event);
            })
            .map_err(message)
    })
    .await
    .map_err(message)?
}

#[tauri::command]
async fn list_content(
    state: State<'_, AppState>,
    name: String,
    kind: String,
) -> Result<tntl_app::ContentListing, String> {
    let kind = tntl_app::ProjectKind::from_key(&kind).ok_or_else(|| format!("unknown project kind {kind}"))?;
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || launcher.content(&name, kind).map_err(message))
        .await
        .map_err(message)?
}

#[tauri::command]
fn set_content_enabled(
    state: State<'_, AppState>,
    name: String,
    kind: String,
    file: String,
    enabled: bool,
) -> Result<tntl_app::ContentListing, String> {
    ensure_stopped(&state, &name)?;
    let kind = tntl_app::ProjectKind::from_key(&kind).ok_or_else(|| format!("unknown project kind {kind}"))?;
    state_launcher(&state)
        .set_content_enabled(&name, kind, &file, enabled)
        .map_err(message)
}

#[tauri::command]
fn move_resource_pack(
    state: State<'_, AppState>,
    name: String,
    file: String,
    up: bool,
) -> Result<tntl_app::ContentListing, String> {
    ensure_stopped(&state, &name)?;
    state_launcher(&state)
        .move_resource_pack(&name, &file, up)
        .map_err(message)
}

#[tauri::command]
fn remove_content(
    state: State<'_, AppState>,
    name: String,
    kind: String,
    file: String,
) -> Result<tntl_app::ContentListing, String> {
    ensure_stopped(&state, &name)?;
    let kind = tntl_app::ProjectKind::from_key(&kind).ok_or_else(|| format!("unknown project kind {kind}"))?;
    state_launcher(&state)
        .remove_content(&name, kind, &file)
        .map_err(message)
}

#[tauri::command]
async fn content_icon(
    state: State<'_, AppState>,
    name: String,
    kind: String,
    file: String,
) -> Result<tauri::ipc::Response, String> {
    let kind = tntl_app::ProjectKind::from_key(&kind).ok_or_else(|| format!("unknown project kind {kind}"))?;
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let bytes = launcher.content_icon(&name, kind, &file).map_err(message)?;
        Ok(tauri::ipc::Response::new(bytes))
    })
    .await
    .map_err(message)?
}

#[tauri::command]
async fn search_projects(
    state: State<'_, AppState>,
    instance: String,
    kind: String,
    query: String,
    sort: String,
    source: String,
    game_version: Option<String>,
    limit: u32,
    offset: u32,
) -> Result<ModSearchPage, String> {
    let launcher = state_launcher(&state);
    let kind = tntl_app::ProjectKind::from_key(&kind).ok_or_else(|| format!("unknown project kind {kind}"))?;
    let source = tntl_app::ContentSource::from_key(&source).ok_or_else(|| format!("unknown source {source}"))?;
    tauri::async_runtime::spawn_blocking(move || {
        launcher
            .search_projects(
                &instance,
                kind,
                &query,
                &sort,
                limit,
                offset,
                source,
                game_version.as_deref(),
            )
            .map_err(message)
    })
    .await
    .map_err(message)?
}

#[tauri::command]
async fn mod_game_versions(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || launcher.mod_game_versions().map_err(message))
        .await
        .map_err(message)?
}

#[tauri::command]
async fn install_projects(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    instance: String,
    kind: String,
    projects: Vec<String>,
    source: String,
    game_version: Option<String>,
) -> Result<Vec<InstalledMod>, String> {
    let _cancel = tntl_source::cancel::begin();
    ensure_stopped(&state, &instance)?;
    let launcher = state_launcher(&state);
    let kind = tntl_app::ProjectKind::from_key(&kind).ok_or_else(|| format!("unknown project kind {kind}"))?;
    let source = tntl_app::ContentSource::from_key(&source).ok_or_else(|| format!("unknown source {source}"))?;
    tauri::async_runtime::spawn_blocking(move || {
        launcher
            .install_projects(&instance, kind, &projects, source, game_version.as_deref(), &|event| {
                let _ = app.emit("mod-progress", event);
            })
            .map_err(message)
    })
    .await
    .map_err(message)?
}

#[tauri::command]
async fn project_versions(
    state: State<'_, AppState>,
    instance: String,
    kind: String,
    project: String,
) -> Result<Vec<tntl_modrinth::VersionChoice>, String> {
    let kind = tntl_app::ProjectKind::from_key(&kind).ok_or_else(|| format!("unknown project kind {kind}"))?;
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || launcher.project_versions(&instance, kind, &project).map_err(message))
        .await
        .map_err(message)?
}

#[tauri::command]
async fn install_project_version(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    instance: String,
    kind: String,
    project: String,
    version_id: String,
    source: String,
) -> Result<Vec<InstalledMod>, String> {
    let _cancel = tntl_source::cancel::begin();
    ensure_stopped(&state, &instance)?;
    let kind = tntl_app::ProjectKind::from_key(&kind).ok_or_else(|| format!("unknown project kind {kind}"))?;
    let source = tntl_app::ContentSource::from_key(&source).ok_or_else(|| format!("unknown source {source}"))?;
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || {
        launcher
            .install_project_version(&instance, kind, &project, &version_id, source, &|event| {
                let _ = app.emit("mod-progress", event);
            })
            .map_err(message)
    })
    .await
    .map_err(message)?
}

#[tauri::command]
async fn check_dependency_updates(
    state: State<'_, AppState>,
    instance: String,
) -> Result<Vec<tntl_app::ModUpdate>, String> {
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || launcher.check_dependency_updates(&instance).map_err(message))
        .await
        .map_err(message)?
}

/// Bands the memory slider draws: green = sane, yellow = squeezes the OS,
/// red = will not run a modern build or would starve the machine.
#[tauri::command]
async fn memory_advice() -> Result<tntl_app::MemoryAdvice, String> {
    Ok(tntl_app::advice_from_total(total_memory_mb()))
}

#[cfg(target_os = "linux")]
fn total_memory_mb() -> Option<u64> {
    tntl_app::system_memory_mb()
}

#[cfg(target_os = "windows")]
fn total_memory_mb() -> Option<u64> {
    use sysinfo::System;
    let mut system = System::new();
    system.refresh_memory();
    Some(system.total_memory() / (1024 * 1024))
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn total_memory_mb() -> Option<u64> {
    None
}

#[tauri::command]
async fn check_updates(
    state: State<'_, AppState>,
    instance: String,
    kind: String,
) -> Result<Vec<tntl_app::ModUpdate>, String> {
    let launcher = state_launcher(&state);
    let kind = tntl_app::ProjectKind::from_key(&kind).ok_or_else(|| format!("unknown project kind {kind}"))?;
    tauri::async_runtime::spawn_blocking(move || launcher.check_updates(&instance, kind).map_err(message))
        .await
        .map_err(message)?
}

#[tauri::command]
async fn update_content(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    instance: String,
    kind: String,
    files: Vec<String>,
) -> Result<Vec<InstalledMod>, String> {
    let _cancel = tntl_source::cancel::begin();
    ensure_stopped(&state, &instance)?;
    let launcher = state_launcher(&state);
    let kind = tntl_app::ProjectKind::from_key(&kind).ok_or_else(|| format!("unknown project kind {kind}"))?;
    tauri::async_runtime::spawn_blocking(move || {
        launcher
            .update_content(&instance, kind, &files, &|event| {
                let _ = app.emit("mod-progress", event);
            })
            .map_err(message)
    })
    .await
    .map_err(message)?
}

#[tauri::command]
async fn check_pack_update(state: State<'_, AppState>, instance: String) -> Result<tntl_app::PackUpdate, String> {
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || launcher.check_pack_update(&instance).map_err(message))
        .await
        .map_err(message)?
}

#[tauri::command]
async fn update_pack(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    instance: String,
    version_id: String,
) -> Result<tntl_app::PackUpdateReport, String> {
    let _cancel = tntl_source::cancel::begin();
    ensure_stopped(&state, &instance)?;
    let launcher = state_launcher(&state);
    let job = format!("update:{instance}");
    tauri::async_runtime::spawn_blocking(move || {
        launcher
            .update_pack(
                &instance,
                &version_id,
                &|event| {
                    emit_tagged(&app, "install-progress", &job, &event);
                },
                &|event| {
                    emit_tagged(&app, "pack-progress", &job, &event);
                },
            )
            .map_err(message)
    })
    .await
    .map_err(message)?
}

#[tauri::command]
async fn launch_instance(app: tauri::AppHandle, state: State<'_, AppState>, name: String) -> Result<u32, String> {
    {
        let running = state.game.lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some(game) = running.as_ref() {
            return Err(format!("instance {} is already running", game.name));
        }
    }
    if state.launching.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return Err("another instance is already starting".to_string());
    }
    let launcher = state_launcher(&state);
    let prepared = {
        let app = app.clone();
        let name = name.clone();
        let launcher = launcher.clone();
        tauri::async_runtime::spawn_blocking(move || -> Result<_, String> {
            // The in-process slot is empty after the launcher restarts, but the
            // game may still be running; the marker catches that case.
            launcher.ensure_not_running(&name).map_err(message)?;
            let instance = launcher.load_instance(&name).map_err(message)?;
            launcher.prepare_skin(&name).map_err(message)?;
            let plan = launcher
                .plan_instance(&instance, None, VerifyMode::Exists)
                .map_err(message)?;
            if !plan.missing.is_empty() {
                let java_missing = plan.missing.iter().any(|missing| missing.kind == MissingKind::Java);
                if java_missing {
                    let layout = instance.layout(&launcher.root);
                    let required = launcher
                        .load_chain(&layout, &instance.version_id)
                        .map(|chain| launcher.required_java(&chain))
                        .unwrap_or(8);
                    return Err(format!(
                        "no usable Java runtime found; this instance needs Java {required}"
                    ));
                }
                return Err(format!(
                    "instance {name} is incomplete; install {} again",
                    instance.version_id
                ));
            }
            let log_path = launcher
                .instance_dir(&name)
                .map_err(message)?
                .join("logs")
                .join(format!("{}.log", timestamp()));
            let settings = launcher.settings();
            let mut options = LaunchOptions::new(&log_path);
            if settings.quit_on_launch {
                // Nothing reads the pipes once this process is gone, so hand
                // the log file straight to the game.
                options.detach_output = true;
            } else {
                let sink_app = app.clone();
                options.line_sink = Some(std::sync::Arc::new(move |line: &str| {
                    let _ = sink_app.emit("game-line", line.to_string());
                }));
            }
            let process = start_game(&plan, &options).map_err(message)?;
            launcher.mark_running(&name, process.id()).map_err(message)?;
            let game_dir = launcher
                .instance_dir(&name)
                .map(|dir| instance.game_dir(&dir))
                .unwrap_or_default();
            Ok((process, settings.quit_on_launch, game_dir))
        })
        .await
        .map_err(message)
    };
    state.launching.store(false, std::sync::atomic::Ordering::SeqCst);
    let (process, quit_on_launch, game_dir) = prepared??;
    let pid = process.id();
    let handle = process.handle();
    {
        let mut running = state.game.lock().unwrap_or_else(|poison| poison.into_inner());
        *running = Some(RunningGame {
            name: name.clone(),
            game_dir,
            handle,
        });
    }

    if quit_on_launch {
        app.exit(0);
        return Ok(pid);
    }

    let exit_app = app.clone();
    std::thread::spawn(move || {
        let exit = match process.wait() {
            Ok(outcome) => GameExit {
                instance: name.clone(),
                exit_code: outcome.exit_code,
                duration_ms: outcome.duration_ms,
                crash: outcome.crash.map(|crash| CrashInfo {
                    kind: format!("{:?}", crash.kind),
                    summary: crash.summary,
                    hints: crash.hints,
                }),
            },
            Err(error) => GameExit {
                instance: name.clone(),
                exit_code: None,
                duration_ms: 0,
                crash: Some(CrashInfo {
                    kind: "LaunchFailure".to_string(),
                    summary: format!("{error:#}"),
                    hints: Vec::new(),
                }),
            },
        };
        if let Some(state) = exit_app.try_state::<AppState>() {
            let launcher = state_launcher(&state);
            launcher.clear_running(&name);
            let _ = launcher.record_playtime(&name, exit.duration_ms);
            let mut running = state.game.lock().unwrap_or_else(|poison| poison.into_inner());
            if running.as_ref().is_some_and(|game| game.name == name) {
                *running = None;
            }
        }
        let _ = exit_app.emit("game-exit", exit.clone());
    });
    Ok(pid)
}

fn start_game(plan: &tntl_resolve::LaunchPlan, options: &LaunchOptions) -> anyhow::Result<GameProcess> {
    Ok(tntl_launch::launch(plan, options)?)
}

#[tauri::command]
fn stop_game(state: State<'_, AppState>) -> Result<bool, String> {
    let running = state.game.lock().unwrap_or_else(|poison| poison.into_inner());
    match running.as_ref() {
        Some(game) => {
            game.handle.kill();
            Ok(true)
        }
        None => Ok(false),
    }
}

#[tauri::command]
fn running_instance(state: State<'_, AppState>) -> Option<String> {
    let running = state.game.lock().unwrap_or_else(|poison| poison.into_inner());
    running.as_ref().map(|game| game.name.clone())
}

#[tauri::command]
fn read_instance_log(state: State<'_, AppState>, name: String) -> Result<String, String> {
    let launcher = state_launcher(&state);
    let dir = launcher.instance_dir(&name).map_err(message)?;
    match tntl_app::newest_log(&dir) {
        Some(path) => Ok(tntl_launch::read_tail(&path, 128 * 1024)),
        None => Ok(String::new()),
    }
}

#[tauri::command]
async fn list_worlds(
    state: State<'_, AppState>,
    name: String,
) -> Result<Vec<tntl_instance::worlds::WorldInfo>, String> {
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || launcher.worlds(&name).map_err(message))
        .await
        .map_err(message)?
}

#[tauri::command]
async fn duplicate_world(
    state: State<'_, AppState>,
    name: String,
    world: String,
) -> Result<tntl_instance::worlds::WorldInfo, String> {
    ensure_stopped(&state, &name)?;
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || launcher.duplicate_world(&name, &world).map_err(message))
        .await
        .map_err(message)?
}

#[tauri::command]
async fn list_world_backups(state: State<'_, AppState>, name: String) -> Result<Vec<tntl_app::WorldBackup>, String> {
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || launcher.world_backups(&name).map_err(message))
        .await
        .map_err(message)?
}

#[tauri::command]
async fn backup_world(state: State<'_, AppState>, name: String, world: String) -> Result<String, String> {
    ensure_stopped(&state, &name)?;
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || {
        launcher
            .backup_world(&name, &world)
            .map(|path| path.display().to_string())
            .map_err(message)
    })
    .await
    .map_err(message)?
}

#[tauri::command]
async fn delete_world(state: State<'_, AppState>, name: String, world: String) -> Result<(), String> {
    ensure_stopped(&state, &name)?;
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || launcher.delete_world(&name, &world).map_err(message))
        .await
        .map_err(message)?
}

#[tauri::command]
async fn delete_world_backup(
    state: State<'_, AppState>,
    name: String,
    file: String,
) -> Result<Vec<tntl_app::WorldBackup>, String> {
    ensure_stopped(&state, &name)?;
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || launcher.delete_world_backup(&name, &file).map_err(message))
        .await
        .map_err(message)?
}

#[tauri::command]
async fn import_world(
    state: State<'_, AppState>,
    name: String,
    path: String,
) -> Result<tntl_instance::worlds::WorldInfo, String> {
    ensure_stopped(&state, &name)?;
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || {
        launcher
            .import_world(&name, std::path::Path::new(&path))
            .map_err(message)
    })
    .await
    .map_err(message)?
}

#[tauri::command]
async fn add_local_mod(
    state: State<'_, AppState>,
    name: String,
    path: String,
) -> Result<Vec<tntl_instance::mods::ModInfo>, String> {
    ensure_stopped(&state, &name)?;
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || {
        launcher
            .add_local_mod(&name, std::path::Path::new(&path))
            .map_err(message)
    })
    .await
    .map_err(message)?
}

#[tauri::command]
async fn add_local_content(
    state: State<'_, AppState>,
    name: String,
    kind: String,
    path: String,
) -> Result<tntl_app::ContentListing, String> {
    ensure_stopped(&state, &name)?;
    let kind = tntl_app::ProjectKind::from_key(&kind).ok_or_else(|| format!("unknown content kind {kind}"))?;
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || {
        launcher
            .add_local_content(&name, kind, std::path::Path::new(&path))
            .map_err(message)
    })
    .await
    .map_err(message)?
}

#[tauri::command]
async fn duplicate_instance(state: State<'_, AppState>, name: String, new_name: String) -> Result<String, String> {
    ensure_stopped(&state, &name)?;
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || {
        launcher
            .duplicate_instance(&name, &new_name)
            .map(|path| path.display().to_string())
            .map_err(message)
    })
    .await
    .map_err(message)?
}

#[tauri::command]
async fn export_instance(
    state: State<'_, AppState>,
    name: String,
    path: String,
    pack_name: Option<String>,
    pack_version: String,
) -> Result<tntl_app::ExportReport, String> {
    ensure_stopped(&state, &name)?;
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || {
        launcher
            .export_instance(&name, std::path::Path::new(&path), pack_name.as_deref(), &pack_version)
            .map_err(message)
    })
    .await
    .map_err(message)?
}

#[tauri::command]
fn launcher_settings(state: State<'_, AppState>) -> tntl_app::LauncherSettings {
    state_launcher(&state).settings()
}

#[tauri::command]
fn save_launcher_settings(
    state: State<'_, AppState>,
    settings: tntl_app::LauncherSettings,
) -> Result<tntl_app::LauncherSettings, String> {
    state_launcher(&state).save_settings(&settings).map_err(message)
}

#[tauri::command]
async fn set_instance_icon(state: State<'_, AppState>, name: String, path: String) -> Result<String, String> {
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || {
        launcher
            .set_instance_icon(&name, std::path::Path::new(&path))
            .map_err(message)
    })
    .await
    .map_err(message)?
}

#[tauri::command]
fn clear_instance_icon(state: State<'_, AppState>, name: String) -> Result<(), String> {
    state_launcher(&state).clear_instance_icon(&name).map_err(message)
}

/// Datapacks of one world, enabled ones first.
#[tauri::command]
async fn list_datapacks(
    state: State<'_, AppState>,
    name: String,
    world: String,
) -> Result<Vec<tntl_app::DatapackInfo>, String> {
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || launcher.datapacks(&name, &world).map_err(message))
        .await
        .map_err(message)?
}

#[tauri::command]
fn set_datapack_enabled(
    state: State<'_, AppState>,
    name: String,
    world: String,
    file: String,
    enabled: bool,
) -> Result<Vec<tntl_app::DatapackInfo>, String> {
    ensure_stopped(&state, &name)?;
    state_launcher(&state)
        .set_datapack_enabled(&name, &world, &file, enabled)
        .map_err(message)
}

#[tauri::command]
async fn add_datapack(
    state: State<'_, AppState>,
    name: String,
    world: String,
    path: String,
) -> Result<Vec<tntl_app::DatapackInfo>, String> {
    ensure_stopped(&state, &name)?;
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || {
        launcher
            .add_datapack(&name, &world, std::path::Path::new(&path))
            .map_err(message)
    })
    .await
    .map_err(message)?
}

#[tauri::command]
async fn delete_datapack(
    state: State<'_, AppState>,
    name: String,
    world: String,
    file: String,
) -> Result<Vec<tntl_app::DatapackInfo>, String> {
    ensure_stopped(&state, &name)?;
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || launcher.delete_datapack(&name, &world, &file).map_err(message))
        .await
        .map_err(message)?
}

/// Background image bytes; empty when none is configured.
#[tauri::command]
async fn background_image(state: State<'_, AppState>) -> Result<tauri::ipc::Response, String> {
    const MAX_BACKGROUND: u64 = 24 * 1024 * 1024;
    let settings = state_launcher(&state).settings();
    let Some(path) = settings.background else {
        return Ok(tauri::ipc::Response::new(Vec::new()));
    };
    tauri::async_runtime::spawn_blocking(move || {
        let path = PathBuf::from(path);
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase())
            .unwrap_or_default();
        if !matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif") {
            return Err(format!("{} is not a PNG, JPEG, WebP or GIF image", path.display()));
        }
        let size = std::fs::metadata(&path).map_err(message)?.len();
        if size > MAX_BACKGROUND {
            return Err(format!("{} is larger than 24 MiB", path.display()));
        }
        let bytes = std::fs::read(&path).map_err(message)?;
        Ok(tauri::ipc::Response::new(bytes))
    })
    .await
    .map_err(message)?
}

/// Copy the newest log of an instance to `path`.
#[tauri::command]
fn export_log(state: State<'_, AppState>, name: String, path: String) -> Result<(), String> {
    let launcher = state_launcher(&state);
    let dir = launcher.instance_dir(&name).map_err(message)?;
    let log = tntl_app::newest_log(&dir).ok_or_else(|| format!("{name} has no log yet"))?;
    std::fs::copy(&log, &path).map_err(message)?;
    Ok(())
}

/// Icon bytes; empty when the instance has no custom icon.
#[tauri::command]
async fn instance_icon(state: State<'_, AppState>, name: String) -> Result<tauri::ipc::Response, String> {
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let bytes = launcher.instance_icon(&name).map_err(message)?.unwrap_or_default();
        Ok(tauri::ipc::Response::new(bytes))
    })
    .await
    .map_err(message)?
}

#[tauri::command]
async fn export_archive(
    state: State<'_, AppState>,
    name: String,
    path: String,
) -> Result<tntl_instance::archive::ArchiveReport, String> {
    ensure_stopped(&state, &name)?;
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || {
        launcher
            .export_archive(&name, std::path::Path::new(&path))
            .map_err(message)
    })
    .await
    .map_err(message)?
}

#[tauri::command]
async fn import_archive(state: State<'_, AppState>, path: String, name: String) -> Result<Instance, String> {
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || {
        launcher
            .import_archive(std::path::Path::new(&path), &name)
            .map_err(message)
    })
    .await
    .map_err(message)?
}

#[tauri::command]
async fn list_crash_reports(
    state: State<'_, AppState>,
    name: String,
) -> Result<Vec<tntl_instance::crashes::CrashReport>, String> {
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || launcher.crash_reports(&name).map_err(message))
        .await
        .map_err(message)?
}

#[tauri::command]
async fn read_crash_report(state: State<'_, AppState>, name: String, file: String) -> Result<String, String> {
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || launcher.read_crash_report(&name, &file).map_err(message))
        .await
        .map_err(message)?
}

#[tauri::command]
fn delete_crash_report(state: State<'_, AppState>, name: String, file: String) -> Result<(), String> {
    state_launcher(&state)
        .delete_crash_report(&name, &file)
        .map_err(message)
}

#[tauri::command]
async fn list_screenshots(
    state: State<'_, AppState>,
    name: String,
) -> Result<Vec<tntl_instance::screenshots::Screenshot>, String> {
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || launcher.screenshots(&name).map_err(message))
        .await
        .map_err(message)?
}

#[tauri::command]
async fn read_screenshot(
    state: State<'_, AppState>,
    name: String,
    file: String,
) -> Result<tauri::ipc::Response, String> {
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let bytes = launcher.read_screenshot(&name, &file).map_err(message)?;
        Ok(tauri::ipc::Response::new(bytes))
    })
    .await
    .map_err(message)?
}

#[tauri::command]
fn delete_screenshot(state: State<'_, AppState>, name: String, file: String) -> Result<(), String> {
    state_launcher(&state).delete_screenshot(&name, &file).map_err(message)
}

#[tauri::command]
fn list_servers(state: State<'_, AppState>, name: String) -> Result<Vec<tntl_instance::servers::ServerEntry>, String> {
    state_launcher(&state).servers(&name).map_err(message)
}

#[tauri::command]
fn save_servers(
    state: State<'_, AppState>,
    name: String,
    servers: Vec<tntl_instance::servers::ServerEntry>,
) -> Result<Vec<tntl_instance::servers::ServerEntry>, String> {
    ensure_stopped(&state, &name)?;
    state_launcher(&state).save_servers(&name, &servers).map_err(message)
}

#[tauri::command]
async fn disk_usage(state: State<'_, AppState>, name: String) -> Result<tntl_app::DiskUsage, String> {
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || launcher.disk_usage(&name).map_err(message))
        .await
        .map_err(message)?
}

#[tauri::command]
async fn cleanup_instance(state: State<'_, AppState>, name: String) -> Result<tntl_app::CleanupReport, String> {
    ensure_stopped(&state, &name)?;
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || launcher.cleanup(&name).map_err(message))
        .await
        .map_err(message)?
}

#[tauri::command]
fn list_accounts(state: State<'_, AppState>) -> Result<tntl_app::AccountsResponse, String> {
    state_launcher(&state).accounts().map_err(message)
}

#[tauri::command]
fn add_offline_account(state: State<'_, AppState>, name: String) -> Result<tntl_app::AccountView, String> {
    state_launcher(&state).add_offline_account(&name).map_err(message)
}

#[tauri::command]
fn remove_account(state: State<'_, AppState>, id: String) -> Result<(), String> {
    state_launcher(&state).remove_account(&id).map_err(message)
}

#[tauri::command]
fn rename_offline_account(
    state: State<'_, AppState>,
    id: String,
    name: String,
) -> Result<tntl_app::AccountView, String> {
    state_launcher(&state)
        .rename_offline_account(&id, &name)
        .map_err(message)
}

#[tauri::command]
fn set_current_account(state: State<'_, AppState>, id: String) -> Result<tntl_app::AccountView, String> {
    state_launcher(&state).set_current_account(&id).map_err(message)
}

#[tauri::command]
fn update_account(
    state: State<'_, AppState>,
    id: String,
    skin_model: Option<String>,
    apply_skin: Option<bool>,
) -> Result<tntl_app::AccountView, String> {
    state_launcher(&state)
        .update_account(&id, skin_model.as_deref(), apply_skin)
        .map_err(message)
}

#[tauri::command]
async fn set_account_skin(
    state: State<'_, AppState>,
    id: String,
    path: String,
    model: Option<String>,
) -> Result<tntl_app::AccountView, String> {
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let bytes = std::fs::read(&path).map_err(|error| format!("cannot read {path}: {error}"))?;
        launcher
            .set_account_skin(&id, &bytes, model.as_deref())
            .map_err(message)
    })
    .await
    .map_err(message)?
}

#[tauri::command]
async fn clear_account_skin(state: State<'_, AppState>, id: String) -> Result<tntl_app::AccountView, String> {
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || launcher.clear_account_skin(&id).map_err(message))
        .await
        .map_err(message)?
}

#[tauri::command]
fn account_skin(state: State<'_, AppState>, id: String) -> Result<tauri::ipc::Response, String> {
    let bytes = state_launcher(&state)
        .account_skin(&id)
        .map_err(message)?
        .unwrap_or_default();
    Ok(tauri::ipc::Response::new(bytes))
}

#[tauri::command]
fn account_cape(state: State<'_, AppState>, id: String) -> Result<tauri::ipc::Response, String> {
    let bytes = state_launcher(&state)
        .account_cape(&id)
        .map_err(message)?
        .unwrap_or_default();
    Ok(tauri::ipc::Response::new(bytes))
}

#[tauri::command]
fn default_skins(state: State<'_, AppState>) -> Vec<&'static str> {
    state_launcher(&state).default_skins()
}

#[tauri::command]
fn default_skin_image(state: State<'_, AppState>, key: String) -> Result<tauri::ipc::Response, String> {
    let bytes = state_launcher(&state).default_skin_image(&key).map_err(message)?;
    Ok(tauri::ipc::Response::new(bytes))
}

#[tauri::command]
async fn set_default_skin(
    state: State<'_, AppState>,
    id: String,
    key: String,
) -> Result<tntl_app::AccountView, String> {
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || launcher.set_default_skin(&id, &key).map_err(message))
        .await
        .map_err(message)?
}

#[tauri::command]
async fn preview_online_skin(
    state: State<'_, AppState>,
    nickname: String,
    source: Option<String>,
) -> Result<tntl_app::OnlineSkinPreview, String> {
    let launcher = state_launcher(&state);
    let source = source.unwrap_or_else(|| "auto".to_string());
    tauri::async_runtime::spawn_blocking(move || launcher.preview_online_skin(&nickname, &source).map_err(message))
        .await
        .map_err(message)?
}

#[tauri::command]
async fn apply_online_skin(
    state: State<'_, AppState>,
    id: String,
    nickname: String,
    source: Option<String>,
) -> Result<tntl_app::AccountView, String> {
    let launcher = state_launcher(&state);
    let source = source.unwrap_or_else(|| "auto".to_string());
    tauri::async_runtime::spawn_blocking(move || launcher.apply_online_skin(&id, &nickname, &source).map_err(message))
        .await
        .map_err(message)?
}

#[tauri::command]
async fn apply_gallery_skin(
    state: State<'_, AppState>,
    id: String,
    skin: Vec<u8>,
    slim: bool,
) -> Result<tntl_app::AccountView, String> {
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || launcher.apply_gallery_skin(&id, &skin, slim).map_err(message))
        .await
        .map_err(message)?
}

#[tauri::command]
async fn skin_gallery(
    state: State<'_, AppState>,
    filter: Option<String>,
    after: Option<String>,
    count: Option<usize>,
) -> Result<tntl_app::GalleryPage, String> {
    let launcher = state_launcher(&state);
    let category = filter.unwrap_or_else(|| "latest".to_string());
    tauri::async_runtime::spawn_blocking(move || {
        launcher
            .skin_gallery(&category, after.as_deref(), count.unwrap_or(16))
            .map_err(message)
    })
    .await
    .map_err(message)?
}

#[tauri::command]
async fn set_account_cape(
    state: State<'_, AppState>,
    id: String,
    cape: Option<String>,
) -> Result<tntl_app::AccountView, String> {
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || launcher.set_account_cape(&id, cape.as_deref()).map_err(message))
        .await
        .map_err(message)?
}

#[tauri::command]
async fn refresh_account(state: State<'_, AppState>, id: String) -> Result<tntl_app::AccountView, String> {
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || launcher.refresh_account(&id).map_err(message))
        .await
        .map_err(message)?
}

#[tauri::command]
async fn start_msa_login(state: State<'_, AppState>) -> Result<tntl_app::DeviceCode, String> {
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || launcher.start_msa_login().map_err(message))
        .await
        .map_err(message)?
}

#[tauri::command]
async fn poll_msa_login(state: State<'_, AppState>, device_code: String) -> Result<tntl_app::MsaPoll, String> {
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || launcher.poll_msa_login(&device_code).map_err(message))
        .await
        .map_err(message)?
}

#[tauri::command]
async fn search_modpacks(
    state: State<'_, AppState>,
    query: String,
    sort: String,
    limit: u32,
    offset: u32,
) -> Result<tntl_app::PackPage, String> {
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || {
        launcher.search_modpacks(&query, &sort, limit, offset).map_err(message)
    })
    .await
    .map_err(message)?
}

#[tauri::command]
async fn install_modpack_project(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    project: String,
    instance_name: Option<String>,
) -> Result<tntl_app::ModpackReport, String> {
    let launcher = state_launcher(&state);
    let job = "modpack".to_string();
    tauri::async_runtime::spawn_blocking(move || {
        let _cancel = tntl_source::cancel::begin();
        launcher
            .install_modpack_project(
                &project,
                instance_name.as_deref(),
                &|event| {
                    emit_tagged(&app, "install-progress", &job, &event);
                },
                &|event| {
                    emit_tagged(&app, "pack-progress", &job, &event);
                },
            )
            .map_err(message)
    })
    .await
    .map_err(message)?
}

#[tauri::command]
async fn search_curseforge_packs(
    state: State<'_, AppState>,
    query: String,
    sort: String,
    limit: u32,
    offset: u32,
) -> Result<tntl_app::CursePackPage, String> {
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || {
        launcher
            .search_curseforge_packs(&query, &sort, limit, offset)
            .map_err(message)
    })
    .await
    .map_err(message)?
}

#[tauri::command]
async fn install_curseforge_pack(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    project_id: u64,
    file_id: Option<u64>,
    instance_name: Option<String>,
) -> Result<tntl_app::ModpackReport, String> {
    let launcher = state_launcher(&state);
    let job = "modpack".to_string();
    tauri::async_runtime::spawn_blocking(move || {
        let _cancel = tntl_source::cancel::begin();
        launcher
            .install_curseforge_pack(
                project_id,
                file_id,
                instance_name.as_deref(),
                &|event| {
                    emit_tagged(&app, "install-progress", &job, &event);
                },
                &|event| {
                    emit_tagged(&app, "pack-progress", &job, &event);
                },
            )
            .map_err(message)
    })
    .await
    .map_err(message)?
}

#[tauri::command]
async fn search_ftb_packs(
    state: State<'_, AppState>,
    query: String,
    limit: u32,
) -> Result<Vec<tntl_app::FtbPackView>, String> {
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || launcher.search_ftb_packs(&query, limit).map_err(message))
        .await
        .map_err(message)?
}

#[tauri::command]
async fn ftb_pack(state: State<'_, AppState>, id: u64) -> Result<tntl_app::FtbPackView, String> {
    let launcher = state_launcher(&state);
    tauri::async_runtime::spawn_blocking(move || launcher.ftb_pack(id).map_err(message))
        .await
        .map_err(message)?
}

#[tauri::command]
async fn install_ftb_pack(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    pack_id: u64,
    version_id: Option<u64>,
    instance_name: Option<String>,
) -> Result<tntl_app::ModpackReport, String> {
    let launcher = state_launcher(&state);
    let job = "modpack".to_string();
    tauri::async_runtime::spawn_blocking(move || {
        let _cancel = tntl_source::cancel::begin();
        launcher
            .install_ftb_pack(
                pack_id,
                version_id,
                instance_name.as_deref(),
                &|event| {
                    emit_tagged(&app, "install-progress", &job, &event);
                },
                &|event| {
                    emit_tagged(&app, "pack-progress", &job, &event);
                },
            )
            .map_err(message)
    })
    .await
    .map_err(message)?
}

#[tauri::command]
async fn server_status(host: String, port: u16) -> Result<tntl_status::ServerStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        tntl_status::ping(&host, port, std::time::Duration::from_millis(4000)).map_err(message)
    })
    .await
    .map_err(message)?
}

/// Native save dialog. Returns the chosen path.
#[tauri::command]
async fn pick_save_file(title: String, file_name: String, extensions: Vec<String>) -> Option<String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut dialog = rfd::FileDialog::new().set_title(title).set_file_name(file_name);
        if !extensions.is_empty() {
            let filters: Vec<&str> = extensions.iter().map(String::as_str).collect();
            dialog = dialog.add_filter("Supported files", &filters);
        }
        dialog.save_file().map(|path| path.display().to_string())
    })
    .await
    .ok()
    .flatten()
}

/// Native file picker. `extensions` are bare suffixes such as `jar` or `zip`.
#[tauri::command]
async fn pick_file(title: String, extensions: Vec<String>) -> Option<String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut dialog = rfd::FileDialog::new().set_title(title);
        if !extensions.is_empty() {
            let filters: Vec<&str> = extensions.iter().map(String::as_str).collect();
            dialog = dialog.add_filter("Supported files", &filters);
        }
        dialog.pick_file().map(|path| path.display().to_string())
    })
    .await
    .ok()
    .flatten()
}

#[tauri::command]
async fn pick_folder(title: String) -> Option<String> {
    tauri::async_runtime::spawn_blocking(move || {
        rfd::FileDialog::new()
            .set_title(title)
            .pick_folder()
            .map(|path| path.display().to_string())
    })
    .await
    .ok()
    .flatten()
}

#[tauri::command]
fn is_directory(path: String) -> bool {
    PathBuf::from(path).is_dir()
}

#[tauri::command]
fn open_path(path: String) -> Result<(), String> {
    let path = PathBuf::from(path);
    if !path.exists() {
        return Err(format!("{} does not exist", path.display()));
    }
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("explorer");
        command.arg(&path);
        command
    };
    #[cfg(target_os = "linux")]
    let mut command = {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(&path);
        command
    };
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("cannot open {}: {error}", path.display()))
}

#[tauri::command]
fn open_url(url: String) -> Result<(), String> {
    let url = url.trim();
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return Err("only http(s) links can be opened".to_string());
    }
    #[cfg(target_os = "windows")]
    let mut command = {
        use std::os::windows::process::CommandExt;
        let mut command = std::process::Command::new("cmd");
        // `cmd /C start` re-parses its arguments; a bare URL containing `&`
        // would split the command line and run the tail. Quote it as a single
        // raw argument.
        command
            .arg("/C")
            .arg("start")
            .arg("")
            .raw_arg(format!("\"{}\"", url.replace('"', "%22")));
        command.creation_flags(0x0800_0000);
        command
    };
    #[cfg(target_os = "linux")]
    let mut command = {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(url);
        command
    };
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("cannot open {url}: {error}"))
}

fn message(error: impl std::fmt::Display) -> String {
    format!("{error:#}")
}

/// The frontend calls this once its views are mounted: the last startup stage
/// in `launcher.log`. A window that opens but never logs this stays blank.
#[tauri::command]
fn frontend_ready() {
    append_log("frontend", "the interface finished mounting");
}

/// Milliseconds: two launches within the same second used to share a log file.
fn timestamp() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

/// WebKitGTK renders through GBM buffers by default. On the NVIDIA proprietary
/// driver that allocation fails ("Failed to create GBM buffer of size ..."),
/// which leaves the whole webview blank, so fall back to the shared memory path.
/// An explicit environment value always wins.
#[cfg(target_os = "linux")]
fn configure_webview() {
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }
}

#[derive(Default, Serialize, serde::Deserialize)]
struct RootConfig {
    #[serde(default)]
    roots: Vec<String>,
    #[serde(default)]
    current: Option<String>,
}

fn root_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("tntl").join("roots.json"))
}

fn load_root_config() -> RootConfig {
    let Some(path) = root_config_path() else {
        return RootConfig::default();
    };
    let Ok(bytes) = std::fs::read(path) else {
        return RootConfig::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

fn save_root_config(config: &RootConfig) {
    let Some(path) = root_config_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(body) = serde_json::to_vec_pretty(config) {
        let _ = std::fs::write(path, body);
    }
}

fn initial_root() -> PathBuf {
    if let Some(explicit) = std::env::var_os("TNTL_ROOT")
        .map(PathBuf::from)
        .or_else(|| std::env::args().nth(1).map(PathBuf::from))
    {
        return explicit;
    }
    let config = load_root_config();
    if let Some(current) = config.current.filter(|path| PathBuf::from(path).is_dir()) {
        return PathBuf::from(current);
    }
    Launcher::from_root(None).root
}

/// A GUI-subsystem binary dies without a word when startup fails or a thread
/// panics, which is impossible to report from the user's side. Append to a log
/// file and show the reason in a dialog instead. Startup stages go here too, so
/// a run that dies before the window opens shows how far it got.
fn append_log(kind: &str, detail: &str) -> Option<PathBuf> {
    let path = dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("tntl")
        .join("logs")
        .join("launcher.log");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Every start appends, so keep the file from growing forever.
    if std::fs::metadata(&path).is_ok_and(|meta| meta.len() > 256 * 1024) {
        let _ = std::fs::remove_file(&path);
    }
    let entry = format!("[{}] {kind}\n{detail}\n\n", timestamp());
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        use std::io::Write;
        let _ = file.write_all(entry.as_bytes());
    }
    Some(path)
}

/// Details the launcher cannot ask the user for. A start that dies before the
/// window opens is almost always a missing WebView2 runtime or a data folder
/// the current account cannot write, so record both.
fn startup_notes() -> String {
    let mut notes = vec![format!(
        "version {} on {} {}",
        env!("CARGO_PKG_VERSION_MAJOR"),
        std::env::consts::OS,
        std::env::consts::ARCH
    )];
    match dirs::data_local_dir() {
        Some(local) => {
            let webview = local.join("io.github.busybin.tntl");
            notes.push(format!(
                "webview data {}: exists={} writable={}",
                webview.display(),
                webview.is_dir(),
                writable(&webview)
            ));
            notes.push(webview_runtime(&local));
        }
        None => notes.push("no local app data directory".to_string()),
    }
    if let Some(data) = dirs::data_dir() {
        let launcher = data.join("tntl");
        notes.push(format!(
            "launcher data {} writable={}",
            launcher.display(),
            writable(&launcher)
        ));
    }
    notes.push(format!("elevated={}", elevated()));
    notes.join("\n")
}

fn writable(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join("startup-probe");
    let written = std::fs::write(&probe, b"ok").is_ok();
    let _ = std::fs::remove_file(probe);
    written
}

/// Whether the token carries the high mandatory level, i.e. whether this is the
/// "run as administrator" launch the user compared against.
fn elevated() -> bool {
    #[cfg(not(target_os = "windows"))]
    {
        false
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("whoami")
            .arg("/groups")
            .creation_flags(0x0800_0000)
            .output()
            .map(|out| String::from_utf8_lossy(&out.stdout).contains("S-1-16-12288"))
            .unwrap_or(false)
    }
}

fn webview_runtime(local: &Path) -> String {
    let roots = [
        PathBuf::from(r"C:\Program Files (x86)\Microsoft\EdgeWebView\Application"),
        local.join(r"Microsoft\EdgeWebView\Application"),
    ];
    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        // The runtime keeps its builds in version folders next to `SetupMetrics`.
        let mut versions: Vec<String> = entries
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.starts_with(|first: char| first.is_ascii_digit()))
            .collect();
        versions.sort_by_key(|name| {
            let mut parts = [0u64; 4];
            for (slot, part) in parts.iter_mut().zip(name.split('.')) {
                *slot = part.parse().unwrap_or(0);
            }
            parts
        });
        if let Some(latest) = versions.last() {
            return format!("webview2 runtime {latest} at {}", root.display());
        }
    }
    "webview2 runtime not found".to_string()
}

fn report_failure(title: &str, detail: &str) {
    let path = append_log(title, detail);
    let where_to = path
        .map(|path| format!("\n\nA log was written to {}", path.display()))
        .unwrap_or_default();
    let _ = rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title(format!("TNTL {}", env!("CARGO_PKG_VERSION_MAJOR")))
        .set_description(format!("{title}\n\n{detail}{where_to}"))
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let detail = format!("{info}\n\n{}", std::backtrace::Backtrace::force_capture());
        report_failure("TNTL stopped unexpectedly", &detail);
    }));
}

fn main() {
    // Answer `--version` before the toolkit starts: the packaged shell has no
    // CLI binary, so this is how a terminal finds out what is installed.
    if std::env::args().skip(1).any(|arg| arg == "--version" || arg == "-V") {
        // A GUI-subsystem process may have no usable stdout handle (double
        // click, or a terminal that did not pass one on); writing to it must
        // not panic before the error hook exists.
        use std::io::Write;
        let _ = writeln!(std::io::stdout(), "tntl {}", env!("CARGO_PKG_VERSION_MAJOR"));
        return;
    }

    install_panic_hook();
    append_log("start", &startup_notes());

    #[cfg(target_os = "linux")]
    configure_webview();

    let root = initial_root();
    let started = tauri::Builder::default()
        .setup(|_| {
            // The window and the webview exist at this point. A log that stops
            // at "window" means the frontend never became interactive.
            append_log("window", "the main window was created");
            Ok(())
        })
        .manage(AppState {
            root: Mutex::new(root),
            game: Mutex::new(None),
            launching: std::sync::atomic::AtomicBool::new(false),
        })
        .invoke_handler(tauri::generate_handler![
            roots,
            set_root,
            forget_root,
            bootstrap,
            list_instances,
            list_versions,
            list_loaders,
            loader_status,
            install_loader,
            remove_loader,
            check_loader_update,
            install_version,
            modpack_info,
            archive_kind,
            install_modpack,
            java_offers,
            install_java,
            create_instance,
            save_instance,
            clear_instance_java,
            load_instance,
            delete_instance,
            installed_versions,
            delete_version,
            create_instance_from_version,
            rename_instance,
            import_candidates,
            import_instance,
            list_mods,
            diagnose_instance,
            analyze_log,
            set_mod_enabled,
            remove_mod,
            search_mods,
            install_mods,
            install_missing_deps,
            fix_dependency_versions,
            search_projects,
            mod_game_versions,
            install_projects,
            project_versions,
            install_project_version,
            list_content,
            set_content_enabled,
            move_resource_pack,
            remove_content,
            content_icon,
            check_updates,
            check_dependency_updates,
            memory_advice,
            update_content,
            check_pack_update,
            update_pack,
            repair_instance,
            cancel_download,
            list_worlds,
            duplicate_world,
            list_datapacks,
            set_datapack_enabled,
            add_datapack,
            delete_datapack,
            background_image,
            export_log,
            list_world_backups,
            backup_world,
            delete_world_backup,
            delete_world,
            import_world,
            add_local_mod,
            add_local_content,
            duplicate_instance,
            export_instance,
            launcher_settings,
            save_launcher_settings,
            set_instance_icon,
            clear_instance_icon,
            instance_icon,
            export_archive,
            import_archive,
            list_crash_reports,
            read_crash_report,
            delete_crash_report,
            list_screenshots,
            read_screenshot,
            delete_screenshot,
            list_servers,
            save_servers,
            disk_usage,
            cleanup_instance,
            pick_file,
            pick_folder,
            is_directory,
            pick_save_file,
            open_path,
            open_url,
            frontend_ready,
            launch_instance,
            stop_game,
            running_instance,
            read_instance_log,
            list_accounts,
            add_offline_account,
            remove_account,
            rename_offline_account,
            set_current_account,
            update_account,
            set_account_skin,
            clear_account_skin,
            account_skin,
            account_cape,
            set_default_skin,
            preview_online_skin,
            apply_online_skin,
            apply_gallery_skin,
            skin_gallery,
            default_skins,
            default_skin_image,
            set_account_cape,
            refresh_account,
            start_msa_login,
            poll_msa_login,
            search_modpacks,
            install_modpack_project,
            search_curseforge_packs,
            install_curseforge_pack,
            search_ftb_packs,
            ftb_pack,
            install_ftb_pack,
            server_status
        ])
        .run(tauri::generate_context!());
    if let Err(error) = started {
        report_failure("TNTL could not start", &format!("{error:#}"));
        std::process::exit(1);
    }
    append_log("exit", "the event loop ended");
}
