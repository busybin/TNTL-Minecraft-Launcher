<script setup lang="ts">
import { computed, onUnmounted, ref, watch } from "vue";
import { api, type AccountView, type AccountsResponse, type DeviceCode } from "../api";
import SkinHead from "../components/SkinHead.vue";
import SkinEditor from "./SkinEditor.vue";
import { t } from "../i18n";

const props = defineProps<{ accounts: AccountsResponse }>();
const emit = defineEmits<{ changed: []; notify: [message: string]; openSettings: [] }>();

const heads = ref<Record<string, ArrayBuffer | null>>({});
const busy = ref(false);
const error = ref<string | null>(null);

const offlineOpen = ref(false);
const offlineName = ref("");
const offlineError = ref<string | null>(null);

const loginOpen = ref(false);
const code = ref<DeviceCode | null>(null);
const loginError = ref<string | null>(null);
const loggingIn = ref(false);
const started = ref(false);
let timer = 0;
let startedAt = 0;
let slowDown = 0;

const editor = ref<AccountView | null>(null);

const editing = ref<AccountView | null>(null);
const editName = ref("");
const editError = ref<string | null>(null);
const editBusy = ref(false);

/** Offline UUIDs are 32 hex chars; the dashed form is what other tools show. */
function dashedUuid(value: string): string {
  const hex = value.replace(/-/g, "");
  if (hex.length !== 32) return value;
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

function openEdit(account: AccountView) {
  editing.value = account;
  editName.value = account.name;
  editError.value = null;
}

async function saveEdit() {
  const account = editing.value;
  if (!account) return;
  const name = editName.value.trim();
  if (!name || name === account.name) {
    editing.value = null;
    return;
  }
  editBusy.value = true;
  editError.value = null;
  try {
    await api.renameOfflineAccount(account.id, name);
    editing.value = null;
    emit("changed");
    emit("notify", t("accounts.saved", { name }));
  } catch (cause) {
    editError.value = String(cause);
  } finally {
    editBusy.value = false;
  }
}

async function loadHeads() {
  const next: Record<string, ArrayBuffer | null> = {};
  for (const account of props.accounts.accounts) {
    if (!account.has_skin) {
      next[account.id] = null;
      continue;
    }
    try {
      next[account.id] = await api.accountSkin(account.id);
    } catch {
      next[account.id] = null;
    }
  }
  heads.value = next;
}

watch(() => props.accounts, loadHeads, { deep: true, immediate: true });

async function run(action: () => Promise<unknown>, message?: string) {
  busy.value = true;
  error.value = null;
  try {
    await action();
    emit("changed");
    if (message) emit("notify", message);
  } catch (cause) {
    error.value = String(cause);
  } finally {
    busy.value = false;
  }
}

async function use(account: AccountView) {
  await run(() => api.setCurrentAccount(account.id), t("accounts.switched", { name: account.name }));
}

async function remove(account: AccountView) {
  if (!window.confirm(t("accounts.removeConfirm", { name: account.name }))) return;
  await run(() => api.removeAccount(account.id), t("accounts.removed", { name: account.name }));
}

async function addOffline() {
  const name = offlineName.value.trim();
  if (!name) return;
  offlineError.value = null;
  busy.value = true;
  try {
    await api.addOfflineAccount(name);
    offlineOpen.value = false;
    offlineName.value = "";
    emit("changed");
    emit("notify", t("accounts.added", { name }));
  } catch (cause) {
    offlineError.value = String(cause);
  } finally {
    busy.value = false;
  }
}

function openLogin() {
  loginOpen.value = true;
  code.value = null;
  loginError.value = null;
  started.value = false;
  loggingIn.value = false;
}

let loginGeneration = 0;

function stopLogin() {
  loginGeneration += 1;
  window.clearTimeout(timer);
  timer = 0;
  loggingIn.value = false;
}

function closeLogin() {
  stopLogin();
  loginOpen.value = false;
  code.value = null;
}

function goToSettings() {
  closeLogin();
  emit("openSettings");
}

async function startLogin() {
  // "Restart" used to leave the previous poll loop running.
  stopLogin();
  const generation = loginGeneration;
  loginError.value = null;
  loggingIn.value = true;
  try {
    const next = await api.startMsaLogin();
    if (generation !== loginGeneration) return;
    code.value = next;
    started.value = true;
    startedAt = Date.now();
    slowDown = 0;
    void poll(generation);
  } catch (cause) {
    if (generation !== loginGeneration) return;
    loginError.value = String(cause);
    loggingIn.value = false;
  }
}

async function poll(generation: number) {
  if (generation !== loginGeneration || !code.value) return;
  try {
    const result = await api.pollMsaLogin(code.value.device_code);
    // A restart while this request was in flight must not schedule another
    // loop for the old device code.
    if (generation !== loginGeneration) return;
    if (result.status === "signed_in") {
      const account = result.account;
      stopLogin();
      loginOpen.value = false;
      code.value = null;
      emit("changed");
      emit("notify", t("accounts.signedIn", { name: account.name }));
      return;
    }
    if (result.status === "slow_down") slowDown += 5;
  } catch (cause) {
    if (generation !== loginGeneration) return;
    loginError.value = String(cause);
    stopLogin();
    return;
  }
  if (generation !== loginGeneration || !code.value) return;
  if (Date.now() - startedAt > Math.max(60, code.value.expires_in) * 1000) {
    loginError.value = t("accounts.loginExpired");
    stopLogin();
    return;
  }
  const wait = Math.min(60, Math.max(2, code.value.interval) + slowDown);
  timer = window.setTimeout(() => void poll(generation), wait * 1000);
}

function openInBrowser() {
  if (code.value) void api.openUrl(code.value.verification_uri);
}

function kindLabel(account: AccountView) {
  return account.microsoft ? t("accounts.microsoft") : t("accounts.offline");
}

const editKind = computed(() => (editing.value ? kindLabel(editing.value) : ""));

onUnmounted(stopLogin);
</script>

<template>
  <div class="accounts-page">
    <div class="accounts-toolbar">
      <div class="accounts-intro">
        <p>{{ t("accounts.hint") }}</p>
      </div>
      <div class="accounts-add">
        <button class="ghost" :disabled="busy" @click="offlineOpen = true">
          {{ t("accounts.addOffline") }}
        </button>
        <button class="primary" :disabled="busy" @click="openLogin">
          {{ t("accounts.addMicrosoft") }}
        </button>
      </div>
    </div>

    <div v-if="error" class="error error-banner">
      <span class="error-text">{{ error }}</span>
      <button class="ghost error-close" type="button" :title="t('action.close')" @click="error = null">✕</button>
    </div>

    <div v-if="!accounts.accounts.length" class="accounts-empty">
      <h3>{{ t("accounts.empty") }}</h3>
      <p class="hint">{{ t("accounts.emptyHint") }}</p>
    </div>

    <div v-else class="account-grid">
      <article
        v-for="account in accounts.accounts"
        :key="account.id"
        class="account-card"
        :class="{ active: account.active }"
      >
        <SkinHead :bytes="heads[account.id] ?? null" :size="54" />
        <div class="account-meta">
          <div class="account-title">
            <strong>{{ account.name }}</strong>
            <span class="badge">{{ kindLabel(account) }}</span>
            <span v-if="account.active" class="active-pill">{{ t("accounts.active") }}</span>
          </div>
          <span class="mono account-id">{{ dashedUuid(account.uuid) }}</span>
        </div>
        <div class="account-actions">
          <button v-if="!account.active" class="ghost small" :disabled="busy" @click="use(account)">
            {{ t("accounts.use") }}
          </button>
          <button class="ghost small" :disabled="busy" @click="openEdit(account)">
            {{ t("accounts.edit") }}
          </button>
          <button class="ghost small" :disabled="busy" @click="editor = account">
            {{ t("accounts.skin") }}
          </button>
          <button class="ghost small danger" :disabled="busy" @click="remove(account)">
            {{ t("accounts.remove") }}
          </button>
        </div>
      </article>
    </div>

    <div v-if="offlineOpen" class="dialog-overlay" @click.self="offlineOpen = false">
      <section class="small-dialog">
        <h3>{{ t("accounts.addOffline") }}</h3>
        <p class="hint">{{ t("accounts.offlineHint") }}</p>
        <label>
          <span>{{ t("accounts.name") }}</span>
          <input
            v-model="offlineName"
            spellcheck="false"
            maxlength="16"
            :placeholder="t('accounts.namePlaceholder')"
            @keyup.enter="addOffline"
          />
        </label>
        <p v-if="offlineError" class="error">{{ offlineError }}</p>
        <div class="dialog-actions">
          <button class="ghost" @click="offlineOpen = false">{{ t("action.cancel") }}</button>
          <button class="primary" :disabled="busy || !offlineName.trim()" @click="addOffline">
            {{ busy ? t("editor.working") : t("accounts.add") }}
          </button>
        </div>
      </section>
    </div>

    <div v-if="editing" class="dialog-overlay" @click.self="editing = null">
      <section class="small-dialog">
        <h3>{{ t("accounts.editTitle") }}</h3>
        <p class="hint">
          {{ editKind }}<template v-if="editing.active"> · {{ t("accounts.active") }}</template>
        </p>
        <label v-if="!editing.microsoft">
          <span>{{ t("accounts.name") }}</span>
          <input v-model="editName" spellcheck="false" maxlength="16" @keyup.enter="saveEdit" />
        </label>
        <label v-else>
          <span>{{ t("accounts.name") }}</span>
          <input
            class="mono"
            :value="editing.name"
            readonly
            spellcheck="false"
            @focus="($event.target as HTMLInputElement).select()"
          />
        </label>
        <label>
          <span>{{ t("accounts.uuid") }}</span>
          <input
            class="mono"
            :value="dashedUuid(editing.uuid)"
            readonly
            spellcheck="false"
            @focus="($event.target as HTMLInputElement).select()"
          />
        </label>
        <p v-if="editing.microsoft" class="hint">{{ t("accounts.microsoftLocked") }}</p>
        <template v-else>
          <p class="hint">{{ t("accounts.uuidHint") }}</p>
          <p class="hint">{{ t("accounts.renameHint") }}</p>
        </template>
        <p v-if="editError" class="error">{{ editError }}</p>
        <div class="dialog-actions">
          <button class="ghost" :disabled="editBusy" @click="editing = null">
            {{ editing.microsoft ? t("action.close") : t("action.cancel") }}
          </button>
          <button
            v-if="!editing.microsoft"
            class="primary"
            :disabled="editBusy || !editName.trim()"
            @click="saveEdit"
          >
            {{ editBusy ? t("editor.working") : t("accounts.save") }}
          </button>
        </div>
      </section>
    </div>

    <div v-if="loginOpen" class="dialog-overlay" @click.self="closeLogin">
      <section class="small-dialog login-dialog">
        <h3>{{ t("accounts.addMicrosoft") }}</h3>

        <template v-if="!accounts.client_id">
          <p class="hint">{{ t("accounts.clientIdMissing") }}</p>
          <div class="dialog-actions">
            <button class="ghost" @click="closeLogin">{{ t("action.cancel") }}</button>
            <button class="primary" @click="goToSettings">
              {{ t("accounts.openSettings") }}
            </button>
          </div>
        </template>

        <template v-else-if="!code">
          <p class="hint">{{ t("accounts.loginHint") }}</p>
          <p v-if="loginError" class="error">{{ loginError }}</p>
          <div class="dialog-actions">
            <button class="ghost" @click="closeLogin">{{ t("action.cancel") }}</button>
            <button class="primary" :disabled="loggingIn" @click="startLogin">
              {{ loggingIn ? t("editor.working") : t("accounts.startLogin") }}
            </button>
          </div>
        </template>

        <template v-else>
          <p class="hint">{{ t("accounts.enterCode") }}</p>
          <div class="code-box mono">{{ code.user_code }}</div>
          <div class="login-actions">
            <button class="primary" @click="openInBrowser">{{ t("accounts.openPage") }}</button>
            <span class="hint">{{ t("accounts.waiting") }}</span>
          </div>
          <p class="hint">{{ code.message }}</p>
          <p v-if="loginError" class="error">{{ loginError }}</p>
          <div class="dialog-actions">
            <button class="ghost" @click="closeLogin">{{ t("action.cancel") }}</button>
            <button class="ghost" :disabled="!started" @click="startLogin">
              {{ t("accounts.restart") }}
            </button>
          </div>
        </template>
      </section>
    </div>

    <SkinEditor
      v-if="editor"
      :account="editor"
      @close="editor = null"
      @changed="emit('changed')"
    />
  </div>
</template>

<style scoped>
.accounts-page {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  gap: 16px;
  overflow-y: auto;
  padding-bottom: 24px;
}

.accounts-toolbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 18px;
}

