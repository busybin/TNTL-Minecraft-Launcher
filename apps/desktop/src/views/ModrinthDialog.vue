<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import {
  api,
  events,
  formatBytes,
  type ContentSource,
  type ModProgress,
  type ModSearchHit,
  type ProjectKind,
  type SearchIndex,
  type UnlistenFn,
  type VersionChoice,
} from "../api";
import Icon from "../components/Icon.vue";
import TransferBar from "../components/TransferBar.vue";
import { t } from "../i18n";
import { useTransfer } from "../progress";

const props = defineProps<{ instance: string; kind?: ProjectKind }>();
const emit = defineEmits<{ close: []; installed: [] }>();

const KIND_KEYS: ProjectKind[] = ["mod", "resourcepack", "shader"];
const SOURCE_KEYS: ContentSource[] = ["modrinth", "curseforge"];
/** Where a CurseForge API key is issued. */
const CURSEFORGE_KEYS = "https://console.curseforge.com/?#/api-keys";

const query = ref("");
const hits = ref<ModSearchHit[]>([]);
const total = ref(0);
const game = ref("");
const loader = ref("");
/** Empty selects every game version; it starts at the instance's version. */
const version = ref("");
const versionChosen = ref(false);
const loaded = ref(false);
const gameVersions = ref<string[]>([]);
const kind = ref<ProjectKind>(props.kind ?? "mod");
const source = ref<ContentSource>("modrinth");
/** CurseForge needs the user's own key, so the source is gated on it. */
const hasCurseforgeKey = ref(false);
const sort = ref<SearchIndex>("relevance");
const hideInstalled = ref(false);
const visibleHits = computed(() =>
  hideInstalled.value ? hits.value.filter((hit) => !hit.installed) : hits.value,
);
const SORTS: SearchIndex[] = ["relevance", "downloads", "follows", "newest", "updated"];
const searching = ref(false);
const loadingMore = ref(false);
const installing = ref<string | null>(null);

function cancelDownload() {
  void api.cancelDownload().catch(() => {});
}
const activeTitle = ref("");
const activeFile = ref("");
/** Project whose build list is open, and that list. */
const versionsFor = ref<string | null>(null);
const versions = ref<VersionChoice[]>([]);
const versionsBusy = ref(false);
const versionsError = ref<string | null>(null);
let versionsRequest = 0;
const status = ref<string | null>(null);
const error = ref<string | null>(null);
const {
  bytes: transferBytes,
  total: transferTotal,
  speed: transferSpeed,
  track: trackTransfer,
  reset: resetTransfer,
} = useTransfer();
let unlisten: UnlistenFn | null = null;

onMounted(async () => {
  unlisten = await events.onModProgress(onProgress);
  const settings = await api.launcherSettings().catch(() => null);
  hasCurseforgeKey.value = Boolean(settings?.curseforge_api_key);
  void api
    .modGameVersions()
    .then((list) => (gameVersions.value = list))
    .catch(() => {});
  await search(false);
});

onBeforeUnmount(() => unlisten?.());

let requestId = 0;

async function search(more: boolean) {
  if (more && (searching.value || loadingMore.value)) return;
  const serial = more ? requestId : ++requestId;
  const offset = more ? hits.value.length : 0;
  if (more) {
    loadingMore.value = true;
  } else {
    searching.value = true;
  }
  error.value = null;
  try {
    const page = await api.searchProjects(
      props.instance,
      kind.value,
      query.value.trim(),
      sort.value,
      source.value,
      versionChosen.value ? version.value : null,
      20,
      offset,
    );
    // A filter change while this request was in flight starts a new search;
    // never append a page from the old query onto the new one.
    if (serial !== requestId) return;
    game.value = page.game_version;
    loader.value = page.loader;
    if (!versionChosen.value) {
      version.value = page.game_version;
      versionChosen.value = true;
    }
    total.value = page.total_hits;
    hits.value = more ? [...hits.value, ...page.hits] : page.hits;
    loaded.value = true;
  } catch (cause) {
    if (serial === requestId) error.value = String(cause);
  } finally {
    if (serial === requestId) {
      searching.value = false;
      loadingMore.value = false;
    }
  }
}

