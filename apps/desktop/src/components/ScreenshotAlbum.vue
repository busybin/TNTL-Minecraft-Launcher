<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, reactive, ref, watch } from "vue";
import {
  api,
  formatBytes,
  formatDateTime,
  imageUrl,
  type Screenshot,
} from "../api";
import Icon from "./Icon.vue";
import { t } from "../i18n";

const props = defineProps<{ instance: string; shots: Screenshot[] }>();
const emit = defineEmits<{ changed: []; error: [message: string] }>();

const thumbs = reactive<Record<string, string>>({});
/** Files whose PNG could not be read; the tile shows a warning instead of a spinner. */
const failed = reactive<Record<string, boolean>>({});
const pending = new Set<string>();
const queue: string[] = [];
let active = 0;
let disposed = false;
let observer: IntersectionObserver | null = null;
const gridRef = ref<HTMLElement | null>(null);

function ensureObserver(): IntersectionObserver {
  if (observer) return observer;
  observer = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        if (!entry.isIntersecting) continue;
        const file = (entry.target as HTMLElement).dataset.shot;
        if (file) loadThumb(file);
        observer?.unobserve(entry.target);
      }
    },
    { rootMargin: "320px" },
  );
  return observer;
}

/** Only tiles near the viewport ask the shell for a thumbnail. */
function observeTiles() {
  const grid = gridRef.value;
  if (!grid) return;
  for (const tile of grid.querySelectorAll<HTMLElement>(".tile-button")) {
    const file = tile.dataset.shot;
    if (!file || thumbs[file]) continue;
    ensureObserver().observe(tile);
  }
}

function loadThumb(file: string) {
  if (disposed || thumbs[file] || pending.has(file)) return;
  pending.add(file);
  queue.push(file);
  pump();
}

function pump() {
  while (!disposed && active < 2 && queue.length) {
    const file = queue.shift();
    if (file) void runThumb(file);
  }
}

async function runThumb(file: string) {
  active += 1;
  try {
    const bytes = await api.readScreenshotThumbnail(props.instance, file);
    if (disposed) return;
    const url = imageUrl(bytes, "png");
    // Deleting the file while its read was in flight must not leak the blob.
    if (!props.shots.some((shot) => shot.file === file)) {
      URL.revokeObjectURL(url);
      return;
    }
    thumbs[file] = url;
  } catch (cause) {
    if (!disposed) {
      failed[file] = true;
      emit("error", String(cause));
    }
  } finally {
    active -= 1;
    pending.delete(file);
    pump();
  }
}

const viewerFile = ref<string | null>(null);
const viewerUrl = ref<string | null>(null);
const viewerBusy = ref(false);
let viewerToken = 0;

const viewerIndex = computed(() =>
  viewerFile.value ? props.shots.findIndex((shot) => shot.file === viewerFile.value) : -1,
);
const viewerShot = computed(() => (viewerIndex.value >= 0 ? props.shots[viewerIndex.value] : null));

function revokeViewer() {
  if (viewerUrl.value) URL.revokeObjectURL(viewerUrl.value);
  viewerUrl.value = null;
}

async function openShot(shot: Screenshot) {
  viewerFile.value = shot.file;
  const token = ++viewerToken;
  viewerBusy.value = true;
  // Drop the previous image now: the header already names the new file, and
  // keeping the old picture under it invites deleting the wrong one.
  revokeViewer();
  try {
    const bytes = await api.readScreenshot(props.instance, shot.file);
    if (token !== viewerToken) return;
    viewerUrl.value = imageUrl(bytes, shot.file);
  } catch (cause) {
    if (token === viewerToken) {
      emit("error", String(cause));
      closeViewer();
    }
  } finally {
    if (token === viewerToken) viewerBusy.value = false;
  }
}

function closeViewer() {
  viewerToken += 1;
  viewerFile.value = null;
  viewerBusy.value = false;
  revokeViewer();
}

function step(delta: number) {
  const count = props.shots.length;
  if (!count) return;
  const index = viewerIndex.value < 0 ? 0 : viewerIndex.value;
  void openShot(props.shots[(index + delta + count) % count]);
}

function shotDate(timestamp: number | null): string {
  return formatDateTime(timestamp);
}

function dropThumb(file: string) {
  delete failed[file];
  const url = thumbs[file];
  if (!url) return;
  URL.revokeObjectURL(url);
  delete thumbs[file];
}

