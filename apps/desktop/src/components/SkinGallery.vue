<script setup lang="ts">
/**
 * Online skin panel of the skin editor: nickname lookup (Mojang first, ely.by
 * as the fallback) and the newest public uploads. Selecting a texture only
 * asks the editor to preview it; wearing it stays a separate click.
 */
import { onMounted, ref } from "vue";
import { api, type GalleryFilter, type SkinSource } from "../api";
import SkinHead from "../components/SkinHead.vue";
import { t } from "../i18n";

const emit = defineEmits<{
  preview: [{ nickname: string | null; skin: ArrayBuffer; slim: boolean; source: string }];
}>();

interface Tile {
  id: number;
  bytes: ArrayBuffer;
  slim: boolean | null;
}

const nickname = ref("");
const source = ref<SkinSource>("auto");
const lookupBusy = ref(false);
const lookupError = ref<string | null>(null);
const found = ref<{ name: string; nickname: string; slim: boolean; source: string; bytes: ArrayBuffer } | null>(
  null,
);

const page = ref(0);
/** Cursor used to reach each page, so "previous" can walk back. */
const cursors = ref<(string | null)[]>([]);
const nextCursor = ref<string | null>(null);
const filter = ref<GalleryFilter>("latest");
const tiles = ref<Tile[]>([]);
const galleryBusy = ref(false);
const galleryError = ref<string | null>(null);
const selected = ref<number | null>(null);
const model = ref<"wide" | "slim">("wide");

const sources: SkinSource[] = ["auto", "mojang", "ely"];
const filters: GalleryFilter[] = ["latest", "top", "staff", "slim", "hd", "random"];
const FILTER_LABELS: Record<GalleryFilter, string> = {
  latest: "skin.filterLatest",
  top: "skin.filterTop",
  staff: "skin.filterStaff",
  slim: "skin.filterSlim",
  hd: "skin.filterHd",
  random: "skin.filterRandom",
};
const SOURCE_LABELS: Record<SkinSource, string> = {
  auto: "skin.sourceAuto",
  mojang: "skin.sourceMojang",
  ely: "skin.sourceEly",
};

function toBuffer(bytes: number[]): ArrayBuffer {
  return new Uint8Array(bytes).buffer as ArrayBuffer;
}

async function lookup() {
  const name = nickname.value.trim();
  if (!name) return;
  lookupBusy.value = true;
  lookupError.value = null;
  found.value = null;
  try {
    const result = await api.previewOnlineSkin(name, source.value);
    found.value = {
      name: result.name,
      nickname: result.nickname,
      slim: result.slim,
      source: result.source,
      bytes: toBuffer(result.skin),
    };
    model.value = result.slim ? "slim" : "wide";
    selected.value = null;
    emit("preview", {
      nickname: result.nickname,
      skin: found.value.bytes,
      slim: result.slim,
      source: result.source,
    });
  } catch (cause) {
    lookupError.value = String(cause);
  } finally {
    lookupBusy.value = false;
  }
}

async function load(cursor: string | null, index: number) {
  galleryBusy.value = true;
  galleryError.value = null;
  try {
    const result = await api.skinGallery(filter.value, cursor);
    page.value = index;
    cursors.value = [...cursors.value.slice(0, index), cursor];
    nextCursor.value = result.next;
    tiles.value = result.skins.map((entry) => ({
      id: entry.id,
      bytes: toBuffer(entry.skin),
      slim: entry.slim,
    }));
    selected.value = null;
    if (!result.skins.length) galleryError.value = t("skin.galleryEmpty");
  } catch (cause) {
    galleryError.value = String(cause);
  } finally {
    galleryBusy.value = false;
  }
}

function previous() {
  if (page.value > 0) void load(cursors.value[page.value - 1] ?? null, page.value - 1);
}

function next() {
  if (nextCursor.value) void load(nextCursor.value, page.value + 1);
}

function changeFilter(value: GalleryFilter) {
  filter.value = value;
  cursors.value = [];
  nextCursor.value = null;
  void load(null, 0);
}

function pick(tile: Tile) {
  selected.value = tile.id;
  found.value = null;
  const slim = tile.slim ?? model.value === "slim";
  model.value = slim ? "slim" : "wide";
  emit("preview", { nickname: null, skin: tile.bytes, slim, source: "MineSkin" });
}

/** The model buttons used to repaint themselves while the editor kept the
 *  model captured by the last preview, so Apply wore the old one. */
function changeModel(next: "wide" | "slim") {
  model.value = next;
  const slim = next === "slim";
  if (selected.value !== null) {
    const tile = tiles.value.find((entry) => entry.id === selected.value);
    if (tile) {
      emit("preview", { nickname: null, skin: tile.bytes, slim, source: "MineSkin" });
    }
  } else if (found.value) {
    emit("preview", {
      nickname: found.value.nickname,
      skin: found.value.bytes,
      slim,
      source: found.value.source,
    });
  }
}

