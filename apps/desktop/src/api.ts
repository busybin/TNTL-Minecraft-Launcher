import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface InstanceView {
  name: string;
  version_id: string;
  username: string;
  max_memory_mb: number;
  java: string | null;
  imported: boolean;
  account_name: string | null;
  dir: string;
  game_dir: string;
  installed: boolean;
  last_log: string | null;
  modpack: string | null;
  group: string | null;
  notes: string;
  icon: string | null;
  /** Total seconds played across finished launches. */
  playtime_seconds: number;
  launches: number;
  /** Unix timestamp of the last finished launch. */
  last_played: number | null;
}

export interface VersionView {
  id: string;
  version_type: string;
  release_time: string;
  installed: boolean;
}

export interface JavaView {
  path: string;
  major: number;
  version: string;
  vendor: string | null;
}

export interface JavaOffer {
  component: string;
  version: string;
  released: string | null;
}

export interface InstalledRuntime {
  component: string;
  version: string;
  home: string;
  java: string;
  files: number;
  bytes: number;
  skipped: boolean;
}

export type RuntimeProgress =
  | { stage: "manifest"; component: string; version: string; files: number }
  | { stage: "downloading"; path: string; done: number; total: number | null }
  | { stage: "installed"; path: string; bytes: number }
  | { stage: "skipped"; path: string };

export interface LoaderView {
  version: string;
  stable: boolean;
}

export type LoaderKind = "fabric" | "quilt" | "legacyfabric" | "forge" | "neoforge";

export interface LoaderStatus {
  game_version: string;
  kind: LoaderKind | null;
  version: string | null;
  version_id: string;
}

export interface LoaderUpdate {
  kind: LoaderKind;
  current: string;
  latest: string;
  newer: boolean;
}

export interface ModInfo {
  file: string;
  id: string;
  name: string;
  version: string;
  loader: "fabric" | "quilt" | "forge" | "neoforge" | "unknown";
  enabled: boolean;
  size: number;
  /** Seconds since the Unix epoch, 0 when unknown. */
  modified: number;
  missing_dependencies: string[];
  /** Required ids only switched-off jars provide. */
  disabled_dependencies: string[];
  /** Every mod id this jar requires, satisfied or not. */
  dependency: string[];
  duplicate: boolean;
  /** Set when the instance's loader or game version cannot satisfy this jar. */
  blocked?: BlockedReason;
}

export interface BlockedReason {
  kind: "loader" | "game";
  /** Loader name (`Forge`, `NeoForge`, ...) or `Minecraft`. */
  loader: string;
  required: string[];
  actual: string;
}

export type FindingSeverity = "error" | "warning" | "info";

/** Mirrors `tntl_doctor::Finding`; the UI localizes the text by `code`. */
export interface Finding {
  severity: FindingSeverity;
  code: string;
  subject: string | null;
  params: Record<string, string>;
  evidence: string | null;
  line: number | null;
}

export type LogLevel = "debug" | "info" | "warn" | "error" | "context";

export interface LogAnalysis {
  counts: {
    total: number;
    debug: number;
    info: number;
    warn: number;
    error: number;
    context: number;
  };
  /** One entry per analyzed line. */
  levels: LogLevel[];
  findings: Finding[];
}

export interface DiagnosticReport {
  checked_at: number;
  game_version: string;
  loader: string | null;
  loader_version: string | null;
  mods: number;
  enabled_mods: number;
  memory_mb: number;
  java_major: number | null;
  findings: Finding[];
}

export interface Bootstrap {
  root: string;
  instances: InstanceView[];
  installed_versions: string[];
  java: JavaView[];
  accounts: AccountsResponse;
}

export interface RootsState {
  current: string;
  roots: string[];
}

export interface InstalledVersion {
  id: string;
  bytes: number;
  /** Instances whose version chain contains this id; empty means unused. */
  instances: string[];
}

export interface CapeView {
  id: string;
  alias: string;
  active: boolean;
}

export interface AccountView {
  id: string;
  kind: "offline" | "microsoft";
  name: string;
  uuid: string;
  active: boolean;
  skin_model: "wide" | "slim";
  has_skin: boolean;
  has_cape: boolean;
  /** Offline skin is painted into the game through a resource pack. */
  apply_skin: boolean;
  cape: string | null;
  capes: CapeView[];
  expires_at: number | null;
  microsoft: boolean;
}

