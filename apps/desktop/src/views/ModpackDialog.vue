<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import {
  api,
  events,
  formatBytes,
  type CursePack,
  type FtbPack,
  type InstallEvent,
  type JobEvent,
  jobTag,
  type ModpackInfo,
  type PackSummary,
  type PackProgress,
  type SearchIndex,
  type UnlistenFn,
} from "../api";
import Icon from "../components/Icon.vue";
import TransferBar from "../components/TransferBar.vue";
import { t } from "../i18n";
import { maybePromptJava } from "../javaPrompt";
import { useTransfer } from "../progress";

const emit = defineEmits<{ close: []; installed: [name: string] }>();

const props = defineProps<{ initialPath?: string | null }>();

const path = ref(props.initialPath ?? "");
const name = ref("");
const info = ref<ModpackInfo | null>(null);
const inspecting = ref(false);
const installing = ref(false);
const error = ref<string | null>(null);
const stage = ref<InstallEvent | null>(null);
const phase = ref<"idle" | "archive" | "version" | "pack" | "extract">("idle");
const packFile = ref<string | null>(null);
const packDone = ref(0);
const packFilesTotal = ref(0);
const packOverrides = ref(0);
const transfer = useTransfer();
const stageTransfer = useTransfer();
let stageName = "";

const unlisteners: UnlistenFn[] = [];

const mode = ref<"file" | "browse">("file");
const query = ref("");
const packs = ref<PackSummary[]>([]);
const packTotal = ref(0);
const packOffset = ref(0);
const packLoading = ref(false);
const packError = ref<string | null>(null);
const browsing = ref(false);

const source = ref<"modrinth" | "curseforge" | "ftb">("modrinth");
const sort = ref<SearchIndex>("downloads");
const SORTS: SearchIndex[] = ["downloads", "follows", "newest", "updated", "relevance"];
const ftbPacks = ref<FtbPack[]>([]);
const ftbLoading = ref(false);
const ftbError = ref<string | null>(null);
const ftbChoice = ref<Record<number, number>>({});

const cursePacks = ref<CursePack[]>([]);
const curseTotal = ref(0);
const curseOffset = ref(0);
const curseLoading = ref(false);
const curseError = ref<string | null>(null);
const curseChoice = ref<Record<number, number>>({});

function curseFile(pack: CursePack): number | null {
  const chosen = curseChoice.value[pack.project_id];
  if (chosen && pack.files.some((file) => file.file_id === chosen)) return chosen;
  // The backend sorts files newest first; install the newest build unless the
  // user picked another one instead of silently falling back to the oldest.
  return pack.files.length ? pack.files[0].file_id : null;
}

function curseLabel(file: CursePack["files"][number]): string {
  const game = file.game_versions.filter((version) => version.startsWith("1.") || version.startsWith("26.")).slice(0, 2);
  const minecraft = game.length ? ` · MC ${game.join("/")}` : "";
  return `${file.version}${minecraft} · ${["release", "beta", "alpha"][file.release_type - 1] ?? "release"}`;
}

async function searchCurse(next = 0) {
  curseLoading.value = true;
  curseError.value = null;
  try {
    const page = await api.searchCurseforgePacks(query.value, sort.value, 20, next);
    cursePacks.value = page.hits;
    curseTotal.value = page.total_hits;
    curseOffset.value = page.offset;
  } catch (cause) {
    curseError.value = String(cause);
  } finally {
    curseLoading.value = false;
  }
}

async function installCurse(pack: CursePack) {
  if (installing.value) return;
  installing.value = true;
  error.value = null;
  resetProgress();
  try {
    const report = await api.installCurseforgePack(pack.project_id, curseFile(pack), name.value.trim() || null);
    emit("installed", report.instance);
  } catch (cause) {
    const message = String(cause);
    if (!maybePromptJava(message, () => void installCurse(pack))) error.value = message;
    installing.value = false;
  }
}

function ftbVersion(pack: FtbPack): number | null {
  const chosen = ftbChoice.value[pack.id];
  if (chosen && pack.versions.some((version) => version.id === chosen)) return chosen;
  return pack.versions.length ? pack.versions[pack.versions.length - 1].id : null;
}