onMounted(() => void load(null, 0));
</script>

<template>
  <div class="gallery-panel">
    <div class="lookup">
      <input
        v-model="nickname"
        maxlength="16"
        spellcheck="false"
        :placeholder="t('skin.onlinePlaceholder')"
        @keyup.enter="lookup"
      />
      <select v-model="source" :title="t('skin.source')">
        <option v-for="entry in sources" :key="entry" :value="entry">{{ t(SOURCE_LABELS[entry]) }}</option>
      </select>
      <button class="ghost small" :disabled="lookupBusy || !nickname.trim()" @click="lookup">
        {{ lookupBusy ? t("skin.galleryLoading") : t("skin.onlineLookup") }}
      </button>
    </div>
    <p class="hint">{{ t("skin.onlineHint") }}</p>
    <p v-if="lookupError" class="error">{{ lookupError }}</p>

    <div v-if="found" class="found">
      <SkinHead :bytes="found.bytes" :size="36" />
      <div class="found-meta">
        <strong>{{ found.name }}</strong>
        <span class="hint">
          {{ t("skin.fromSource", { source: found.source }) }} ·
          {{ t("skin.onlineModel", { model: found.slim ? t("skin.slim") : t("skin.wide") }) }}
        </span>
      </div>
    </div>

    <header class="gallery-head">
      <span class="label">{{ t("skin.gallery") }}</span>
      <select
        class="gallery-filter"
        :value="filter"
        :title="t('skin.galleryFilter')"
        :disabled="galleryBusy"
        @change="changeFilter(($event.target as HTMLSelectElement).value as GalleryFilter)"
      >
        <option v-for="entry in filters" :key="entry" :value="entry">{{ t(FILTER_LABELS[entry]) }}</option>
      </select>
      <span class="hint">{{ t("skin.galleryPage", { page: page + 1 }) }}</span>
      <div class="gallery-actions">
        <button class="ghost small" :disabled="galleryBusy || page === 0" @click="previous">
          {{ t("skin.galleryPrev") }}
        </button>
        <button class="ghost small" :disabled="galleryBusy || !nextCursor" @click="next">
          {{ t("skin.galleryNext") }}
        </button>
      </div>
    </header>

    <p v-if="galleryBusy" class="loading">{{ t("skin.galleryLoading") }}</p>
    <p v-else-if="galleryError" class="error">{{ galleryError }}</p>

    <div class="tiles" :class="{ busy: galleryBusy }">
      <button
        v-for="tile in tiles"
        :key="tile.id"
        class="tile"
        :class="{ active: selected === tile.id }"
        :disabled="galleryBusy"
        @click="pick(tile)"
      >
        <SkinHead :bytes="tile.bytes" :size="44" />
      </button>
    </div>

    <div class="gallery-foot">
      <div class="segmented">
        <button :class="{ active: model === 'wide' }" @click="changeModel('wide')">{{ t("skin.wide") }}</button>
        <button :class="{ active: model === 'slim' }" @click="changeModel('slim')">{{ t("skin.slim") }}</button>
      </div>
      <span class="hint">{{ t("skin.galleryHint") }}</span>
    </div>
  </div>
</template>

<style scoped>
.gallery-panel {
  display: flex;
  flex-direction: column;
  gap: 10px;
  min-height: 0;
}

.lookup {
  display: flex;
  gap: 8px;
}

.lookup input {
  flex: 1;
  min-width: 0;
  padding: 7px 10px;
  font-size: 12px;
}

.lookup select {
  max-width: 140px;
  font-size: 11.5px;
}

.found {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 8px;
  border-radius: 12px;
  border: 1px solid rgba(111, 220, 111, 0.3);
  background: var(--accent-soft);
}

.found-meta {
  display: flex;
  flex-direction: column;
  gap: 2px;
  font-size: 12px;
}

.gallery-head {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.gallery-actions {
  margin-inline-start: auto;
  display: flex;
  gap: 6px;
}

.gallery-filter {
  max-width: 140px;
  font-size: 11.5px;
}

.loading {
  margin: 0;
  font-size: 12px;
  color: var(--accent);
}

.tiles {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(56px, 1fr));
  gap: 8px;
  padding: 10px;
  border-radius: 12px;
  border: 1px solid var(--border);
  background: var(--field-soft);
}

.tiles.busy {
  opacity: 0.45;
}

.tile {
  display: grid;
  place-items: center;
  padding: 5px;
  border-radius: 10px;
  border: 1px solid transparent;
  background: var(--surface-3);
}

.tile:hover {
  border-color: var(--border-strong);
}

.tile.active {
  border-color: var(--accent);
  background: var(--accent-soft);
}

.gallery-foot {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
}
</style>
