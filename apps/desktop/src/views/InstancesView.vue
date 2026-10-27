<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { api, formatDate, formatPlaytime, imageUrl, type InstanceView } from "../api";
import CoverArt from "../components/CoverArt.vue";
import Icon from "../components/Icon.vue";
import { t } from "../i18n";

const props = defineProps<{
  instances: InstanceView[];
  running: string | null;
  busy: string | null;
}>();

defineEmits<{
  launch: [name: string];
  stop: [];
  log: [name: string];
  open: [name: string];
  remove: [name: string];
  new: [];
  importMc: [];
  importArchive: [];
  modpack: [];
  refresh: [];
}>();

const query = ref("");
const activeGroup = ref("*");
type IconEntry = { path: string; url: string };

const iconCache = ref<Record<string, IconEntry>>({});
const broken = ref<Record<string, string>>({});

function iconFor(instance: InstanceView): string | null {
  const path = instance.icon;
  if (!path || broken.value[instance.name] === path) return null;
  const cached = iconCache.value[instance.name];
  return cached && cached.path === path ? cached.url : null;
}

function markBroken(name: string) {
  const icon = props.instances.find((instance) => instance.name === name)?.icon;
  if (icon) broken.value[name] = icon;
}

function forgetIcon(name: string) {
  const cached = iconCache.value[name];
  if (cached) URL.revokeObjectURL(cached.url);
  delete iconCache.value[name];
}

async function loadIcons() {
  const live = new Map(props.instances.map((instance) => [instance.name, instance.icon ?? ""]));
  for (const name of Object.keys(iconCache.value)) {
    if (!live.has(name)) forgetIcon(name);
  }
  for (const instance of props.instances) {
    const path = instance.icon ?? "";
    if (!path) {
      forgetIcon(instance.name);
      delete broken.value[instance.name];
      continue;
    }
    // The icon file keeps its name (`icon.png`) when replaced, so the cached
    // bitmap is refreshed on every list change.
    forgetIcon(instance.name);
    delete broken.value[instance.name];
    try {
      const bytes = await api.instanceIcon(instance.name);
      // The icon may have changed again while this request was in flight.
      const current = props.instances.find((entry) => entry.name === instance.name)?.icon;
      if (!bytes.byteLength || current !== instance.icon) continue;
      iconCache.value[instance.name] = { path, url: imageUrl(bytes, path) };
    } catch {
      // An unreadable icon just falls back to the generated cover.
    }
  }
}

watch(() => props.instances, loadIcons, { immediate: true });

onBeforeUnmount(() => {
  Object.keys(iconCache.value).forEach(forgetIcon);
});
const groups = computed(() => {
  const seen = new Set<string>();
  for (const instance of props.instances) {
    if (instance.group) seen.add(instance.group);
  }
  return [...seen].sort((left, right) => left.localeCompare(right));
});

// Deleting the last instance of a group used to leave the filter pointing at
// a group that no chip represents, showing an empty list with no way back.
watch(groups, (next) => {
  if (activeGroup.value !== "*" && !next.includes(activeGroup.value)) activeGroup.value = "*";
});

const visible = computed(() => {
  const needle = query.value.trim().toLowerCase();
  return props.instances.filter((instance) => {
    if (activeGroup.value !== "*" && instance.group !== activeGroup.value) return false;
    if (!needle) return true;
    return (
      instance.name.toLowerCase().includes(needle) ||
      instance.version_id.toLowerCase().includes(needle) ||
      (instance.modpack ?? "").toLowerCase().includes(needle) ||
      (instance.notes ?? "").toLowerCase().includes(needle)
    );
  });
});

function lastPlayed(instance: InstanceView): number {
  if (instance.last_played) return instance.last_played;
  const match = instance.last_log?.match(/(\d+)\.log$/);
  return match ? Number(match[1]) : 0;
}

function playtimeText(instance: InstanceView): string {
  if (!instance.launches) return t("instances.neverPlayed");
  const time = formatPlaytime(instance.playtime_seconds);
  const date = formatDate(instance.last_played);
  return date ? t("instances.playtimeDate", { time, date }) : t("instances.playtime", { time });
}

const featured = computed(() => {
  if (!visible.value.length) return null;
  return [...visible.value].sort((left, right) => lastPlayed(right) - lastPlayed(left))[0];
});

const rest = computed(() => visible.value.filter((instance) => instance !== featured.value));
</script>

