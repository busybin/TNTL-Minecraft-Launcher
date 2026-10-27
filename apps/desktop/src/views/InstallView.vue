<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from "vue";
import {
  api,
  events,
  formatBytes,
  formatSpeed,
  type InstalledVersion,
  type LoaderKind,
  type LoaderView,
  type UnlistenFn,
  type VersionView,
} from "../api";
import Icon from "../components/Icon.vue";
import { t } from "../i18n";
import { maybePromptJava } from "../javaPrompt";
import { installSpeed as speed, installState, resetInstallTransfer } from "../installState";

const props = defineProps<{ installed: string[] }>();
const emit = defineEmits<{ installed: []; open: [name: string] }>();

const versions = ref<VersionView[]>([]);
const loading = ref(false);
const loadError = ref<string | null>(null);
const search = ref("");
const channel = ref<"release" | "snapshot" | "all">("release");

/** Versions on this machine, with sizes and the instances that need them. */
const local = ref<InstalledVersion[]>([]);
const localOpen = ref(false);
const localBusy = ref<string | null>(null);
const localTotal = computed(() => local.value.reduce((sum, version) => sum + version.bytes, 0));
const unusedLocal = computed(() => local.value.filter((version) => !version.instances.length));

async function loadLocal() {
  try {
    const list = await api.installedVersions();
    // Unused versions first: they are the ones worth deleting.
    local.value = list.sort((left, right) => left.instances.length - right.instances.length);
  } catch {
    local.value = [];
  }
}

function toggleLocal() {
  localOpen.value = !localOpen.value;
  if (localOpen.value) void loadLocal();
}

async function removeVersion(version: InstalledVersion) {
  if (!window.confirm(t("install.local.deleteConfirm", { id: version.id }))) return;
  localBusy.value = version.id;
  try {
    await api.deleteVersion(version.id);
    await loadLocal();
    emit("installed");
  } catch (cause) {
    loadError.value = String(cause);
  } finally {
    localBusy.value = null;
  }
}

async function removeUnused() {
  const unused = unusedLocal.value;
  if (!unused.length) return;
  if (!window.confirm(t("install.local.cleanupConfirm", { count: unused.length }))) return;
  localBusy.value = "*";
  for (const version of unused) {
    try {
      await api.deleteVersion(version.id);
    } catch {
      // A version that turned out to be in use stays; the list shows why.
    }
  }
  await loadLocal();
  localBusy.value = null;
  emit("installed");
}

const selected = ref<string | null>(null);
const instanceName = ref("");
const withAssets = ref(true);
const { installing, stage, progress, result, installError } = installState;

/** Inline dialog state for "new instance from this installed version". */
const createFrom = ref<string | null>(null);
const createName = ref("");
const createError = ref<string | null>(null);
const createBusy = ref(false);
const createInput = ref<HTMLInputElement | null>(null);

function beginCreate(versionId: string) {
  const used = new Set(local.value.flatMap((entry) => entry.instances));
  let name = versionId;
  let index = 2;
  while (used.has(name)) name = `${versionId}-${index++}`;
  createFrom.value = versionId;
  createName.value = name;
  createError.value = null;
  void nextTick(() => createInput.value?.select());
}

async function confirmCreate() {
  const versionId = createFrom.value;
  const name = createName.value.trim();
  if (!versionId || !name) return;
  createBusy.value = true;
  createError.value = null;
  try {
    await api.createInstanceFromVersion(versionId, name);
    createFrom.value = null;
    await loadLocal();
    emit("installed");
    emit("open", name);
  } catch (cause) {
    createError.value = String(cause);
  } finally {
    createBusy.value = false;
  }
}

const loaderKind = ref<"vanilla" | LoaderKind>("vanilla");
const loaderVersions = ref<LoaderView[]>([]);
const loaderVersion = ref<string | null>(null);
const loaderLoading = ref(false);
const loaderError = ref<string | null>(null);

const unlisteners: UnlistenFn[] = [];

const filtered = computed(() => {
  const term = search.value.trim().toLowerCase();
  if (!term) return versions.value;
  return versions.value.filter((version) => version.id.toLowerCase().includes(term));
});

const installedSet = computed(() => new Set(props.installed));
/** Instance names already using the picked version, for the open button. */
const pickedInstances = computed(() => local.value.find((entry) => entry.id === picked.value?.id)?.instances ?? []);
const pickedInstalled = computed(
  () => installedSet.value.has(picked.value?.id ?? "") || picked.value?.installed === true,
);

