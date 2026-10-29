<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import {
  api,
  events,
  formatBytes,
  type DiskUsage,
  type InstanceView,
  type JavaOffer,
  type JavaView,
  type LauncherSettings,
  type RuntimeProgress,
  type UnlistenFn,
} from "../api";
import Icon from "../components/Icon.vue";
import TransferBar from "../components/TransferBar.vue";
import { languageChoices, choice, setLocale, t } from "../i18n";
import { useTransfer } from "../progress";

const props = defineProps<{
  root: string;
  java: JavaView[];
  instances: InstanceView[];
  settings: LauncherSettings;
}>();

const emit = defineEmits<{ refresh: []; settings: [LauncherSettings] }>();

const local = ref<LauncherSettings>({ ...props.settings });
const THEMES = ["dark", "light", "system"] as const;

/** Official pages that hand out what the Microsoft and CurseForge cards need. */
const CLIENT_ID_GUIDE = "https://learn.microsoft.com/en-us/entra/identity-platform/quickstart-register-app";
const CURSEFORGE_KEYS = "https://console.curseforge.com/?#/api-keys";

function openGuide(url: string) {
  void api.openUrl(url);
}
const savingSettings = ref(false);
const settingsStatus = ref<string | null>(null);
const appVersion = ref("");

onMounted(async () => {
  // The About card must not hard-code the version; the backend owns it.
  appVersion.value = await api.appVersion().catch(() => "");
});
let settingsRequest = 0;
const storageInstance = ref<string | null>(props.instances[0]?.name ?? null);
const usage = ref<DiskUsage | null>(null);
const storageBusy = ref(false);
const storageStatus = ref<string | null>(null);

watch(
  () => props.settings,
  (next, previous) => {
    if (!previous) {
      local.value = { ...next };
      return;
    }
    // A theme or background click saves one field; keep edits the user made in
    // other panels instead of letting the incoming object reset them.
    const merged = { ...next };
    for (const key of Object.keys(merged) as (keyof LauncherSettings)[]) {
      if (local.value[key] !== previous[key]) {
        (merged as Record<string, unknown>)[key] = local.value[key];
      }
    }
    local.value = merged;
  },
);

// The selected instance can be deleted (or the list can load late).
watch(
  () => props.instances,
  (list) => {
    if (!list.some((entry) => entry.name === storageInstance.value)) {
      storageInstance.value = list[0]?.name ?? null;
    }
  },
);

async function saveSettings() {
  const token = ++settingsRequest;
  savingSettings.value = true;
  settingsStatus.value = null;
  try {
    const saved = await api.saveLauncherSettings({
      ...local.value,
      concurrency: Math.min(32, Math.max(1, Math.round(Number(local.value.concurrency) || 8))),
    });
    if (token !== settingsRequest) return;
    local.value = saved;
    emit("settings", saved);
    settingsStatus.value = t("settings.saved");
  } catch (cause) {
    if (token !== settingsRequest) return;
    settingsStatus.value = String(cause);
  } finally {
    if (token === settingsRequest) savingSettings.value = false;
  }
}

/** Save one changed field without persisting edits from other panels, and
 *  ignore a response that a later click has already superseded. */
async function persistSettings(patch: Partial<LauncherSettings>) {
  const token = ++settingsRequest;
  savingSettings.value = true;
  settingsStatus.value = null;
  try {
    const saved = await api.saveLauncherSettings({ ...props.settings, ...patch });
    if (token !== settingsRequest) return;
    emit("settings", saved);
    settingsStatus.value = t("settings.saved");
  } catch (cause) {
    if (token !== settingsRequest) return;
    settingsStatus.value = String(cause);
  } finally {
    if (token === settingsRequest) savingSettings.value = false;
  }
}

const backgroundBusy = ref(false);

const backgroundName = computed(() => {
  const path = local.value.background;
  if (!path) return t("settings.noBackground");
  return path.split(/[\\/]/).pop() ?? path;
});

async function applyTheme(theme: string) {
  local.value.theme = theme;
  await persistSettings({ theme });
}

async function chooseBackground() {
  const path = await api.pickFile(t("settings.background"), ["png", "jpg", "jpeg", "webp", "gif"]);
  if (!path) return;
  backgroundBusy.value = true;
  local.value.background = path;
  await persistSettings({ background: path });
  backgroundBusy.value = false;
}

async function clearBackground() {
  backgroundBusy.value = true;
  local.value.background = null;
  await persistSettings({ background: null });
  backgroundBusy.value = false;
}