export interface AccountsResponse {
  current: string | null;
  accounts: AccountView[];
  /** A Microsoft client id is configured. */
  client_id: boolean;
}

/** Skin found online, before the user applies it. */
export interface OnlineSkinPreview {
  /** Name as typed. */
  nickname: string;
  /** Name the source reports, which can differ in case. */
  name: string;
  slim: boolean;
  /** PNG bytes, serialized as a number array by the IPC layer. */
  skin: number[];
  /** `Mojang` or `ely.by`. */
  source: string;
}

/** One texture from the public gallery. */
export interface GallerySkin {
  id: number;
  /** Upload time, seconds since the Unix epoch. */
  time: number;
  /** PNG bytes, serialized as a number array by the IPC layer. */
  skin: number[];
  /** Arm model the upload was tagged with, when the list reports it. */
  slim: boolean | null;
}

/** One page of the skin gallery plus the cursor that continues it. */
export interface GalleryPage {
  skins: GallerySkin[];
  /** Pass back as `after` to fetch the next page; null on the last page. */
  next: string | null;
}

/** Categories the MineSkin gallery exposes. */
export type GalleryFilter = "latest" | "top" | "staff" | "slim" | "hd" | "random";

/** Which service a nickname lookup asks. */
export type SkinSource = "auto" | "mojang" | "ely";

export interface DeviceCode {
  device_code: string;
  user_code: string;
  verification_uri: string;
  expires_in: number;
  interval: number;
  message: string;
}

/** Mirrors `tntl_app::MsaPoll`. */
export type MsaPoll =
  | { status: "pending" }
  | { status: "slow_down" }
  | { status: "signed_in"; account: AccountView };

export interface PackSummary {
  project_id: string;
  slug: string;
  title: string;
  description: string;
  icon_url: string | null;
  downloads: number;
  author: string;
}

export interface PackPage {
  hits: PackSummary[];
  total_hits: number;
  offset: number;
  limit: number;
}

export interface CursePackFile {
  file_id: number;
  name: string;
  version: string;
  game_versions: string[];
  release_type: number;
  file_date: string;
  size: number;
}

export interface CursePack {
  project_id: number;
  slug: string;
  title: string;
  description: string;
  icon_url: string | null;
  downloads: number;
  author: string;
  files: CursePackFile[];
}

export interface CursePackPage {
  hits: CursePack[];
  total_hits: number;
  offset: number;
  limit: number;
}

export interface FtbVersion {
  id: number;
  name: string;
  kind: string;
  game_version: string;
  loader: string | null;
  loader_version: string | null;
  java: string | null;
  recommended_memory_mb: number | null;
  files: number;
  bytes: number;
}

export interface FtbPack {
  id: number;
  name: string;
  synopsis: string;
  description: string;
  icon_url: string | null;
  installs: number;
  plays: number;
  tags: string[];
  versions: FtbVersion[];
}

export interface ServerStatus {
  version: string;
  protocol: number;
  online: number;
  max: number;
  sample: string[];
  motd: string;
  icon: string | null;
  latency_ms: number;
  address: string;
}

export type ProjectKind = "mod" | "resourcepack" | "shader";

/** Catalogue a project comes from. */
export type ContentSource = "modrinth" | "curseforge";

export interface ContentFile {
  file: string;
  bytes: number;
  /** Seconds since the Unix epoch, 0 when unknown. */
  modified: number;
  enabled: boolean;
  /** `pack.description` from pack.mcmeta. */
  description?: string | null;
  /** `pack.pack_format`. */
  format?: number | null;
  /** `supported_formats` bounds, from packs built for 1.20.2 and newer. */
  min_format?: number | null;
  max_format?: number | null;
}

export interface ContentListing {
  files: ContentFile[];
  /** Shaders can only be switched when Iris or OptiFine is installed. */
  toggle_supported: boolean;
  /** Pack format the instance's game version expects. */
  expected_format?: number | null;
}

/** One build of a project, as offered by the version picker. */
export interface VersionChoice {
  id: string;
  version_number: string;
  version_type: string;
  game_versions: string[];
  loaders: string[];
  date_published: string;
  file: string;
  size: number;
}