const picked = computed(() => versions.value.find((version) => version.id === selected.value) ?? null);

const LOADER_LABELS: Record<LoaderKind, string> = {
  fabric: "Fabric",
  quilt: "Quilt",
  legacyfabric: "Legacy Fabric",
  forge: "Forge",
  neoforge: "NeoForge",
};

/** Legacy Fabric stops at 1.13.2; the meta server answers 400 past that. */
function legacyFabricSupported(game: string | null): boolean {
  if (!game) return false;
  const match = /^1\.(\d+)(?:\.|$)/.exec(game.trim());
  return match !== null && Number(match[1]) <= 13;
}

function loaderAvailable(kind: "vanilla" | LoaderKind): boolean {
  if (kind !== "legacyfabric") return true;
  return legacyFabricSupported(selected.value);
}

const loaderLabel = computed(() =>
  loaderKind.value === "vanilla" ? "Vanilla" : LOADER_LABELS[loaderKind.value],
);

const stableLoaders = computed(() => loaderVersions.value.filter((entry) => entry.stable));
const betaLoaders = computed(() => loaderVersions.value.filter((entry) => !entry.stable));

const KIND_KEYS: Record<string, string> = {
  release: "install.kind.release",
  snapshot: "install.kind.snapshot",
  old_beta: "install.kind.old_beta",
  old_alpha: "install.kind.old_alpha",
};

function kindLabel(kind: string): string {
  const key = KIND_KEYS[kind];
  return key ? t(key) : kind;
}

/** Vanilla keeps the plain version id, loaders get a `-fabric` suffix. */
function suggestedName(game: string) {
  return loaderKind.value === "vanilla" ? game : `${game}-${loaderKind.value}`;
}

let lastName = "";

async function loadLoaders() {
  loaderVersions.value = [];
  loaderVersion.value = null;
  loaderError.value = null;
  const game = selected.value;
  const kind = loaderKind.value;
  if (!game || kind === "vanilla") return;
  if (!loaderAvailable(kind)) {
    loaderError.value = t("install.loaderUnsupported", { loader: loaderLabel.value, game });
    return;
  }
  loaderLoading.value = true;
  try {
    const list = await api.listLoaders(game, kind);
    if (game !== selected.value || kind !== loaderKind.value) return;
    loaderVersions.value = list;
    loaderVersion.value = (list.find((entry) => entry.stable) ?? list[0])?.version ?? null;
    if (!list.length) loaderError.value = t("install.loaderNoBuild", { loader: loaderLabel.value, game });
  } catch (cause) {
    loaderError.value = String(cause);
  } finally {
    loaderLoading.value = false;
  }
}

watch([selected, loaderKind], () => {
  const game = selected.value;
  if (!loaderAvailable(loaderKind.value)) loaderKind.value = "vanilla";
  const next = game ? suggestedName(game) : "";
  if (!instanceName.value || instanceName.value === lastName) instanceName.value = next;
  lastName = next;
  void loadLoaders();
});

const stageLabel = computed(() => {
  const stageName = stage.value?.stage;
  if (stageName && ["manifest", "metadata", "libraries", "assets", "installer", "processors"].includes(stageName)) {
    return t(`install.stage.${stageName}`);
  }
  return t("install.stage.preparing");
});

async function load() {
  loading.value = true;
  loadError.value = null;
  const requested = channel.value;
  try {
    const list = await api.listVersions(requested);
    if (requested === channel.value) versions.value = list;
  } catch (cause) {
    if (requested === channel.value) loadError.value = String(cause);
  } finally {
    if (requested === channel.value) loading.value = false;
  }
}

function pick(version: VersionView) {
  selected.value = version.id;
  instanceName.value = suggestedName(version.id);
  lastName = instanceName.value;
  result.value = null;
  installError.value = null;
  stage.value = null;
  progress.value = 0;
}

function cancelInstall() {
  void api.cancelDownload().catch(() => {});
}

