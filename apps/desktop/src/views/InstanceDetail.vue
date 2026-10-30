<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import {
  api,
  events,
  formatBytes,
  formatDate,
  formatPlaytime,
  imageUrl,
  type ContentFile,
  type CrashReport,
  type DatapackInfo,
  type DiagnosticReport,
  type InstallEvent,
  type InstanceConfig,
  type JavaView,
  type JobEvent,
  type LoaderKind,
  type LoaderStatus,
  type LoaderUpdate,
  type LoaderView,
  type LogAnalysis,
  type ModInfo,
  type ModProgress,
  type ModUpdate,
  type PackProgress,
  type PackTarget,
  type PackUpdate,
  type ProjectKind,
  type AccountsResponse,
  type Screenshot,
  type ServerEntry,
  type ServerStatus,
  type UnlistenFn,
  jobTag,
  type WorldBackup,
  type WorldInfo,
} from "../api";
import Icon from "../components/Icon.vue";
import TransferBar from "../components/TransferBar.vue";
import UpdateList from "../components/UpdateList.vue";
import ModrinthDialog from "./ModrinthDialog.vue";
import DoctorDialog from "../components/DoctorDialog.vue";
import FindingCard from "../components/FindingCard.vue";
import { findingsReport } from "../findings";
import MemorySlider from "../components/MemorySlider.vue";
import { t } from "../i18n";
import { maybePromptJava } from "../javaPrompt";
import { useTransfer } from "../progress";

const props = defineProps<{
  name: string;
  modpack?: string | null;
  running?: boolean;
  busy?: boolean;
  reload?: number;
}>();
const emit = defineEmits<{
  close: [];
  saved: [name: string];
  changed: [];
  duplicated: [name: string];
  launch: [name: string];
  stop: [];
  log: [name: string];
  remove: [name: string];
}>();

type Tab =
  | "overview"
  | "loader"
  | "launch"
  | "mods"
  | "resourcepacks"
  | "shaders"
  | "worlds"
  | "datapacks"
  | "servers"
  | "logs"
  | "integrity"
  | "manage";

const tab = ref<Tab>("overview");

const TAB_GROUPS: { label: string; items: { id: Tab; label: string; icon: string }[] }[] = [
  {
    label: "detail.group.instance",
    items: [
      { id: "overview", label: "detail.overview", icon: "cube" },
      { id: "loader", label: "editor.loader", icon: "package" },
      { id: "launch", label: "editor.launch", icon: "play" },
    ],
  },
  {
    label: "detail.group.content",
    items: [
      { id: "mods", label: "editor.mods", icon: "spark" },
      { id: "resourcepacks", label: "editor.resourcePacks", icon: "image" },
      { id: "shaders", label: "editor.shaders", icon: "image" },
      { id: "worlds", label: "editor.worlds", icon: "folder" },
      { id: "datapacks", label: "editor.datapacks", icon: "package" },
    ],
  },
  {
    label: "detail.group.data",
    items: [
      { id: "servers", label: "editor.servers", icon: "server" },
      { id: "logs", label: "detail.logsTab", icon: "terminal" },
      { id: "integrity", label: "editor.integrity", icon: "check" },
    ],
  },
  {
    label: "detail.group.manage",
    items: [{ id: "manage", label: "detail.manage", icon: "settings" }],
  },
];

const config = ref<InstanceConfig | null>(null);
/** Slider needs plain numbers even before the instance json lands. */
const memoryMin = computed(() => config.value?.min_memory_mb ?? 4096);
const memoryMax = computed(() => config.value?.max_memory_mb ?? 4096);
function setMemory(lo: number, hi: number) {
  if (!config.value) return;
  config.value.min_memory_mb = lo;
  config.value.max_memory_mb = hi;
}
const playtime = ref<{ seconds: number; launches: number; last: number | null } | null>(null);
const playtimeText = computed(() => {
  const value = playtime.value;
  if (!value || !value.launches) return "";
  const time = formatPlaytime(value.seconds);
  const date = formatDate(value.last);
  return date ? t("editor.playtimeDate", { time, date }) : t("editor.playtime", { time });
});
const java = ref<JavaView[]>([]);
const error = ref<string | null>(null);
const saving = ref(false);
const jvmArgs = ref("");
const gameArgs = ref("");
const envText = ref("");
const serverText = ref("");
const name = ref("");
const mods = ref<ModInfo[]>([]);
const modFilter = ref("");
const modOnlyDisabled = ref(false);
const modSort = ref<SortKey>("name");
const resourcePacks = ref<ContentFile[]>([]);
const packFilter = ref("");
const packSort = ref<PackSort>("manual");
/** Pack format the instance's game version expects, for the mismatch hint. */
const packExpectedFormat = ref<number | null>(null);
/** pack.png of each resource pack, as a blob URL. */
const packIcons = ref<Record<string, string>>({});
let packIconUrls: string[] = [];
const shaders = ref<ContentFile[]>([]);
const shaderFilter = ref("");
const shaderSort = ref<SortKey>("name");
const shaderToggle = ref(false);
const worlds = ref<WorldInfo[]>([]);
const datapacks = ref<DatapackInfo[]>([]);
const datapackWorld = ref<string | null>(null);
const datapackBusy = ref<string | null>(null);
const datapackStatus = ref<string | null>(null);
const worldBackups = ref<WorldBackup[]>([]);
const worldBusy = ref<string | null>(null);
const worldStatus = ref<string | null>(null);
const showBackups = ref(false);
const localBusy = ref<ProjectKind | null>(null);
const duplicateOpen = ref(false);
const duplicateName = ref("");
const duplicating = ref(false);
const exporting = ref(false);
const exportStatus = ref<string | null>(null);
const browseKind = ref<ProjectKind>("mod");
const toggling = ref<string | null>(null);
const removing = ref<string | null>(null);
const togglingContent = ref<string | null>(null);
const browsing = ref(false);
const modUpdates = ref<ModUpdate[]>([]);
const packUpdates = ref<ModUpdate[]>([]);
const shaderUpdates = ref<ModUpdate[]>([]);
const checking = ref<ProjectKind | null>(null);
const checkingPack = ref(false);
const updating = ref<string | null>(null);
const packUpdate = ref<PackUpdate | null>(null);
const packUpdating = ref<string | null>(null);
const modStatus = ref<string | null>(null);
const packStatus = ref<string | null>(null);
const shaderStatus = ref<string | null>(null);
const bundleStatus = ref<string | null>(null);
const modFile = ref("");
const {
  bytes: modBytes,
  total: modTotal,
  speed: modSpeed,
  track: trackMod,
  reset: resetMod,
} = useTransfer();
const {
  bytes: packBytes,
  total: packTotalBytes,
  speed: packSpeed,
  track: trackPack,
  reset: resetPack,
} = useTransfer();
const {
  bytes: loaderBytes,
  total: loaderTotal,
  speed: loaderSpeed,
  track: trackLoader,
  reset: resetLoader,
} = useTransfer();
const iconUrl = ref<string | null>(null);
const servers = ref<ServerEntry[]>([]);
const serverStatus = ref<string | null>(null);
/** Absolute game directory, resolved once the instance list is loaded. */
const gameDir = ref<string | null>(null);
const accounts = ref<AccountsResponse>({ current: null, accounts: [], client_id: false });
/** The null pick is no "launcher default": it *is* the current account. */
const accountCurrentLabel = computed(() => {
  const current = accounts.value.accounts.find((entry) => entry.id === accounts.value.current)?.name;
  return current ? t("editor.accountCurrent", { name: current }) : t("accounts.empty");
});

interface PingState {
  status: ServerStatus | null;
  error: string | null;
  loading: boolean;
}

const pings = ref<Record<string, PingState>>({});
const crashes = ref<CrashReport[]>([]);
const crashFile = ref<string | null>(null);
const crashText = ref<string | null>(null);
const crashAnalysis = ref<LogAnalysis | null>(null);
let crashRequest = 0;
const shots = ref<Screenshot[]>([]);
const shotPreview = ref<{ file: string; url: string } | null>(null);
const archiveStatus = ref<string | null>(null);
const archiving = ref(false);
const loaderStatus = ref<LoaderStatus | null>(null);
const loaderPick = ref<"vanilla" | LoaderKind>("vanilla");
const loaderVersion = ref<string | null>(null);
const loaderVersions = ref<LoaderView[]>([]);
const loaderLoading = ref(false);
const loaderBusy = ref(false);
const loaderChecking = ref(false);
const loaderUpdate = ref<LoaderUpdate | null>(null);
const loaderStatusText = ref<string | null>(null);
const loaderStage = ref<InstallEvent | null>(null);
let unlisten: UnlistenFn[] = [];

const javaChoices = computed<JavaView[]>(() => {
  const choices = [...java.value];
  const current = config.value?.java;
  if (current && !choices.some((runtime) => runtime.path === current)) {
    choices.push({ path: current, major: 0, version: "", vendor: null });
  }
  return choices;
});

const LOADER_LABELS: Record<LoaderKind, string> = {
  fabric: "Fabric",
  quilt: "Quilt",
  legacyfabric: "Legacy Fabric",
  forge: "Forge",
  neoforge: "NeoForge",
};

const LOADER_STAGES = ["manifest", "metadata", "libraries", "assets", "installer", "processors"];

const stableLoaderVersions = computed(() => loaderVersions.value.filter((entry) => entry.stable));
const betaLoaderVersions = computed(() => loaderVersions.value.filter((entry) => !entry.stable));
const loaderLabel = computed(() =>
  loaderPick.value === "vanilla" ? "Vanilla" : LOADER_LABELS[loaderPick.value],
);
const loaderStageLabel = computed(() => {
  const stage = loaderStage.value;
  if (!stage) return "";
  const name = LOADER_STAGES.includes(stage.stage) ? t(`install.stage.${stage.stage}`) : stage.stage;
  return t("editor.loaderProgress", { stage: name, done: stage.done, total: stage.total });
});

watch(loaderPick, (kind) => {
  loaderUpdate.value = null;
  if (kind === "vanilla") {
    loaderVersions.value = [];
    loaderVersion.value = null;
    return;
  }
  void loadLoaderBuilds(kind);
});

async function loadLoaderBuilds(kind: LoaderKind) {
  const game = loaderStatus.value?.game_version;
  if (!game) return;
  loaderLoading.value = true;
  loaderVersions.value = [];
  loaderVersion.value = null;
  try {
    loaderVersions.value = await api.listLoaders(game, kind);
    const current = loaderStatus.value?.kind === kind ? loaderStatus.value.version : null;
    loaderVersion.value =
      current && loaderVersions.value.some((entry) => entry.version === current)
        ? current
        : (loaderVersions.value.find((entry) => entry.stable)?.version ?? loaderVersions.value[0]?.version ?? null);
  } catch (cause) {
    error.value = String(cause);
  } finally {
    loaderLoading.value = false;
  }
}

async function reloadLoader() {
  try {
    const status = await api.loaderStatus(config.value?.name ?? props.name);
    loaderStatus.value = status;
    loaderPick.value = (status.kind ?? "vanilla") as "vanilla" | LoaderKind;
  } catch (cause) {
    error.value = String(cause);
  }
}

async function switchLoader(kind: "vanilla" | LoaderKind, version: string | null) {
  const instance = config.value?.name ?? props.name;
  if (props.running) {
    error.value = t("editor.runningLocked");
    return;
  }
  loaderBusy.value = true;
  loaderStage.value = null;
  resetLoader();
  loaderStatusText.value = null;
  loaderUpdate.value = null;
  try {
    const status =
      kind === "vanilla" ? await api.removeLoader(instance) : await api.installLoader(instance, kind, version);
    loaderStatus.value = status;
    if (config.value) config.value.version_id = status.version_id;
    loaderStatusText.value =
      kind === "vanilla"
        ? t("editor.loaderRemoved", { version: status.game_version })
        : t("editor.loaderSwitched", { loader: LOADER_LABELS[kind], version: status.version ?? version ?? "" });
    loaderPick.value = (status.kind ?? "vanilla") as "vanilla" | LoaderKind;
    mods.value = await api.listMods(instance);
  } catch (cause) {
    const message = String(cause);
    if (!maybePromptJava(message, () => void switchLoader(kind, version))) error.value = message;
  } finally {
    loaderBusy.value = false;
    loaderStage.value = null;
  }
}

function applyLoader() {
  if (loaderPick.value === "vanilla") {
    const status = loaderStatus.value;
    if (!status?.kind) return;
    const label = LOADER_LABELS[status.kind];
    if (!window.confirm(t("editor.loaderRemoveConfirm", { loader: label, version: status.game_version }))) return;
    void switchLoader("vanilla", null);
    return;
  }
  void switchLoader(loaderPick.value, loaderVersion.value);
}

function applyLoaderUpdate() {
  const update = loaderUpdate.value;
  if (update) void switchLoader(update.kind, update.latest);
}