export interface PackTarget {
  version_id: string;
  version_number: string;
  version_type: string;
  game_version: string;
  loader: string | null;
  in_place: boolean;
  url: string;
  sha1: string | null;
  size: number;
}

export interface PackUpdate {
  instance: string;
  name: string;
  project_id: string;
  current_version: string;
  current_game_version: string;
  targets: PackTarget[];
}

export interface PackUpdateReport {
  instance: string;
  name: string;
  version_number: string;
  version_id: string;
  game_version: string;
  loader: string | null;
  removed: number;
  files: PackReport;
}

export interface RepairReport {
  checked: number;
  fetched: number;
  bytes: number;
  pack: string | null;
  /** True when the loader had to be installed again. */
  loader_rebuilt: boolean;
}

export interface ModSearchHit {
  project_id: string;
  slug: string;
  title: string;
  description: string;
  author: string;
  categories: string[];
  downloads: number;
  icon_url: string | null;
  client_side: string;
  server_side: string;
  installed: boolean;
}

export interface ModSearchPage {
  game_version: string;
  loader: string;
  hits: ModSearchHit[];
  total_hits: number;
  offset: number;
  limit: number;
}

export interface ModUpdate {
  file: string;
  name: string;
  project_id: string;
  kind: ProjectKind;
  current_version: string;
  latest_version: string;
  latest_file: string;
  url: string;
  sha1: string | null;
  size: number;
}

export interface InstalledMod {  project_id: string;
  slug: string;
  title: string;
  version: string;
  file: string;
  bytes: number;
  dependency: boolean;
  skipped: boolean;
}

export interface DependencyUpdate {
  dep: string;
  name: string;
  file: string;
  from: string;
  to: string;
}

export interface DependencyFixReport {
  updated: DependencyUpdate[];
  unresolved: string[];
}

export type ModProgress =
  | { stage: "resolving"; project: string }
  | { stage: "downloading"; project: string; file: string; done: number; total: number | null }
  | {
      stage: "installed";
      project: string;
      title: string;
      file: string;
      version: string;
      bytes: number;
      dependency: boolean;
    }
  | { stage: "skipped"; project: string; title: string; dependency: boolean };

/** Mirrors `tntl_instance::Instance`. */
export interface InstanceConfig {
  name: string;
  version_id: string;
  root: string | null;
  game_dir: string | null;
  java: string | null;
  min_memory_mb: number | null;
  max_memory_mb: number;
  jvm_args: string[];
  game_args: string[];
  env: Record<string, string>;
  server: { host: string; port: number } | null;
  username: string;
  demo: boolean;
  width: number | null;
  height: number | null;
  created_at: string | null;
  group: string | null;
  notes: string;
  icon: string | null;
}

export interface LauncherSettings {
  /** Parallel downloads, 1-32. */
  concurrency: number;
  hide_on_launch: boolean;
  quit_on_launch: boolean;
  /** `dark`, `light` or `system`. */
  theme: string;
  /** Path of the background image, or null. */
  background: string | null;
  /** `none`, `system`, `http` or `socks5`. */
  proxy_mode: string;
  /** Proxy URL for the `http` and `socks5` modes. */
  proxy_url: string | null;
  /** Optional account for an authenticated proxy. */
  proxy_username: string | null;
  proxy_password: string | null;
  onboarded: boolean;
  /** Public Entra client id for Microsoft device-code sign-in. */
  msa_client_id: string | null;
  /** CurseForge API key, supplied by the user. */
  curseforge_api_key: string | null;
}

/** Mirrors `tntl_instance::datapacks::DatapackInfo`. */
export interface DatapackInfo {
  name: string;
  bytes: number;
  enabled: boolean;
  folder: boolean;
}

export interface CrashReport {
  file: string;
  bytes: number;
  modified_ms: number | null;
}

export interface Screenshot {
  file: string;
  bytes: number;
  modified_ms: number | null;
}

/** Mirrors `tntl_instance::servers::ServerEntry`. */
export interface ServerEntry {
  name: string;
  ip: string;
  accept_textures: boolean;
  /** Row in `servers.dat`; absent for a new server. */
  index?: number | null;
}

