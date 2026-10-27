<script setup lang="ts">
import { ref } from "vue";
import { api, type LauncherSettings } from "../api";
import { languageChoices, choice, setLocale, t } from "../i18n";
import Icon from "../components/Icon.vue";

const props = defineProps<{ settings: LauncherSettings }>();
const emit = defineEmits<{
  close: [];
  settings: [LauncherSettings];
  install: [];
  importMc: [];
}>();

const theme = ref(props.settings.theme);
const saving = ref(false);

const THEMES = ["dark", "light", "system"] as const;

async function finish(action: "close" | "install" | "importMc") {
  saving.value = true;
  try {
    const saved = await api.saveLauncherSettings({ ...props.settings, theme: theme.value, onboarded: true });
    emit("settings", saved);
  } catch {
    // The guide is dismissed even if the settings file cannot be written.
  } finally {
    saving.value = false;
  }
  emit("close");
  if (action === "install") emit("install");
  if (action === "importMc") emit("importMc");
}
</script>

<template>
  <div class="onboarding-overlay">
    <section class="onboarding">
      <div class="head">
        <span class="logo"><img src="/logo.png" alt="TNTL" /></span>
        <div>
          <h2>{{ t("onboarding.title") }}</h2>
          <p>{{ t("onboarding.body") }}</p>
        </div>
      </div>

      <label class="row">
        <span>{{ t("onboarding.language") }}</span>
        <div class="segmented">
          <button
            v-for="entry in languageChoices"
            :key="entry"
            :class="{ active: choice === entry }"
            @click="setLocale(entry)"
          >
            {{ entry === "system" ? t("language.system") : t(`language.${entry}`) }}
          </button>
        </div>
      </label>

      <label class="row">
        <span>{{ t("onboarding.theme") }}</span>
        <div class="segmented">
          <button
            v-for="entry in THEMES"
            :key="entry"
            :class="{ active: theme === entry }"
            @click="theme = entry"
          >
            {{ t(`settings.theme${entry[0].toUpperCase()}${entry.slice(1)}`) }}
          </button>
        </div>
      </label>

      <div class="actions">
        <button class="ghost" :disabled="saving" @click="finish('close')">{{ t("onboarding.later") }}</button>
        <button class="ghost" :disabled="saving" @click="finish('importMc')">
          <Icon name="folder" :size="14" />{{ t("onboarding.importMc") }}
        </button>
        <button class="primary" :disabled="saving" @click="finish('install')">
          <Icon name="download" :size="14" />{{ t("onboarding.install") }}
        </button>
      </div>
    </section>
  </div>
</template>

<style scoped>
.onboarding-overlay {
  position: fixed;
  inset: 0;
  z-index: 26;
  display: grid;
  place-items: center;
  padding: 36px;
  background: var(--overlay);
  backdrop-filter: blur(8px);
}

.onboarding {
  width: min(560px, 100%);
  max-height: calc(100vh - 72px);
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  gap: 16px;
  padding: 26px;
  border-radius: 20px;
  border: 1px solid var(--border-strong);
  background: linear-gradient(180deg, var(--dialog-a), var(--dialog-b));
  box-shadow: 0 30px 80px rgba(0, 0, 0, 0.45);
  animation: rise 220ms ease both;
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
  box-shadow: 0 10px 26px rgba(0, 0, 0, 0.28);
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
  flex-direction: column;
  gap: 7px;
  font-size: 12px;
  color: var(--muted);
}

.segmented {
  display: flex;
  gap: 6px;
  padding: 4px;
  border-radius: 11px;
  background: var(--field);
  border: 1px solid var(--border);
}

.segmented button {
  flex: 1;
  padding: 7px 10px;
  font-size: 12px;
  border-radius: 8px;
  border: 1px solid transparent;
  background: transparent;
  color: var(--muted);
}

.segmented button.active {
  color: var(--text);
  background: var(--accent-soft);
  border-color: rgba(111, 220, 111, 0.35);
}

.actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  flex-wrap: wrap;
  margin-top: 4px;
}
</style>