async function checkLoaderUpdates() {
  loaderChecking.value = true;
  loaderUpdate.value = null;
  loaderStatusText.value = null;
  try {
    const update = await api.checkLoaderUpdate(config.value?.name ?? props.name);
    loaderUpdate.value = update;
    if (!update.newer) {
      loaderStatusText.value = t("editor.loaderUpToDate", {
        loader: LOADER_LABELS[update.kind],
        version: update.current,
      });
    }
  } catch (cause) {
    error.value = String(cause);
  } finally {
    loaderChecking.value = false;
  }
}

function onLoaderProgress({ job, event }: JobEvent<InstallEvent>) {
  if (job !== jobTag.loader(props.name) && job !== jobTag.repair(props.name)) return;
  if (repairBusy.value) {
    repairStage.value = event.stage;
    trackRepair(event.bytes, event.bytes_total ?? null);
    return;
  }
  if (!loaderBusy.value) return;
  loaderStage.value = event;
  trackLoader(event.bytes, event.bytes_total);
}

onMounted(async () => {
  for (const listen of [
    () => events.onModProgress(onProgress),
    () => events.onPackProgress(onPackProgress),
    () => events.onInstallProgress(onLoaderProgress),
  ]) {
    try {
      unlisten.push(await listen());
    } catch {
      // Progress bars are optional; the page itself must still load.
    }
  }
  try {
    config.value = await api.loadInstance(props.name);
    name.value = config.value.name;
    jvmArgs.value = config.value.jvm_args.join("\n");
    gameArgs.value = config.value.game_args.join("\n");
    envText.value = Object.entries(config.value.env ?? {})
      .map(([key, value]) => `${key}=${value}`)
      .join("\n");
    serverText.value = serverToText(config.value.server);
    await reloadLists();
    await loadIcon();
  } catch (cause) {
    error.value = String(cause);
  }
});

/** The toolbar refresh keeps the form as typed and only reloads derived lists. */
async function reloadLists() {
  const instance = config.value?.name ?? props.name;
  const boot = await api.bootstrap();
  java.value = boot.java;
  accounts.value = boot.accounts;
  const view = boot.instances.find((entry) => entry.name === instance);
  if (view) {
    playtime.value = { seconds: view.playtime_seconds, launches: view.launches, last: view.last_played };
    gameDir.value = view.game_dir;
  }
  mods.value = await api.listMods(instance);
  await Promise.all([
    reloadContent(),
    reloadWorlds(),
    reloadServers().then(pingAll),
    reloadMedia(),
    reloadLoader(),
  ]);
}

watch(
  () => props.reload,
  () => {
    if (!config.value) return;
    reloadLists().catch((cause) => (error.value = String(cause)));
  },
);

// The game writes new worlds behind our back, so re-read the folder whenever
// the saves tab is opened instead of trusting the list from page load.
watch(tab, (next) => {
  if (next === "worlds" && config.value) {
    reloadWorlds().catch((cause) => (error.value = String(cause)));
  }
});

onBeforeUnmount(() => {
  unlisten.forEach((off) => off());
  revokeIcon();
  revokeShot();
  clearPackIcons();
});

async function loadIcon() {
  try {
    const bytes = await api.instanceIcon(config.value?.name ?? props.name);
    revokeIcon();
    if (bytes.byteLength) iconUrl.value = imageUrl(bytes, config.value?.icon);
  } catch (cause) {
    error.value = String(cause);
  }
}

function revokeIcon() {
  if (iconUrl.value) URL.revokeObjectURL(iconUrl.value);
  iconUrl.value = null;
}

async function chooseIcon() {
  const path = await api.pickFile(t("editor.iconPicker"), ["png", "jpg", "jpeg", "webp", "gif"]);
  if (!path) return;
  try {
    // Do not reload the whole config here: it would throw away unsaved edits.
    const icon = await api.setInstanceIcon(config.value?.name ?? props.name, path);
    if (config.value) config.value.icon = icon;
    await loadIcon();
    // The library caches icons per name; without this the list keeps showing
    // the old cover until the launcher restarts.
    emit("changed");
  } catch (cause) {
    error.value = String(cause);
  }
}

async function clearIcon() {
  try {
    await api.clearInstanceIcon(config.value?.name ?? props.name);
    if (config.value) config.value.icon = null;
    revokeIcon();
    emit("changed");
  } catch (cause) {
    error.value = String(cause);
  }
}

async function reloadServers() {
  try {
    servers.value = (await api.listServers(config.value?.name ?? props.name)).map((entry) => ({ ...entry }));
    pings.value = {};
  } catch (cause) {
    error.value = String(cause);
  }
}

function addServer() {
  servers.value.push({ name: "", ip: "", accept_textures: true, index: null });
}

async function saveServers() {
  serverStatus.value = null;
  const entries = servers.value.filter((entry) => entry.ip.trim());
  try {
    servers.value = (await api.saveServers(config.value?.name ?? props.name, entries)).map((entry) => ({
      ...entry,
    }));
    serverStatus.value = t("editor.serversSaved");
    pingAll();
  } catch (cause) {
    error.value = String(cause);
  }
}

function splitAddress(text: string): { host: string; port: number } {
  const value = text.trim();
  if (value.startsWith("[")) {
    const end = value.indexOf("]");
    if (end > 0) {
      const rest = value.slice(end + 1);
      const port = rest.startsWith(":") ? Number(rest.slice(1)) : 25565;
      return { host: value.slice(1, end), port: port > 0 ? port : 25565 };
    }
  }
  const index = value.lastIndexOf(":");
  if (index > 0 && /^\d+$/.test(value.slice(index + 1))) {
    return { host: value.slice(0, index), port: Number(value.slice(index + 1)) };
  }
  return { host: value, port: 25565 };
}

function pingOf(server: ServerEntry): PingState | undefined {
  return pings.value[server.ip.trim()];
}

async function pingServer(server: ServerEntry) {
  if (!server.ip.trim()) return;
  // Results belong to the address, not the row: deleting or editing a row must
  // not move another server's answer onto it.
  const key = server.ip.trim();
  const { host, port } = splitAddress(server.ip);
  pings.value = { ...pings.value, [key]: { status: null, error: null, loading: true } };
  try {
    const status = await api.serverStatus(host, port);
    pings.value = { ...pings.value, [key]: { status, error: null, loading: false } };
  } catch (cause) {
    pings.value = { ...pings.value, [key]: { status: null, error: String(cause), loading: false } };
  }
}

function pingAll() {
  for (const entry of servers.value) {
    if (entry.ip.trim()) void pingServer(entry);
  }
}

async function reloadMedia() {
  const instance = config.value?.name ?? props.name;
  try {
    crashes.value = await api.listCrashReports(instance);
    shots.value = await api.listScreenshots(instance);
  } catch (cause) {
    error.value = String(cause);
  }
}

async function showCrash(report: CrashReport) {
  const token = ++crashRequest;
  try {
    const text = await api.readCrashReport(config.value?.name ?? props.name, report.file);
    if (token !== crashRequest) return;
    crashText.value = text;
    crashFile.value = report.file;
    crashAnalysis.value = null;
    const analysis = await api.analyzeLog(text);
    if (token !== crashRequest) return;
    crashAnalysis.value = analysis;
  } catch (cause) {
    if (token !== crashRequest) return;
    error.value = String(cause);
  }
}

async function copyCrashReport() {
  if (crashAnalysis.value) await navigator.clipboard.writeText(findingsReport(crashAnalysis.value.findings));
}

async function deleteCrash(report: CrashReport) {
  if (!window.confirm(t("editor.crashDeleteConfirm", { file: report.file }))) return;
  try {
    await api.deleteCrashReport(config.value?.name ?? props.name, report.file);
    if (crashFile.value === report.file) {
      crashFile.value = null;
      crashText.value = null;
      crashAnalysis.value = null;
    }
    await reloadMedia();
  } catch (cause) {
    error.value = String(cause);
  }
}

async function viewShot(shot: Screenshot) {
  try {
    const bytes = await api.readScreenshot(config.value?.name ?? props.name, shot.file);
    revokeShot();
    shotPreview.value = { file: shot.file, url: imageUrl(bytes, shot.file) };
  } catch (cause) {
    error.value = String(cause);
  }
}

function revokeShot() {
  if (shotPreview.value) URL.revokeObjectURL(shotPreview.value.url);
  shotPreview.value = null;
}

async function deleteShot(shot: Screenshot) {
  if (!window.confirm(t("editor.screenshotDeleteConfirm", { file: shot.file }))) return;
  try {
    await api.deleteScreenshot(config.value?.name ?? props.name, shot.file);
    if (shotPreview.value?.file === shot.file) revokeShot();
    await reloadMedia();
  } catch (cause) {
    error.value = String(cause);
  }
}

function applyJvmPreset(preset: "g1" | "aikar" | "zgc") {
  const presets: Record<string, string[]> = {
    g1: ["-XX:+UseG1GC", "-XX:+ParallelRefProcEnabled", "-XX:MaxGCPauseMillis=200"],
    aikar: [
      "-XX:+UseG1GC",
      "-XX:+ParallelRefProcEnabled",
      "-XX:MaxGCPauseMillis=200",
      "-XX:+UnlockExperimentalVMOptions",
      "-XX:+DisableExplicitGC",
      "-XX:+AlwaysPreTouch",
      "-XX:G1NewSizePercent=30",
      "-XX:G1MaxNewSizePercent=40",
      "-XX:G1HeapRegionSize=8M",
      "-XX:G1ReservePercent=20",
      "-XX:G1HeapWastePercent=5",
      "-XX:G1MixedGCCountTarget=4",
      "-XX:InitiatingHeapOccupancyPercent=15",
      "-XX:G1MixedGCLiveThresholdPercent=90",
      "-XX:G1RSetUpdatingPauseTimePercent=5",
      "-XX:SurvivorRatio=32",
      "-XX:+PerfDisableSharedMem",
      "-XX:MaxTenuringThreshold=1",
    ],
    zgc: ["-XX:+UseZGC", "-XX:+ZGenerational"],
  };
  jvmArgs.value = presets[preset].join("\n");
}

async function exportArchive() {
  const target = await api.pickSaveFile(t("editor.exportArchiveTitle"), `${props.name}.zip`, ["zip"]);
  if (!target) return;
  archiving.value = true;
  archiveStatus.value = null;
  try {
    const report = await api.exportArchive(props.name, target);
    const file = target.split(/[\\/]/).pop() ?? target;
    archiveStatus.value = t("editor.archived", {
      file,
      files: report.files,
      size: formatBytes(report.bytes),
    });
  } catch (cause) {
    error.value = String(cause);
  } finally {
    archiving.value = false;
  }
}

function onProgress(event: ModProgress) {
  if (event.stage === "downloading") {
    const size = event.total ? `${formatBytes(event.done)} / ${formatBytes(event.total)}` : formatBytes(event.done);
    modFile.value = event.file;
    trackMod(event.done, event.total);
    modStatus.value = t("modrinth.progress", { name: event.project, size });
  } else if (event.stage === "resolving") {
    resetMod();
    modStatus.value = t("modrinth.resolving", { name: event.project });
  }
}

function onPackProgress({ job, event }: JobEvent<PackProgress>) {
  if (job !== jobTag.update(props.name) && job !== jobTag.repair(props.name)) return;
  if (repairBusy.value && event.stage === "downloading") {
    repairStage.value = t("editor.repairStagePack");
    trackRepair(event.bytes_done, event.bytes_total || null);
    return;
  }
  if (repairBusy.value) return;
  if (event.stage === "archive") {
    trackPack(event.done, event.total);
    bundleStatus.value = t("modpack.phaseArchive");
  } else if (event.stage === "downloading") {
    const size = event.total ? `${formatBytes(event.done)} / ${formatBytes(event.total)}` : formatBytes(event.done);
    trackPack(event.bytes_done, event.bytes_total || null);
    bundleStatus.value = t("editor.packDownloading", { size });
  } else if (event.stage === "extracting") {
    bundleStatus.value = t("editor.packOverrides", { prefix: event.prefix, count: event.entries });
  }
}

async function reloadMods() {
  try {
    mods.value = await api.listMods(config.value?.name ?? props.name);
  } catch (cause) {
    error.value = String(cause);
  }
}

function clearPackIcons() {
  for (const url of packIconUrls) URL.revokeObjectURL(url);
  packIconUrls = [];
  packIcons.value = {};
}

async function loadPackIcons(instance: string, files: ContentFile[]) {
  clearPackIcons();
  const icons: Record<string, string> = {};
  for (const file of files) {
    try {
      const bytes = await api.contentIcon(instance, "resourcepack", file.file);
      if (bytes.byteLength) {
        const url = imageUrl(bytes, "png");
        packIconUrls.push(url);
        icons[file.file] = url;
      }
    } catch {
      // A pack without pack.png simply has no icon.
    }
  }
  packIcons.value = icons;
}