async function remove(file: string) {
  if (!window.confirm(t("editor.screenshotDeleteConfirm", { file }))) return;
  try {
    await api.deleteScreenshot(props.instance, file);
    dropThumb(file);
    if (viewerFile.value === file) closeViewer();
    emit("changed");
  } catch (cause) {
    emit("error", String(cause));
  }
}

function onKey(event: KeyboardEvent) {
  if (!viewerFile.value) return;
  if (event.key === "Escape") {
    event.preventDefault();
    closeViewer();
  } else if (event.key === "ArrowLeft") {
    event.preventDefault();
    step(-1);
  } else if (event.key === "ArrowRight") {
    event.preventDefault();
    step(1);
  }
}

function reset() {
  closeViewer();
  for (const file of Object.keys(thumbs)) dropThumb(file);
  for (const file of Object.keys(failed)) delete failed[file];
  pending.clear();
  queue.length = 0;
}

watch(
  () => props.instance,
  () => reset(),
);

watch(
  () => props.shots,
  async (list) => {
    const present = new Set(list.map((shot) => shot.file));
    if (viewerFile.value && !present.has(viewerFile.value)) closeViewer();
    for (const file of Object.keys(thumbs)) {
      if (!present.has(file)) dropThumb(file);
    }
    for (const file of Object.keys(failed)) {
      if (present.has(file)) delete failed[file];
    }
    await nextTick();
    observeTiles();
  },
);

onMounted(() => {
  window.addEventListener("keydown", onKey);
  observeTiles();
});

onBeforeUnmount(() => {
  disposed = true;
  window.removeEventListener("keydown", onKey);
  observer?.disconnect();
  observer = null;
  reset();
});
</script>

<template>
  <div class="album">
    <div ref="gridRef" class="album-grid">
      <figure v-for="shot in shots" :key="shot.file" class="album-tile">
        <button
          type="button"
          class="tile-button"
          :data-shot="shot.file"
          :title="shot.file"
          @click="openShot(shot)"
        >
          <img v-if="thumbs[shot.file]" :src="thumbs[shot.file]" alt="" />
          <span v-else-if="failed[shot.file]" class="tile-wait tile-failed">
            <Icon name="alert" :size="24" />
          </span>
          <span v-else class="tile-wait"><Icon name="image" :size="26" /></span>
          <span class="tile-shade">
            <span class="tile-name">{{ shot.file }}</span>
            <span class="tile-meta">
              <span>{{ shotDate(shot.modified_ms) }}</span>
              <span>{{ formatBytes(shot.bytes) }}</span>
            </span>
          </span>
        </button>
        <button
          type="button"
          class="tile-delete icon danger"
          :title="t('editor.deleteScreenshot')"
          @click.stop="remove(shot.file)"
        >
          <Icon name="trash" :size="13" />
        </button>
      </figure>
    </div>

    <Teleport to="body">
      <div v-if="viewerFile" class="viewer" role="dialog" aria-modal="true" @click.self="closeViewer">
        <header class="viewer-bar">
          <div class="viewer-title">
            <strong class="mono">{{ viewerFile }}</strong>
            <small v-if="viewerShot">
              {{ shotDate(viewerShot.modified_ms) }} · {{ formatBytes(viewerShot.bytes) }}
            </small>
          </div>
          <button class="ghost danger-text" type="button" @click="remove(viewerFile)">
            <Icon name="trash" :size="14" />{{ t("editor.deleteScreenshot") }}
          </button>
          <button class="icon" type="button" :title="t('action.close')" @click="closeViewer">
            <Icon name="close" :size="16" />
          </button>
        </header>

        <button
          class="viewer-nav prev"
          type="button"
          :title="t('editor.screenshotPrevious')"
          :disabled="shots.length < 2"
          @click="step(-1)"
        >
          <Icon name="chevron" :size="22" />
        </button>

        <div class="viewer-stage">
          <img v-if="viewerUrl" :src="viewerUrl" :alt="viewerFile" />
          <span v-else-if="viewerBusy" class="viewer-loading">{{ t("editor.loadingImage") }}</span>
        </div>

        <button
          class="viewer-nav next"
          type="button"
          :title="t('editor.screenshotNext')"
          :disabled="shots.length < 2"
          @click="step(1)"
        >
          <Icon name="chevron" :size="22" />
        </button>

        <footer class="viewer-foot">
          {{ Math.max(viewerIndex, 0) + 1 }} / {{ shots.length }}
        </footer>
      </div>
    </Teleport>
  </div>
</template>

<style scoped>
.album {
  min-width: 0;
}

