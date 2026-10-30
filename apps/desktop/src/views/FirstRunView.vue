<script setup lang="ts">
import { ref } from "vue";
import { api, type PathInfo } from "../api";
import { t } from "../i18n";
import Icon from "../components/Icon.vue";

const props = defineProps<{ legacy: string | null }>();
const emit = defineEmits<{
  done: [games: string | null];
}>();

const busy = ref(false);
const error = ref<string | null>(null);
const picked = ref<PathInfo | null>(null);
/** Game directory the user pointed at before choosing a data directory. */
const gamePath = ref<string | null>(null);
const report = ref<{ instances: number; suggested: string[]; backup: string } | null>(null);

async function pick() {
  error.value = null;
  try {
    const path = await api.pickFolder(t("firstRun.pick"));
    if (!path) return;
    const info = await api.pathInfo(path);
    picked.value = info;
    if (info.kind !== "data" && info.kind !== "empty") {
      // A game directory can only be imported once a data directory exists.
      gamePath.value = info.path;
    } else {
      gamePath.value = null;
    }
  } catch (cause) {
    error.value = String(cause);
  }
}

async function use() {
  const target = picked.value;
  if (!target) return;
  busy.value = true;
  error.value = null;
  try {
    await api.chooseDataRoot(target.path);
    emit("done", gamePath.value);
  } catch (cause) {
    error.value = String(cause);
  } finally {
    busy.value = false;
  }
}

async function migrate() {
  busy.value = true;
  error.value = null;
  try {
    const migrated = await api.migrateLegacy();
    report.value = {
      instances: migrated.instances,
      suggested: migrated.suggested,
      backup: migrated.backup,
    };
  } catch (cause) {
    error.value = String(cause);
  } finally {
    busy.value = false;
  }
}

function finish() {
  const games = report.value?.suggested.length ? report.value.suggested[0] : null;
  emit("done", games);
}
</script>

<template>
  <div class="first-run">
    <section class="panel">
      <div class="head">
        <span class="logo"><img src="/logo.png" alt="TNTL" /></span>
        <div>
          <h2>{{ t("firstRun.title") }}</h2>
          <p>{{ t("firstRun.body") }}</p>
        </div>
      </div>

      <template v-if="report">
        <p class="ok">{{ t("firstRun.migrated", { count: report.instances }) }}</p>
        <p class="muted">{{ t("firstRun.backup", { path: report.backup }) }}</p>
        <p v-if="report.suggested.length" class="muted">
          {{ t("firstRun.suggested", { count: report.suggested.length }) }}
        </p>
        <button class="primary" type="button" @click="finish">{{ t("firstRun.continue") }}</button>
      </template>

      <template v-else>
        <div class="row">
          <button class="ghost" type="button" :disabled="busy" @click="pick">
            <Icon name="folder" :size="14" />{{ t("firstRun.pick") }}
          </button>
          <span v-if="picked" class="path" :title="picked.path">{{ picked.path }}</span>
        </div>

        <p v-if="picked" class="muted">
          <template v-if="picked.kind === 'data'">
            {{ t("firstRun.existing", { count: picked.instances }) }}
          </template>
          <template v-else-if="picked.kind === 'empty'">
            {{ t("firstRun.empty") }}
          </template>
          <template v-else>
            {{ t("firstRun.gameDir") }}
          </template>
        </p>
        <p v-if="picked && !picked.writable" class="warn">{{ t("firstRun.unwritable") }}</p>

        <button
          v-if="picked && (picked.kind === 'data' || picked.kind === 'empty')"
          class="primary"
          type="button"
          :disabled="busy"
          @click="use"
        >
          {{ t("firstRun.use") }}
        </button>
        <button
          v-else-if="gamePath"
          class="primary"
          type="button"
          :disabled="busy"
          @click="pick"
        >
          {{ t("firstRun.chooseDataDir") }}
        </button>

        <div v-if="props.legacy" class="legacy">
          <p class="muted">{{ t("firstRun.migrateBody", { path: props.legacy }) }}</p>
          <button class="ghost" type="button" :disabled="busy" @click="migrate">
            {{ t("firstRun.migrate") }}
          </button>
        </div>
      </template>

      <p v-if="error" class="error">{{ error }}</p>
    </section>
  </div>
</template>

<style scoped>
.first-run {
  position: fixed;
  inset: 0;
  z-index: 40;
  display: grid;
  place-items: center;
  padding: 36px;
  background: var(--overlay);
  backdrop-filter: blur(8px);
}

.panel {
  width: min(620px, 100%);
  display: flex;
  flex-direction: column;
  gap: 14px;
  padding: 26px;
  border-radius: 20px;
  border: 1px solid var(--border-strong);
  background: linear-gradient(180deg, var(--dialog-a), var(--dialog-b));
  box-shadow: 0 30px 80px rgba(0, 0, 0, 0.45);
}

.head {
  display: flex;
  gap: 16px;
  align-items: flex-start;
}

.logo {
  display: grid;
  place-items: center;
  width: 52px;
  height: 52px;
  flex: none;
  border-radius: 16px;
  overflow: hidden;
  background: rgba(124, 240, 165, 0.08);
}

.logo img {
  width: 52px;
  height: 52px;
  object-fit: contain;
}

h2 {
  margin: 0;
  font-size: 19px;
  font-weight: 660;
}

.head p {
  margin: 6px 0 0;
  font-size: 12.5px;
  line-height: 1.6;
  color: var(--muted);
}

.row {
  display: flex;
  gap: 10px;
  align-items: center;
  flex-wrap: wrap;
}

.path {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  max-width: 100%;
  font-size: 12px;
  color: var(--muted);
}

.muted {
  margin: 0;
  font-size: 12.5px;
  line-height: 1.6;
  color: var(--muted);
}

.warn {
  margin: 0;
  font-size: 12.5px;
  color: #f0b45a;
}

.error {
  margin: 0;
  font-size: 12.5px;
  color: #f07474;
}

.ok {
  margin: 0;
  font-size: 13px;
  color: #7cf0a5;
}

.legacy {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding-top: 10px;
  border-top: 1px solid var(--border);
}

.actions {
  display: flex;
  gap: 8px;
  justify-content: flex-end;
}
</style>