async function reloadContent() {
  const instance = config.value?.name ?? props.name;
  try {
    const packs = await api.listContent(instance, "resourcepack");
    resourcePacks.value = packs.files;
    packExpectedFormat.value = packs.expected_format ?? null;
    void loadPackIcons(instance, packs.files);
    const shaderListing = await api.listContent(instance, "shader");
    shaders.value = shaderListing.files;
    shaderToggle.value = shaderListing.toggle_supported;
  } catch (cause) {
    error.value = String(cause);
  }
}

function openBrowse(kind: ProjectKind) {
  browseKind.value = kind;
  browsing.value = true;
}
async function onBrowsed() {
  await Promise.all([reloadMods(), reloadContent()]);
}

function serverToText(server: InstanceConfig["server"]): string {
  if (!server) return "";
  return server.port === 25565 ? server.host : `${server.host}:${server.port}`;
}

function textToServer(text: string): InstanceConfig["server"] {
  const value = text.trim();
  if (!value) return null;
  const match = value.match(/^(.*):(\d+)$/);
  if (match && match[1] && !match[1].endsWith(":")) {
    return { host: match[1], port: Number(match[2]) };
  }
  return { host: value, port: 25565 };
}

function parseEnv(text: string): Record<string, string> {
  const env: Record<string, string> = {};
  for (const line of text.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("#")) continue;
    const index = trimmed.indexOf("=");
    if (index <= 0) continue;
    env[trimmed.slice(0, index).trim()] = trimmed.slice(index + 1);
  }
  return env;
}

async function reloadWorlds() {
  const instance = config.value?.name ?? props.name;
  try {
    worlds.value = await api.listWorlds(instance);
    worldBackups.value = await api.listWorldBackups(instance);
    if (!datapackWorld.value || !worlds.value.some((world) => world.dir === datapackWorld.value)) {
      datapackWorld.value = worlds.value[0]?.dir ?? null;
    }
    await reloadDatapacks();
  } catch (cause) {
    error.value = String(cause);
  }
}

async function reloadDatapacks() {
  const instance = config.value?.name ?? props.name;
  const world = datapackWorld.value;
  if (!world) {
    datapacks.value = [];
    return;
  }
  try {
    datapacks.value = await api.listDatapacks(instance, world);
  } catch (cause) {
    error.value = String(cause);
  }
}

async function toggleDatapack(pack: DatapackInfo) {
  if (!datapackWorld.value) return;
  datapackBusy.value = pack.name;
  datapackStatus.value = null;
  try {
    datapacks.value = await api.setDatapackEnabled(
      config.value?.name ?? props.name,
      datapackWorld.value,
      pack.name,
      !pack.enabled,
    );
  } catch (cause) {
    error.value = String(cause);
  } finally {
    datapackBusy.value = null;
  }
}

async function addDatapack() {
  if (!datapackWorld.value) return;
  const path = await api.pickFile(t("editor.addDatapack"), ["zip"]);
  if (!path) return;
  datapackBusy.value = "add";
  datapackStatus.value = null;
  try {
    datapacks.value = await api.addDatapack(config.value?.name ?? props.name, datapackWorld.value, path);
    datapackStatus.value = t("editor.datapackAdded", { file: path.split(/[\\/]/).pop() ?? path });
  } catch (cause) {
    error.value = String(cause);
  } finally {
    datapackBusy.value = null;
  }
}

async function deleteDatapack(pack: DatapackInfo) {
  if (!datapackWorld.value) return;
  if (!window.confirm(t("editor.datapackDeleteConfirm", { file: pack.name }))) return;
  datapackBusy.value = pack.name;
  datapackStatus.value = null;
  try {
    datapacks.value = await api.deleteDatapack(
      config.value?.name ?? props.name,
      datapackWorld.value,
      pack.name,
    );
  } catch (cause) {
    error.value = String(cause);
  } finally {
    datapackBusy.value = null;
  }
}

function openInGameDir(relative = "") {
  api
    .openInstanceFolder(props.name, relative || undefined)
    .catch((cause) => (error.value = String(cause)));
}

function openDatapacks() {
  if (!datapackWorld.value) return;
  openInGameDir(`saves/${datapackWorld.value}`);
}

function worldPlayed(world: WorldInfo): string {
  if (!world.last_played_ms) return "";
  const age = Math.max(0, Date.now() - world.last_played_ms) / 1000;
  if (age < 3600) return t("editor.minutesAgo", { count: Math.max(1, Math.round(age / 60)) });
  if (age < 86400) return t("editor.hoursAgo", { count: Math.round(age / 3600) });
  return t("editor.daysAgo", { count: Math.round(age / 86400) });
}

async function backupWorld(world: WorldInfo) {
  worldBusy.value = world.dir;
  worldStatus.value = null;
  try {
    const path = await api.backupWorld(config.value?.name ?? props.name, world.dir);
    worldStatus.value = t("editor.worldBackedUp", { file: path.split(/[\\/]/).pop() ?? path });
    worldBackups.value = await api.listWorldBackups(config.value?.name ?? props.name);
  } catch (cause) {
    error.value = String(cause);
  } finally {
    worldBusy.value = null;
  }
}

async function duplicateWorld(world: WorldInfo) {
  worldBusy.value = world.dir;
  worldStatus.value = null;
  try {
    const copy = await api.duplicateWorld(config.value?.name ?? props.name, world.dir);
    worldStatus.value = t("editor.worldDuplicated", { name: copy.dir });
    await reloadWorlds();
  } catch (cause) {
    error.value = String(cause);
  } finally {
    worldBusy.value = null;
  }
}

async function deleteWorld(world: WorldInfo) {  if (!window.confirm(t("editor.worldDeleteConfirm", { name: world.name }))) return;
  worldBusy.value = world.dir;
  try {
    await api.deleteWorld(config.value?.name ?? props.name, world.dir);
    await reloadWorlds();
    worldStatus.value = t("editor.worldDeleted", { name: world.name });
  } catch (cause) {
    error.value = String(cause);
  } finally {
    worldBusy.value = null;
  }
}

async function importWorld() {
  const path = await api.pickFile(t("editor.importWorld"), ["zip"]);
  if (!path) return;
  worldBusy.value = "import";
  try {
    const world = await api.importWorld(config.value?.name ?? props.name, path);
    await reloadWorlds();
    worldStatus.value = t("editor.worldImported", { name: world.name });
  } catch (cause) {
    error.value = String(cause);
  } finally {
    worldBusy.value = null;
  }
}

async function restoreWorld(backup: WorldBackup) {
  // The backend imports a world; it cannot replace the damaged save, so say so.
  if (!window.confirm(t("editor.worldRestoreConfirm", { name: backup.file }))) return;
  worldBusy.value = backup.file;
  try {
    const world = await api.importWorld(config.value?.name ?? props.name, backup.path);
    await reloadWorlds();
    worldStatus.value = t("editor.worldRestored", { name: world.name });
  } catch (cause) {
    error.value = String(cause);
  } finally {
    worldBusy.value = null;
  }
}

async function deleteBackup(backup: WorldBackup) {
  if (!window.confirm(t("editor.backupDeleteConfirm", { name: backup.file }))) return;
  worldBusy.value = backup.file;
  worldStatus.value = null;
  try {
    worldBackups.value = await api.deleteWorldBackup(config.value?.name ?? props.name, backup.file);
    worldStatus.value = t("editor.backupDeleted", { file: backup.file });
  } catch (cause) {
    error.value = String(cause);
  } finally {
    worldBusy.value = null;
  }
}

function openSaves() {
  openInGameDir("saves");
}

async function addLocal(kind: ProjectKind) {
  const extensions = kind === "mod" ? ["jar"] : ["zip", "jar"];
  const path = await api.pickFile(t("editor.addFile"), extensions);
  if (!path) return;
  localBusy.value = kind;
  try {
    if (kind === "mod") {
      mods.value = await api.addLocalMod(config.value?.name ?? props.name, path);
    } else {
      const listing = await api.addLocalContent(config.value?.name ?? props.name, kind, path);
      if (kind === "resourcepack") resourcePacks.value = listing.files;
      else shaders.value = listing.files;
      shaderToggle.value = listing.toggle_supported;
    }
    await reloadContent();
  } catch (cause) {
    error.value = String(cause);
    // The install guard may have switched the jar off before failing.
    if (kind === "mod") {
      mods.value = await api.listMods(config.value?.name ?? props.name).catch(() => mods.value);
    }
  } finally {
    localBusy.value = null;
  }
}

async function pickJava() {
  const path = await api.pickFile(t("editor.javaPicker"), []);
  if (path && config.value) config.value.java = path;
}

async function duplicate() {
  const target = duplicateName.value.trim();
  if (!target) return;
  duplicating.value = true;
  try {
    await api.duplicateInstance(props.name, target);
    duplicateOpen.value = false;
    duplicateName.value = "";
    emit("duplicated", target);
  } catch (cause) {
    error.value = String(cause);
  } finally {
    duplicating.value = false;
  }
}

async function exportPack() {
  const target = await api.pickSaveFile(t("editor.exportPack"), `${props.name}.mrpack`, ["mrpack"]);
  if (!target) return;
  exporting.value = true;
  exportStatus.value = null;
  try {
    const report = await api.exportInstance(props.name, target, name.value.trim() || props.name, "2");
    const file = target.split(/[\\/]/).pop() ?? target;
    exportStatus.value = t("editor.exported", { file, files: report.files, size: formatBytes(report.bytes) });
  } catch (cause) {
    error.value = String(cause);
  } finally {
    exporting.value = false;
  }
}

function updatesOf(kind: ProjectKind): ModUpdate[] {
  return kind === "mod" ? modUpdates.value : kind === "resourcepack" ? packUpdates.value : shaderUpdates.value;
}

function setUpdates(kind: ProjectKind, list: ModUpdate[]) {
  if (kind === "mod") modUpdates.value = list;
  else if (kind === "resourcepack") packUpdates.value = list;
  else shaderUpdates.value = list;
}

function setStatus(kind: ProjectKind, text: string | null) {
  if (kind === "mod") modStatus.value = text;
  else if (kind === "resourcepack") packStatus.value = text;
  else shaderStatus.value = text;
}

async function checkKind(kind: ProjectKind) {
  checking.value = kind;
  error.value = null;
  setStatus(kind, t("editor.checkingModrinth"));
  try {
    const list = await api.checkUpdates(props.name, kind);
    setUpdates(kind, list);
    setStatus(kind, list.length ? null : t("editor.noUpdates"));
  } catch (cause) {
    error.value = String(cause);
    setStatus(kind, null);
  } finally {
    checking.value = null;
  }
}

async function applyUpdates(kind: ProjectKind, files: string[]) {
  updating.value = files.length === 1 ? files[0] : "*";
  if (kind === "mod") resetMod();
  error.value = null;
  try {
    const applied = await api.updateContent(props.name, kind, files);
    setStatus(kind, applied.length ? t("editor.updated", { count: applied.length }) : t("editor.nothingToUpdate"));
    setUpdates(kind, files.length ? updatesOf(kind).filter((update) => !files.includes(update.file)) : []);
    if (kind === "mod") await reloadMods();
    else await reloadContent();
  } catch (cause) {
    error.value = String(cause);
  } finally {
    updating.value = null;
  }
}

async function toggleMod(mod: ModInfo) {
  if (props.running) return;
  toggling.value = mod.file;
  error.value = null;
  try {
    mods.value = await api.setModEnabled(props.name, mod.file, !mod.enabled);
  } catch (cause) {
    error.value = String(cause);
  } finally {
    toggling.value = null;
  }
}

async function removeMod(mod: ModInfo) {
  if (props.running || removing.value) return;
  if (!window.confirm(t("editor.confirmRemoveMod", { name: mod.name }))) return;
  removing.value = mod.file;
  error.value = null;
  try {
    mods.value = await api.removeMod(props.name, mod.file);
  } catch (cause) {
    error.value = String(cause);
  } finally {
    removing.value = null;
  }
}

const depsBusy = ref(false);
const depsStatus = ref<string | null>(null);

const repairBusy = ref(false);
const repairStatus = ref<string | null>(null);
const repairStage = ref("");
const {
  bytes: repairBytes,
  total: repairTotal,
  speed: repairSpeed,
  track: trackRepair,
  reset: resetRepair,
} = useTransfer();

const busy = computed(
  () => repairBusy.value || updating.value !== null || loaderBusy.value || depsBusy.value,
);