<template>
  <div class="page scroll">
    <div v-if="!instances.length" class="empty card">
      <div class="empty-art">
        <Icon name="cube" :size="52" />
      </div>
      <h2>{{ t("instances.emptyTitle") }}</h2>
      <p>{{ t("instances.emptyHint") }}</p>
      <div class="empty-actions">
        <button class="primary" @click="$emit('new')"><Icon name="download" />{{ t("instances.browse") }}</button>
        <button class="ghost" @click="$emit('modpack')"><Icon name="package" />{{ t("instances.modpack") }}</button>
        <button class="ghost" @click="$emit('importMc')"><Icon name="folder" />{{ t("instances.importMc") }}</button>
      </div>
    </div>

    <template v-else>
      <div class="page-actions">
        <button class="primary small" @click="$emit('new')">
          <Icon name="plus" :size="14" /> {{ t("instances.newInstance") }}
        </button>
        <button class="ghost small" @click="$emit('modpack')">
          <Icon name="package" :size="14" /> {{ t("instances.modpack") }}
        </button>
        <button class="ghost small" @click="$emit('importMc')">
          <Icon name="folder" :size="13" /> {{ t("instances.import") }}
        </button>
        <button class="ghost small" @click="$emit('importArchive')">
          <Icon name="package" :size="13" /> {{ t("instances.importArchive") }}
        </button>
      </div>

      <div class="library-bar">
        <label class="search">
          <Icon name="search" :size="15" />
          <input v-model="query" spellcheck="false" :placeholder="t('instances.search')" />
        </label>
        <div class="group-chips">
          <button :class="{ active: activeGroup === '*' }" @click="activeGroup = '*'">
            {{ t("instances.allGroups") }}
          </button>
          <button
            v-for="group in groups"
            :key="group"
            :class="{ active: activeGroup === group }"
            @click="activeGroup = group"
          >
            {{ group }}
          </button>
        </div>
      </div>

      <p v-if="!visible.length" class="empty-filter">{{ t("instances.noMatches") }}</p>

      <section v-if="featured" class="hero card">
        <div class="hero-wash" aria-hidden="true"><CoverArt :seed="featured.name" /></div>
        <div class="hero-art" :title="t('instances.openTitle')" @click="$emit('open', featured.name)">
          <img v-if="iconFor(featured)" :src="iconFor(featured)!" alt="" @error="markBroken(featured.name)" />
          <CoverArt v-else :seed="featured.name" rounded />
        </div>

        <div class="hero-body">
          <span class="eyebrow"><Icon name="spark" :size="13" />{{ t("instances.ready") }}</span>
          <h2 :title="t('instances.openTitle')" @click="$emit('open', featured.name)">{{ featured.name }}</h2>

          <div class="chips">
            <span class="badge version mono">{{ featured.version_id }}</span>
            <span v-if="featured.username" class="badge"><Icon name="user" :size="12" />{{ featured.username }}</span>
            <span class="badge"><Icon name="cpu" :size="12" />{{ featured.max_memory_mb }} MB</span>
            <span class="badge"><Icon name="clock" :size="12" />{{ playtimeText(featured) }}</span>
            <span v-if="featured.modpack" class="badge"><Icon name="package" :size="12" />{{ featured.modpack }}</span>
            <span v-if="featured.group" class="badge"><Icon name="grid" :size="12" />{{ featured.group }}</span>
            <span v-if="featured.imported" class="badge">{{ t("instances.importedBadge") }}</span>
            <span v-if="!featured.installed" class="badge warn">{{ t("instances.missing") }}</span>
            <span v-if="running === featured.name" class="badge ok">{{ t("instances.running") }}</span>
          </div>

          <div class="hero-actions">
            <button
              v-if="running !== featured.name"
              class="primary play"
              :disabled="busy === featured.name"
              @click="$emit('launch', featured.name)"
            >
              <Icon name="play" :size="18" />
              {{ busy === featured.name ? t("instances.starting") : t("instances.play") }}
            </button>
            <button v-else class="danger play" @click="$emit('stop')">
              <Icon name="stop" :size="16" />
              {{ t("instances.stop") }}
            </button>

            <button class="ghost" @click="$emit('log', featured.name)">
              <Icon name="terminal" :size="15" />{{ t("instances.logs") }}
            </button>
            <button class="icon" :title="t('instances.openTitle')" @click="$emit('open', featured.name)">
              <Icon name="edit" />
            </button>
            <button
              class="icon danger"
              :title="t('instances.deleteTitle')"
              :disabled="running === featured.name"
              @click="$emit('remove', featured.name)"
            >
              <Icon name="trash" />
            </button>
          </div>
        </div>
      </section>

      <section v-if="rest.length" class="others">
        <header class="section-head">
          <h3>{{ t("instances.all") }}</h3>
          <span class="count">{{ visible.length }}</span>
        </header>

        <div class="grid">
          <article
            v-for="instance in rest"
            :key="instance.name"
            class="tile openable"
            :title="t('instances.openTitle')"
            @click="$emit('open', instance.name)"
          >
            <div class="tile-art">
              <img v-if="iconFor(instance)" :src="iconFor(instance)!" alt="" @error="markBroken(instance.name)" />
              <CoverArt v-else :seed="instance.name" />
              <div class="tile-overlay">
                <button
                  v-if="running !== instance.name"
                  class="play-btn"
                  :disabled="busy === instance.name"
                  :title="t('instances.playTitle', { name: instance.name })"
                  @click.stop="$emit('launch', instance.name)"
                >
                  <Icon name="play" :size="18" />
                </button>
                <button
                  v-else
                  class="play-btn stop"
                  :title="t('instances.stopTitle')"
                  @click.stop="$emit('stop')"
                >
                  <Icon name="stop" :size="14" />
                </button>
              </div>
            </div>

            <div class="tile-body">
              <div class="tile-text">
                <h4>{{ instance.name }}</h4>
                <p class="mono">{{ instance.version_id }}</p>
                <p v-if="instance.modpack" class="pack">{{ instance.modpack }}</p>
                <p v-if="instance.group" class="pack">{{ instance.group }}</p>
                <p class="pack">{{ playtimeText(instance) }}</p>
              </div>
              <div class="tile-actions">
                <button class="icon" :title="t('instances.logsTitle')" @click.stop="$emit('log', instance.name)">
                  <Icon name="terminal" :size="15" />
                </button>
                <button class="icon" :title="t('instances.openTitle')" @click.stop="$emit('open', instance.name)">
                  <Icon name="edit" :size="15" />
                </button>
                <button
                  class="icon danger"
                  :title="t('instances.deleteShort')"
                  :disabled="running === instance.name"
                  @click.stop="$emit('remove', instance.name)"
                >
                  <Icon name="trash" :size="15" />
                </button>
              </div>
            </div>
          </article>
        </div>
      </section>
    </template>
  </div>