export interface DiskEntry {
  /** Translation key suffix: `settings.disk.<key>`. */
  key: string;
  label: string;
  path: string;
  bytes: number;
}

export interface DiskUsage {
  entries: DiskEntry[];
  total: number;
}

export interface CleanupReport {
  removed: string[];
  bytes: number;
}

export interface ArchiveReport {
  path: string;
  files: number;
  bytes: number;
}

/** Mirrors `tntl_instance::worlds::WorldInfo`. */
export interface WorldInfo {
  dir: string;
  name: string;
  version: string | null;
  game_mode: string | null;
  hardcore: boolean;
  /** String: 64-bit seeds do not survive a JSON number. */
  seed: string | null;
  last_played_ms: number | null;
  size: number;
}

/** Mirrors `tntl_app::WorldBackup`. */
export interface WorldBackup {
  file: string;
  path: string;
  bytes: number;
  created_ms: number | null;
}

export interface InstallEvent {
  stage: string;
  done: number;
  total: number;
  bytes: number;
  bytes_total: number | null;
}

export interface InstallDone {
  version: string;
  downloaded: number;
  skipped: number;
  bytes: number;
  /** Instance registered with the install, when one was requested. */
  instance: string | null;
}

/** Band edges for the memory slider, in MiB. */
export interface MemoryAdvice {
  total_mb: number | null;
  green_min_mb: number;
  green_max_mb: number;
  warn_max_mb: number;
  limit_mb: number;
}

export type PackProgress =
  | { stage: "archive"; done: number; total: number | null }
  | {
      stage: "downloading";
      path: string;
      done: number;
      total: number | null;
      files_done: number;
      files_total: number;
      bytes_done: number;
      bytes_total: number;
    }
  | { stage: "installed"; path: string; bytes: number }
  | { stage: "skipped"; path: string }
  | { stage: "extracting"; prefix: string; entries: number };

/** Modrinth search order. */
export type SearchIndex = "relevance" | "downloads" | "follows" | "newest" | "updated";

export interface PackReport {
  files: number;
  downloaded: number;
  skipped: number;
  bytes: number;
  overrides: number;
}

export interface ModpackInfo {
  name: string;
  summary: string;
  game_version: string;
  loader: string | null;
  loader_version: string | null;
  files: number;
  bytes: number;
}

export interface ModpackReport {
  instance: string;
  name: string;
  game_version: string;
  loader: string | null;
  version_id: string;
  files: PackReport;
}

export interface CrashInfo {
  kind: string;
  summary: string;
  hints: string[];
}

export interface GameExit {
  instance: string;
  exit_code: number | null;
  duration_ms: number;
  crash: CrashInfo | null;
}