async function repairInstance() {
  if (props.running || repairBusy.value) return;
  repairBusy.value = true;
  repairStatus.value = null;
  repairStage.value = "";
  error.value = null;
  resetRepair();
  try {
    const report = await api.repairInstance(props.name);
    const status = report.fetched
      ? t("editor.repairFixed", {
          checked: report.checked,
          fetched: report.fetched,
          size: formatBytes(report.bytes),
        })
      : t("editor.repairClean", { checked: report.checked });
    repairStatus.value = report.loader_rebuilt ? `${status} ${t("editor.repairLoader")}` : status;
    mods.value = await api.listMods(props.name);
    await reloadContent();
  } catch (cause) {
    error.value = String(cause);
  } finally {
    repairBusy.value = false;
  }
}

const doctorOpen = ref(false);
const doctorBusy = ref(false);
const doctorReport = ref<DiagnosticReport | null>(null);
const doctorError = ref<string | null>(null);
const doctorAction = ref<string | null>(null);

async function runDoctor() {
  doctorBusy.value = true;
  doctorError.value = null;
  try {
    doctorReport.value = await api.diagnoseInstance(props.name);
  } catch (cause) {
    doctorError.value = String(cause);
  } finally {
    doctorBusy.value = false;
  }
}

function openDoctor() {
  doctorOpen.value = true;
  void runDoctor();
}

async function onDoctorAction(kind: "deps" | "versions" | "repair" | "mods" | "log") {
  if (kind === "mods") {
    openInGameDir("mods");
    return;
  }
  if (kind === "log") {
    // The console opens at a lower z-index than this dialog; keep the dialog
    // on top and the button looks dead.
    doctorOpen.value = false;
    emit("log", props.name);
    return;
  }
  doctorAction.value = kind;
  try {
    if (kind === "deps") await installMissingDeps();
    else if (kind === "versions") await fixDependencyVersions();
    else await repairInstance();
    await runDoctor();
  } finally {
    doctorAction.value = null;
  }
}

const modsEnabled = computed(() => mods.value.filter((mod) => mod.enabled).length);

const blockedMods = computed(() => mods.value.filter((mod) => mod.blocked));

function blockedText(blocked: ModInfo["blocked"]): string {
  if (!blocked) return "";
  return t("editor.blockedMod", {
    loader: blocked.loader,
    required: blocked.required.join(" / "),
    actual: blocked.actual,
  });
}

type SortKey = "name" | "size" | "modified";
type PackSort = SortKey | "manual";

function sortBy<T>(
  key: SortKey,
  name: (entry: T) => string,
  size: (entry: T) => number,
  modified: (entry: T) => number,
): (left: T, right: T) => number {
  return (left, right) => {
    if (key === "size") return size(right) - size(left);
    if (key === "modified") return modified(right) - modified(left);
    return name(left).localeCompare(name(right), undefined, { numeric: true, sensitivity: "base" });
  };
}

function matchFiles(files: ContentFile[], needle: string): ContentFile[] {
  const query = needle.trim().toLowerCase();
  if (!query) return files;
  return files.filter((file) => file.file.toLowerCase().includes(query));
}

const filteredMods = computed(() => {
  const needle = modFilter.value.trim().toLowerCase();
  const list = mods.value.filter((mod) => {
    if (modOnlyDisabled.value && mod.enabled) return false;
    if (!needle) return true;
    return (
      mod.name.toLowerCase().includes(needle) ||
      mod.file.toLowerCase().includes(needle) ||
      mod.version.toLowerCase().includes(needle) ||
      mod.loader.toLowerCase().includes(needle)
    );
  });
  return [...list].sort(sortBy<ModInfo>(modSort.value, (mod) => mod.name, (mod) => mod.size, (mod) => mod.modified));
});

const filteredPacks = computed(() => {
  const list = matchFiles(resourcePacks.value, packFilter.value);
  // The backend order is the pack load order; sorting by name hid the result
  // of the up/down buttons, so "custom" keeps that order untouched.
  if (packSort.value === "manual") return list;
  return [...list].sort(sortBy<ContentFile>(packSort.value, (pack) => pack.file, (pack) => pack.bytes, (pack) => pack.modified));
});

const filteredShaders = computed(() => {
  const list = matchFiles(shaders.value, shaderFilter.value);
  return [...list].sort(sortBy<ContentFile>(shaderSort.value, (shader) => shader.file, (shader) => shader.bytes, (shader) => shader.modified));
});

const missingDeps = computed(() => {
  const ids = new Set<string>();
  for (const mod of mods.value) {
    for (const id of mod.missing_dependencies) ids.add(id);
  }
  return [...ids];
});

function tabCount(id: Tab): number | null {
  switch (id) {
    case "mods":
      return mods.value.length || null;
    case "resourcepacks":
      return resourcePacks.value.length || null;
    case "shaders":
      return shaders.value.length || null;
    case "worlds":
      return worlds.value.length || null;
    case "datapacks":
      return datapacks.value.length || null;
    case "servers":
      return servers.value.length || null;
    case "logs":
      return crashes.value.length + shots.value.length || null;
    default:
      return null;
  }
}

function openGameDir() {
  openInGameDir();
}

function removeInstance() {
  emit("remove", props.name);
}

/** One-click "fill in whatever prerequisites are missing" (catalogue-backed). */
async function installMissingDeps() {
  if (props.running) return;
  depsBusy.value = true;
  depsStatus.value = null;
  resetMod();
  try {
    const report = await api.installMissingDeps(props.name);
    mods.value = await api.listMods(props.name);
    depsStatus.value = report.unresolved.length
      ? t("editor.depsUnresolved", { list: report.unresolved.join(", ") })
      : report.installed.length
        ? t("editor.depsInstalled", { count: report.installed.length })
        : t("editor.depsNone");
  } catch (cause) {
    error.value = String(cause);
  } finally {
    depsBusy.value = false;
  }
}

async function fixDependencyVersions() {
  if (props.running) return;
  depsBusy.value = true;
  depsStatus.value = null;
  resetMod();
  try {
    const report = await api.fixDependencyVersions(props.name);
    mods.value = await api.listMods(props.name);
    depsStatus.value = report.updated.length
      ? t("editor.versionsFixed", { count: report.updated.length })
      : report.unresolved.length
        ? t("editor.versionsUnresolved", { list: report.unresolved.join(", ") })
        : t("editor.versionsNone");
  } catch (cause) {
    error.value = String(cause);
  } finally {
    depsBusy.value = false;
  }
}

function fixFinding(kind: "deps" | "versions") {
  void (kind === "versions" ? fixDependencyVersions() : installMissingDeps());
}

async function toggleContent(kind: "resourcepack" | "shader", file: ContentFile) {
  if (props.running) return;
  togglingContent.value = file.file;
  error.value = null;
  try {
    const listing = await api.setContentEnabled(props.name, kind, file.file, !file.enabled);
    if (kind === "resourcepack") resourcePacks.value = listing.files;
    else shaders.value = listing.files;
  } catch (cause) {
    error.value = String(cause);
  } finally {
    togglingContent.value = null;
  }
}

async function movePack(pack: ContentFile, up: boolean) {
  if (props.running || togglingContent.value) return;
  togglingContent.value = pack.file;
  error.value = null;
  try {
    resourcePacks.value = (await api.moveResourcePack(props.name, pack.file, up)).files;
  } catch (cause) {
    error.value = String(cause);
  } finally {
    togglingContent.value = null;
  }
}

async function removeContent(kind: "resourcepack" | "shader", file: ContentFile) {
  if (props.running) return;
  if (!window.confirm(t("editor.confirmRemoveContent", { file: file.file }))) return;
  togglingContent.value = file.file;
  error.value = null;
  try {
    const listing = await api.removeContent(props.name, kind, file.file);
    if (kind === "resourcepack") {
      resourcePacks.value = listing.files;
      void loadPackIcons(props.name, listing.files);
    } else {
      shaders.value = listing.files;
    }
  } catch (cause) {
    error.value = String(cause);
  } finally {
    togglingContent.value = null;
  }
}

/** Packs built for another game version still load, but after a warning. */
function packMismatch(pack: ContentFile): boolean {
  const expected = packExpectedFormat.value;
  const format = pack.format ?? null;
  if (!expected || format === null) return false;
  if (format === expected) return false;
  const min = pack.min_format ?? null;
  const max = pack.max_format ?? null;
  if (min !== null && max !== null && expected >= min && expected <= max) return false;
  return true;
}

function packMismatchText(pack: ContentFile): string {
  return t("editor.packVersionMismatch", {
    format: pack.format ?? "?",
    expected: packExpectedFormat.value ?? "?",
  });
}

const enabledPacks = computed(() => resourcePacks.value.filter((pack) => pack.enabled).length);

async function checkPackUpdate() {
  checkingPack.value = true;
  error.value = null;
  bundleStatus.value = t("editor.checkingModrinth");
  try {
    const update = await api.checkPackUpdate(props.name);
    packUpdate.value = update;
    bundleStatus.value = update.targets.length
      ? t("editor.packUpdateAvailable", { current: update.current_version })
      : t("editor.packUpToDate");
  } catch (cause) {
    error.value = String(cause);
    bundleStatus.value = null;
  } finally {
    checkingPack.value = false;
  }
}

async function applyPackUpdate(target: PackTarget) {
  packUpdating.value = target.version_id;
  resetPack();
  error.value = null;
  bundleStatus.value = t("editor.packUpdating", { version: target.version_number });
  try {
    const report = await api.updatePack(props.name, target.version_id);
    bundleStatus.value = t("editor.packUpdated", { version: report.version_number });
    packUpdate.value = null;
    // Only the fields the update touched: reloading the whole config would
    // throw away unsaved edits in the other tabs.
    const fresh = await api.loadInstance(props.name);
    if (config.value) {
      config.value.version_id = fresh.version_id;
      config.value.game_dir = fresh.game_dir;
    }
    await Promise.all([reloadMods(), reloadContent(), reloadLoader()]);
    emit("saved", config.value?.name ?? props.name);
  } catch (cause) {
    const message = String(cause);
    if (!maybePromptJava(message, () => void applyPackUpdate(target))) error.value = message;
    bundleStatus.value = null;
  } finally {
    packUpdating.value = null;
  }
}

function splitArgs(text: string): string[] {
  return text
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean);
}

function positiveInt(value: unknown, min: number, max: number): number | null {
  const number = Number(value);
  if (!Number.isFinite(number) || number <= 0) return null;
  return Math.min(max, Math.max(min, Math.round(number)));
}

async function save() {
  if (!config.value || props.running) return;
  saving.value = true;
  error.value = null;
  try {
    const nextName = name.value.trim() || props.name;
    if (nextName !== props.name) {
      await api.renameInstance(props.name, nextName);
      config.value.name = nextName;
      name.value = nextName;
    }
    await api.saveInstance({
      ...config.value,
      java: config.value.java?.trim() || null,
      jvm_args: splitArgs(jvmArgs.value),
      game_args: splitArgs(gameArgs.value),
      env: parseEnv(envText.value),
      server: textToServer(serverText.value),
      min_memory_mb: positiveInt(config.value.min_memory_mb, 128, 65536),
      max_memory_mb: Math.max(512, Math.round(Number(config.value.max_memory_mb) || 4096)),
      width: positiveInt(config.value.width, 320, 16384),
      height: positiveInt(config.value.height, 240, 16384),
      group: config.value.group?.trim() || null,
      notes: config.value.notes?.trim() ?? "",
    });
    emit("saved", nextName);
  } catch (cause) {
    error.value = String(cause);
  } finally {
    saving.value = false;
  }
}
</script>