let usageRequest = 0;

async function loadUsage() {
  const name = storageInstance.value;
  const token = ++usageRequest;
  usage.value = null;
  storageStatus.value = null;
  if (!name) {
    storageBusy.value = false;
    return;
  }
  storageBusy.value = true;
  try {
    const next = await api.diskUsage(name);
    if (token === usageRequest) usage.value = next;
  } catch (cause) {
    if (token === usageRequest) storageStatus.value = String(cause);
  } finally {
    if (token === usageRequest) storageBusy.value = false;
  }
}

async function cleanup() {
  if (!storageInstance.value) return;
  if (!window.confirm(t("settings.cleanupConfirm", { name: storageInstance.value }))) return;
  storageBusy.value = true;
  storageStatus.value = null;
  try {
    const report = await api.cleanupInstance(storageInstance.value);
    storageStatus.value = report.removed.length
      ? t("settings.cleanupDone", {
          list: report.removed.map((key) => t(`settings.cleanupKind.${key}`)).join(", "),
          size: formatBytes(report.bytes),
        })
      : t("settings.nothingToClean");
    await loadUsage();
  } catch (cause) {
    storageStatus.value = String(cause);
  } finally {
    storageBusy.value = false;
  }
}

watch(storageInstance, loadUsage);

const copied = ref(false);

const offers = ref<JavaOffer[]>([]);
const offerError = ref<string | null>(null);
const selectedOffer = ref<string | null>(null);
const installingJava = ref(false);
const javaStage = ref<RuntimeProgress | null>(null);
const javaPercent = ref(0);
const {
  bytes: javaBytes,
  total: javaTotal,
  speed: javaSpeed,
  track: trackJava,
  reset: resetJava,
} = useTransfer();
const javaResult = ref<string | null>(null);
const javaError = ref<string | null>(null);

const unlisteners: UnlistenFn[] = [];

const selectedOfferInfo = computed(() => offers.value.find((offer) => offer.component === selectedOffer.value) ?? null);

async function loadOffers() {
  try {
    offers.value = await api.javaOffers();
    const preferred =
      offers.value.find((offer) => offer.component === "java-runtime-gamma") ?? offers.value[0] ?? null;
    selectedOffer.value = preferred?.component ?? null;
  } catch (cause) {
    offerError.value = String(cause);
  }
}

async function installJava() {
  if (!selectedOffer.value) return;
  installingJava.value = true;
  javaError.value = null;
  javaResult.value = null;
  javaPercent.value = 0;
  javaStage.value = null;
  resetJava();
  try {
    const runtime = await api.installJava(selectedOffer.value);
    javaResult.value = runtime.skipped
      ? t("settings.javaAlready", { component: runtime.component, version: runtime.version })
      : t("settings.javaInstalled", { component: runtime.component, version: runtime.version });
    emit("refresh");
  } catch (cause) {
    javaError.value = String(cause);
  } finally {
    installingJava.value = false;
  }
}

onMounted(async () => {
  await loadOffers();
  await loadUsage();
  try {
    unlisteners.push(
      await events.onJavaProgress((event) => {
        javaStage.value = event;
        if (event.stage === "downloading") {
          trackJava(event.done, event.total);
          if (event.total) javaPercent.value = Math.min(100, (event.done / event.total) * 100);
        }
        if (event.stage === "manifest") {
          resetJava();
          javaPercent.value = 0;
        }
        if (event.stage === "installed") javaPercent.value = 100;
      }),
    );
  } catch (cause) {
    javaError.value = String(cause);
  }
});

onUnmounted(() => {
  unlisteners.forEach((unlisten) => unlisten());
});

async function copyRoot() {
  try {
    await navigator.clipboard.writeText(props.root);
    copied.value = true;
    window.setTimeout(() => (copied.value = false), 1600);
  } catch {
    copied.value = false;
  }
}
</script>