async function searchFtb() {
  ftbLoading.value = true;
  ftbError.value = null;
  try {
    ftbPacks.value = await api.searchFtbPacks(query.value, 20);
  } catch (cause) {
    ftbError.value = String(cause);
  } finally {
    ftbLoading.value = false;
  }
}

async function installFtb(pack: FtbPack) {
  if (installing.value) return;
  installing.value = true;
  error.value = null;
  resetProgress();
  try {
    const report = await api.installFtbPack(pack.id, ftbVersion(pack), name.value.trim() || null);
    emit("installed", report.instance);
  } catch (cause) {
    const message = String(cause);
    if (!maybePromptJava(message, () => void installFtb(pack))) error.value = message;
    installing.value = false;
  }
}

function switchSource(next: "modrinth" | "curseforge" | "ftb") {
  if (installing.value) return;
  source.value = next;
  if (next === "ftb" && !ftbPacks.value.length) void searchFtb();
  if (next === "curseforge" && !cursePacks.value.length) void searchCurse();
}

function versionLabel(pack: FtbPack, id: number): string {
  const version = pack.versions.find((entry) => entry.id === id);
  if (!version) return String(id);
  const loader = version.loader ? ` · ${version.loader}` : "";
  return `${version.name} · MC ${version.game_version}${loader} · ${version.kind}`;
}

function resetProgress() {
  phase.value = "idle";
  stage.value = null;
  stageName = "";
  packFile.value = null;
  packDone.value = 0;
  packFilesTotal.value = 0;
  packOverrides.value = 0;
  transfer.reset();
  stageTransfer.reset();
}

function pickSort(next: SearchIndex) {
  if (sort.value === next || installing.value) return;
  sort.value = next;
  void searchPacks(0);
}

async function searchPacks(next = 0) {
  browsing.value = true;
  packLoading.value = true;
  packError.value = null;
  try {
    const page = await api.searchModpacks(query.value, sort.value, 20, next);
    packs.value = page.hits;
    packTotal.value = page.total_hits;
    packOffset.value = page.offset;
  } catch (cause) {
    packError.value = String(cause);
  } finally {
    packLoading.value = false;
  }
}

async function installProject(hit: PackSummary) {
  if (installing.value) return;
  installing.value = true;
  error.value = null;
  resetProgress();
  try {
    const report = await api.installModpackProject(hit.project_id, name.value.trim() || null);
    emit("installed", report.instance);
  } catch (cause) {
    const message = String(cause);
    if (!maybePromptJava(message, () => void installProject(hit))) error.value = message;
    installing.value = false;
  }
}

function hideImage(event: Event) {
  const target = event.target as HTMLImageElement | null;
  if (target) target.style.display = "none";
}

function switchMode(next: "file" | "browse") {
  if (installing.value) return;
  mode.value = next;
  if (next === "browse" && !packs.value.length) void searchPacks();
}

const packCount = computed(() => info.value?.files ?? 0);

const activeTransfer = computed(() => (phase.value === "version" ? stageTransfer : transfer));
const percent = computed(() => (phase.value === "idle" ? 0 : activeTransfer.value.percent.value));
const activeBytes = computed(() => activeTransfer.value.bytes.value);
const activeTotal = computed(() => activeTransfer.value.total.value);
const activeSpeed = computed(() => activeTransfer.value.speed.value);

function cancelDownload() {
  void api.cancelDownload().catch(() => {});
}

const progressDetail = computed(() => {
  if (phase.value === "extract") return packFile.value ?? "";
  if (phase.value === "pack") {
    const total = packFilesTotal.value || packCount.value;
    return total ? `${packDone.value}/${total}` : (packFile.value ?? "");
  }
  if (phase.value === "version" && stage.value) return `${stage.value.done}/${stage.value.total}`;
  return "";
});

const phaseLabel = computed(() => {
  if (phase.value === "extract") return t("modpack.phaseOverrides");
  if (phase.value === "archive") return t("modpack.phaseArchive");
  if (phase.value === "pack") return t("modpack.phasePack");
  if (phase.value === "version") {
    const stageName = stage.value?.stage;
    if (stageName && ["manifest", "metadata", "libraries", "assets", "installer", "processors"].includes(stageName)) {
      return t(`install.stage.${stageName}`);
    }
    return t("modpack.phaseVersion");
  }
  return t("install.stage.preparing");
});