<template>
  <section class="detail">
    <header class="detail-head">
      <button class="icon back" :title="t('detail.back')" @click="$emit('close')">
        <Icon name="chevron" />
      </button>
      <div class="head-art">
        <img v-if="iconUrl" :src="iconUrl" alt="" />
        <Icon v-else name="cube" :size="26" />
      </div>
      <div class="head-text">
        <h1>{{ name }}</h1>
        <div class="chips">
          <span class="badge version mono">{{ config?.version_id ?? "…" }}</span>
          <span v-if="loaderStatus?.kind" class="badge">
            {{ LOADER_LABELS[loaderStatus.kind] }} {{ loaderStatus.version }}
          </span>
          <span v-else class="badge">Vanilla</span>
          <span v-if="modpack" class="badge"><Icon name="package" :size="12" />{{ modpack }}</span>
          <span v-if="playtimeText" class="badge"><Icon name="clock" :size="12" />{{ playtimeText }}</span>
          <span v-if="running" class="badge ok">{{ t("detail.running") }}</span>
        </div>
      </div>
      <div class="head-actions">
        <button class="ghost" @click="$emit('log', props.name)">
          <Icon name="terminal" :size="15" />{{ t("detail.logs") }}
        </button>
        <button
          v-if="missingDeps.length"
          class="ghost fix-deps"
          :disabled="depsBusy || running"
          @click="installMissingDeps"
        >
          <Icon name="download" :size="15" />
          {{ depsBusy ? t("editor.working") : t("editor.fillDeps", { count: missingDeps.length }) }}
        </button>
        <button class="ghost" :disabled="doctorBusy" @click="openDoctor">
          <Icon name="diagnosis" :size="15" />{{ doctorBusy ? t("doctor.checking") : t("doctor.check") }}
        </button>
        <button v-if="running" class="danger" @click="$emit('stop')">
          <Icon name="stop" :size="14" />{{ t("detail.stop") }}
        </button>
        <button
          v-else
          class="primary play"
          :disabled="props.busy"
          @click="$emit('launch', props.name)"
        >
          <Icon name="play" :size="16" />
          {{ props.busy ? t("instances.starting") : t("detail.play") }}
        </button>
      </div>
    </header>

    <div class="detail-body">
      <nav class="side">
        <div class="side-scroll scroll">
          <template v-for="group in TAB_GROUPS" :key="group.label">
            <p class="side-label">{{ t(group.label) }}</p>
            <button
              v-for="item in group.items"
              :key="item.id"
              class="side-item"
              :class="{ active: tab === item.id }"
              @click="tab = item.id"
            >
              <Icon :name="item.icon" :size="16" />
              <span>{{ t(item.label) }}</span>
              <span v-if="tabCount(item.id)" class="count">{{ tabCount(item.id) }}</span>
            </button>
          </template>
        </div>
        <div class="side-foot">
          <button class="primary" :disabled="saving || !config || running" @click="save">
            {{ saving ? t("editor.saving") : t("editor.save") }}
          </button>
        </div>
      </nav>

      <div class="panel scroll">
        <div v-if="error" class="error error-banner">
          <span class="error-text">{{ error }}</span>
          <button class="ghost error-close" type="button" :title="t('action.close')" @click="error = null">✕</button>
        </div>
        <p v-if="running" class="warn-note"><Icon name="alert" :size="15" />{{ t("editor.runningLocked") }}</p>

        <div v-if="config" class="pane-body">
        <div v-if="tab === 'overview' && modpack" class="group">
          <div class="group-head">
            <h3>
              {{ t("editor.modpack") }}
              <span class="count">{{ modpack }}</span>
            </h3>
            <button class="ghost small" :disabled="checkingPack || packUpdating !== null" @click="checkPackUpdate">
              <Icon name="refresh" :size="13" /> {{ checkingPack ? t("editor.checking") : t("editor.checkPackUpdate") }}
            </button>
          </div>
          <div v-if="packUpdate?.targets.length" class="updates">
            <div class="updates-head">
              <span>{{ t("editor.packUpdateAvailable", { current: packUpdate.current_version }) }}</span>
            </div>
            <ul>
              <li v-for="target in packUpdate.targets" :key="target.version_id">
                <div class="mod-text">
                  <strong>{{ target.version_number }}</strong>
                  <small class="mono">
                    {{ t("modpack.gameVersion", { version: target.game_version }) }} · {{ target.version_type }} ·
                    <template v-if="target.size">{{ formatBytes(target.size) }}</template>
                    <template v-else>{{ t("editor.sizeUnknown") }}</template>
                  </small>
                  <small v-if="!target.in_place">{{ t("editor.packNewGame") }}</small>
                </div>
                <button
                  class="ghost small"
                  :disabled="packUpdating !== null"
                  @click="applyPackUpdate(target)"
                >
                  {{ packUpdating === target.version_id ? t("editor.updating") : t("editor.update") }}
                </button>
              </li>
            </ul>
          </div>
          <TransferBar
            v-if="packUpdating !== null"
            :label="t('editor.packDownloading', { size: '' })"
            :done="packBytes"
            :total="packTotalBytes"
            :speed="packSpeed"
            :detail="bundleStatus ?? ''"
          />
          <p v-else-if="bundleStatus" class="hint">{{ bundleStatus }}</p>
        </div>

        <div v-if="tab === 'overview'" class="group">
          <h3>{{ t("editor.identity") }}</h3>
          <div class="identity-row">
            <div class="icon-preview">
              <img v-if="iconUrl" :src="iconUrl" alt="" />
              <Icon v-else name="cube" :size="26" />
            </div>
            <div class="identity-fields">
              <div class="grid">
                <label>
                  <span>{{ t("editor.group") }} <small>{{ t("editor.groupHint") }}</small></span>
                  <input v-model="config.group" spellcheck="false" :placeholder="t('editor.groupPlaceholder')" />
                </label>
              </div>
              <div class="field-row">
                <button class="ghost small" type="button" @click="chooseIcon">
                  <Icon name="image" :size="13" /> {{ t("editor.chooseIcon") }}
                </button>
                <button v-if="iconUrl" class="ghost small" type="button" @click="clearIcon">
                  {{ t("editor.clearIcon") }}
                </button>
              </div>
            </div>
          </div>
          <label>
            <span>{{ t("editor.notes") }}</span>
            <textarea v-model="config.notes" rows="2" spellcheck="false" :placeholder="t('editor.notesHint')" />
          </label>
        </div>

        <div v-if="tab === 'overview'" class="group">
          <h3>{{ t("editor.game") }}</h3>
          <div class="grid">
            <label>
              <span>{{ t("editor.instanceName") }}</span>
              <input v-model="name" spellcheck="false" />
            </label>
            <label>
              <span>{{ t("editor.version") }}</span>
              <input v-model="config.version_id" class="mono" spellcheck="false" />
            </label>
            <label>
              <span>{{ t("editor.memory") }}</span>
              <MemorySlider :min="memoryMin" :max="memoryMax" @update="setMemory" />
            </label>
            <label>
              <span>{{ t("editor.javaRuntime") }}</span>
              <div class="field-row">
                <select v-model="config.java">
                  <option :value="null">{{ t("editor.automatic") }}</option>
                  <option v-for="runtime in javaChoices" :key="runtime.path" :value="runtime.path">
                    {{ t("editor.javaOption", { major: runtime.major, path: runtime.path }) }}
                  </option>
                </select>
                <button class="ghost small" type="button" @click="pickJava">{{ t("editor.browse") }}</button>
              </div>
            </label>
            <label>
              <span>{{ t("editor.resolution") }}</span>
              <div class="field-row">
                <input v-model.number="config.width" type="number" min="320" step="1" placeholder="854" />
                <input v-model.number="config.height" type="number" min="240" step="1" placeholder="480" />
              </div>
            </label>
          </div>
        </div>

        <div v-if="tab === 'overview' && gameDir" class="group">
          <h3>{{ t("detail.gameDir") }}</h3>
          <div class="field-row">
            <span class="path mono">{{ gameDir }}</span>
            <button class="ghost small" @click="openGameDir">
              <Icon name="folder" :size="13" /> {{ t("editor.openFolder") }}
            </button>
          </div>
        </div>

        <div v-if="tab === 'loader'" class="group">
          <div class="group-head">
            <h3>
              {{ t("editor.loader") }}
              <span v-if="loaderStatus?.kind" class="count">
                {{ loaderStatus.kind ? LOADER_LABELS[loaderStatus.kind] : "" }}
              </span>
            </h3>
            <button
              class="ghost small"
              :disabled="loaderChecking || loaderBusy || !loaderStatus?.kind"
              @click="checkLoaderUpdates"
            >
              <Icon name="refresh" :size="13" />
              {{ loaderChecking ? t("editor.checking") : t("editor.loaderCheckUpdate") }}
            </button>
          </div>
          <p v-if="loaderStatus" class="hint">
            {{
              loaderStatus.kind
                ? t("editor.loaderCurrent", {
                    loader: LOADER_LABELS[loaderStatus.kind],
                    version: loaderStatus.version ?? "",
                  })
                : t("editor.loaderVanilla", { version: loaderStatus.game_version })
            }}
          </p>
          <div v-if="loaderUpdate?.newer" class="updates">
            <div class="updates-head">
              <span>
                {{ t("editor.loaderUpdateAvailable", { current: loaderUpdate.current, latest: loaderUpdate.latest }) }}
              </span>
              <button class="ghost small" :disabled="loaderBusy || running" @click="applyLoaderUpdate">
                {{ t("editor.update") }}
              </button>
            </div>
          </div>
          <label>
            <span>{{ t("editor.loaderPick") }}</span>
            <div class="segmented">
              <button
                :class="{ active: loaderPick === 'vanilla' }"
                :disabled="loaderBusy || running"
                @click="loaderPick = 'vanilla'"
              >
                Vanilla
              </button>
              <button
                :class="{ active: loaderPick === 'fabric' }"
                :disabled="loaderBusy || running"
                @click="loaderPick = 'fabric'"
              >
                Fabric
              </button>
              <button
                :class="{ active: loaderPick === 'quilt' }"
                :disabled="loaderBusy || running"
                @click="loaderPick = 'quilt'"
              >
                Quilt
              </button>
              <button
                :class="{ active: loaderPick === 'legacyfabric' }"
                :disabled="loaderBusy || running"
                @click="loaderPick = 'legacyfabric'"
              >
                Legacy Fabric
              </button>
              <button
                :class="{ active: loaderPick === 'forge' }"
                :disabled="loaderBusy || running"
                @click="loaderPick = 'forge'"
              >
                Forge
              </button>
              <button
                :class="{ active: loaderPick === 'neoforge' }"
                :disabled="loaderBusy || running"
                @click="loaderPick = 'neoforge'"
              >
                NeoForge
              </button>
            </div>
          </label>
          <label v-if="loaderPick !== 'vanilla'">
            <span>{{ t("install.build", { loader: loaderLabel }) }}</span>
            <select v-model="loaderVersion" :disabled="loaderBusy || running || loaderLoading">
              <option v-if="loaderLoading || !loaderVersions.length" :value="null">
                {{ loaderLoading ? t("install.loadingBuilds") : t("install.noBuilds") }}
              </option>
              <optgroup v-if="stableLoaderVersions.length" :label="t('install.stable')">
                <option v-for="entry in stableLoaderVersions" :key="entry.version" :value="entry.version">
                  {{ entry.version }}
                </option>
              </optgroup>
              <optgroup v-if="betaLoaderVersions.length" :label="t('install.beta')">
                <option v-for="entry in betaLoaderVersions" :key="entry.version" :value="entry.version">
                  {{ entry.version }}
                </option>
              </optgroup>
            </select>
          </label>
          <div class="field-row">
            <button
              class="primary small"
              :disabled="
                loaderBusy || running || (loaderPick === 'vanilla' ? !loaderStatus?.kind : !loaderVersion)
              "
              @click="applyLoader"
            >
              {{ loaderBusy ? t("editor.loaderInstalling") : t("editor.loaderApply") }}
            </button>
          </div>
          <TransferBar
            v-if="loaderBusy && loaderStage"
            :label="loaderStageLabel"
            :done="loaderBytes"
            :total="loaderTotal"
            :speed="loaderSpeed"
            :detail="`${loaderStage.done}/${loaderStage.total}`"
          />
          <p v-if="loaderStatusText" class="hint">{{ loaderStatusText }}</p>
          <p class="hint">{{ t("editor.loaderHint") }}</p>
        </div>

        <div v-if="tab === 'launch'" class="group">
          <h3>{{ t("editor.launch") }}</h3>
          <div class="grid">
            <label>
              <span>{{ t("editor.joinServer") }} <small>{{ t("editor.joinServerHint") }}</small></span>
              <input v-model="serverText" class="mono" spellcheck="false" placeholder="mc.example.com:25565" />
            </label>
            <label>
              <span>{{ t("editor.account") }} <small>{{ t("editor.accountHint") }}</small></span>
              <div class="account-readout">{{ accountCurrentLabel }}</div>
            </label>
          </div>
          <label class="check-line">
            <input v-model="config.demo" type="checkbox" :disabled="props.running" />
            <span>{{ t("editor.demo") }} <small>{{ t("editor.demoHint") }}</small></span>
          </label>
          <label>
            <span>{{ t("editor.env") }} <small>{{ t("editor.envHint") }}</small></span>
            <textarea v-model="envText" rows="3" spellcheck="false" placeholder="JAVA_TOOL_OPTIONS=-Dfile.encoding=UTF-8" />
          </label>
        </div>

        <div v-if="tab === 'servers'" class="group">
          <div class="group-head">
            <h3>{{ t("editor.servers") }} <span v-if="servers.length" class="count">{{ servers.length }}</span></h3>
            <button class="ghost small" @click="addServer">
              <Icon name="plus" :size="13" /> {{ t("editor.addServer") }}
            </button>
            <button class="ghost small" :disabled="!servers.length" @click="saveServers">
              <Icon name="check" :size="13" /> {{ t("editor.saveServers") }}
            </button>
          </div>
          <p v-if="serverStatus" class="hint">{{ serverStatus }}</p>
          <p v-if="!servers.length" class="mods-empty">{{ t("editor.noServers") }}</p>
          <ul v-else class="mods">
            <li v-for="(server, index) in servers" :key="index">
              <div class="mod-text server-fields">
                <input v-model="server.name" :placeholder="t('editor.serverName')" spellcheck="false" />
                <input v-model="server.ip" class="mono" :placeholder="t('editor.serverAddress')" spellcheck="false" />
                <div class="server-ping">
                  <button class="ghost small" :disabled="pingOf(server)?.loading" @click="pingServer(server)">
                    {{ pingOf(server)?.loading ? t("editor.pinging") : t("editor.ping") }}
                  </button>
                  <img
                    v-if="pingOf(server)?.status?.icon"
                    :src="pingOf(server)?.status?.icon ?? undefined"
                    alt=""
                    class="server-icon"
                  />
                  <span v-if="pingOf(server)?.status" class="mono server-answer">
                    {{ pingOf(server)?.status?.motd || t("editor.noMotd") }} ·
                    {{ pingOf(server)?.status?.online }}/{{ pingOf(server)?.status?.max }} ·
                    {{ pingOf(server)?.status?.latency_ms }} ms
                    <template v-if="pingOf(server)?.status?.version"> · {{ pingOf(server)?.status?.version }}</template>
                  </span>
                  <span v-else-if="pingOf(server)?.error" class="server-error">{{ pingOf(server)?.error }}</span>
                </div>
              </div>
              <button class="icon danger" :title="t('editor.removeServer')" @click="servers.splice(index, 1)">
                <Icon name="trash" :size="14" />
              </button>
            </li>
          </ul>
        </div>

        <div v-if="tab === 'worlds'" class="group">
          <div class="group-head">
            <h3>{{ t("editor.worlds") }} <span v-if="worlds.length" class="count">{{ worlds.length }}</span></h3>
            <button class="ghost small" :disabled="worldBusy !== null" @click="importWorld">
              <Icon name="plus" :size="13" /> {{ t("editor.importWorld") }}
            </button>
            <button class="ghost small" @click="showBackups = !showBackups">
              <Icon name="package" :size="13" /> {{ t("editor.backups") }}
              <span v-if="worldBackups.length" class="count">{{ worldBackups.length }}</span>
            </button>
            <button class="ghost small" @click="openSaves">
              <Icon name="folder" :size="13" /> {{ t("editor.openFolder") }}
            </button>
            <button class="ghost small" @click="reloadWorlds">
              <Icon name="refresh" :size="13" /> {{ t("editor.refreshMedia") }}
            </button>
          </div>
          <p v-if="worldStatus" class="hint">{{ worldStatus }}</p>
          <p v-if="!worlds.length" class="mods-empty">{{ t("editor.noWorlds") }}</p>
          <ul v-else class="mods">
            <li v-for="world in worlds" :key="world.dir">
              <div class="mod-text">
                <strong>{{ world.name }}</strong>
                <small class="mono">
                  {{ world.version ?? "?" }} · {{ world.game_mode ?? "?" }} · {{ formatBytes(world.size) }}
                  <template v-if="world.seed !== null"> · {{ t("editor.seed", { seed: world.seed }) }}</template>
                  <template v-if="world.last_played_ms"> · {{ worldPlayed(world) }}</template>
                  <template v-if="world.hardcore"> · {{ t("editor.hardcore") }}</template>
                </small>
              </div>
              <button class="ghost small" :disabled="worldBusy !== null" @click="duplicateWorld(world)">
                {{ t("editor.duplicateWorld") }}
              </button>
              <button class="ghost small" :disabled="worldBusy !== null" @click="backupWorld(world)">
                {{ worldBusy === world.dir ? t("editor.working") : t("editor.backup") }}
              </button>
              <button
                class="icon danger"
                :title="t('editor.deleteWorld')"
                :disabled="worldBusy !== null"
                @click="deleteWorld(world)"
              >
                <Icon name="trash" :size="14" />
              </button>
            </li>
          </ul>
          <template v-if="showBackups">
            <h4 class="sub-head">{{ t("editor.backups") }}</h4>
            <p v-if="!worldBackups.length" class="mods-empty">{{ t("editor.noBackups") }}</p>
            <ul v-else class="mods">
              <li v-for="backup in worldBackups" :key="backup.file">
                <div class="mod-text">
                  <strong class="mono">{{ backup.file }}</strong>
                  <small>{{ formatBytes(backup.bytes) }}</small>
                </div>
                <button class="ghost small" :disabled="worldBusy !== null" @click="restoreWorld(backup)">
                  {{ t("editor.restore") }}
                </button>
                <button
                  class="icon danger"
                  :title="t('editor.deleteBackup')"
                  :disabled="worldBusy !== null"
                  @click="deleteBackup(backup)"
                >
                  <Icon name="trash" :size="14" />
                </button>
              </li>
            </ul>
          </template>
        </div>

        <div v-if="tab === 'datapacks'" class="group">
          <div class="group-head">
            <h3>{{ t("editor.datapacks") }} <span v-if="datapacks.length" class="count">{{ datapacks.length }}</span></h3>
            <button class="ghost small" :disabled="datapackBusy !== null || running || !datapackWorld" @click="addDatapack">
              <Icon name="plus" :size="13" />
              {{ datapackBusy === "add" ? t("editor.working") : t("editor.addDatapack") }}
            </button>
            <button class="ghost small" :disabled="!datapackWorld" @click="openDatapacks">
              <Icon name="folder" :size="13" /> {{ t("editor.openFolder") }}
            </button>
          </div>
          <p class="hint">{{ t("editor.datapacksHint") }}</p>
          <p v-if="!worlds.length" class="mods-empty">{{ t("editor.noWorldDatapacks") }}</p>
          <template v-else>
            <label class="field-row">
              <span class="hint">{{ t("editor.datapackWorld") }}</span>
              <select v-model="datapackWorld" @change="reloadDatapacks">
                <option v-for="world in worlds" :key="world.dir" :value="world.dir">{{ world.name }}</option>
              </select>
            </label>
            <p v-if="!datapacks.length" class="mods-empty">{{ t("editor.noDatapacks") }}</p>
            <ul v-else class="mods">
              <li v-for="pack in datapacks" :key="pack.name" :class="{ off: !pack.enabled }">
                <div class="mod-text">
                  <strong>{{ pack.name }}</strong>
                  <small class="mono">
                    {{ formatBytes(pack.bytes) }}
                    <template v-if="pack.folder"> · {{ t("editor.folderPack") }}</template>
                  </small>
                </div>
                <button
                  class="toggle"
                  :class="{ on: pack.enabled }"
                  :disabled="datapackBusy !== null || running"
                  @click="toggleDatapack(pack)"
                >
                  {{ pack.enabled ? t("editor.enabled") : t("editor.disabled") }}
                </button>
                <button
                  class="icon danger"
                  :title="t('action.delete')"
                  :disabled="datapackBusy !== null || running"
                  @click="deleteDatapack(pack)"
                >
                  <Icon name="trash" :size="14" />
                </button>
              </li>
            </ul>
          </template>
          <p v-if="datapackStatus" class="hint">{{ datapackStatus }}</p>
        </div>

        <div v-if="tab === 'logs'" class="group">
          <div class="group-head">
            <h3>{{ t("editor.crashes") }} <span v-if="crashes.length" class="count">{{ crashes.length }}</span></h3>
            <button class="ghost small" @click="reloadMedia">
              <Icon name="refresh" :size="13" /> {{ t("editor.refreshMedia") }}
            </button>
          </div>
          <p v-if="!crashes.length" class="mods-empty">{{ t("editor.noCrashes") }}</p>
          <ul v-else class="mods">
            <li v-for="report in crashes" :key="report.file">
              <div class="mod-text">
                <strong class="mono">{{ report.file }}</strong>
                <small>{{ formatBytes(report.bytes) }}</small>
              </div>
              <button class="ghost small" @click="showCrash(report)">{{ t("editor.view") }}</button>
              <button class="icon danger" :title="t('editor.deleteCrash')" @click="deleteCrash(report)">
                <Icon name="trash" :size="14" />
              </button>
            </li>
          </ul>
          <div v-if="crashFile" class="crash-diagnosis">
            <div class="group-head">
              <h3>{{ t("log.diagnosis") }}</h3>
              <button
                v-if="crashAnalysis?.findings.length"
                class="ghost small"
                type="button"
                @click="copyCrashReport"
              >
                <Icon name="copy" :size="13" />{{ t("log.copyReport") }}
              </button>
            </div>
            <p v-if="!crashAnalysis" class="hint">{{ t("log.analyzing") }}</p>
            <p v-else-if="!crashAnalysis.findings.length" class="hint">{{ t("log.noFindings") }}</p>
            <div v-else class="crash-findings">
              <FindingCard
                v-for="(finding, index) in crashAnalysis.findings"
                :key="index"
                :finding="finding"
                @fix="fixFinding"
              />
            </div>
          </div>
          <pre v-if="crashText" class="crash-text scroll">{{ crashText }}</pre>

          <div class="group-head">
            <h3>
              {{ t("editor.screenshots") }}
              <span v-if="shots.length" class="count">{{ shots.length }}</span>
            </h3>
          </div>
          <p v-if="!shots.length" class="mods-empty">{{ t("editor.noScreenshots") }}</p>
          <ul v-else class="mods">
            <li v-for="shot in shots" :key="shot.file">
              <div class="mod-text">
                <strong class="mono">{{ shot.file }}</strong>
                <small>{{ formatBytes(shot.bytes) }}</small>
              </div>
              <button class="ghost small" @click="viewShot(shot)">{{ t("editor.view") }}</button>
              <button class="icon danger" :title="t('editor.deleteScreenshot')" @click="deleteShot(shot)">
                <Icon name="trash" :size="14" />
              </button>
            </li>
          </ul>
          <div v-if="shotPreview" class="shot-preview">
            <img :src="shotPreview.url" :alt="shotPreview.file" />
          </div>
        </div>

        <div v-if="tab === 'launch'" class="group">
          <h3>{{ t("editor.arguments") }}</h3>
          <label>
            <span>{{ t("editor.jvmArgs") }} <small>{{ t("editor.onePerLine") }}</small></span>
            <textarea v-model="jvmArgs" rows="3" spellcheck="false" placeholder="-XX:+UseG1GC" />
          </label>
          <div class="field-row presets">
            <span class="hint">{{ t("editor.jvmPresets") }}</span>
            <button class="ghost small" type="button" @click="applyJvmPreset('g1')">{{ t("editor.presetG1") }}</button>
            <button class="ghost small" type="button" @click="applyJvmPreset('aikar')">
              {{ t("editor.presetAikar") }}
            </button>
            <button class="ghost small" type="button" @click="applyJvmPreset('zgc')">{{ t("editor.presetZgc") }}</button>
          </div>
          <label>
            <span>{{ t("editor.gameArgs") }} <small>{{ t("editor.onePerLine") }}</small></span>
            <textarea v-model="gameArgs" rows="2" spellcheck="false" placeholder="--fullscreen" />
          </label>
        </div>

        <div v-if="tab === 'mods'" class="group">
          <div class="group-head">
            <h3>
              {{ t("editor.mods") }}
              <span v-if="mods.length" class="count">{{
                t("editor.enabledCount", { enabled: modsEnabled, total: mods.length })
              }}</span>
            </h3>
            <button class="ghost small" :disabled="checking !== null || !mods.length" @click="checkKind('mod')">
              <Icon name="refresh" :size="13" /> {{ checking === "mod" ? t("editor.checking") : t("editor.checkUpdates") }}
            </button>
            <button class="ghost small" :disabled="!config || updating !== null" @click="browsing = true">
              <Icon name="spark" :size="13" /> {{ t("editor.browseModrinth") }}
            </button>
            <button class="ghost small" :disabled="localBusy !== null" @click="addLocal('mod')">
              <Icon name="plus" :size="13" /> {{ localBusy === "mod" ? t("editor.working") : t("editor.addFile") }}
            </button>
            <button v-if="mods.length" class="ghost small" @click="openInGameDir('mods')">
              <Icon name="folder" :size="13" /> {{ t("editor.openFolder") }}
            </button>
            <button
              v-if="mods.length"
              class="ghost small fix-deps"
              :disabled="depsBusy || running"
              @click="installMissingDeps"
            >
              <Icon name="download" :size="13" />
              {{
                depsBusy
                  ? t("editor.working")
                  : missingDeps.length
                    ? t("editor.fillDeps", { count: missingDeps.length })
                    : t("editor.fixDeps")
              }}
            </button>
          </div>
          <p v-if="blockedMods.length" class="warn-note">
            <Icon name="alert" :size="15" />
            {{ t("editor.blockedBanner", { count: blockedMods.length }) }}
          </p>
          <p v-if="missingDeps.length" class="warn-note fix-banner">
            <Icon name="alert" :size="15" />
            {{ t("editor.missingDepsBanner", { list: missingDeps.join(", ") }) }}
          </p>
          <p v-if="depsStatus" class="hint">{{ depsStatus }}</p>
          <UpdateList
            :updates="modUpdates"
            :busy="updating"
            @apply="(files) => applyUpdates('mod', files)"
          />
          <TransferBar
            v-if="updating !== null || depsBusy"
            :label="t('editor.updating')"
            :done="modBytes"
            :total="modTotal"
            :speed="modSpeed"
            :detail="modFile"
          />
          <p v-if="!modUpdates.length && modStatus" class="hint">{{ modStatus }}</p>
          <div v-if="mods.length" class="filter-row">
            <input
              v-model="modFilter"
              class="filter"
              spellcheck="false"
              :placeholder="t('editor.filterMods')"
            />
            <label class="filter-toggle">
              <input v-model="modOnlyDisabled" type="checkbox" />
              {{ t("editor.onlyDisabled") }}
            </label>
            <select v-model="modSort" class="sort" :title="t('editor.sort')">
              <option value="name">{{ t("editor.sortName") }}</option>
              <option value="size">{{ t("editor.sortSize") }}</option>
              <option value="modified">{{ t("editor.sortModified") }}</option>
            </select>
            <span v-if="modFilter.trim() || modOnlyDisabled" class="count">
              {{ filteredMods.length }}/{{ mods.length }}
            </span>
          </div>
          <p v-if="!mods.length" class="mods-empty">
            {{ t("editor.noMods") }}
          </p>
          <p v-else-if="!filteredMods.length" class="mods-empty">{{ t("editor.noModsMatch") }}</p>
          <ul v-else class="mods">
            <li v-for="mod in filteredMods" :key="mod.file" :class="{ off: !mod.enabled }">
              <div class="mod-text">
                <strong>{{ mod.name }}</strong>
                <small class="mono">{{ mod.version }} · {{ mod.loader }} · {{ formatBytes(mod.size) }}</small>
                <small v-if="mod.blocked" class="missing-deps">{{ blockedText(mod.blocked) }}</small>
                <small v-if="mod.missing_dependencies.length" class="missing-deps">
                  {{ t("editor.missingDeps", { list: mod.missing_dependencies.join(", ") }) }}
                </small>
                <small v-if="mod.disabled_dependencies?.length" class="missing-deps">
                  {{ t("editor.disabledDeps", { list: mod.disabled_dependencies.join(", ") }) }}
                </small>
                <small v-if="mod.duplicate" class="missing-deps">{{ t("editor.duplicateMod") }}</small>
              </div>
              <button
                class="toggle"
                :class="{ on: mod.enabled }"
                :disabled="toggling === mod.file || removing === mod.file"
                @click="toggleMod(mod)"
              >
                {{ mod.enabled ? t("editor.enabled") : t("editor.disabled") }}
              </button>
              <button
                class="icon danger"
                type="button"
                :title="t('editor.removeMod')"
                :disabled="toggling === mod.file || removing === mod.file || running"
                @click="removeMod(mod)"
              >
                <Icon name="trash" :size="14" />
              </button>
            </li>
          </ul>
        </div>

        <div v-if="tab === 'resourcepacks'" class="group">
          <div class="group-head">
            <h3>
              {{ t("editor.resourcePacks") }}
              <span v-if="resourcePacks.length" class="count">{{ resourcePacks.length }}</span>
            </h3>
            <button
              class="ghost small"
              :disabled="checking !== null || !resourcePacks.length"
              @click="checkKind('resourcepack')"
            >
              <Icon name="refresh" :size="13" />
              {{ checking === "resourcepack" ? t("editor.checking") : t("editor.checkUpdates") }}
            </button>
            <button class="ghost small" @click="openBrowse('resourcepack')">
              <Icon name="spark" :size="13" /> {{ t("editor.browseModrinth") }}
            </button>
            <button class="ghost small" :disabled="localBusy !== null" @click="addLocal('resourcepack')">
              <Icon name="plus" :size="13" />
              {{ localBusy === "resourcepack" ? t("editor.working") : t("editor.addFile") }}
            </button>
            <button v-if="resourcePacks.length" class="ghost small" @click="openInGameDir('resourcepacks')">
              <Icon name="folder" :size="13" /> {{ t("editor.openFolder") }}
            </button>
          </div>
          <UpdateList
            :updates="packUpdates"
            :busy="updating"
            @apply="(files) => applyUpdates('resourcepack', files)"
          />
          <p v-if="!packUpdates.length && packStatus" class="hint">{{ packStatus }}</p>
          <div v-if="resourcePacks.length" class="filter-row">
            <input v-model="packFilter" class="filter" spellcheck="false" :placeholder="t('editor.filterFiles')" />
            <select v-model="packSort" class="sort" :title="t('editor.sort')">
              <option value="name">{{ t("editor.sortName") }}</option>
              <option value="size">{{ t("editor.sortSize") }}</option>
              <option value="modified">{{ t("editor.sortModified") }}</option>
              <option value="manual">{{ t("editor.sortManual") }}</option>
            </select>
            <span v-if="packFilter.trim()" class="count">{{ filteredPacks.length }}/{{ resourcePacks.length }}</span>
          </div>
          <p v-if="!resourcePacks.length" class="mods-empty">{{ t("editor.noResourcePacks") }}</p>
          <p v-else-if="!filteredPacks.length" class="mods-empty">{{ t("editor.noFilesMatch") }}</p>
          <ul v-else class="mods">
            <li v-for="pack in filteredPacks" :key="pack.file" :class="{ off: !pack.enabled }">
              <img v-if="packIcons[pack.file]" class="pack-icon" :src="packIcons[pack.file]" alt="" />
              <div class="mod-text">
                <strong>{{ pack.file }}</strong>
                <small v-if="pack.description">{{ pack.description }}</small>
                <small class="mono">{{ formatBytes(pack.bytes) }}</small>
                <small v-if="packMismatch(pack)" class="missing-deps">{{ packMismatchText(pack) }}</small>
              </div>
              <template v-if="packSort === 'manual' && pack.enabled && enabledPacks > 1">
                <button
                  class="icon-button"
                  type="button"
                  :title="t('editor.moveUp')"
                  :disabled="props.running || togglingContent === pack.file"
                  @click="movePack(pack, true)"
                >
                  <Icon name="chevron-up" :size="14" />
                </button>
                <button
                  class="icon-button"
                  type="button"
                  :title="t('editor.moveDown')"
                  :disabled="props.running || togglingContent === pack.file"
                  @click="movePack(pack, false)"
                >
                  <Icon name="chevron-down" :size="14" />
                </button>
              </template>
              <button
                class="toggle"
                :class="{ on: pack.enabled }"
                :disabled="togglingContent === pack.file"
                @click="toggleContent('resourcepack', pack)"
              >
                {{ pack.enabled ? t("editor.enabled") : t("editor.disabled") }}
              </button>
              <button
                class="icon danger"
                type="button"
                :title="t('editor.removeMod')"
                :disabled="props.running || togglingContent === pack.file"
                @click="removeContent('resourcepack', pack)"
              >
                <Icon name="trash" :size="14" />
              </button>
            </li>
          </ul>
        </div>

        <div v-if="tab === 'shaders'" class="group">
          <div class="group-head">
            <h3>
              {{ t("editor.shaders") }}
              <span v-if="shaders.length" class="count">{{ shaders.length }}</span>
            </h3>
            <button class="ghost small" :disabled="checking !== null || !shaders.length" @click="checkKind('shader')">
              <Icon name="refresh" :size="13" />
              {{ checking === "shader" ? t("editor.checking") : t("editor.checkUpdates") }}
            </button>
            <button class="ghost small" @click="openBrowse('shader')">
              <Icon name="spark" :size="13" /> {{ t("editor.browseModrinth") }}
            </button>
            <button class="ghost small" :disabled="localBusy !== null" @click="addLocal('shader')">
              <Icon name="plus" :size="13" />
              {{ localBusy === "shader" ? t("editor.working") : t("editor.addFile") }}
            </button>
            <button v-if="shaders.length" class="ghost small" @click="openInGameDir('shaderpacks')">
              <Icon name="folder" :size="13" /> {{ t("editor.openFolder") }}
            </button>
          </div>
          <UpdateList
            :updates="shaderUpdates"
            :busy="updating"
            @apply="(files) => applyUpdates('shader', files)"
          />
          <p v-if="!shaderUpdates.length && shaderStatus" class="hint">{{ shaderStatus }}</p>
          <div v-if="shaders.length" class="filter-row">
            <input v-model="shaderFilter" class="filter" spellcheck="false" :placeholder="t('editor.filterFiles')" />
            <select v-model="shaderSort" class="sort" :title="t('editor.sort')">
              <option value="name">{{ t("editor.sortName") }}</option>
              <option value="size">{{ t("editor.sortSize") }}</option>
              <option value="modified">{{ t("editor.sortModified") }}</option>
            </select>
            <span v-if="shaderFilter.trim()" class="count">{{ filteredShaders.length }}/{{ shaders.length }}</span>
          </div>
          <p v-if="!shaders.length" class="mods-empty">{{ t("editor.noShaders") }}</p>
          <p v-else-if="!filteredShaders.length" class="mods-empty">{{ t("editor.noFilesMatch") }}</p>
          <ul v-else class="mods">
            <li v-for="shader in filteredShaders" :key="shader.file" :class="{ off: !shader.enabled }">
              <div class="mod-text">
                <strong>{{ shader.file }}</strong>
                <small class="mono">{{ formatBytes(shader.bytes) }}</small>
              </div>
              <button
                v-if="shaderToggle"
                class="toggle"
                :class="{ on: shader.enabled }"
                :disabled="togglingContent === shader.file"
                @click="toggleContent('shader', shader)"
              >
                {{ shader.enabled ? t("editor.enabled") : t("editor.disabled") }}
              </button>
              <button
                class="icon danger"
                type="button"
                :title="t('editor.removeMod')"
                :disabled="props.running || togglingContent === shader.file"
                @click="removeContent('shader', shader)"
              >
                <Icon name="trash" :size="14" />
              </button>
            </li>
          </ul>
          <p v-if="shaders.length && !shaderToggle" class="hint">{{ t("editor.shadersNeedMod") }}</p>
        </div>

        <div v-if="tab === 'integrity'" class="group">
          <div class="group-head">
            <h3>{{ t("editor.integrity") }}</h3>
            <button class="ghost small" :disabled="doctorBusy" @click="openDoctor">
              <Icon name="diagnosis" :size="13" />{{ t("doctor.check") }}
            </button>
            <button
              class="ghost small"
              :disabled="busy || props.running"
              @click="repairInstance"
            >
              <Icon name="refresh" :size="13" />
              {{ repairBusy ? t("editor.repairing") : t("editor.repair") }}
            </button>
          </div>
          <p class="hint">{{ t("editor.repairHint") }}</p>
          <TransferBar
            v-if="repairBusy"
            :label="t('editor.repairing')"
            :done="repairBytes"
            :total="repairTotal"
            :speed="repairSpeed"
            :detail="repairStage"
          />
          <p v-if="repairStatus" class="ok banner">
            <Icon name="check" :size="14" />{{ repairStatus }}
          </p>
        </div>

        <div v-if="tab === 'manage'" class="group">
          <h3>{{ t("detail.manage") }}</h3>
          <div class="manage-row">
            <button class="ghost" @click="duplicateOpen = !duplicateOpen">
              <Icon name="copy" :size="14" />{{ t("editor.duplicate") }}
            </button>
            <button class="ghost" :disabled="archiving" @click="exportArchive">
              <Icon name="package" :size="14" />
              {{ archiving ? t("editor.working") : t("editor.exportArchive") }}
            </button>
            <button class="ghost" :disabled="exporting" @click="exportPack">
              <Icon name="download" :size="14" />
              {{ exporting ? t("editor.working") : t("editor.export") }}
            </button>
          </div>
          <div v-if="duplicateOpen" class="field-row">
            <input v-model="duplicateName" spellcheck="false" :placeholder="t('editor.duplicateName')" />
            <button class="ghost small" @click="duplicateOpen = false">{{ t("action.cancel") }}</button>
            <button class="primary small" :disabled="duplicating || !duplicateName.trim()" @click="duplicate">
              {{ duplicating ? t("editor.working") : t("editor.duplicate") }}
            </button>
          </div>
          <p v-if="exportStatus" class="hint">{{ exportStatus }}</p>
          <p v-if="archiveStatus" class="hint">{{ archiveStatus }}</p>
        </div>

        <div v-if="tab === 'manage'" class="group danger-zone">
          <h3>{{ t("detail.dangerZone") }}</h3>
          <p class="hint">{{ t("detail.deleteHint") }}</p>
          <div class="manage-row">
            <button class="danger" :disabled="running" @click="removeInstance">
              <Icon name="trash" :size="14" />{{ t("detail.delete") }}
            </button>
          </div>
        </div>
        </div>
      </div>
    </div>

    <DoctorDialog
      v-if="doctorOpen"
      :name="props.name"
      :report="doctorReport"
      :busy="doctorBusy"
      :error="doctorError"
      :action-busy="doctorAction"
      @close="doctorOpen = false"
      @rerun="runDoctor"
      @action="onDoctorAction"
    />

    <ModrinthDialog
      v-if="browsing"
      :instance="config?.name ?? name"
      :kind="browseKind"
      @close="browsing = false"
      @installed="onBrowsed"
    />
  </section>