<template>
  <div class="page scroll">
    <section class="card panel">
      <header>
        <span class="chip"><Icon name="info" :size="16" /></span>
        <div>
          <h2>{{ t("settings.language") }}</h2>
          <p>{{ t("settings.languageHint") }}</p>
        </div>
      </header>

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
    </section>

    <section class="card panel">
      <header>
        <span class="chip"><Icon name="image" :size="16" /></span>
        <div>
          <h2>{{ t("settings.appearance") }}</h2>
          <p>{{ t("settings.appearanceHint") }}</p>
        </div>
      </header>

      <div class="setting-row">
        <div class="setting-label">
          <strong>{{ t("settings.theme") }}</strong>
          <small>{{ t("settings.themeHint") }}</small>
        </div>
        <div class="segmented compact">
          <button
            v-for="entry in THEMES"
            :key="entry"
            :class="{ active: local.theme === entry }"
            @click="applyTheme(entry)"
          >
            {{ t(`settings.theme${entry[0].toUpperCase()}${entry.slice(1)}`) }}
          </button>
        </div>
      </div>

      <div class="setting-row">
        <div class="setting-label">
          <strong>{{ t("settings.background") }}</strong>
          <small class="mono background-name" :title="local.background ?? ''">{{ backgroundName }}</small>
        </div>
        <div class="row-actions">
          <button class="ghost small" :disabled="backgroundBusy" @click="chooseBackground">
            <Icon name="image" :size="13" /> {{ t("settings.chooseBackground") }}
          </button>
          <button v-if="local.background" class="ghost small" :disabled="backgroundBusy" @click="clearBackground">
            <Icon name="trash" :size="13" /> {{ t("settings.clearBackground") }}
          </button>
        </div>
      </div>
      <p class="hint">{{ t("settings.backgroundHint") }}</p>
    </section>

    <section class="card panel">
      <header>
        <span class="chip"><Icon name="settings" :size="16" /></span>
        <div>
          <h2>{{ t("settings.launcher") }}</h2>
          <p>{{ t("settings.launcherHint") }}</p>
        </div>
      </header>

      <div class="setting-row">
        <div class="setting-label">
          <strong>{{ t("settings.concurrency") }}</strong>
          <small>{{ t("settings.concurrencyHint") }}</small>
        </div>
        <input v-model.number="local.concurrency" type="number" min="1" max="32" />
      </div>

      <div class="setting-row">
        <div class="setting-label">
          <strong>{{ t("settings.hideOnLaunch") }}</strong>
          <small>{{ t("settings.hideOnLaunchHint") }}</small>
        </div>
        <button
          class="toggle-btn"
          :class="{ on: local.hide_on_launch }"
          @click="local.hide_on_launch = !local.hide_on_launch"
        >
          {{ local.hide_on_launch ? t("editor.enabled") : t("editor.disabled") }}
        </button>
      </div>

      <div class="setting-row">
        <div class="setting-label">
          <strong>{{ t("settings.quitOnLaunch") }}</strong>
          <small>{{ t("settings.quitOnLaunchHint") }}</small>
        </div>
        <button
          class="toggle-btn"
          :class="{ on: local.quit_on_launch }"
          @click="local.quit_on_launch = !local.quit_on_launch"
        >
          {{ local.quit_on_launch ? t("editor.enabled") : t("editor.disabled") }}
        </button>
      </div>

      <div class="setting-actions">
        <button class="primary small" :disabled="savingSettings" @click="saveSettings">
          {{ savingSettings ? t("editor.working") : t("settings.save") }}
        </button>
        <span v-if="settingsStatus" class="hint">{{ settingsStatus }}</span>
      </div>
    </section>

    <section class="card panel">
      <header>
        <span class="chip"><Icon name="user" :size="16" /></span>
        <div>
          <h2>{{ t("settings.microsoft") }}</h2>
          <p>{{ t("settings.microsoftHint") }}</p>
        </div>
      </header>

      <div class="setting-row">
        <div class="setting-label">
          <strong>{{ t("settings.clientId") }}</strong>
          <small>{{ t("settings.clientIdHint") }}</small>
        </div>
        <input
          v-model="local.msa_client_id"
          type="text"
          class="mono"
          spellcheck="false"
          placeholder="00000000-0000-0000-0000-000000000000"
        />
      </div>

      <div class="setting-actions">
        <button class="ghost small" type="button" @click="openGuide(CLIENT_ID_GUIDE)">
          <Icon name="external" :size="13" /> {{ t("settings.clientIdGuide") }}
        </button>
      </div>

      <div class="setting-actions">
        <button class="primary small" :disabled="savingSettings" @click="saveSettings">
          {{ savingSettings ? t("editor.working") : t("settings.save") }}
        </button>
        <span v-if="settingsStatus" class="hint">{{ settingsStatus }}</span>
      </div>
    </section>

    <section class="card panel">
      <header>
        <span class="chip"><Icon name="spark" :size="16" /></span>
        <div>
          <h2>{{ t("settings.curseforge") }}</h2>
          <p>{{ t("settings.curseforgeHint") }}</p>
        </div>
      </header>

      <div class="setting-row">
        <div class="setting-label">
          <strong>{{ t("settings.curseforgeKey") }}</strong>
          <small>{{ t("settings.curseforgeKeyHint") }}</small>
        </div>
        <input
          v-model="local.curseforge_api_key"
          type="password"
          class="mono"
          spellcheck="false"
          autocomplete="off"
          :placeholder="t('settings.curseforgePlaceholder')"
        />
      </div>

      <div class="setting-actions">
        <button class="ghost small" type="button" @click="openGuide(CURSEFORGE_KEYS)">
          <Icon name="external" :size="13" /> {{ t("settings.curseforgeGuide") }}
        </button>
      </div>

      <div class="setting-actions">
        <button class="primary small" :disabled="savingSettings" @click="saveSettings">
          {{ savingSettings ? t("editor.working") : t("settings.save") }}
        </button>
        <span v-if="settingsStatus" class="hint">{{ settingsStatus }}</span>
      </div>
    </section>

    <section class="card panel">
      <header>
        <span class="chip"><Icon name="server" :size="16" /></span>
        <div>
          <h2>{{ t("settings.network") }}</h2>
          <p>{{ t("settings.networkHint") }}</p>
        </div>
      </header>

      <div class="setting-row">
        <div class="setting-label">
          <strong>{{ t("settings.proxy") }}</strong>
          <small>{{ t("settings.proxyHint") }}</small>
        </div>
        <select v-model="local.proxy_mode">
          <option value="none">{{ t("settings.proxyNone") }}</option>
          <option value="http">{{ t("settings.proxyHttp") }}</option>
          <option value="socks5">{{ t("settings.proxySocks5") }}</option>
        </select>
      </div>

      <div v-if="local.proxy_mode === 'http' || local.proxy_mode === 'socks5'" class="setting-row">
        <div class="setting-label">
          <strong>{{ t("settings.proxyUrl") }}</strong>
          <small>{{ t("settings.proxyUrlHint") }}</small>
        </div>
        <input
          v-model="local.proxy_url"
          type="text"
          :placeholder="local.proxy_mode === 'socks5' ? 'socks5://127.0.0.1:1080' : 'http://127.0.0.1:7890'"
        />
      </div>

      <div v-if="local.proxy_mode === 'http' || local.proxy_mode === 'socks5'" class="setting-row">
        <div class="setting-label">
          <strong>{{ t("settings.proxyUsername") }}</strong>
          <small>{{ t("settings.proxyUsernameHint") }}</small>
        </div>
        <input v-model="local.proxy_username" type="text" spellcheck="false" autocomplete="off" />
      </div>

      <div v-if="local.proxy_mode === 'http' || local.proxy_mode === 'socks5'" class="setting-row">
        <div class="setting-label">
          <strong>{{ t("settings.proxyPassword") }}</strong>
          <small>{{ t("settings.proxyPasswordHint") }}</small>
        </div>
        <input v-model="local.proxy_password" type="password" spellcheck="false" autocomplete="new-password" />
      </div>

      <div class="setting-actions">
        <button class="primary small" :disabled="savingSettings" @click="saveSettings">
          {{ savingSettings ? t("editor.working") : t("settings.save") }}
        </button>
        <span v-if="settingsStatus" class="hint">{{ settingsStatus }}</span>
      </div>
    </section>

    <section class="card panel">
      <header>
        <span class="chip"><Icon name="folder" :size="16" /></span>
        <div>
          <h2>{{ t("settings.dataDir") }}</h2>
          <p>{{ t("settings.dataDirHint") }}</p>
        </div>
      </header>

      <div class="path-row">
        <span class="mono path">{{ root }}</span>
        <button class="ghost small" @click="copyRoot">
          <Icon :name="copied ? 'check' : 'copy'" :size="14" />{{ copied ? t("settings.copied") : t("settings.copy") }}
        </button>
      </div>
      <p class="hint"><code>MLC_ROOT=/path</code> — {{ t("settings.rootHint") }}</p>
    </section>

    <section class="card panel">
      <header>
        <span class="chip"><Icon name="package" :size="16" /></span>
        <div>
          <h2>{{ t("settings.storage") }}</h2>
          <p>{{ t("settings.storageHint") }}</p>
        </div>
      </header>

      <div class="setting-row">
        <div class="setting-label">
          <strong>{{ t("settings.storageInstance") }}</strong>
        </div>
        <select v-model="storageInstance" :disabled="!instances.length">
          <option v-for="instance in instances" :key="instance.name" :value="instance.name">
            {{ instance.name }}
          </option>
        </select>
      </div>

      <div v-if="usage" class="usage-list">
        <div v-for="entry in usage.entries" :key="entry.key" class="usage-row">
          <span>{{ t(`settings.disk.${entry.key}`) }}</span>
          <span class="mono usage-path">{{ entry.path }}</span>
          <strong class="mono">{{ formatBytes(entry.bytes) }}</strong>
        </div>
        <div class="usage-row total">
          <span>{{ t("settings.total") }}</span>
          <span />
          <strong class="mono">{{ formatBytes(usage.total) }}</strong>
        </div>
      </div>

      <div class="setting-actions">
        <button class="ghost small" :disabled="storageBusy || !storageInstance" @click="loadUsage">
          {{ t("settings.recalculate") }}
        </button>
        <button class="ghost small" :disabled="storageBusy || !storageInstance" @click="cleanup">
          <Icon name="trash" :size="13" /> {{ t("settings.cleanup") }}
        </button>
        <span v-if="storageStatus" class="hint">{{ storageStatus }}</span>
      </div>
    </section>

    <section class="card panel">
      <header>
        <span class="chip"><Icon name="cpu" :size="16" /></span>
        <div>
          <h2>{{ t("settings.java") }}</h2>
          <p>{{ t("settings.javaHint") }}</p>
        </div>
      </header>

      <div v-if="java.length" class="runtime-list">
        <div v-for="runtime in java" :key="runtime.path" class="runtime">
          <span class="badge ok">Java {{ runtime.major }}</span>
          <span class="mono path">{{ runtime.path }}</span>
          <span class="version mono">{{ runtime.version }}</span>
          <span v-if="runtime.vendor" class="vendor">{{ runtime.vendor }}</span>
        </div>
      </div>
      <p v-else class="warn-note"><Icon name="alert" :size="15" />{{ t("settings.noJava") }}</p>

      <div class="download">
        <div class="download-head">
          <strong>{{ t("settings.javaDownload") }}</strong>
          <small>{{ t("settings.javaDownloadHint") }}</small>
        </div>
        <div class="download-row">
          <select v-model="selectedOffer" :disabled="installingJava || !offers.length">
            <option v-for="offer in offers" :key="offer.component" :value="offer.component">
              {{ offer.component }} · {{ offer.version }}
            </option>
          </select>
          <button class="primary" :disabled="installingJava || !selectedOffer" @click="installJava">
            <Icon name="download" :size="15" />
            {{ installingJava ? t("settings.javaInstalling") : t("settings.javaInstall") }}
          </button>
        </div>
        <p v-if="offerError" class="error">{{ offerError }}</p>
        <TransferBar
          v-if="installingJava"
          :label="
            javaStage?.stage === 'manifest'
              ? t('settings.javaReading', { component: selectedOfferInfo?.component ?? '' })
              : t('settings.javaDownloading')
          "
          :done="javaBytes"
          :total="javaTotal"
          :speed="javaSpeed"
          :detail="javaStage && 'path' in javaStage ? javaStage.path : ''"
          :indeterminate="javaStage?.stage === 'manifest'"
        />
        <p v-if="javaResult" class="ok banner"><Icon name="check" :size="14" />{{ javaResult }}</p>
        <p v-if="javaError" class="error">{{ javaError }}</p>
      </div>
    </section>

    <section class="card panel">
      <header>
        <span class="chip"><Icon name="info" :size="16" /></span>
        <div>
          <h2>{{ t("settings.about") }}</h2>
          <p>{{ t("settings.aboutText", { version: appVersion || "?" }) }}</p>
        </div>
      </header>
      <div class="facts">
        <div class="fact"><span>{{ t("settings.instances") }}</span><strong>{{ instances.length }}</strong></div>
        <div class="fact"><span>{{ t("settings.gameFiles") }}</span><strong>Mojang</strong></div>
        <div class="fact"><span>{{ t("settings.platforms") }}</span><strong>Windows · Linux</strong></div>
      </div>
      <p class="hint">{{ t("settings.privacy") }}</p>
    </section>
  </div>