.accounts-intro p {
  margin: 0;
  font-size: 12.5px;
  color: var(--muted);
  max-width: 640px;
}

.accounts-add {
  display: flex;
  gap: 8px;
  flex: none;
}

.accounts-empty {
  padding: 42px 20px;
  border-radius: 18px;
  border: 1px dashed var(--border-strong);
  text-align: center;
  background: var(--surface-1);
}

.accounts-empty h3 {
  margin: 0 0 6px;
  font-size: 14px;
}

.account-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(360px, 1fr));
  gap: 12px;
}

.account-card {
  display: flex;
  align-items: center;
  gap: 14px;
  padding: 14px;
  border-radius: 16px;
  border: 1px solid var(--border);
  background: linear-gradient(180deg, var(--card-a), var(--card-b));
}

.account-card.active {
  border-color: rgba(111, 220, 111, 0.36);
  box-shadow: inset 0 0 0 1px rgba(111, 220, 111, 0.08);
}

.account-meta {
  display: flex;
  flex-direction: column;
  gap: 4px;
  min-width: 0;
  flex: 1;
}

.account-title {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.account-title strong {
  font-size: 14px;
}

.badge {
  font-size: 10px;
  text-transform: uppercase;
  letter-spacing: 0.06em;
  color: var(--muted);
  border: 1px solid var(--border);
  padding: 2px 6px;
  border-radius: 6px;
}

.active-pill {
  font-size: 10px;
  color: var(--accent);
  background: var(--accent-soft);
  border-radius: 999px;
  padding: 2px 8px;
}

.account-id {
  font-size: 10.5px;
  color: var(--muted-2);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.account-actions {
  display: flex;
  flex-direction: column;
  gap: 6px;
  flex: none;
}

.dialog-overlay {
  position: fixed;
  inset: 0;
  z-index: 26;
  display: grid;
  place-items: center;
  padding: 32px;
  background: var(--overlay);
  backdrop-filter: blur(6px);
}

.small-dialog {
  width: min(430px, 100%);
  display: flex;
  flex-direction: column;
  gap: 12px;
  padding: 20px;
  border-radius: 18px;
  border: 1px solid var(--border-strong);
  background: linear-gradient(180deg, var(--dialog-a), var(--dialog-b));
  box-shadow: 0 30px 80px rgba(0, 0, 0, 0.6);
}

.small-dialog h3 {
  margin: 0;
  font-size: 13px;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  color: var(--muted);
}

.small-dialog label {
  display: flex;
  flex-direction: column;
  gap: 6px;
  font-size: 12px;
  color: var(--muted);
}

.dialog-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
}

.login-dialog {
  width: min(500px, 100%);
}

.code-box {
  font-size: 30px;
  letter-spacing: 0.18em;
  text-align: center;
  padding: 14px;
  border-radius: 12px;
  background: var(--field-strong);
  border: 1px solid var(--border-strong);
}

.login-actions {
  display: flex;
  align-items: center;
  gap: 12px;
}

.hint {
  margin: 0;
  font-size: 11.5px;
  color: var(--muted-2);
  line-height: 1.6;
}
</style>