</template>

<style scoped>
.detail {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  gap: 14px;
  animation: rise 180ms ease both;
}

.detail-head {
  flex: none;
  display: flex;
  align-items: center;
  gap: 14px;
  padding: 16px 18px;
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: linear-gradient(180deg, var(--card-a), var(--card-b));
}

.back svg {
  transform: rotate(180deg);
}

[dir="rtl"] .back svg {
  transform: none;
}

.head-art {
  flex: none;
  width: 54px;
  height: 54px;
  border-radius: 14px;
  overflow: hidden;
  display: grid;
  place-items: center;
  color: var(--accent);
  background: rgba(111, 220, 111, 0.1);
  border: 1px solid rgba(111, 220, 111, 0.2);
}

.head-art img {
  width: 100%;
  height: 100%;
  object-fit: cover;
}

.head-text {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 7px;
}

.head-text h1 {
  margin: 0;
  font-size: 20px;
  line-height: 1.15;
  letter-spacing: -0.01em;
  word-break: break-word;
}

.chips {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}

.chips .badge svg {
  opacity: 0.75;
}

.badge.version {
  color: var(--text);
  border-color: var(--border-strong);
  background: var(--surface-4);
}

.head-actions {
  flex: none;
  display: flex;
  align-items: center;
  gap: 8px;
}