const loaderLabel = computed(() => {
  const value = info.value;
  if (!value) return "";
  if (!value.loader) return t("modpack.vanilla");
  return value.loader_version ? `${value.loader} ${value.loader_version}` : value.loader;
});

async function inspect() {
  const target = path.value.trim();
  if (!target) {
    error.value = t("modpack.pointAt");
    return;
  }
  inspecting.value = true;
  error.value = null;
  info.value = null;
  try {
    info.value = await api.modpackInfo(target);
  } catch (cause) {
    error.value = String(cause);
  } finally {
    inspecting.value = false;
  }
}

async function run() {
  const target = path.value.trim();
  if (!target) {
    error.value = t("modpack.pointAt");
    return;
  }
  if (!info.value) await inspect();
  if (!info.value) return;
  installing.value = true;
  error.value = null;
  resetProgress();
  try {
    const report = await api.installModpack(target, name.value.trim() || null);
    emit("installed", report.instance);
  } catch (cause) {
    const message = String(cause);
    if (!maybePromptJava(message, () => void run())) error.value = message;
    installing.value = false;
  }
}

function onPack({ job, event }: JobEvent<PackProgress>) {
  if (job !== jobTag.modpack) return;
  switch (event.stage) {
    case "archive":
      phase.value = "archive";
      transfer.track(event.done, event.total);
      break;
    case "downloading":
      phase.value = "pack";
      packFile.value = event.path;
      packDone.value = event.files_done;
      packFilesTotal.value = event.files_total;
      transfer.track(event.bytes_done, event.bytes_total || null);
      break;
    case "installed":
    case "skipped":
      phase.value = "pack";
      packFile.value = event.path;
      packDone.value = Math.min(packDone.value + 1, packFilesTotal.value || Number.MAX_SAFE_INTEGER);
      break;
    case "extracting":
      phase.value = "extract";
      packOverrides.value += event.entries;
      packFile.value = `${event.prefix}/ · ${packOverrides.value}`;
      break;
  }
}

function onInstall({ job, event }: JobEvent<InstallEvent>) {
  if (job !== jobTag.modpack) return;
  if (phase.value === "idle" || phase.value === "archive") phase.value = "version";
  if (event.stage !== stageName) {
    stageName = event.stage;
    stageTransfer.reset();
  }
  stage.value = event;
  const total = event.bytes_total && event.bytes_total > 0 ? event.bytes_total : event.total;
  const done = event.bytes_total && event.bytes_total > 0 ? event.bytes : event.done;
  stageTransfer.track(done, total || null);
}

onMounted(async () => {
  for (const listen of [
    () => events.onInstallProgress(onInstall),
    () => events.onPackProgress(onPack),
  ]) {
    try {
      unlisteners.push(await listen());
    } catch (cause) {
      error.value = String(cause);
    }
  }
  if (props.initialPath) await inspect();
});

onUnmounted(() => {
  unlisteners.forEach((unlisten) => unlisten());
});
</script>