function pickKind(next: ProjectKind) {
  if (kind.value === next || searching.value || loadingMore.value) return;
  kind.value = next;
  hits.value = [];
  total.value = 0;
  void search(false);
}

function pickSource(next: ContentSource) {
  if (source.value === next || searching.value || loadingMore.value) return;
  source.value = next;
  hits.value = [];
  total.value = 0;
  error.value = null;
  // Without a key the backend can only fail; the panel below explains how to
  // get one instead of showing a network error.
  if (next === "curseforge" && !hasCurseforgeKey.value) {
    status.value = null;
    return;
  }
  void search(false);
}

function pickVersion(next: string) {
  versionChosen.value = true;
  if (version.value === next || searching.value || loadingMore.value) return;
  version.value = next;
  hits.value = [];
  total.value = 0;
  void search(false);
}

function pickSort(next: SearchIndex) {
  if (sort.value === next || searching.value || loadingMore.value) return;
  sort.value = next;
  hits.value = [];
  total.value = 0;
  void search(false);
}

function onProgress(event: ModProgress) {
  switch (event.stage) {
    case "resolving":
      activeTitle.value = event.project;
      resetTransfer();
      status.value = t("modrinth.resolving", { name: event.project });
      break;
    case "downloading": {
      const size = event.total
        ? `${formatBytes(event.done)} / ${formatBytes(event.total)}`
        : formatBytes(event.done);
      activeTitle.value = event.project;
      activeFile.value = event.file;
      trackTransfer(event.done, event.total);
      status.value = t("modrinth.progress", { name: event.project, size });
      break;
    }
    case "installed":
      status.value =
        t("modrinth.installedVersion", { name: event.title, version: event.version }) +
        (event.dependency ? t("modrinth.dependency") : "");
      break;
    case "skipped":
      status.value = t("modrinth.skipped", { name: event.title });
      break;
  }
}

function openSite(hit: ModSearchHit) {
  const slug = hit.slug || hit.project_id;
  const page = kind.value === "mod" ? "mod" : kind.value === "resourcepack" ? "resourcepack" : "shader";
  const url =
    source.value === "curseforge"
      ? `https://www.curseforge.com/minecraft/${page === "mod" ? "mc-mods" : page === "resourcepack" ? "texture-packs" : "shaders"}/${slug}`
      : `https://modrinth.com/${page}/${slug}`;
  void api.openUrl(url);
}

async function install(hit: ModSearchHit) {
  installing.value = hit.slug;
  error.value = null;
  activeTitle.value = hit.title;
  activeFile.value = "";
  resetTransfer();
  status.value = t("modrinth.preparing", { name: hit.title });
  try {
    const mods = await api.installProjects(
      props.instance,
      kind.value,
      [hit.project_id],
      source.value,
      version.value || null,
    );
    const fresh = mods.filter((mod) => !mod.skipped).length;
    status.value = fresh
      ? t("modrinth.installedCount", { count: fresh, name: hit.title })
      : t("modrinth.alreadyInstalled", { name: hit.title });
    // The install also pulls required dependencies; mark every returned
    // project so the list does not keep offering them as downloads.
    for (const mod of mods) {
      const entry = hits.value.find((item) => item.project_id === mod.project_id);
      if (entry) entry.installed = true;
    }
  } catch (cause) {
    error.value = String(cause);
  } finally {
    installing.value = null;
    // Refresh the instance even when the guard switched the jar off: the
    // reason is shown in the mod list.
    emit("installed");
  }
}

async function toggleVersions(hit: ModSearchHit) {
  if (versionsFor.value === hit.project_id) {
    versionsRequest += 1;
    versionsFor.value = null;
    versionsBusy.value = false;
    return;
  }
  const token = ++versionsRequest;
  versionsFor.value = hit.project_id;
  versions.value = [];
  versionsError.value = null;
  versionsBusy.value = true;
  try {
    const list = await api.projectVersions(props.instance, kind.value, hit.project_id);
    if (token !== versionsRequest) return;
    versions.value = list;
  } catch (cause) {
    if (token !== versionsRequest) return;
    versionsError.value = String(cause);
  } finally {
    if (token === versionsRequest) versionsBusy.value = false;
  }
}