.head-actions .play {
  height: 40px;
  padding: 0 20px;
}

.detail-body {
  flex: 1;
  min-height: 0;
  display: grid;
  grid-template-columns: 208px minmax(0, 1fr);
  gap: 14px;
}

.side {
  min-height: 0;
  overflow: hidden;
  display: flex;
  flex-direction: column;
  gap: 2px;
  padding: 8px;
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: linear-gradient(180deg, var(--card-a), var(--card-b));
}

.side-scroll {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  gap: 1px;
}

.side-label {
  margin: 8px 6px 2px;
  font-size: 9px;
  font-weight: 650;
  line-height: 1.2;
  letter-spacing: 0.14em;
  text-transform: uppercase;
  color: var(--muted-2);
}

.side-label:first-child {
  margin-top: 0;
}

.side-item {
  width: 100%;
  flex-direction: row;
  justify-content: flex-start;
  gap: 8px;
  padding: 6px 9px;
  border-color: transparent;
  background: transparent;
  color: var(--muted);
  font-size: 12px;
  border-radius: 9px;
  text-align: start;
}

.side-item:hover:not(:disabled) {
  color: var(--text);
  background: var(--surface-3);
  border-color: transparent;
}

.side-item.active {
  color: var(--text);
  background: var(--accent-soft);
  border-color: rgba(111, 220, 111, 0.22);
}