</template>

<style scoped>
.page {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  gap: 22px;
}

/* Sections keep their height; the page scrolls instead of squeezing them. */
.page > * {
  flex-shrink: 0;
}

.library-bar {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
}

/* Creation lives above the library, not inside whichever instance happens to
   be featured. */
.page-actions {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: 10px;
  flex-wrap: wrap;
}

.page-actions .small {
  font-size: 12px;
  padding: 7px 12px;
}

.library-bar .search {
  display: flex;
  align-items: center;
  gap: 8px;
  flex: 1;
  min-width: 180px;
  padding: 8px 12px;
  border: 1px solid var(--border);
  border-radius: 10px;
  background: var(--surface-2);
  color: var(--muted);
}

.library-bar .search input {
  flex: 1;
  min-width: 0;
  border: 0;
  padding: 0;
  background: transparent;
  color: var(--text);
  font: inherit;
  outline: none;
}

.group-chips {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}

.group-chips button {
  padding: 5px 11px;
  border-radius: 999px;
  font-size: 11px;
  color: var(--muted);
  background: transparent;
}

.group-chips button.active {
  color: var(--accent);
  border-color: rgba(111, 220, 111, 0.4);
  background: rgba(111, 220, 111, 0.12);
}

.empty-filter {
  margin: 0;
  color: var(--muted);
  font-size: 13px;
}

.hero-art img,
.tile-art img {
  display: block;
  width: 100%;
  height: 100%;
  object-fit: cover;
}

.empty {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 10px;
  padding: 40px;
  text-align: center;
  background:
    radial-gradient(420px 240px at 50% 0%, rgba(111, 220, 111, 0.1), transparent 70%),
    linear-gradient(180deg, var(--card-a), var(--card-b));
}

.empty-art {
  display: grid;
  place-items: center;
  width: 96px;
  height: 96px;
  color: var(--accent);
  background: var(--slot);
  border: 2px solid var(--border);
  box-shadow: inset 2px 2px 0 var(--bevel-hi), inset -2px -2px 0 var(--bevel-lo);
  margin-bottom: 6px;
}

.empty h2 {
  margin: 0;
  font-size: 24px;
  font-weight: 400;
  text-shadow: var(--heading-shadow);
}

.empty p {
  margin: 0 0 8px;
  max-width: 420px;
  color: var(--muted);
  font-size: 13px;
  line-height: 1.6;
}

.empty-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 10px;
  justify-content: center;
}