async function start() {
  if (!selected.value) return;
  if (loaderKind.value !== "vanilla" && !loaderVersion.value) {
    installError.value = t("install.pickBuild", { loader: loaderLabel.value });
    return;
  }
  installing.value = true;
  installError.value = null;
  result.value = null;
  progress.value = 0;
  try {
    await api.installVersion(
      selected.value,
      withAssets.value,
      instanceName.value.trim() || null,
      loaderKind.value === "vanilla" ? null : loaderKind.value,
      loaderKind.value === "vanilla" ? null : loaderVersion.value,
    );
  } catch (cause) {
    const message = String(cause);
    if (message.toLowerCase().includes("cancelled")) installError.value = t("install.cancelled");
    else if (!maybePromptJava(message, () => void start())) installError.value = message;
    installing.value = false;
  }
}

onMounted(async () => {
  await load();
  void loadLocal();
  try {
    unlisteners.push(
      await events.onInstallStart((payload) => {
        stage.value = null;
        progress.value = 0;
        resetInstallTransfer();
        result.value = null;
        installError.value = null;
        installing.value = true;
        if (!instanceName.value)
          instanceName.value =
            payload.instance ?? (payload.loader ? `${payload.version}-${payload.loader}` : payload.version);
      }),
    );
  } catch (cause) {
    installError.value = String(cause);
  }
});

onUnmounted(() => unlisteners.forEach((unlisten) => unlisten()));

watch(() => props.installed, () => void loadLocal());
watch(installing, (now, before) => {
  if (before && !now) void loadLocal();
});
</script>