.side-item.active svg {
  color: var(--accent);
}

.side-item .count {
  margin-inline-start: auto;
  font-size: 10px;
  color: var(--muted-2);
}

.side-foot {
  margin-top: auto;
  padding-top: 10px;
}

.side-foot .primary {
  width: 100%;
  justify-content: center;
}

.panel {
  min-height: 0;
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  gap: 16px;
  padding: 18px;
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: linear-gradient(180deg, var(--dialog-a), var(--dialog-b));
}

.pane-body {
  display: flex;
  flex-direction: column;
  gap: 20px;
}

.path {
  flex: 1;
  min-width: 0;
  font-size: 11px;
  color: var(--muted-2);
  word-break: break-all;
}

.manage-row {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
}

.danger-zone {
  border-top: 1px solid var(--border);
  padding-top: 16px;
}

.group {
  display: flex;
  flex-direction: column;
  gap: 10px;
}

h3 {
  margin: 0;
  font-size: 11px;
  font-weight: 650;
  letter-spacing: 0.09em;
  text-transform: uppercase;
  color: var(--muted);
}

.grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 12px;
}

label {
  display: flex;
  flex-direction: column;
  gap: 6px;
  font-size: 11.5px;
  color: var(--muted);
}

label small {
  color: var(--muted-2);
  font-size: 10.5px;
  margin-inline-start: 4px;
}

.check-line {
  flex-direction: row;
  align-items: center;
  gap: 8px;
  cursor: pointer;
}

.check-line input {
  width: 13px;
  height: 13px;
  margin: 0;
  padding: 0;
  accent-color: var(--accent);
}

.account-readout {
  padding: 8px 10px;
  border: 1px solid var(--border);
  border-radius: 9px;
  background: var(--field);
  color: var(--text);
  font-size: 12px;
}

textarea {
  font-family: var(--mono);
  font-size: 12px;
  resize: vertical;
}

.fix-deps {
  color: var(--accent);
  border-color: rgba(111, 220, 111, 0.35);
  background: rgba(111, 220, 111, 0.08);
}

.fix-banner {
  align-items: flex-start;
  margin-bottom: 10px;
}

.warn-note {
  display: flex;
  align-items: center;
  gap: 8px;
  margin: 0;
  font-size: 12px;
  color: var(--warning);
  background: rgba(255, 200, 87, 0.08);
  border: 1px solid rgba(255, 200, 87, 0.22);
  border-radius: 12px;
  padding: 10px 12px;
}

.hint {
  display: flex;
  align-items: center;
  gap: 7px;
  margin: 0;
  font-size: 11px;
  color: var(--muted-2);
  word-break: break-all;
}

.count {
  margin-inline-start: 6px;
  font-size: 10.5px;
  color: var(--muted-2);
}

.group-head {
  display: flex;
  align-items: center;
  gap: 8px;
}

.group-head h3 {
  margin-inline-end: auto;
}

.group-head .ghost {
  padding: 4px 10px;
  font-size: 11px;
}

.updates {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 10px 12px;
  border: 1px solid rgba(111, 220, 111, 0.32);
  border-radius: 12px;
  background: rgba(111, 220, 111, 0.06);
}

.updates-head {
  display: flex;
  align-items: center;
  gap: 10px;
  font-size: 11.5px;
  color: var(--accent);
}

.updates ul {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.updates li {
  display: flex;
  align-items: center;
  gap: 12px;
}

.updates li .ghost {
  margin-inline-start: auto;
  padding: 4px 10px;
  font-size: 11px;
}

.mods-empty {
  margin: 0;
  font-size: 11.5px;
  color: var(--muted-2);
  line-height: 1.5;
}

.filter-row {
  display: flex;
  align-items: center;
  gap: 10px;
}
.filter-row .filter {
  flex: 1;
  min-width: 0;
  padding: 5px 9px;
  font-size: 11.5px;
  border-radius: 8px;
}
.filter-row .sort {
  flex-shrink: 0;
  padding: 5px 9px;
  font-size: 11.5px;
  border-radius: 8px;
}

.filter-toggle {
  display: flex;
  flex-direction: row;
  align-items: center;
  gap: 5px;
  flex-shrink: 0;
  font-size: 11px;
  color: var(--muted-2);
  cursor: pointer;
  white-space: nowrap;
}

.filter-toggle input {
  width: 13px;
  height: 13px;
  padding: 0;
  margin: 0;
  accent-color: var(--accent);
}

.mods {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.mods li {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 8px 10px;
  border: 1px solid var(--border);
  border-radius: 10px;
  background: var(--row-bg);
}

.mods li.off {
  opacity: 0.62;
}

.mod-text {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.mod-text strong {
  font-size: 12.5px;
  font-weight: 600;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.mod-text small {
  font-size: 10.5px;
  color: var(--muted-2);
}

.toggle {
  margin-inline-start: auto;
  flex: none;
  border: 1px solid var(--border);
  background: var(--surface-3);
  color: var(--muted);
  border-radius: 999px;
  font-size: 11px;
  padding: 5px 12px;
}

.toggle.on {
  color: var(--accent);
  border-color: rgba(111, 220, 111, 0.4);
  background: rgba(111, 220, 111, 0.12);
}

.pack-icon {
  flex: none;
  width: 34px;
  height: 34px;
  border-radius: 7px;
  object-fit: cover;
  image-rendering: pixelated;
  border: 1px solid var(--border);
  background: var(--surface-3);
}

.icon-button {
  flex: none;
  display: grid;
  place-items: center;
  width: 24px;
  height: 24px;
  padding: 0;
  border: 1px solid var(--border);
  border-radius: 7px;
  background: transparent;
  color: var(--muted);
}

.icon-button:hover:not(:disabled) {
  color: var(--text);
  background: var(--surface-3);
}

.field-row {
  display: flex;
  gap: 6px;
  align-items: center;
}

.field-row select,
.field-row input {
  min-width: 0;
  flex: 1;
}

.sub-head {
  margin: 14px 0 6px;
  font-size: 11px;
  letter-spacing: 0.14em;
  text-transform: uppercase;
  color: var(--muted);
}

.missing-deps {
  color: var(--warning);
}

.identity-row {
  display: flex;
  gap: 14px;
  align-items: flex-start;
}

.icon-preview {
  width: 72px;
  height: 72px;
  flex: none;
  border-radius: 14px;
  overflow: hidden;
  display: grid;
  place-items: center;
  color: var(--muted);
  background: var(--surface-3);
  border: 1px solid var(--border);
}

.icon-preview img {
  width: 100%;
  height: 100%;
  object-fit: cover;
}

.identity-fields {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.presets {
  flex-wrap: wrap;
  gap: 8px;
}

.presets .hint {
  margin-inline-end: 4px;
}

.server-fields {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  flex: 1;
  min-width: 0;
}

.server-ping {
  width: 100%;
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
  padding-top: 4px;
}

.server-icon {
  width: 22px;
  height: 22px;
  border-radius: 5px;
  image-rendering: pixelated;
  background: var(--surface-3);
}

.server-answer {
  font-size: 11px;
  color: var(--muted);
}

.server-error {
  font-size: 11px;
  color: var(--danger);
}

.server-fields input {
  flex: 1;
  min-width: 0;
}

.crash-diagnosis {
  margin-top: 10px;
}

.crash-findings {
  display: grid;
  gap: 8px;
  margin-bottom: 10px;
}

.crash-text {
  max-height: 240px;
  overflow: auto;
  margin: 0;
  padding: 12px;
  border-radius: 10px;
  border: 1px solid var(--border);
  background: rgba(0, 0, 0, 0.35);
  color: var(--muted);
  font-family: var(--mono);
  font-size: 11px;
  line-height: 1.5;
  white-space: pre-wrap;
}

.shot-preview {
  border-radius: 12px;
  overflow: hidden;
  border: 1px solid var(--border);
  background: rgba(0, 0, 0, 0.3);
}

.shot-preview img {
  display: block;
  width: 100%;
  height: auto;
}

.segmented {
  display: inline-flex;
  flex-wrap: wrap;
  padding: 3px;
  gap: 2px;
  background: var(--surface-3);
  border: 1px solid var(--border);
  border-radius: 10px;
}

.segmented button {
  border: 0;
  background: transparent;
  color: var(--muted);
  font-size: 12px;
  padding: 6px 12px;
  border-radius: 7px;
}

.segmented button:hover:not(:disabled) {
  background: var(--surface-4);
}

.segmented button.active {
  background: rgba(111, 220, 111, 0.14);
  color: var(--text);
  box-shadow: inset 0 0 0 1px rgba(111, 220, 111, 0.22);
}

.progress-block {
  display: flex;
  flex-direction: column;
  gap: 7px;
  padding: 12px;
  border-radius: 12px;
  border: 1px solid var(--border);
  background: var(--field-soft);
}

.progress-head {
  display: flex;
  justify-content: space-between;
  gap: 8px;
  font-size: 11.5px;
  color: var(--muted);
}

.bar {
  height: 6px;
  border-radius: 999px;
  background: var(--surface-5);
  overflow: hidden;
}

.fill {
  height: 100%;
  border-radius: 999px;
  background: linear-gradient(90deg, var(--teal), var(--accent));
  box-shadow: 0 0 14px rgba(111, 220, 111, 0.5);
  transition: width 160ms ease;
}

.progress-foot {
  display: flex;
  justify-content: space-between;
  font-size: 10.5px;
  color: var(--muted-2);
}

.banner {
  display: flex;
  align-items: flex-start;
  gap: 8px;
  margin: 0;
  background: var(--ok-banner-bg);
  border: 1px solid rgba(111, 220, 111, 0.28);
  color: var(--ok-banner-text);
  border-radius: 12px;
  padding: 10px 12px;
  font-size: 11.5px;
  line-height: 1.5;
}
</style>