async function installVersion(hit: ModSearchHit, choice: VersionChoice) {
  installing.value = hit.slug;
  error.value = null;
  activeTitle.value = hit.title;
  activeFile.value = choice.file;
  resetTransfer();
  status.value = t("modrinth.preparing", { name: hit.title });
  try {
    const mods = await api.installProjectVersion(
      props.instance,
      kind.value,
      hit.project_id,
      choice.id,
      source.value,
    );
    const fresh = mods.filter((mod) => !mod.skipped).length;
    status.value = fresh
      ? t("modrinth.installedVersion", { name: hit.title, version: choice.version_number })
      : t("modrinth.alreadyInstalled", { name: hit.title });
    hit.installed = true;
    versionsFor.value = null;
  } catch (cause) {
    error.value = String(cause);
  } finally {
    installing.value = null;
    emit("installed");
  }
}

function formatDownloads(value: number): string {
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}M`;
  if (value >= 1_000) return `${(value / 1_000).toFixed(1)}K`;
  return String(value);
}
</script>

<template>
  <Teleport to="body">
    <div class="overlay" @click.self="$emit('close')">
      <section class="dialog">
      <header>
        <span class="chip"><Icon name="spark" :size="16" /></span>
        <div class="head-text">
          <h2>{{ t("modrinth.title") }}</h2>
          <p v-if="loaded">{{ t("modrinth.summary", { game: game || t("modrinth.allVersions"), loader: loader || t(`modrinth.kind.${kind}`), count: total }) }}</p>
          <p v-else>{{ t("modrinth.loading") }}</p>
        </div>
        <button class="icon" :title="t('action.close')" @click="$emit('close')"><Icon name="close" /></button>
      </header>

      <div class="sources">
        <button
          v-for="entry in SOURCE_KEYS"
          :key="entry"
          :class="{ active: source === entry }"
          :disabled="searching || installing !== null"
          :title="entry === 'curseforge' && !hasCurseforgeKey ? t('modrinth.needKey') : ''"
          @click="pickSource(entry)"
        >
          {{ t(`modrinth.source.${entry}`) }}
        </button>
      </div>

      <p v-if="source === 'curseforge' && !hasCurseforgeKey" class="empty">
        {{ t("modrinth.needKey") }}
        <button class="ghost small" type="button" @click="api.openUrl(CURSEFORGE_KEYS)">
          <Icon name="external" :size="13" /> {{ t("settings.curseforgeGuide") }}
        </button>
      </p>

      <div class="kinds">
        <button
          v-for="entry in KIND_KEYS"
          :key="entry"
          :class="{ active: kind === entry }"
          :disabled="searching || installing !== null"
          @click="pickKind(entry)"
        >
          {{ t(`modrinth.kind.${entry}`) }}
        </button>
      </div>

      <form class="search" @submit.prevent="search(false)">
        <span class="search-icon"><Icon name="search" :size="14" /></span>
        <input
          v-model="query"
          spellcheck="false"
          :placeholder="t('modrinth.searchPlaceholder')"
          :disabled="searching || installing !== null"
        />
        <select
          class="sort mono"
          :value="version"
          :disabled="searching || installing !== null"
          :title="t('modrinth.gameVersion')"
          @change="pickVersion(($event.target as HTMLSelectElement).value)"
        >
          <option value="">{{ t("modrinth.allVersions") }}</option>
          <option v-for="entry in gameVersions" :key="entry" :value="entry">{{ entry }}</option>
        </select>
        <select
          class="sort mono"
          :value="sort"
          :disabled="searching || installing !== null"
          :title="t('modrinth.sortBy')"
          @change="pickSort(($event.target as HTMLSelectElement).value as SearchIndex)"
        >
          <option v-for="key in SORTS" :key="key" :value="key">{{ t(`modrinth.sort.${key}`) }}</option>
        </select>
        <button class="ghost" type="submit" :disabled="searching || installing !== null">
          {{ searching ? t("modrinth.searching") : t("modrinth.search") }}
        </button>
      </form>

      <label class="hide-installed">
        <input v-model="hideInstalled" type="checkbox" />
        {{ t("modrinth.hideInstalled") }}
      </label>

      <p v-if="error" class="error">{{ error }}</p>

      <div class="results scroll">
        <p v-if="searching && !hits.length" class="empty">{{ t("modrinth.searchingCatalogue") }}</p>
        <p v-else-if="!hits.length" class="empty">{{ t("modrinth.noResults") }}</p>
        <p v-else-if="!visibleHits.length" class="empty">{{ t("modrinth.allInstalled") }}</p>
        <ul v-else>
          <li v-for="hit in visibleHits" :key="hit.project_id">
            <img v-if="hit.icon_url" :src="hit.icon_url" alt="" loading="lazy" />
            <span v-else class="fallback"><Icon name="cube" :size="18" /></span>
            <div class="text">
              <div class="title-line">
                <strong>{{ hit.title }}</strong>
                <small>{{ t("modrinth.by", { author: hit.author, downloads: formatDownloads(hit.downloads) }) }}</small>
              </div>
              <p>{{ hit.description }}</p>
            </div>
            <div class="actions">
              <button class="ghost small" type="button" @click="openSite(hit)">
                <Icon name="external" :size="12" />{{ t("modrinth.openSite") }}
              </button>
              <button
                v-if="source === 'modrinth'"
                class="ghost small"
                :disabled="installing !== null"
                @click="toggleVersions(hit)"
              >
                {{ t("modrinth.versions") }}
              </button>
              <button
                class="primary small"
                :disabled="installing !== null || hit.installed"
                @click="install(hit)"
              >
                {{ hit.installed ? t("modrinth.installed") : installing === hit.slug ? t("modrinth.installing") : t("modrinth.install") }}
              </button>
            </div>
            <div v-if="versionsFor === hit.project_id" class="versions">
              <p v-if="versionsBusy" class="empty">{{ t("modrinth.loading") }}</p>
              <p v-else-if="versionsError" class="error">{{ versionsError }}</p>
              <p v-else-if="!versions.length" class="empty">{{ t("modrinth.noVersions") }}</p>
              <template v-else>
                <div v-for="choice in versions" :key="choice.id" class="version-row">
                  <div class="text">
                    <strong>{{ choice.version_number }}</strong>
                    <small class="mono">
                      {{ choice.version_type }} · {{ choice.game_versions.join(", ") }} · {{ formatBytes(choice.size) }}
                    </small>
                  </div>
                  <button class="ghost small" :disabled="installing !== null" @click="installVersion(hit, choice)">
                    {{ t("modrinth.installThis") }}
                  </button>
                </div>
              </template>
            </div>
          </li>
        </ul>
        <button
          v-if="hits.length < total"
          class="ghost more"
          :disabled="loadingMore || installing !== null"
          @click="search(true)"
        >
          {{ loadingMore ? t("modrinth.loadingMore") : t("modrinth.loadMore") }}
        </button>
      </div>

      <TransferBar
        v-if="installing"
        :label="activeTitle ? t('modrinth.installingName', { name: activeTitle }) : t('modrinth.installing')"
        :done="transferBytes"
        :total="transferTotal"
        :speed="transferSpeed"
        :detail="activeFile"
        :busy="installing !== null"
        @cancel="cancelDownload"
      />

      <footer>
        <span v-if="status" class="status">{{ status }}</span>
        <button class="ghost" @click="$emit('close')">{{ t("action.close") }}</button>
      </footer>
      </section>
    </div>
  </Teleport>
</template>

<style scoped>
.overlay {
  position: fixed;
  inset: 0;
  z-index: 30;
  display: grid;
  place-items: center;
  padding: 28px;
  background: var(--overlay);
  backdrop-filter: blur(3px);
}

.dialog {
  width: min(760px, 100%);
  max-height: calc(100vh - 56px);
  display: flex;
  flex-direction: column;
  gap: 12px;
  padding: 18px 20px 14px;
  border: 1px solid var(--border);
  border-radius: 16px;
  background: linear-gradient(180deg, var(--dialog-a), var(--dialog-b));
  box-shadow: 0 30px 70px rgba(0, 0, 0, 0.55);
  overflow: hidden;
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

.head-text {
  min-width: 0;
}

.head-text h2 {
  margin: 0;
  font-size: 16px;
}

.head-text p {
  margin: 2px 0 0;
  font-size: 11.5px;
  color: var(--muted);
}

header .icon {
  margin-inline-start: auto;
}

.sources {
  display: flex;
  gap: 6px;
}

.sources button {
  padding: 5px 12px;
  font-size: 11.5px;
  border-radius: 999px;
  border: 1px solid var(--border);
  background: var(--field);
  color: var(--muted);
}

.sources button.active {
  border-color: rgba(111, 220, 111, 0.5);
  background: rgba(111, 220, 111, 0.12);
  color: var(--text);
}

.sources button:disabled {
  opacity: 0.45;
}

.kinds {
  display: flex;
  gap: 6px;
  padding: 4px;
  border-radius: 11px;
  background: var(--field);
  border: 1px solid var(--border);
}

.kinds button {
  flex: 1;
  padding: 6px 10px;
  font-size: 11.5px;
  border-radius: 8px;
  border: 1px solid transparent;
  background: transparent;
  color: var(--muted);
}

.kinds button.active {
  color: var(--text);
  background: rgba(111, 220, 111, 0.12);
  border-color: rgba(111, 220, 111, 0.35);
}

.search {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 6px 6px 12px;
  border: 1px solid var(--border);
  border-radius: 12px;
  background: var(--row-bg);
}

.search-icon {
  display: grid;
  place-items: center;
  color: var(--muted-2);
}

.search input {
  flex: 1;
  min-width: 0;
  border: 0;
  background: transparent;
  padding: 6px 0;
}

.search .sort {
  width: auto;
  max-width: 130px;
  font-size: 11.5px;
}

.results {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  gap: 8px;
  overflow-y: auto;
  padding-inline-end: 4px;
  scrollbar-gutter: stable;
}

.results ul {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.hide-installed {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 11.5px;
  color: var(--muted);
  cursor: pointer;
}

.results li {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 12px;
  padding: 10px;
  border: 1px solid var(--border);
  border-radius: 12px;
  background: var(--row-bg);
}

.actions {
  display: flex;
  flex-direction: column;
  gap: 6px;
  flex: none;
}

.versions {
  flex-basis: 100%;
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding-top: 6px;
  border-top: 1px dashed var(--border);
}

.version-row {
  display: flex;
  align-items: center;
  gap: 10px;
}

.version-row .text {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.version-row .text small {
  font-size: 10.5px;
  color: var(--muted-2);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.version-row button {
  margin-inline-start: auto;
  flex: none;
}

.results img,
.fallback {
  flex: none;
  width: 40px;
  height: 40px;
  border-radius: 9px;
  object-fit: cover;
  background: var(--surface-4);
}

.fallback {
  display: grid;
  place-items: center;
  color: var(--muted-2);
}

.text {
  min-width: 0;
  flex: 1;
}

.title-line {
  display: flex;
  align-items: baseline;
  gap: 8px;
  min-width: 0;
}

.title-line strong {
  font-size: 13px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.title-line small {
  font-size: 10.5px;
  color: var(--muted-2);
  white-space: nowrap;
}

.text p {
  margin: 3px 0 0;
  font-size: 11.5px;
  line-height: 1.45;
  color: var(--muted);
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow: hidden;
}

.primary.small {
  flex: none;
  padding: 6px 14px;
  font-size: 11.5px;
}

.empty {
  margin: 0;
  padding: 28px 0;
  text-align: center;
  font-size: 12px;
  color: var(--muted-2);
}

.more {
  align-self: center;
  margin: 4px 0 2px;
}

footer {
  display: flex;
  align-items: center;
  gap: 12px;
  padding-top: 10px;
  border-top: 1px solid var(--border);
}

.status {
  flex: 1;
  min-width: 0;
  font-size: 11.5px;
  color: var(--muted);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

footer .ghost {
  margin-inline-start: auto;
}
</style>