export const api = {
  bootstrap: () => invoke<Bootstrap>("bootstrap"),
  roots: () => invoke<RootsState>("roots"),
  setRoot: (path: string) => invoke<RootsState>("set_root", { path }),
  forgetRoot: (path: string) => invoke<RootsState>("forget_root", { path }),
  listInstances: () => invoke<InstanceView[]>("list_instances"),
  listVersions: (channel: "release" | "snapshot" | "all") =>
    invoke<VersionView[]>("list_versions", { channel }),
  listLoaders: (game: string, loader: LoaderKind) =>
    invoke<LoaderView[]>("list_loaders", { game, loader }),
  loaderStatus: (name: string) => invoke<LoaderStatus>("loader_status", { name }),
  installLoader: (name: string, loader: LoaderKind, version: string | null) =>
    invoke<LoaderStatus>("install_loader", { name, loader, version }),
  removeLoader: (name: string) => invoke<LoaderStatus>("remove_loader", { name }),
  checkLoaderUpdate: (name: string) => invoke<LoaderUpdate>("check_loader_update", { name }),
  cancelDownload: () => invoke<void>("cancel_download"),
  installVersion: (
    version: string,
    assets: boolean,
    instanceName: string | null,
    loader: LoaderKind | null,
    loaderVersion: string | null,
  ) => invoke<void>("install_version", { version, assets, instanceName, loader, loaderVersion }),
  createInstance: (name: string, versionId: string, username: string | null, maxMemoryMb: number | null) =>
    invoke<void>("create_instance", { name, versionId, username, maxMemoryMb }),
  loadInstance: (name: string) => invoke<InstanceConfig>("load_instance", { name }),
  saveInstance: (instance: InstanceConfig) => invoke<void>("save_instance", { instance }),
  clearInstanceJava: (name: string) => invoke<void>("clear_instance_java", { name }),
  deleteInstance: (name: string, cleanup = false) =>
    invoke<string[]>("delete_instance", { name, cleanup }),
  installedVersions: () => invoke<InstalledVersion[]>("installed_versions"),
  deleteVersion: (id: string) => invoke<void>("delete_version", { id }),
  createInstanceFromVersion: (versionId: string, name: string) =>
    invoke<string>("create_instance_from_version", { versionId, name }),
  renameInstance: (name: string, newName: string) => invoke<string>("rename_instance", { name, newName }),
  duplicateInstance: (name: string, newName: string) =>
    invoke<string>("duplicate_instance", { name, newName }),
  pickFile: (title: string, extensions: string[]) => invoke<string | null>("pick_file", { title, extensions }),
  pickFolder: (title: string) => invoke<string | null>("pick_folder", { title }),
  isDirectory: (path: string) => invoke<boolean>("is_directory", { path }),
  pickSaveFile: (title: string, fileName: string, extensions: string[]) =>
    invoke<string | null>("pick_save_file", { title, fileName, extensions }),
  exportInstance: (name: string, path: string, packName: string | null, packVersion: string) =>
    invoke<{ files: number; bytes: number }>("export_instance", { name, path, packName, packVersion }),
  launcherSettings: () => invoke<LauncherSettings>("launcher_settings"),
  saveLauncherSettings: (settings: LauncherSettings) =>
    invoke<LauncherSettings>("save_launcher_settings", { settings }),
  setInstanceIcon: (name: string, path: string) => invoke<string>("set_instance_icon", { name, path }),
  clearInstanceIcon: (name: string) => invoke<void>("clear_instance_icon", { name }),
  instanceIcon: (name: string) => invoke<ArrayBuffer>("instance_icon", { name }),
  exportArchive: (name: string, path: string) =>
    invoke<ArchiveReport>("export_archive", { name, path }),
  archiveKind: (path: string) =>
    invoke<"instance" | "world" | "modpack" | "resource_pack" | "unknown">("archive_kind", { path }),
  importArchive: (path: string, name: string) => invoke<InstanceConfig>("import_archive", { path, name }),
  listCrashReports: (name: string) => invoke<CrashReport[]>("list_crash_reports", { name }),
  readCrashReport: (name: string, file: string) =>
    invoke<string>("read_crash_report", { name, file }),
  deleteCrashReport: (name: string, file: string) =>
    invoke<void>("delete_crash_report", { name, file }),
  listScreenshots: (name: string) => invoke<Screenshot[]>("list_screenshots", { name }),
  readScreenshot: (name: string, file: string) =>
    invoke<ArrayBuffer>("read_screenshot", { name, file }),
  deleteScreenshot: (name: string, file: string) => invoke<void>("delete_screenshot", { name, file }),
  listServers: (name: string) => invoke<ServerEntry[]>("list_servers", { name }),
  saveServers: (name: string, servers: ServerEntry[]) =>
    invoke<ServerEntry[]>("save_servers", { name, servers }),
  diskUsage: (name: string) => invoke<DiskUsage>("disk_usage", { name }),
  cleanupInstance: (name: string) => invoke<CleanupReport>("cleanup_instance", { name }),
  openPath: (path: string) => invoke<void>("open_path", { path }),
  openUrl: (url: string) => invoke<void>("open_url", { url }),
  listWorlds: (name: string) => invoke<WorldInfo[]>("list_worlds", { name }),
  listDatapacks: (name: string, world: string) =>
    invoke<DatapackInfo[]>("list_datapacks", { name, world }),
  setDatapackEnabled: (name: string, world: string, file: string, enabled: boolean) =>
    invoke<DatapackInfo[]>("set_datapack_enabled", { name, world, file, enabled }),
  addDatapack: (name: string, world: string, path: string) =>
    invoke<DatapackInfo[]>("add_datapack", { name, world, path }),
  deleteDatapack: (name: string, world: string, file: string) =>
    invoke<DatapackInfo[]>("delete_datapack", { name, world, file }),
  backgroundImage: () => invoke<ArrayBuffer>("background_image"),
  exportLog: (name: string, path: string) => invoke<void>("export_log", { name, path }),
  listWorldBackups: (name: string) => invoke<WorldBackup[]>("list_world_backups", { name }),
  backupWorld: (name: string, world: string) => invoke<string>("backup_world", { name, world }),
  deleteWorldBackup: (name: string, file: string) =>
    invoke<WorldBackup[]>("delete_world_backup", { name, file }),
  deleteWorld: (name: string, world: string) => invoke<void>("delete_world", { name, world }),
  duplicateWorld: (name: string, world: string) => invoke<WorldInfo>("duplicate_world", { name, world }),
  importWorld: (name: string, path: string) => invoke<WorldInfo>("import_world", { name, path }),
  addLocalMod: (name: string, path: string) => invoke<ModInfo[]>("add_local_mod", { name, path }),
  addLocalContent: (name: string, kind: ProjectKind, path: string) =>
    invoke<ContentListing>("add_local_content", { name, kind, path }),
  importInstance: (path: string, name: string | null, version: string | null) =>
    invoke<string>("import_instance", { path, name, version }),
  listMods: (name: string) => invoke<ModInfo[]>("list_mods", { name }),
  diagnoseInstance: (name: string) => invoke<DiagnosticReport>("diagnose_instance", { name }),
  analyzeLog: (text: string) => invoke<LogAnalysis>("analyze_log", { text }),
  setModEnabled: (name: string, file: string, enabled: boolean) =>
    invoke<ModInfo[]>("set_mod_enabled", { name, file, enabled }),
  removeMod: (name: string, file: string) => invoke<ModInfo[]>("remove_mod", { name, file }),
  searchMods: (instance: string, query: string, sort: SearchIndex, limit: number, offset: number) =>
    invoke<ModSearchPage>("search_mods", { instance, query, sort, limit, offset }),
  searchProjects: (
    instance: string,
    kind: ProjectKind,
    query: string,
    sort: SearchIndex,
    source: ContentSource,
    gameVersion: string | null,
    limit: number,
    offset: number,
  ) =>
    invoke<ModSearchPage>("search_projects", {
      instance,
      kind,
      query,
      sort,
      source,
      gameVersion,
      limit,
      offset,
    }),
  installProjects: (
    instance: string,
    kind: ProjectKind,
    projects: string[],
    source: ContentSource,
    gameVersion: string | null,
  ) => invoke<InstalledMod[]>("install_projects", { instance, kind, projects, source, gameVersion }),
  modGameVersions: () => invoke<string[]>("mod_game_versions"),
  listContent: (name: string, kind: ProjectKind) =>
    invoke<ContentListing>("list_content", { name, kind }),
  setContentEnabled: (name: string, kind: ProjectKind, file: string, enabled: boolean) =>
    invoke<ContentListing>("set_content_enabled", { name, kind, file, enabled }),
  moveResourcePack: (name: string, file: string, up: boolean) =>
    invoke<ContentListing>("move_resource_pack", { name, file, up }),
  removeContent: (name: string, kind: ProjectKind, file: string) =>
    invoke<ContentListing>("remove_content", { name, kind, file }),
  contentIcon: (name: string, kind: ProjectKind, file: string) =>
    invoke<ArrayBuffer>("content_icon", { name, kind, file }),
  installMods: (instance: string, projects: string[]) =>
    invoke<InstalledMod[]>("install_mods", { instance, projects }),
  installMissingDeps: (instance: string) =>
    invoke<{ installed: InstalledMod[]; unresolved: string[] }>("install_missing_deps", { instance }),
  fixDependencyVersions: (instance: string) =>
    invoke<DependencyFixReport>("fix_dependency_versions", { instance }),
  projectVersions: (instance: string, kind: ProjectKind, project: string) =>
    invoke<VersionChoice[]>("project_versions", { instance, kind, project }),
  installProjectVersion: (
    instance: string,
    kind: ProjectKind,
    project: string,
    versionId: string,
    source: ContentSource,
  ) =>
    invoke<InstalledMod[]>("install_project_version", { instance, kind, project, versionId, source }),
  memoryAdvice: () => invoke<MemoryAdvice>("memory_advice"),
  checkUpdates: (instance: string, kind: ProjectKind) =>
    invoke<ModUpdate[]>("check_updates", { instance, kind }),
  updateContent: (instance: string, kind: ProjectKind, files: string[]) =>
    invoke<InstalledMod[]>("update_content", { instance, kind, files }),
  checkPackUpdate: (instance: string) => invoke<PackUpdate>("check_pack_update", { instance }),
  updatePack: (instance: string, versionId: string) =>
    invoke<PackUpdateReport>("update_pack", { instance, versionId }),
  repairInstance: (name: string) => invoke<RepairReport>("repair_instance", { name }),
  installModpack: (path: string, instanceName: string | null) =>
    invoke<ModpackReport>("install_modpack", { path, instanceName }),
  modpackInfo: (path: string) => invoke<ModpackInfo>("modpack_info", { path }),
  searchModpacks: (query: string, sort: SearchIndex, limit: number, offset: number) =>
    invoke<PackPage>("search_modpacks", { query, sort, limit, offset }),
  installModpackProject: (project: string, instanceName: string | null) =>
    invoke<ModpackReport>("install_modpack_project", { project, instanceName }),
  searchCurseforgePacks: (query: string, sort: SearchIndex, limit: number, offset: number) =>
    invoke<CursePackPage>("search_curseforge_packs", { query, sort, limit, offset }),
  installCurseforgePack: (projectId: number, fileId: number | null, instanceName: string | null) =>
    invoke<ModpackReport>("install_curseforge_pack", { projectId, fileId, instanceName }),
  searchFtbPacks: (query: string, limit: number) =>
    invoke<FtbPack[]>("search_ftb_packs", { query, limit }),
  ftbPack: (id: number) => invoke<FtbPack>("ftb_pack", { id }),
  installFtbPack: (packId: number, versionId: number | null, instanceName: string | null) =>
    invoke<ModpackReport>("install_ftb_pack", { packId, versionId, instanceName }),
  listAccounts: () => invoke<AccountsResponse>("list_accounts"),
  addOfflineAccount: (name: string) => invoke<AccountView>("add_offline_account", { name }),
  renameOfflineAccount: (id: string, name: string) =>
    invoke<AccountView>("rename_offline_account", { id, name }),
  removeAccount: (id: string) => invoke<void>("remove_account", { id }),
  setCurrentAccount: (id: string) => invoke<AccountView>("set_current_account", { id }),
  updateAccount: (id: string, skinModel: string | null, applySkin: boolean | null) =>
    invoke<AccountView>("update_account", { id, skinModel, applySkin }),
  setAccountSkin: (id: string, path: string, model: string | null) =>
    invoke<AccountView>("set_account_skin", { id, path, model }),
  clearAccountSkin: (id: string) => invoke<AccountView>("clear_account_skin", { id }),
  accountSkin: (id: string) => invoke<ArrayBuffer>("account_skin", { id }),
  accountCape: (id: string) => invoke<ArrayBuffer>("account_cape", { id }),
  defaultSkins: () => invoke<string[]>("default_skins"),
  defaultSkinImage: (key: string) => invoke<ArrayBuffer>("default_skin_image", { key }),
  setDefaultSkin: (id: string, key: string) => invoke<AccountView>("set_default_skin", { id, key }),
  previewOnlineSkin: (nickname: string, source: SkinSource = "auto") =>
    invoke<OnlineSkinPreview>("preview_online_skin", { nickname, source }),
  applyOnlineSkin: (id: string, nickname: string, source: SkinSource = "auto") =>
    invoke<AccountView>("apply_online_skin", { id, nickname, source }),
  applyGallerySkin: (id: string, skin: number[], slim: boolean) =>
    invoke<AccountView>("apply_gallery_skin", { id, skin, slim }),
  skinGallery: (filter: GalleryFilter, after: string | null, count = 16) =>
    invoke<GalleryPage>("skin_gallery", { filter, after, count }),
  setAccountCape: (id: string, cape: string | null) =>
    invoke<AccountView>("set_account_cape", { id, cape }),
  refreshAccount: (id: string) => invoke<AccountView>("refresh_account", { id }),
  startMsaLogin: () => invoke<DeviceCode>("start_msa_login"),
  pollMsaLogin: (deviceCode: string) => invoke<MsaPoll>("poll_msa_login", { deviceCode }),
  serverStatus: (host: string, port: number) =>
    invoke<ServerStatus>("server_status", { host, port }),
  javaOffers: () => invoke<JavaOffer[]>("java_offers"),
  installJava: (component: string) => invoke<InstalledRuntime>("install_java", { component }),
  launchInstance: (name: string) => invoke<number>("launch_instance", { name }),
  stopGame: () => invoke<boolean>("stop_game"),
  runningInstance: () => invoke<string | null>("running_instance"),
  readInstanceLog: (name: string) => invoke<string>("read_instance_log", { name }),
};

