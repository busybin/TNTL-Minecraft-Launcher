<script setup lang="ts">
import { onMounted, onUnmounted, ref } from "vue";
import { api, events, jobTag, type InstallEvent, type UnlistenFn } from "../api";
import Icon from "../components/Icon.vue";
import { t } from "../i18n";

const emit = defineEmits<{ close: []; imported: [name: string] }>();
const props = defineProps<{ initialPath?: string }>();

const path = ref(props.initialPath ?? "");
const name = ref("");
const version = ref("");
const busy = ref(false);
const error = ref<string | null>(null);
const stage = ref<InstallEvent | null>(null);
const unlisteners: UnlistenFn[] = [];

async function browse() {
  try {
    const picked = await api.pickFolder(t("import.folderTitle"));
    if (picked) path.value = picked;
  } catch (cause) {
    error.value = String(cause);
  }
}

onMounted(async () => {
  try {
    unlisteners.push(
      await events.onInstallProgress(({ job, event }) => {
        if (job === jobTag.import) stage.value = event;
      }),
    );
  } catch (cause) {
    error.value = String(cause);
  }
});

onUnmounted(() => unlisteners.forEach((unlisten) => unlisten()));

async function run() {
  const target = path.value.trim();
  if (!target) {
    error.value = t("import.pointAt");
    return;
  }
  busy.value = true;
  error.value = null;
  stage.value = null;
  try {
    const imported = await api.importInstance(
      target,
      name.value.trim() || null,
      version.value.trim() || null,
    );
    emit("imported", imported);
  } catch (cause) {
    error.value = String(cause);
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <div class="overlay" @click.self="!busy && $emit('close')">
    <section class="dialog">
      <header>
        <span class="chip"><Icon name="folder" :size="16" /></span>
        <div class="head-text">
          <h2>{{ t("import.title") }}</h2>
          <p>{{ t("import.subtitle") }}</p>
        </div>
        <button class="icon" :disabled="busy" :title="t('action.close')" @click="$emit('close')">
          <Icon name="close" />
        </button>
      </header>

      <p v-if="error" class="error">{{ error }}</p>

      <div class="body">
        <label>
          <span>{{ t("import.path") }}</span>
          <div class="row">
            <input v-model="path" class="mono" spellcheck="false" placeholder="/home/you/.minecraft" />
            <button type="button" class="ghost" @click="browse">
              <Icon name="folder" :size="14" />{{ t("import.browse") }}
            </button>
          </div>
        </label>
        <label>
          <span>{{ t("import.name") }} <small>{{ t("import.nameHint") }}</small></span>
          <input v-model="name" spellcheck="false" :placeholder="t('import.optional')" />
        </label>
        <label>
          <span>{{ t("import.version") }} <small>{{ t("import.versionHint") }}</small></span>
          <input v-model="version" class="mono" spellcheck="false" :placeholder="t('import.versionPlaceholder')" />
        </label>
        <p class="hint">
          <Icon name="info" :size="13" />
          <span>{{ t("import.hint") }}</span>
        </p>
        <p v-if="busy && stage" class="hint mono">
          {{ stage.stage }} {{ stage.done }}/{{ stage.total }}
        </p>
      </div>

      <footer>
        <button class="ghost" :disabled="busy" @click="$emit('close')">{{ t("action.cancel") }}</button>
        <button class="primary" :disabled="busy" @click="run">
          {{ busy ? t("import.busy") : t("import.button") }}
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
  width: min(520px, 100%);
  max-height: calc(100vh - 56px);
  overflow-y: auto;
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

.row input {
  flex: 1;
  min-width: 0;
}

.row .ghost {
  flex-shrink: 0;
  white-space: nowrap;
}

.hint {
  display: flex;
  gap: 8px;
  align-items: flex-start;
  margin: 2px 0 0;
  font-size: 11.5px;
  line-height: 1.5;
  color: var(--muted);
}

footer {
  display: flex;
  justify-content: flex-end;
  gap: 10px;
  padding-top: 4px;
}
</style>