<template>
  <div class="page">
    <section class="list-pane">
      <div class="toolbar">
        <label class="search">
          <Icon name="search" :size="15" />
          <input v-model="search" :placeholder="t('install.searchPlaceholder')" spellcheck="false" />
        </label>

        <div class="segmented">
          <button :class="{ active: channel === 'release' }" @click="channel = 'release'; load()">
            {{ t("install.releases") }}
          </button>
          <button :class="{ active: channel === 'snapshot' }" @click="channel = 'snapshot'; load()">
            {{ t("install.snapshots") }}
          </button>
          <button :class="{ active: channel === 'all' }" @click="channel = 'all'; load()">
            {{ t("install.allVersions") }}
          </button>
        </div>

        <button class="icon" :title="t('install.reload')" :disabled="loading" @click="load">
          <Icon name="refresh" />
        </button>

        <span class="count">{{ filtered.length }}</span>
      </div>

      <p v-if="loadError" class="error">{{ loadError }}</p>

      <section v-if="local.length" class="local">
        <button class="local-head" @click="toggleLocal">
          <Icon name="cube" :size="13" />
          <span class="local-title">{{ t("install.local.title") }}</span>
          <span class="count">{{ local.length }}</span>
          <span class="local-total mono">{{ formatBytes(localTotal) }}</span>
          <Icon name="chevron" :size="14" class="local-chev" :class="{ open: localOpen }" />
        </button>
        <div v-if="localOpen" class="local-body">
          <p v-if="!local.length" class="empty-note">{{ t("install.local.empty") }}</p>
          <div v-for="version in local" :key="version.id" class="local-row">
            <span class="mono local-id">{{ version.id }}</span>
            <span class="mono local-bytes">{{ formatBytes(version.bytes) }}</span>
            <span class="local-use" :class="{ unused: !version.instances.length }">
              {{ version.instances.length
                ? t("install.local.usedBy", { names: version.instances.join(", ") })
                : t("install.local.unused") }}
            </span>
            <button
              class="icon"
              :disabled="localBusy !== null"
              :title="t('install.local.create')"
              @click="beginCreate(version.id)"
            >
              <Icon name="plus" :size="14" />
            </button>
            <button
              class="icon danger"
              :disabled="version.instances.length > 0 || localBusy !== null"
              :title="version.instances.length
                ? t('install.local.inUse', { names: version.instances.join(', ') })
                : t('install.local.delete')"
              @click="removeVersion(version)"
            >
              <Icon name="trash" :size="14" />
            </button>
          </div>
          <button
            v-if="unusedLocal.length"
            class="ghost local-clean"
            :disabled="localBusy !== null"
            @click="removeUnused"
          >
            <Icon name="trash" :size="13" />
            {{ t("install.local.cleanup", { count: unusedLocal.length }) }}
          </button>
        </div>
      </section>

      <div class="rows scroll">
        <template v-if="loading && !versions.length">
          <div v-for="index in 6" :key="index" class="row skeleton">
            <span class="sk-id" /><span class="sk-chip" /><span class="sk-date" />
          </div>
        </template>

        <button
          v-for="version in filtered"
          :key="version.id"
          class="row"
          :class="{ selected: selected === version.id }"
          @click="pick(version)"
        >
          <span class="id mono">{{ version.id }}</span>
          <span class="kind" :class="version.version_type">{{ kindLabel(version.version_type) }}</span>
          <span class="date">{{ version.release_time.slice(0, 10) }}</span>
          <span v-if="installedSet.has(version.id) || version.installed" class="badge ok">
            <Icon name="check" :size="11" />{{ t("install.installed") }}
          </span>
          <Icon v-else name="chevron" :size="15" class="chev" />
        </button>

        <p v-if="!loading && !filtered.length && versions.length" class="empty-note">{{ t("install.noMatch", { query: search }) }}</p>
      </div>
    </section>

    <aside class="detail card">
      <template v-if="picked">
        <span class="eyebrow"><Icon name="download" :size="13" />{{ t("install.selectedVersion") }}</span>
        <h2 class="mono">{{ picked.id }}</h2>

        <div class="chips">
          <span class="kind" :class="picked.version_type">{{ kindLabel(picked.version_type) }}</span>
          <span class="badge"><Icon name="info" :size="12" />{{ picked.release_time.slice(0, 10) }}</span>
          <span v-if="installedSet.has(picked.id) || picked.installed" class="badge ok">
            <Icon name="check" :size="11" />{{ t("install.installed") }}
          </span>
        </div>

        <div class="field">
          <span>{{ t("install.modLoader") }}</span>
          <div class="segmented">
            <button
              :class="{ active: loaderKind === 'vanilla' }"
              :disabled="installing"
              @click="loaderKind = 'vanilla'"
            >
              Vanilla
            </button>
            <button :class="{ active: loaderKind === 'fabric' }" :disabled="installing" @click="loaderKind = 'fabric'">
              Fabric
            </button>
            <button :class="{ active: loaderKind === 'quilt' }" :disabled="installing" @click="loaderKind = 'quilt'">
              Quilt
            </button>
            <button
              :class="{ active: loaderKind === 'legacyfabric' }"
              :disabled="installing || !loaderAvailable('legacyfabric')"
              :title="loaderAvailable('legacyfabric') ? '' : t('install.loaderUnsupported', { loader: 'Legacy Fabric', game: picked.id })"
              @click="loaderKind = 'legacyfabric'"
            >
              Legacy Fabric
            </button>
            <button :class="{ active: loaderKind === 'forge' }" :disabled="installing" @click="loaderKind = 'forge'">
              Forge
            </button>
            <button
              :class="{ active: loaderKind === 'neoforge' }"
              :disabled="installing"
              @click="loaderKind = 'neoforge'"
            >
              NeoForge
            </button>
          </div>
        </div>

        <label v-if="loaderKind !== 'vanilla'" class="field">
          <span>{{ t("install.build", { loader: loaderLabel }) }}</span>
          <select v-model="loaderVersion" :disabled="installing || loaderLoading">
            <option v-if="loaderLoading || !loaderVersions.length" :value="null">
              {{ loaderLoading ? t("install.loadingBuilds") : t("install.noBuilds") }}
            </option>
            <optgroup v-if="stableLoaders.length" :label="t('install.stable')">
              <option v-for="entry in stableLoaders" :key="entry.version" :value="entry.version">
                {{ entry.version }}
              </option>
            </optgroup>
            <optgroup v-if="betaLoaders.length" :label="t('install.beta')">
              <option v-for="entry in betaLoaders" :key="entry.version" :value="entry.version">
                {{ entry.version }}
              </option>
            </optgroup>
          </select>
        </label>

        <p v-if="loaderError" class="error">{{ loaderError }}</p>

        <label class="field">
          <span>{{ t("install.instanceName") }}</span>
          <input v-model="instanceName" spellcheck="false" :disabled="installing" />
        </label>

        <label class="switch">
          <input v-model="withAssets" type="checkbox" :disabled="installing" />
          <span class="track"><span class="knob" /></span>
          <span class="switch-text">
            <strong>{{ t("install.downloadAssets") }}</strong>
            <small>{{ t("install.downloadAssetsHint") }}</small>
          </span>
        </label>

        <div v-if="pickedInstalled" class="installed-actions">
          <button
            v-if="pickedInstances.length"
            class="primary install open"
            :disabled="installing"
            @click="$emit('open', pickedInstances[0])"
          >
            <Icon name="check" :size="16" />
            {{ t("install.installedOpen") }}
          </button>
          <button v-else class="ghost install" disabled>
            <Icon name="check" :size="16" />
            {{ t("install.installedFlat", { names: pickedInstances.join(", ") }) }}
          </button>
          <button class="ghost install" :disabled="installing" @click="beginCreate(picked.id)">
            <Icon name="plus" :size="16" />
            {{ t("install.local.create") }}
          </button>
        </div>
        <button v-else class="primary install" :disabled="installing" @click="start">
          <Icon name="download" :size="16" />
          {{ installing ? t("install.installing") : t("install.install") }}
        </button>
      </template>

      <div v-else class="placeholder">
        <div class="placeholder-art"><Icon name="cube" :size="34" /></div>
        <p>{{ t("install.pickVersion") }}</p>
        <small>{{ t("install.noMirrors") }}</small>
      </div>

      <!-- The install runs in the backend; keep its progress visible after the
           page remounts with no version selected. -->
      <div v-if="installing || progress" class="progress-block">
        <div class="progress-head">
          <span>{{ stageLabel }}</span>
          <button v-if="installing" class="cancel" type="button" @click="cancelInstall">
            {{ t("action.cancel") }}
          </button>
          <span class="mono">
            {{ stage ? `${stage.done}/${stage.total}` : "" }}
          </span>
        </div>
        <div class="bar"><div class="fill" :style="{ width: `${progress}%` }" /></div>
        <div class="progress-foot mono">
          <span>{{ stage ? formatBytes(stage.bytes) : "0 B" }}</span>
          <span v-if="speed" class="speed">{{ formatSpeed(speed) }}</span>
          <span>{{ Math.round(progress) }}%</span>
        </div>
      </div>

      <p v-if="installError" class="error">{{ installError }}</p>
      <p v-if="result" class="ok banner"><Icon name="check" :size="14" />{{ result }}</p>
    </aside>

    <Teleport to="body">
      <div v-if="createFrom" class="create-overlay" @click.self="createFrom = null">
        <section class="create-dialog">
          <h3>{{ t("install.local.createTitle", { id: createFrom ?? "" }) }}</h3>
          <label class="field">
            <span>{{ t("install.local.createName") }}</span>
            <input ref="createInput" v-model="createName" spellcheck="false" @keyup.enter="confirmCreate" />
          </label>
          <p v-if="createError" class="error">{{ createError }}</p>
          <div class="create-actions">
            <button class="ghost" :disabled="createBusy" @click="createFrom = null">
              {{ t("action.cancel") }}
            </button>
            <button class="primary" :disabled="createBusy || !createName.trim()" @click="confirmCreate">
              {{ createBusy ? t("editor.working") : t("install.local.createConfirm") }}
            </button>
          </div>
        </section>
      </div>
    </Teleport>
  </div>