/** Progress payload: `job` names the operation that produced it. */
export interface JobEvent<T> {
  job: string;
  event: T;
}

/** Tags the shell puts on progress, so each view can ignore other operations. */
export const jobTag = {
  install: "install",
  import: "import",
  modpack: "modpack",
  loader: (name: string) => `loader:${name}`,
  repair: (name: string) => `repair:${name}`,
  update: (name: string) => `update:${name}`,
};

export const events = {
  onInstallProgress: (handler: (payload: JobEvent<InstallEvent>) => void) =>
    listen<JobEvent<InstallEvent>>("install-progress", (event) => handler(event.payload)),
  onInstallStart: (handler: (payload: { version: string; loader: LoaderKind | null; instance: string | null }) => void) =>
    listen<{ version: string; loader: LoaderKind | null; instance: string | null }>("install-start", (event) =>
      handler(event.payload),
    ),
  onInstallDone: (handler: (payload: InstallDone) => void) =>
    listen<InstallDone>("install-done", (event) => handler(event.payload)),
  onInstallFailed: (handler: (payload: string) => void) =>
    listen<string>("install-failed", (event) => handler(event.payload)),
  onGameLine: (handler: (line: string) => void) => listen<string>("game-line", (event) => handler(event.payload)),
  onModProgress: (handler: (event: ModProgress) => void) =>
    listen<ModProgress>("mod-progress", (event) => handler(event.payload)),
  onPackProgress: (handler: (payload: JobEvent<PackProgress>) => void) =>
    listen<JobEvent<PackProgress>>("pack-progress", (event) => handler(event.payload)),
  onJavaProgress: (handler: (event: RuntimeProgress) => void) =>
    listen<RuntimeProgress>("java-progress", (event) => handler(event.payload)),
  onGameExit: (handler: (payload: GameExit) => void) =>
    listen<GameExit>("game-exit", (event) => handler(event.payload)),
};