</template>

<style scoped>
.page {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  gap: 16px;
  padding-bottom: 4px;
}

/* Panels keep their height; the page scrolls instead of squeezing them. */
.page > * {
  flex-shrink: 0;
}

.panel {
  padding: 20px;
  display: flex;
  flex-direction: column;
  gap: 14px;
  animation: rise 220ms ease both;
}

header {
  display: flex;
  gap: 13px;
  align-items: flex-start;
}

.chip {
  display: grid;
  place-items: center;
  width: 38px;
  height: 38px;
  flex: none;
  border-radius: 12px;
  color: var(--accent);
  background: rgba(111, 220, 111, 0.1);
  border: 1px solid rgba(111, 220, 111, 0.2);
}

h2 {
  margin: 0;
  font-size: 15px;
  font-weight: 620;
}

header p {
  margin: 3px 0 0;
  font-size: 12px;
  color: var(--muted);
}

.path-row {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 10px 12px;
  border-radius: 12px;
  background: var(--field);
  border: 1px solid var(--border);
}

.path {
  flex: 1;
  min-width: 0;
  font-size: 12px;
  word-break: break-all;
}

.small {
  font-size: 11.5px;
  padding: 5px 10px;
}

.hint {
  margin: 0;
  font-size: 11.5px;
  color: var(--muted-2);
  line-height: 1.6;
}