</template>

<style scoped>
.page {
  flex: 1;
  min-height: 0;
  height: 100%;
  display: grid;
  grid-template-columns: minmax(0, 1fr) minmax(300px, 340px);
  grid-template-rows: minmax(0, 1fr);
  gap: 18px;
  overflow: hidden;
}

.list-pane {
  display: flex;
  flex-direction: column;
  gap: 12px;
  min-width: 0;
  min-height: 0;
  overflow: hidden;
}

.toolbar {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
}

.search {
  display: flex;
  align-items: center;
  gap: 8px;
  flex: 1;
  min-width: 170px;
  max-width: 320px;
  background: var(--field-strong);
  border: 1px solid var(--border);
  border-radius: 10px;
  padding: 0 12px;
  color: var(--muted-2);
  transition: border-color 140ms ease;
}

.search:focus-within {
  border-color: rgba(111, 220, 111, 0.55);
}

.search input {
  border: 0;
  background: transparent;
  padding: 9px 0;
  width: 100%;
}

.search input:focus {
  background: transparent;
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
  padding: 6px 10px;
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

.count {
  margin-inline-start: auto;
  font-size: 11.5px;
  color: var(--muted);
  background: var(--surface-4);
  padding: 4px 10px;
  border-radius: 999px;
}

.rows {
  flex: 1;
  min-height: 0;
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: var(--row-bg);
  overflow-y: auto;
  overscroll-behavior: contain;
}

.local {
  flex: none;
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: var(--surface-1);
  overflow: hidden;
}

.local-head {
  display: flex;
  align-items: center;
  gap: 8px;
  width: 100%;
  padding: 9px 14px;
  border: 0;
  border-radius: 0;
  background: transparent;
  color: var(--text);
  text-align: start;
}

.local-head:hover {
  background: var(--surface-3);
}

.local-title {
  font-size: 12.5px;
  font-weight: 550;
}

.local-total {
  font-size: 11px;
  color: var(--muted-2);
}

.local-chev {
  color: var(--muted-2);
  transition: transform 140ms ease;
}

.local-chev.open {
  transform: rotate(90deg);
}

.local-body {
  border-top: 1px solid var(--surface-3);
  max-height: 220px;
  overflow-y: auto;
}

.local-row {
  display: grid;
  grid-template-columns: minmax(0, 1fr) 80px minmax(0, 160px) 30px;
  align-items: center;
  gap: 10px;
  padding: 7px 14px;
  font-size: 12px;
  border-bottom: 1px solid var(--surface-2);
}

.local-id {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.local-bytes {
  color: var(--muted-2);
  text-align: end;
}

.local-use {
  font-size: 11px;
  color: var(--muted);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.local-use.unused {
  color: var(--warning);
}

.local-clean {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  margin: 8px 14px 10px;
  font-size: 11.5px;
}

.row {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto 86px 86px;
  align-items: center;
  gap: 14px;
  width: 100%;
  padding: 10px 16px;
  border: 0;
  border-radius: 0;
  background: transparent;
  text-align: start;
  color: var(--text);
  border-bottom: 1px solid var(--surface-3);
}

.row:last-child {
  border-bottom: 0;
}

.row:hover:not(:disabled) {
  background: var(--surface-3);
}

.row.selected {
  background: linear-gradient(90deg, rgba(111, 220, 111, 0.14), rgba(111, 220, 111, 0.03));
  box-shadow: inset 2px 0 0 var(--accent);
}

.row .id {
  font-size: 13px;
  font-weight: 500;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.row .date {
  color: var(--muted-2);
  font-size: 11.5px;
  text-align: end;
}

.row .chev {
  color: var(--muted-2);
  justify-self: end;
}

.kind {
  display: inline-flex;
  align-items: center;
  font-size: 10.5px;
  font-weight: 600;
  letter-spacing: 0.04em;
  text-transform: uppercase;
  padding: 4px 8px;
  border-radius: 999px;
  border: 1px solid rgba(111, 220, 111, 0.26);
  color: var(--accent);
  background: var(--accent-soft);
  justify-self: start;
}

.kind.snapshot,
.kind.old_alpha,
.kind.old_beta {
  color: var(--warning);
  border-color: rgba(255, 200, 87, 0.28);
  background: rgba(255, 200, 87, 0.1);
}

.kind.release {
  color: var(--muted-2);
  border-color: transparent;
  background: transparent;
  padding-inline: 0;
}

.row:hover .chev {
  transform: translateX(2px);
  color: var(--text);
}

.chev {
  transition: transform 140ms ease, color 140ms ease;
}

.skeleton {
  pointer-events: none;
}

.sk-id,
.sk-chip,
.sk-date {
  height: 12px;
  border-radius: 6px;
  background: linear-gradient(90deg, var(--surface-4), var(--surface-6), var(--surface-4));
  background-size: 200% 100%;
  animation: shimmer 1.2s linear infinite;
}

.sk-id {
  width: 40%;
}

.sk-chip {
  width: 54px;
}

.sk-date {
  width: 70px;
  justify-self: end;
}

@keyframes shimmer {
  to {
    background-position: -200% 0;
  }
}

.empty-note {
  color: var(--muted);
  font-size: 12.5px;
  padding: 18px;
  text-align: center;
}

.detail {
  display: flex;
  flex-direction: column;
  gap: 12px;
  padding: 20px;
  align-self: stretch;
  min-height: 0;
  max-height: 100%;
  overflow-y: auto;
  overflow-x: hidden;
  position: relative;
}

.detail::before {
  content: "";
  position: absolute;
  inset: 0 0 auto;
  height: 150px;
  background: radial-gradient(120% 100% at 15% 0%, rgba(111, 220, 111, 0.16), transparent 72%);
  pointer-events: none;
}

.eyebrow {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 10.5px;
  font-weight: 650;
  letter-spacing: 0.1em;
  text-transform: uppercase;
  color: var(--accent);
}

.detail h2 {
  margin: 0;
  font-size: 22px;
  letter-spacing: -0.01em;
  word-break: break-all;
}

.chips {
  display: flex;
  flex-wrap: wrap;
  gap: 7px;
}

.field {
  display: flex;
  flex-direction: column;
  gap: 6px;
  font-size: 11.5px;
  color: var(--muted);
  margin-top: 4px;
  min-width: 0;
}

.field > select,
.field > input {
  width: 100%;
  min-width: 0;
}

.switch {
  display: flex;
  align-items: center;
  gap: 10px;
  cursor: pointer;
  padding: 10px;
  border-radius: 12px;
  border: 1px solid var(--border);
  background: var(--surface-1);
}

.switch input {
  position: absolute;
  opacity: 0;
  pointer-events: none;
}

.track {
  position: relative;
  width: 34px;
  height: 20px;
  border-radius: 999px;
  background: var(--surface-6);
  transition: background 140ms ease;
  flex: none;
}

.knob {
  position: absolute;
  top: 3px;
  inset-inline-start: 3px;
  width: 14px;
  height: 14px;
  border-radius: 50%;
  background: var(--knob);
  transition: transform 140ms ease, background 140ms ease;
}

.switch input:checked + .track {
  background: rgba(111, 220, 111, 0.85);
}

.switch input:checked + .track .knob {
  transform: translateX(14px);
  background: #06210f;
}

.switch-text {
  display: flex;
  flex-direction: column;
  gap: 2px;
  line-height: 1.3;
}

.switch-text strong {
  font-size: 12.5px;
  font-weight: 550;
  color: var(--text);
}

.switch-text small {
  font-size: 11px;
  color: var(--muted-2);
}

.install {
  height: 42px;
  justify-content: center;
  margin-top: 2px;
}

.installed-actions {
  display: flex;
  flex-direction: column;
  gap: 8px;
  margin-top: 2px;
}

.installed-actions .install {
  margin-top: 0;
}

.create-overlay {
  position: fixed;
  inset: 0;
  z-index: 60;
  display: grid;
  place-items: center;
  padding: 24px;
  background: var(--overlay);
  backdrop-filter: blur(3px);
}

.create-dialog {
  width: min(420px, 100%);
  display: flex;
  flex-direction: column;
  gap: 14px;
  padding: 20px;
  border: 1px solid var(--border);
  border-radius: 16px;
  background: linear-gradient(180deg, var(--dialog-a), var(--dialog-b));
  box-shadow: 0 30px 70px rgba(0, 0, 0, 0.55);
}

.create-dialog h3 {
  margin: 0;
  font-size: 15px;
}

.create-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
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

.cancel {
  border: 1px solid var(--border);
  background: transparent;
  color: var(--muted);
  font-size: 11px;
  border-radius: 6px;
  padding: 1px 7px;
  cursor: pointer;
}
.cancel:hover {
  color: var(--text);
  border-color: var(--accent);
}
.progress-foot {
  display: flex;
  justify-content: space-between;
  font-size: 10.5px;
  color: var(--muted-2);
}

.progress-foot .speed {
  color: var(--text);
}

.placeholder {
  display: flex;
  flex-direction: column;
  align-items: center;
  text-align: center;
  gap: 10px;
  padding: 34px 10px;
  color: var(--muted);
}

.placeholder-art {
  display: grid;
  place-items: center;
  width: 72px;
  height: 72px;
  border-radius: 20px;
  color: var(--accent);
  background: rgba(111, 220, 111, 0.08);
  border: 1px solid rgba(111, 220, 111, 0.18);
}

.placeholder p {
  margin: 0;
  font-size: 13px;
  color: var(--text);
}

.placeholder small {
  font-size: 11.5px;
  line-height: 1.55;
  color: var(--muted-2);
}

.banner {
  display: flex;
  align-items: flex-start;
  gap: 8px;
  background: var(--ok-banner-bg);
  border: 1px solid rgba(111, 220, 111, 0.28);
  color: var(--ok-banner-text);
  border-radius: 12px;
  padding: 10px 12px;
  font-size: 11.5px;
  line-height: 1.5;
}
</style>