<template>
  <div class="overlay" @click.self="!installing && $emit('close')">
    <section class="dialog">
      <header>
        <span class="chip"><Icon name="package" :size="16" /></span>
        <div class="head-text">
          <h2>{{ t("modpack.title") }}</h2>
          <p>{{ t("modpack.subtitle") }}</p>
        </div>
        <button class="icon" :title="t('action.close')" :disabled="installing" @click="$emit('close')">
          <Icon name="close" />
        </button>
      </header>

      <p v-if="error" class="error">{{ error }}</p>

      <div class="segmented mode-switch">
        <button :class="{ active: mode === 'file' }" :disabled="installing" @click="switchMode('file')">
          {{ t("modpack.fromFile") }}
        </button>
        <button :class="{ active: mode === 'browse' }" :disabled="installing" @click="switchMode('browse')">
          {{ t("modpack.browse") }}
        </button>
      </div>

      <div v-if="mode === 'browse'" class="body browse scroll">
        <div class="segmented source-switch">
          <button :class="{ active: source === 'modrinth' }" :disabled="installing" @click="switchSource('modrinth')">
            {{ t("modpack.sourceModrinth") }}
          </button>
          <button :class="{ active: source === 'curseforge' }" :disabled="installing" @click="switchSource('curseforge')">
            {{ t("modpack.sourceCurseforge") }}
          </button>
          <button :class="{ active: source === 'ftb' }" :disabled="installing" @click="switchSource('ftb')">
            {{ t("modpack.sourceFtb") }}
          </button>
        </div>
        <div class="row">
          <input
            v-model="query"
            :placeholder="t('modpack.searchPlaceholder')"
            spellcheck="false"
            :disabled="installing"
            @keyup.enter="source === 'ftb' ? searchFtb() : source === 'curseforge' ? searchCurse(0) : searchPacks(0)"
          />
          <button
            class="ghost"
            :disabled="packLoading || ftbLoading || curseLoading || installing"
            @click="source === 'ftb' ? searchFtb() : source === 'curseforge' ? searchCurse(0) : searchPacks(0)"
          >
            {{ packLoading || ftbLoading || curseLoading ? t("modpack.checking") : t("modpack.search") }}
          </button>
        </div>
        <div v-if="source === 'modrinth'" class="row sort-row">
          <span class="hint">{{ t("modrinth.sortBy") }}</span>
          <select
            class="mono"
            :value="sort"
            :disabled="installing || packLoading"
            @change="pickSort(($event.target as HTMLSelectElement).value as SearchIndex)"
          >
            <option v-for="key in SORTS" :key="key" :value="key">{{ t(`modrinth.sort.${key}`) }}</option>
          </select>
        </div>
        <p v-if="packError || ftbError || curseError" class="error">{{ packError || ftbError || curseError }}</p>

        <template v-if="source === 'modrinth'">
          <div v-if="packs.length" class="pack-list">
            <article v-for="hit in packs" :key="hit.project_id" class="pack-card">
              <img v-if="hit.icon_url" :src="hit.icon_url" alt="" class="pack-icon" @error="hideImage" />
              <div class="pack-text">
                <strong>{{ hit.title }}</strong>
                <small>{{ hit.description }}</small>
                <span class="pack-meta mono">
                  {{ t("modpack.downloads", { count: hit.downloads.toLocaleString() }) }}
                  <template v-if="hit.author"> · {{ hit.author }}</template>
                </span>
              </div>
              <button class="primary small" :disabled="installing" @click="installProject(hit)">
                {{ t("modpack.install") }}
              </button>
            </article>
          </div>
          <p v-else-if="!packLoading" class="hint">{{ t("modpack.noResults") }}</p>
          <div v-if="packTotal > packs.length" class="pager">
            <button class="ghost small" :disabled="packOffset === 0 || packLoading" @click="searchPacks(Math.max(0, packOffset - 20))">
              {{ t("modpack.prev") }}
            </button>
            <span class="hint">{{ packs.length }} / {{ packTotal }}</span>
            <button class="ghost small" :disabled="packLoading || packOffset + packs.length >= packTotal" @click="searchPacks(packOffset + 20)">
              {{ t("modpack.next") }}
            </button>
          </div>
        </template>

        <template v-else-if="source === 'curseforge'">
          <div v-if="cursePacks.length" class="pack-list">
            <article v-for="pack in cursePacks" :key="pack.project_id" class="pack-card">
              <img v-if="pack.icon_url" :src="pack.icon_url" alt="" class="pack-icon" @error="hideImage" />
              <div class="pack-text">
                <strong>{{ pack.title }}</strong>
                <small>{{ pack.description }}</small>
                <span class="pack-meta mono">
                  {{ t("modpack.downloads", { count: pack.downloads.toLocaleString() }) }}
                  <template v-if="pack.author"> · {{ pack.author }}</template>
                </span>
              </div>
              <select
                v-if="pack.files.length"
                class="version-pick mono"
                :value="curseFile(pack) ?? undefined"
                :disabled="installing"
                @change="curseChoice[pack.project_id] = Number(($event.target as HTMLSelectElement).value)"
              >
                <option v-for="file in pack.files" :key="file.file_id" :value="file.file_id">
                  {{ curseLabel(file) }}
                </option>
              </select>
              <button class="primary small" :disabled="installing" @click="installCurse(pack)">
                {{ t("modpack.install") }}
              </button>
            </article>
          </div>
          <p v-else-if="!curseLoading" class="hint">{{ t("modpack.noResults") }}</p>
          <div v-if="curseTotal > cursePacks.length" class="pager">
            <button class="ghost small" :disabled="curseOffset === 0 || curseLoading" @click="searchCurse(Math.max(0, curseOffset - 20))">
              {{ t("modpack.prev") }}
            </button>
            <span class="hint">{{ cursePacks.length }} / {{ curseTotal }}</span>
            <button class="ghost small" :disabled="curseLoading || curseOffset + cursePacks.length >= curseTotal" @click="searchCurse(curseOffset + 20)">
              {{ t("modpack.next") }}
            </button>
          </div>
        </template>

        <template v-else>
          <div v-if="ftbPacks.length" class="pack-list">
            <article v-for="pack in ftbPacks" :key="pack.id" class="pack-card">
              <img v-if="pack.icon_url" :src="pack.icon_url" alt="" class="pack-icon" @error="hideImage" />
              <div class="pack-text">
                <strong>{{ pack.name }}</strong>
                <small>{{ pack.synopsis }}</small>
                <span class="pack-meta mono">
                  {{ t("modpack.installs", { count: pack.installs.toLocaleString() }) }}
                  <template v-if="pack.tags.length"> · {{ pack.tags.slice(0, 4).join(", ") }}</template>
                </span>
              </div>
              <select
                class="version-pick mono"
                :value="ftbVersion(pack) ?? undefined"
                :disabled="installing"
                @change="ftbChoice[pack.id] = Number(($event.target as HTMLSelectElement).value)"
              >
                <option v-for="version in [...pack.versions].reverse()" :key="version.id" :value="version.id">
                  {{ versionLabel(pack, version.id) }}
                </option>
              </select>
              <button class="primary small" :disabled="installing" @click="installFtb(pack)">
                {{ t("modpack.install") }}
              </button>
            </article>
          </div>
          <p v-else-if="!ftbLoading" class="hint">{{ t("modpack.noResults") }}</p>
        </template>

        <label class="name-field">
          <span>{{ t("modpack.name") }} <small>{{ t("modpack.nameHint") }}</small></span>
          <input v-model="name" spellcheck="false" :placeholder="t('modpack.optional')" :disabled="installing" />
        </label>

        <TransferBar
          v-if="installing || percent"
          :label="phaseLabel"
          :done="activeBytes"
          :total="activeTotal"
          :speed="activeSpeed"
          :detail="progressDetail"
          :indeterminate="phase === 'extract'"
          :busy="installing"
          @cancel="cancelDownload"
        />
      </div>

      <div v-else class="body">
        <label>
          <span>{{ t("modpack.path") }}</span>
          <div class="row">
            <input
              v-model="path"
              class="mono"
              spellcheck="false"
              :placeholder="t('modpack.pathPlaceholder')"
              :disabled="installing"
              @change="inspect"
            />
            <button class="ghost" :disabled="inspecting || installing" @click="inspect">
              {{ inspecting ? t("modpack.checking") : t("modpack.check") }}
            </button>
          </div>
        </label>

        <div v-if="info" class="summary">
          <div class="summary-head">
            <strong>{{ info.name }}</strong>
            <span class="badge version mono">{{ t("modpack.gameVersion", { version: info.game_version }) }}</span>
            <span class="badge mono">{{ loaderLabel }}</span>
          </div>
          <p v-if="info.summary">{{ info.summary }}</p>
          <small class="mono">{{ t("modpack.files", { count: info.files, size: formatBytes(info.bytes) }) }}</small>
        </div>

        <label>
          <span>{{ t("modpack.name") }} <small>{{ t("modpack.nameHint") }}</small></span>
          <input v-model="name" spellcheck="false" :placeholder="t('modpack.optional')" :disabled="installing" />
        </label>

        <TransferBar
          v-if="installing || percent"
          :label="phaseLabel"
          :done="activeBytes"
          :total="activeTotal"
          :speed="activeSpeed"
          :detail="progressDetail"
          :indeterminate="phase === 'extract'"
          :busy="installing"
          @cancel="cancelDownload"
        />
      </div>

      <footer>
        <button class="ghost" :disabled="installing" @click="$emit('close')">{{ t("action.cancel") }}</button>
        <button v-if="mode === 'file'" class="primary" :disabled="installing || inspecting" @click="run">
          <Icon name="download" :size="15" />
          {{ installing ? t("modpack.installing") : t("modpack.install") }}
        </button>
      </footer>
    </section>
  </div>