export type { UnlistenFn };

export function formatBytes(bytes: number): string {
  if (!bytes) return "0 B";
  const units = ["B", "KiB", "MiB", "GiB"];
  const index = Math.min(units.length - 1, Math.floor(Math.log(bytes) / Math.log(1024)));
  return `${(bytes / 1024 ** index).toFixed(index === 0 ? 0 : 1)} ${units[index]}`;
}

/// Transfer rate, e.g. `8.4 MiB/s`.
export function formatSpeed(bytesPerSecond: number): string {
  if (!Number.isFinite(bytesPerSecond) || bytesPerSecond <= 0) return "";
  return `${formatBytes(bytesPerSecond)}/s`;
}

/// Compact play time: `3h 12m`, `12m`, `40s`.
export function formatPlaytime(seconds: number): string {
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  return `${hours}h ${minutes % 60}m`;
}

/// Date from a unix timestamp, in the user's locale.
export function formatDate(timestamp: number | null): string {
  if (!timestamp) return "";
  return new Date(timestamp * 1000).toLocaleDateString();
}

export function formatDuration(ms: number): string {
  const seconds = Math.round(ms / 1000);
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  return `${minutes}m ${seconds % 60}s`;
}

const IMAGE_TYPES: Record<string, string> = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  webp: "image/webp",
  gif: "image/gif",
};

/** Blob URL for an image the shell returned as bytes. Revoke it when done. */
export function imageUrl(bytes: ArrayBuffer, file?: string | null): string {
  const extension = file?.split(".").pop()?.toLowerCase() ?? "png";
  return URL.createObjectURL(new Blob([bytes], { type: IMAGE_TYPES[extension] ?? "image/png" }));
}