code {
  background: var(--surface-4);
  padding: 1px 5px;
  border-radius: 5px;
  font-size: 11px;
}

.runtime-list {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.runtime {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 10px 12px;
  border-radius: 12px;
  background: var(--surface-1);
  border: 1px solid var(--border);
}

.runtime .path {
  font-size: 11.5px;
  color: var(--text);
}

.version {
  font-size: 11px;
  color: var(--muted);
}

.vendor {
  font-size: 10.5px;
  color: var(--muted-2);
  margin-inline-start: auto;
}

.download {
  display: flex;
  flex-direction: column;
  gap: 10px;
  padding: 14px;
  border-radius: 12px;
  background: var(--surface-1);
  border: 1px solid var(--border);
}

.download-head {
  display: flex;
  flex-direction: column;
  gap: 3px;
}

.download-head strong {
  font-size: 12.5px;
}

.download-head small {
  font-size: 11.5px;
  color: var(--muted);
}

.download-row {
  display: flex;
  gap: 10px;
}

.download-row select {
  flex: 1;
  min-width: 0;
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

.banner {
  display: flex;
  align-items: center;
  gap: 7px;
  margin: 0;
  font-size: 12px;
  color: var(--accent);
  background: rgba(111, 220, 111, 0.1);
  border: 1px solid rgba(111, 220, 111, 0.25);
  border-radius: 12px;
  padding: 9px 12px;
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
  background: rgba(111, 220, 111, 0.12);
  border-color: rgba(111, 220, 111, 0.35);
}

.facts {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 10px;
}

.fact {
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 12px;
  border-radius: 12px;
  background: var(--surface-1);
  border: 1px solid var(--border);
}

.fact span {
  font-size: 11px;
  color: var(--muted);
}

.fact strong {
  font-size: 14px;
  font-weight: 600;
}

.setting-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 14px;
  padding: 10px 12px;
  border-radius: 12px;
  background: var(--surface-1);
  border: 1px solid var(--border);
}

.setting-label {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.setting-label strong {
  font-size: 12.5px;
  font-weight: 600;
}

.setting-label small {
  font-size: 11px;
  color: var(--muted-2);
}

.setting-row input[type="number"] {
  width: 76px;
  flex: none;
}

.toggle-btn {
  flex: none;
  padding: 5px 12px;
  border-radius: 999px;
  font-size: 11px;
  color: var(--muted);
  background: var(--surface-3);
}

.toggle-btn.on {
  color: var(--accent);
  border-color: rgba(111, 220, 111, 0.4);
  background: rgba(111, 220, 111, 0.12);
}

.segmented.compact {
  flex: none;
  padding: 3px;
}

.segmented.compact button {
  flex: none;
  padding: 5px 10px;
  font-size: 11.5px;
}

.row-actions {
  display: flex;
  gap: 6px;
  flex: none;
}

.background-name {
  max-width: 220px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.setting-actions {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
}

.usage-list {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.usage-row {
  display: grid;
  grid-template-columns: 110px 1fr auto;
  gap: 10px;
  align-items: baseline;
  padding: 4px 2px;
  font-size: 11.5px;
  color: var(--muted);
}

.usage-row.total {
  border-top: 1px solid var(--border);
  margin-top: 4px;
  padding-top: 8px;
  color: var(--text);
}

.usage-path {
  font-size: 10.5px;
  color: var(--muted-2);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
</style>