</template>

<style scoped>
.overlay {
  position: fixed;
  inset: 0;
  z-index: 40;
  display: grid;
  place-items: center;
  padding: 28px;
  background: var(--overlay);
  backdrop-filter: blur(2px);
}

.dialog {
  width: min(560px, 100%);
  display: flex;
  flex-direction: column;
  gap: 14px;
  padding: 18px 20px 16px;
  border: 1px solid var(--border);
  border-radius: 16px;
  background: linear-gradient(180deg, var(--dialog-a), var(--dialog-b));
  box-shadow: 0 30px 70px rgba(0, 0, 0, 0.55);
}

header {
  display: flex;
  align-items: center;
  gap: 12px;
}

.chip {
  display: grid;
  place-items: center;
  width: 36px;
  height: 36px;
  border-radius: 10px;
  color: var(--accent);
  background: rgba(111, 220, 111, 0.12);
  border: 1px solid rgba(111, 220, 111, 0.24);
}

.head-text h2 {
  font-size: 16px;
  margin: 0;
}

.head-text p {
  margin: 2px 0 0;
  font-size: 12px;
  color: var(--muted);
}

header .icon {
  margin-inline-start: auto;
}

.body {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.mode-switch {
  align-self: flex-start;
}

.browse {
  max-height: min(520px, 62vh);
  overflow-y: auto;
}

.pack-list {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.pack-card {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 10px;
  border-radius: 12px;
  border: 1px solid var(--border);
  background: var(--surface-1);
  flex-wrap: wrap;
}

.pack-icon {
  width: 44px;
  height: 44px;
  border-radius: 10px;
  object-fit: cover;
  flex: none;
  background: var(--surface-3);
}

.pack-text {
  display: flex;
  flex-direction: column;
  gap: 3px;
  min-width: 160px;
  flex: 1 1 220px;
}

.pack-text strong {
  font-size: 13px;
}

.pack-text small {
  font-size: 11.5px;
  color: var(--muted);
  overflow: hidden;
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
}

.pack-meta {
  font-size: 10.5px;
  color: var(--muted-2);
}

.pager {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 12px;
}

.segmented {
  display: inline-flex;
  padding: 3px;
  gap: 3px;
  border-radius: 10px;
  background: var(--field);
  border: 1px solid var(--border);
}

.source-switch {
  align-self: flex-start;
}

.name-field {
  margin-top: 2px;
}

.version-pick {
  flex: 1 1 200px;
  max-width: 260px;
  padding: 6px 8px;
  border-radius: 8px;
  border: 1px solid var(--border);
  background: var(--field);
  color: var(--text);
  font-size: 11px;
}

.fill.pulse {
  width: 35%;
  animation: sweep 1.1s ease-in-out infinite;
}

@keyframes sweep {
  0% {
    margin-left: -35%;
  }
  100% {
    margin-left: 100%;
  }
}

.segmented button {
  border: 0;
  background: transparent;
  padding: 5px 12px;
  border-radius: 7px;
  font-size: 12px;
}

.segmented button.active {
  background: var(--accent-soft);
  color: var(--accent);
}

.hint {
  margin: 0;
  font-size: 11.5px;
  color: var(--muted-2);
}

.body label {
  display: flex;
  flex-direction: column;
  gap: 6px;
  font-size: 11.5px;
  color: var(--muted);
}

.body label small {
  color: var(--muted-2);
}

.row {
  display: flex;
  gap: 8px;
}

.sort-row {
  align-items: center;
}

.sort-row select {
  width: auto;
}

.row input {
  flex: 1;
  min-width: 0;
}

.summary {
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding: 12px;
  border-radius: 12px;
  border: 1px solid var(--border);
  background: var(--field-soft);
}

.summary-head {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 8px;
}

.summary p {
  margin: 0;
  font-size: 12px;
  line-height: 1.5;
  color: var(--muted);
}

.summary small {
  color: var(--muted-2);
  font-size: 11px;
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
  gap: 10px;
  font-size: 10.5px;
  color: var(--muted-2);
  overflow: hidden;
}

.progress-foot span {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

footer {
  display: flex;
  justify-content: flex-end;
  gap: 10px;
  padding-top: 4px;
}
</style>