.hero {
  display: grid;
  grid-template-columns: 208px 1fr;
  gap: 26px;
  padding: 22px;
  position: relative;
  overflow: hidden;
  animation: rise 220ms ease both;
}

.hero-wash {
  display: none;
}

.hero-art,
.hero-body {
  position: relative;
  z-index: 1;
}

.hero-art {
  width: 208px;
  height: 208px;
  overflow: hidden;
  border: 2px solid var(--border);
  box-shadow: 6px 6px 0 rgba(0, 0, 0, 0.35);
  cursor: pointer;
}

.hero h2 {
  cursor: pointer;
}

.hero-body {
  display: flex;
  flex-direction: column;
  justify-content: center;
  gap: 10px;
  min-width: 0;
}

.eyebrow {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 11px;
  font-weight: 600;
  letter-spacing: 0.1em;
  text-transform: uppercase;
  color: var(--accent);
}

.hero h2 {
  margin: 0;
  font-size: 32px;
  line-height: 1.15;
  letter-spacing: 0.01em;
  font-weight: 400;
  text-shadow: var(--heading-shadow);
  word-break: break-word;
}

.chips {
  display: flex;
  flex-wrap: wrap;
  gap: 7px;
  margin-top: 2px;
}

.chips .badge svg {
  opacity: 0.75;
}

.badge.version {
  color: var(--text);
  border-color: var(--border-strong);
  background: var(--surface-4);
  letter-spacing: 0.01em;
}

.hero-actions {
  display: flex;
  align-items: center;
  gap: 9px;
  margin-top: 8px;
  flex-wrap: wrap;
}

.play {
  height: 44px;
  padding: 0 26px;
  font-size: 16px;
}

.hero-actions .ghost {
  height: 38px;
}

.hero-actions .icon {
  width: 38px;
  height: 38px;
}

.others {
  display: flex;
  flex-direction: column;
  gap: 12px;
  animation: rise 260ms ease both;
}

.section-head {
  display: flex;
  align-items: center;
  gap: 10px;
}

.section-head h3 {
  margin: 0;
  font-size: 18px;
  font-weight: 400;
  text-shadow: var(--heading-shadow);
}

.count {
  font-size: 11px;
  color: var(--muted);
  background: var(--slot);
  border: 2px solid var(--border);
  padding: 2px 7px;
}

.section-head .small {
  margin-inline-start: auto;
  font-size: 12px;
  padding: 6px 12px;
}

.grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(240px, 1fr));
  gap: 16px;
}

.tile {
  background: linear-gradient(180deg, var(--dialog-a), var(--dialog-b));
  border: 1px solid var(--border);
  border-radius: var(--radius);
  overflow: hidden;
  transition: transform 180ms ease, border-color 180ms ease, box-shadow 180ms ease;
}

.tile:hover {
  border-color: var(--gold) !important;
}

.tile.openable {
  cursor: pointer;
}

.tile-art {
  position: relative;
  height: 132px;
}

.tile-overlay {
  position: absolute;
  inset: 0;
  display: grid;
  place-items: center;
  background: rgba(5, 8, 12, 0.45);
  opacity: 0;
  transition: opacity 160ms ease;
}

.tile:hover .tile-overlay {
  opacity: 1;
}

.play-btn {
  width: 52px;
  height: 52px;
  justify-content: center;
  padding: 0;
  background: #4b8f3f;
  border: 2px solid var(--border);
  color: #f4fff1;
  box-shadow: inset 2px 2px 0 #79c46a, inset -2px -2px 0 #27541f;
}

.play-btn:hover:not(:disabled) {
  background: #57a349;
}

.play-btn.stop {
  background: #7d2f2f;
  color: #ffd7d7;
  box-shadow: inset 2px 2px 0 #a85454, inset -2px -2px 0 #491717;
}

.tile-body {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 12px 12px 12px 14px;
}

.tile-text {
  min-width: 0;
  flex: 1;
}

.tile-text h4 {
  margin: 0;
  font-size: 15px;
  font-weight: 400;
  text-shadow: 1px 1px 0 rgba(0, 0, 0, 0.35);
  word-break: break-word;
}

.tile-text p {
  margin: 3px 0 0;
  font-size: 11px;
  color: var(--muted);
}

.tile-text .pack {
  margin-top: 2px;
  color: var(--accent);
  opacity: 0.8;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.tile-actions {
  display: flex;
  gap: 4px;
}

.tile-actions .icon {
  width: 30px;
  height: 30px;
  border-radius: 8px;
}

.tile-actions .icon svg {
  width: 14px;
  height: 14px;
}
</style>