.album-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(210px, 1fr));
  gap: 10px;
}

.album-tile {
  position: relative;
  min-width: 0;
  margin: 0;
}

.tile-button {
  position: relative;
  display: block;
  width: 100%;
  aspect-ratio: 16 / 9;
  padding: 0;
  overflow: hidden;
  border: 1px solid var(--border);
  border-radius: 10px;
  background: var(--surface-3);
  color: var(--muted-2);
  cursor: zoom-in;
}

.tile-button:hover,
.tile-button:focus-visible {
  border-color: var(--accent);
}

.tile-button img {
  display: block;
  width: 100%;
  height: 100%;
  object-fit: cover;
  image-rendering: auto;
}

.tile-wait {
  display: grid;
  place-items: center;
  width: 100%;
  height: 100%;
  background: linear-gradient(135deg, var(--surface-3), var(--surface-2));
}

.tile-failed {
  color: var(--warning);
}

.tile-shade {
  position: absolute;
  inset: auto 0 0 0;
  display: flex;
  flex-direction: column;
  gap: 2px;
  padding: 20px 8px 7px;
  text-align: start;
  background: linear-gradient(180deg, transparent, rgba(0, 0, 0, 0.78));
  opacity: 0;
  transition: opacity 120ms ease;
}

.tile-button:hover .tile-shade,
.tile-button:focus-visible .tile-shade {
  opacity: 1;
}

.tile-name {
  overflow: hidden;
  color: #fff;
  font-family: var(--mono);
  font-size: 10px;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.tile-meta {
  display: flex;
  justify-content: space-between;
  gap: 8px;
  color: rgba(255, 255, 255, 0.72);
  font-size: 9.5px;
}

.tile-delete {
  position: absolute;
  top: 6px;
  inset-inline-end: 6px;
  width: 28px;
  height: 28px;
  border: 1px solid var(--border);
  background: rgba(20, 20, 20, 0.78);
  opacity: 0;
  transition: opacity 120ms ease;
}

.album-tile:hover .tile-delete,
.tile-delete:focus-visible {
  opacity: 1;
}

.viewer {
  position: fixed;
  inset: 0;
  z-index: 80;
  display: grid;
  grid-template-columns: 64px minmax(0, 1fr) 64px;
  grid-template-rows: auto minmax(0, 1fr) auto;
  padding: 14px 16px;
  background: rgba(8, 8, 8, 0.9);
}

.viewer-bar {
  grid-column: 1 / -1;
  display: flex;
  align-items: center;
  gap: 10px;
  padding-bottom: 10px;
}

.viewer-title {
  display: flex;
  flex: 1;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.viewer-title strong {
  overflow: hidden;
  color: var(--text);
  font-size: 12.5px;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.viewer-title small {
  color: var(--muted-2);
  font-size: 10.5px;
}

.viewer-bar .ghost {
  flex: none;
  padding: 5px 10px;
  font-size: 11px;
}

.viewer-stage {
  grid-column: 2;
  grid-row: 2;
  display: grid;
  place-items: center;
  min-width: 0;
  min-height: 0;
}

.viewer-stage img {
  display: block;
  max-width: 100%;
  max-height: calc(100vh - 110px);
  object-fit: contain;
  border: 1px solid var(--border);
  background: #000;
}

.viewer-loading {
  color: var(--muted-2);
  font-size: 12px;
}

.viewer-nav {
  align-self: center;
  justify-self: center;
  display: grid;
  place-items: center;
  width: 44px;
  height: 64px;
  border: 1px solid var(--border);
  background: rgba(40, 40, 40, 0.72);
  color: var(--text);
}

.viewer-nav:hover:not(:disabled) {
  border-color: var(--accent);
  color: var(--accent);
}

.viewer-nav:disabled {
  opacity: 0.3;
}

.viewer-nav.prev {
  grid-column: 1;
  grid-row: 2;
  transform: rotate(180deg);
}

.viewer-nav.next {
  grid-column: 3;
  grid-row: 2;
}

.viewer-foot {
  grid-column: 1 / -1;
  grid-row: 3;
  padding-top: 8px;
  color: var(--muted-2);
  font-size: 11px;
  text-align: center;
}

@media (max-width: 620px) {
  .album-grid {
    grid-template-columns: repeat(auto-fill, minmax(150px, 1fr));
  }

  .viewer {
    grid-template-columns: 44px minmax(0, 1fr) 44px;
    padding: 10px;
  }
}
</style>
