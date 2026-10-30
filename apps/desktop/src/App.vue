<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  api,
  events,
  type Bootstrap,
  type GameExit,
  type InstanceView,
  imageUrl,
  type LauncherSettings,
  type RootsState,
  type SessionState,
  type UnlistenFn,
} from "./api";
import Icon from "./components/Icon.vue";
import SkinHead from "./components/SkinHead.vue";
import JavaPrompt from "./components/JavaPrompt.vue";
import InstancesView from "./views/InstancesView.vue";
import InstallView from "./views/InstallView.vue";
import SettingsView from "./views/SettingsView.vue";
import AccountsView from "./views/AccountsView.vue";
import GameConsole from "./views/GameConsole.vue";
import InstanceDetail from "./views/InstanceDetail.vue";
import ImportDialog from "./views/ImportDialog.vue";
import ModpackDialog from "./views/ModpackDialog.vue";
import OnboardingDialog from "./views/OnboardingDialog.vue";
import FirstRunView from "./views/FirstRunView.vue";
import { t } from "./i18n";
import { maybePromptJava } from "./javaPrompt";
import { startInstallListeners } from "./installState";

type Page = "instances" | "install" | "accounts" | "settings";

const page = ref<Page>("instances");
const bootstrap = ref<Bootstrap | null>(null);
const instances = ref<InstanceView[]>([]);
const running = ref<string | null>(null);
/** Second close request while a game runs always closes; see onCloseRequested. */
let closeArmed = false;
const busy = ref<string | null>(null);
const error = ref<string | null>(null);
const toast = ref<string | null>(null);
/** Instance just removed from the list; files stay until the user deletes them. */
const removedFolder = ref<{ name: string; dir: string } | null>(null);

const consoleOpen = ref(false);
const consoleTitle = ref("");
const consoleInstance = ref<string | null>(null);
const consoleLines = ref<string[]>([]);
const consoleExit = ref<GameExit | null>(null);
const logLoading = ref(false);
let logRequest = 0;

const detailInstance = ref<string | null>(null);
const detailKey = ref(0);
const reloadTick = ref(0);
const detailModpack = computed(
  () => instances.value.find((instance) => instance.name === detailInstance.value)?.modpack ?? null,
);

const importOpen = ref(false);
const importPath = ref("");
const accountOpen = ref(false);
const accountName = ref("");
const accountError = ref<string | null>(null);
const accountBusy = ref(false);
const pendingLaunch = ref<string | null>(null);

/** Avatar of the account that will launch games, shown in the rail's foot. */
const currentAccount = computed(
  () => bootstrap.value?.accounts.accounts.find((entry) => entry.id === bootstrap.value?.accounts.current) ?? null,
);
const currentAccountSkin = ref<ArrayBuffer | null>(null);
watch(
  currentAccount,
  async (account) => {
    if (!account?.has_skin) {
      currentAccountSkin.value = null;
      return;
    }
    try {
      currentAccountSkin.value = await api.accountSkin(account.id);
    } catch {
      currentAccountSkin.value = null;
    }
  },
  { immediate: true },
);
const modpackOpen = ref(false);
const modpackPath = ref<string | null>(null);
const settings = ref<LauncherSettings>({
  concurrency: 8,
  hide_on_launch: true,
  quit_on_launch: false,
  theme: "dark",
  background: null,
  proxy_mode: "none",
  proxy_url: null,
  proxy_username: null,
  proxy_password: null,
  onboarded: true,
  msa_client_id: null,
  curseforge_api_key: null,
});
const backgroundUrl = ref<string | null>(null);
const onboardingOpen = ref(false);
/** Shown until the user picks a data directory; nothing is created for them. */
const firstRun = ref(false);
const session = ref<SessionState | null>(null);
let themeMedia: MediaQueryList | null = null;

const archiveOpen = ref(false);
const archivePath = ref<string | null>(null);
const archiveName = ref("");
const archiveBusy = ref(false);
const archiveError = ref<string | null>(null);

const nav = computed(() => [
  { id: "instances" as Page, label: t("nav.instances"), icon: "grid" },
  { id: "install" as Page, label: t("nav.install"), icon: "download" },
  { id: "accounts" as Page, label: t("nav.accounts"), icon: "user" },
  { id: "settings" as Page, label: t("nav.settings"), icon: "settings" },
]);

const heading = computed<{ title: string; subtitle: string }>(() => {
  switch (page.value) {
    case "install":
      return { title: t("app.install"), subtitle: t("app.installSubtitle") };
    case "accounts":
      return { title: t("app.accounts"), subtitle: t("app.accountsSubtitle") };
    case "settings":
      return { title: t("app.settings"), subtitle: t("app.settingsSubtitle") };
    default:
      return { title: t("app.library"), subtitle: t("app.librarySubtitle") };
  }
});
const root = computed(() => bootstrap.value?.root ?? "…");
const rootMenuOpen = ref(false);
const rootsState = ref<RootsState | null>(null);
const shortRoot = computed(() => {
  const value = root.value;
  return value.length > 34 ? `…${value.slice(-33)}` : value;
});

let refreshToken = 0;

async function refresh() {
  const token = ++refreshToken;
  try {
    const data = await api.bootstrap();
    if (token !== refreshToken) return;
    bootstrap.value = data;
    instances.value = data.instances;
    const current = await api.runningInstance();
    if (token !== refreshToken) return;
    running.value = current;
    error.value = null;
  } catch (cause) {
    if (token === refreshToken) error.value = String(cause);
  }
}

function notify(message: string) {
  toast.value = message;
  window.setTimeout(() => {
    if (toast.value === message) toast.value = null;
  }, 3200);
}

function pushLine(line: string) {
  consoleLines.value.push(line);
  if (consoleLines.value.length > 4000) {
    consoleLines.value.splice(0, consoleLines.value.length - 4000);
  }
}

async function launch(name: string) {
  busy.value = name;
  error.value = null;
  consoleLines.value = [];
  consoleExit.value = null;
  consoleTitle.value = name;
  consoleInstance.value = name;
  try {
    await api.launchInstance(name);
    // A launch bumps the play count and last-played date shown on the cards.
    // Refresh before claiming the running slot: `refresh` re-reads that slot
    // from the backend and must not clear it.
    await refresh();
    running.value = name;
    consoleOpen.value = true;
    if (settings.value.hide_on_launch) {
      await getCurrentWindow().hide().catch(() => undefined);
    }
  } catch (cause) {
    const message = String(cause);
    if (message.includes("add an account first")) {
      pendingLaunch.value = name;
      accountError.value = null;
      accountOpen.value = true;
    } else if (!maybePromptJava(message, () => void relaunchWithNewJava(name))) {
      error.value = message;
    }
  } finally {
    busy.value = null;
  }
}

function closeAccountPrompt() {
  accountOpen.value = false;
  pendingLaunch.value = null;
  accountError.value = null;
}

/** Add the missing offline account, then retry the launch that asked for it. */
async function addAccountAndPlay() {
  const name = accountName.value.trim();
  if (!name) return;
  accountBusy.value = true;
  accountError.value = null;
  try {
    await api.addOfflineAccount(name);
    await refresh();
    const target = pendingLaunch.value;
    accountOpen.value = false;
    accountName.value = "";
    pendingLaunch.value = null;
    if (target) await launch(target);
  } catch (cause) {
    accountError.value = String(cause);
  } finally {
    accountBusy.value = false;
  }
}

/** A stale Java override would keep failing, so drop it before retrying. */
async function relaunchWithNewJava(name: string) {
  try {
    await api.clearInstanceJava(name);
  } catch {
    // The override is only a hint; a failed reset must not block the retry.
  }
  await launch(name);
}

async function stop() {
  try {
    await api.stopGame();
  } catch (cause) {
    error.value = String(cause);
  }
}

async function showLog(name: string) {
  const token = ++logRequest;
  consoleTitle.value = t("app.lastLog", { name });
  consoleInstance.value = name;
  consoleExit.value = null;
  consoleLines.value = [];
  logLoading.value = true;
  consoleOpen.value = true;
  try {
    const text = await api.readInstanceLog(name);
    if (token !== logRequest) return;
    consoleLines.value = text ? text.split("\n") : [t("app.noLog")];
  } catch (cause) {
    if (token !== logRequest) return;
    consoleLines.value = [String(cause)];
  } finally {
    if (token === logRequest) logLoading.value = false;
  }
}

function goPage(next: Page) {
  detailInstance.value = null;
  page.value = next;
}

function openDetail(name: string) {
  page.value = "instances";
  detailInstance.value = name;
  detailKey.value += 1;
}

function closeDetail() {
  detailInstance.value = null;
  page.value = "instances";
}

async function remove(name: string) {
  if (!window.confirm(t("app.removeConfirm", { name }))) return;
  try {
    // Only the list entry goes away; nothing on disk is touched.
    const dir = await api.deleteInstance(name);
    if (detailInstance.value === name) detailInstance.value = null;
    removedFolder.value = { name, dir };
    await refresh();
  } catch (cause) {
    error.value = String(cause);
  }
}

function closeRemovedFolder() {
  removedFolder.value = null;
}

async function openRemovedFolder() {
  const target = removedFolder.value;
  if (!target) return;
  try {
    await api.openPath(target.dir);
  } catch (cause) {
    error.value = String(cause);
  }
}

function onSaved(next: string) {
  notify(t("app.saved"));
  if (next !== detailInstance.value) {
    detailInstance.value = next;
    detailKey.value += 1;
  }
  refresh();
}

function onDuplicated(name: string) {
  notify(t("app.duplicated", { name }));
  refresh();
}

function openImport(initial = "") {
  importPath.value = initial;
  importOpen.value = true;
}

function onImported(name: string) {
  importOpen.value = false;
  notify(t("app.imported", { name }));
  refresh();
}

function onModpackInstalled(name: string) {
  modpackOpen.value = false;
  notify(t("app.modpackInstalled", { name }));
  refresh();
}

async function openRootMenu() {
  if (running.value) return;
  rootMenuOpen.value = !rootMenuOpen.value;
  if (!rootMenuOpen.value) return;
  try {
    rootsState.value = await api.roots();
  } catch (cause) {
    error.value = String(cause);
  }
}

async function applyRoot(path: string) {
  rootMenuOpen.value = false;
  if (path === root.value) return;
  try {
    rootsState.value = await api.setRoot(path);
    detailInstance.value = null;
    await refresh();
  } catch (cause) {
    error.value = String(cause);
  }
}

async function forgetRoot(path: string) {
  try {
    rootsState.value = await api.forgetRoot(path);
    if (rootsState.value.current !== root.value) {
      detailInstance.value = null;
      await refresh();
    }
  } catch (cause) {
    error.value = String(cause);
  }
}

async function chooseRoot() {
  rootMenuOpen.value = false;
  try {
    const path = await api.pickFolder(t("app.chooseRoot"));
    if (!path) return;
    const info = await api.pathInfo(path);
    if (info.kind === "data") {
      await applyRoot(info.path);
    } else if (info.kind === "empty") {
      if (window.confirm(t("app.createRootConfirm", { path: info.path }))) {
        await api.chooseDataRoot(info.path);
        await refresh();
      }
    } else {
      // A game directory: offer to register it instead of switching to it.
      openImport(info.path);
      return;
    }
    rootMenuOpen.value = true;
  } catch (cause) {
    error.value = String(cause);
  }
}

async function pickArchive() {
  const path = await api.pickFile(t("instances.importArchiveTitle"), ["zip"]);
  if (!path) return;
  archivePath.value = path;
  const stem = path.split(/[\\/]/).pop() ?? "instance";
  archiveName.value = stem.replace(/\.zip$/i, "");
  archiveError.value = null;
  archiveOpen.value = true;
}

async function confirmArchive() {
  if (!archivePath.value) return;
  const name = archiveName.value.trim();
  if (!name) return;
  archiveBusy.value = true;
  archiveError.value = null;
  try {
    const instance = await api.importArchive(archivePath.value, name);
    archiveOpen.value = false;
    archivePath.value = null;
    notify(t("app.importedArchive", { name: instance.name }));
    await refresh();
  } catch (cause) {
    archiveError.value = String(cause);
  } finally {
    archiveBusy.value = false;
  }
}

async function onSettings(next: LauncherSettings) {
  settings.value = next;
}

function applyTheme(theme: string) {
  const resolved =
    theme === "system"
      ? window.matchMedia("(prefers-color-scheme: light)").matches
        ? "light"
        : "dark"
      : theme;
  document.documentElement.dataset.theme = resolved === "light" ? "light" : "dark";
}

function followSystemTheme() {
  themeMedia?.removeEventListener("change", onSystemTheme);
  themeMedia = null;
  if (settings.value.theme !== "system") return;
  themeMedia = window.matchMedia("(prefers-color-scheme: light)");
  themeMedia.addEventListener("change", onSystemTheme);
}

function onSystemTheme() {
  applyTheme("system");
}

let backgroundRequest = 0;

async function loadBackground(path: string | null) {
  const token = ++backgroundRequest;
  if (backgroundUrl.value) {
    URL.revokeObjectURL(backgroundUrl.value);
    backgroundUrl.value = null;
  }
  if (!path) return;
  try {
    const bytes = await api.backgroundImage();
    if (token !== backgroundRequest) return;
    if (bytes.byteLength) backgroundUrl.value = imageUrl(bytes, path);
  } catch {
    // A missing background file is not worth an error banner.
  }
}

watch(
  settings,
  (next, previous) => {
    applyTheme(next.theme);
    followSystemTheme();
    // Every settings save emits the whole object; re-reading a 20 MB wallpaper
    // for a theme click is not free.
    if (next.background !== previous?.background) void loadBackground(next.background);
  },
  { deep: true },
);

function openModpack() {
  modpackPath.value = null;
  modpackOpen.value = true;
}

async function onDrop(paths: string[]) {
  const file = paths[0];
  if (!file) return;
  const lower = file.toLowerCase();
  if (await api.isDirectory(file)) {
    openImport(file);
    return;
  }
  if (lower.endsWith(".mrpack")) {
    modpackPath.value = file;
    modpackOpen.value = true;
    return;
  }
  if (!lower.endsWith(".jar") && !lower.endsWith(".zip")) return;
  const name = file.split(/[\\/]/).pop() ?? file;
  try {
    if (lower.endsWith(".jar")) {
      if (!detailInstance.value) {
        notify(t("app.dropNeedInstance"));
        return;
      }
      await api.addLocalMod(detailInstance.value, file);
      notify(t("app.droppedMod", { file: name }));
      reloadTick.value += 1;
      return;
    }
    // A zip can be a resource pack, a world, a pack or an instance archive;
    // guessing "resource pack" used to install world backups into shaderpacks.
    const kind = await api.archiveKind(file);
    if (kind === "modpack") {
      modpackPath.value = file;
      modpackOpen.value = true;
      return;
    }
    if (kind === "instance") {
      archivePath.value = file;
      archiveName.value = name.replace(/\.zip$/i, "");
      archiveError.value = null;
      archiveOpen.value = true;
      return;
    }
    if (!detailInstance.value) {
      notify(t("app.dropNeedInstance"));
      return;
    }
    if (kind === "resource_pack") {
      await api.addLocalContent(detailInstance.value, "resourcepack", file);
      notify(t("app.droppedContent", { file: name }));
    } else if (kind === "world") {
      const world = await api.importWorld(detailInstance.value, file);
      notify(t("app.droppedWorld", { name: world.name }));
    } else {
      notify(t("app.dropUnknown", { file: name }));
      return;
    }
    reloadTick.value += 1;
  } catch (cause) {
    error.value = String(cause);
  }
}

const unlisteners: UnlistenFn[] = [];

async function onFirstRunDone(games: string | null) {
  firstRun.value = false;
  session.value = await api.session().catch(() => null);
  await refresh();
  if (games) openImport(games);
}

onMounted(async () => {
  session.value = await api.session().catch(() => null);
  if (session.value && !session.value.configured) {
    firstRun.value = true;
  } else {
    await refresh();
  }
  void startInstallListeners();
  try {
    settings.value = await api.launcherSettings();
  } catch (cause) {
    error.value = String(cause);
  }
  applyTheme(settings.value.theme);
  followSystemTheme();
  onboardingOpen.value = !settings.value.onboarded;
  for (const listen of [
    () => events.onGameLine((line) => pushLine(line)),
    () =>
      events.onInstallDone((done) => {
        void refresh();
        // Open the new instance only when the user is watching the install;
        // yanking them out of Settings or a half-typed account loses work.
        if (done.instance && page.value === "install") openDetail(done.instance);
      }),
    () =>
      // The game's stdout is a pipe this process reads; closing the window ends
      // it. The game keeps running, but its console log stops here, so ask
      // first instead of dropping it silently. A second close within five
      // seconds proceeds: a webview that refuses to show dialogs (or a wedged
      // one) must not make the window impossible to close.
      getCurrentWindow().onCloseRequested((event) => {
        if (!running.value || closeArmed) return;
        if (window.confirm(t("app.closeWhileRunning"))) return;
        event.preventDefault();
        closeArmed = true;
        window.setTimeout(() => {
          closeArmed = false;
        }, 5000);
      }),
    () =>
      getCurrentWindow().onDragDropEvent((event) => {
        if (event.payload.type === "drop") void onDrop(event.payload.paths);
      }),
    () =>
      events.onGameExit(async (exit) => {
        consoleExit.value = exit;
        if (running.value === exit.instance) running.value = null;
        await getCurrentWindow().show();
        await getCurrentWindow().setFocus().catch(() => undefined);
        refresh();
        // A finished session changes saves, screenshots and crash reports, so
        // the open instance page has to reload its lists.
        reloadTick.value += 1;
      }),
  ]) {
    try {
      unlisteners.push(await listen());
    } catch (cause) {
      error.value = String(cause);
    }
  }
  // Last startup stage in the launcher log; a blank window stops before it.
  await api.frontendReady().catch(() => undefined);
});

onUnmounted(() => {
  unlisteners.forEach((unlisten) => unlisten());
  themeMedia?.removeEventListener("change", onSystemTheme);
  if (backgroundUrl.value) URL.revokeObjectURL(backgroundUrl.value);
});
</script>

<template>
  <FirstRunView
    v-if="firstRun"
    :legacy="session?.legacy ?? null"
    @done="onFirstRunDone"
  />
  <div v-if="backgroundUrl" class="wallpaper" :style="{ backgroundImage: `url(${backgroundUrl})` }" />

  <div class="shell">
    <aside class="rail">
      <div class="logo" title="TNTL (TNT Launcher)">
        <img src="/logo.png" alt="TNTL" />
      </div>

      <nav>
        <button
          v-for="item in nav"
          :key="item.id"
          class="nav-item"
          :class="{ active: page === item.id }"
          :title="item.label"
          @click="goPage(item.id)"
        >
          <Icon :name="item.icon" :size="20" />
          <span>{{ item.label }}</span>
        </button>
      </nav>

      <div class="rail-foot">
        <button
          class="account-pill"
          :title="currentAccount ? currentAccount.name : t('accounts.title')"
          @click="goPage('accounts')"
        >
          <SkinHead :bytes="currentAccountSkin" :size="38" />
        </button>
      </div>
    </aside>

    <main>
      <p v-if="error" class="error" @click="error = null">{{ error }}</p>

      <InstanceDetail
        v-if="detailInstance"
        :key="`${detailInstance}-${detailKey}`"
        :name="detailInstance"
        :modpack="detailModpack"
        :running="running === detailInstance"
        :busy="busy === detailInstance"
        :reload="reloadTick"
        @close="closeDetail"
        @saved="onSaved"
        @changed="refresh"
        @duplicated="onDuplicated"
        @launch="launch"
        @stop="stop"
        @log="showLog"
        @remove="remove"
      />

      <template v-else>
        <header class="topbar">
          <div class="titles">
            <h1>{{ heading.title }}</h1>
            <p class="sub">{{ heading.subtitle }}</p>
          </div>
          <div class="top-actions">
            <div class="root-wrap">
              <button
                class="root mono"
                type="button"
                :title="t('app.rootHint')"
                :disabled="!!running"
                @click="openRootMenu"
              >
                <Icon name="folder" :size="13" />{{ shortRoot }}
                <Icon name="chevron" :size="11" />
              </button>
              <div v-if="rootMenuOpen" class="menu-scrim" @click="rootMenuOpen = false" />
              <div v-if="rootMenuOpen" class="root-menu">
                <div
                  v-for="path in rootsState?.roots ?? []"
                  :key="path"
                  class="root-row"
                >
                  <button
                    type="button"
                    :class="{ active: path === root }"
                    :title="path"
                    @click="applyRoot(path)"
                  >
                    {{ path }}
                  </button>
                  <button
                    class="forget-root"
                    type="button"
                    :title="t('app.forgetRoot')"
                    @click.stop="forgetRoot(path)"
                  >
                    <Icon name="close" :size="11" />
                  </button>
                </div>
                <button class="add-root" type="button" @click="chooseRoot">
                  <Icon name="folder" :size="13" />{{ t("app.addRoot") }}
                </button>
              </div>
            </div>
            <button v-if="running" class="running-pill" @click="consoleOpen = true">
              <i class="dot" />
              {{ running }}
            </button>
          </div>
        </header>

        <Transition name="page" mode="out-in">
          <InstancesView
            v-if="page === 'instances'"
            key="instances"
            :instances="instances"
            :running="running"
            :busy="busy"
            @launch="launch"
            @stop="stop"
            @log="showLog"
            @open="openDetail"
            @remove="remove"
            @new="goPage('install')"
            @import-mc="openImport()"
            @import-archive="pickArchive"
            @modpack="openModpack"
            @refresh="refresh"
          />

          <InstallView
            v-else-if="page === 'install'"
            key="install"
            :installed="bootstrap?.installed_versions ?? []"
            @installed="refresh"
            @open="(name: string) => { if (name) openDetail(name); }"
            @console="showLog"
          />

          <AccountsView
            v-else-if="page === 'accounts'"
            key="accounts"
            :accounts="bootstrap?.accounts ?? { current: null, accounts: [], client_id: false }"
            @changed="refresh"
            @notify="notify"
            @open-settings="goPage('settings')"
          />

          <SettingsView
            v-else
            key="settings"
            :root="root"
            :java="bootstrap?.java ?? []"
            :instances="instances"
            :settings="settings"
            @settings="onSettings"
            @refresh="refresh"
          />
        </Transition>
      </template>
    </main>

    <GameConsole
      v-if="consoleOpen"
      :title="consoleTitle"
      :instance="consoleInstance"
      :lines="consoleLines"
      :exit="consoleExit && consoleExit.instance === consoleInstance ? consoleExit : null"
      :loading="logLoading"
      :running="consoleInstance !== null && running === consoleInstance"
      @close="consoleOpen = false"
      @stop="stop"
    />

    <ImportDialog
      v-if="importOpen"
      :initial-path="importPath"
      @close="importOpen = false"
      @imported="onImported"
    />

    <ModpackDialog
      v-if="modpackOpen"
      :initial-path="modpackPath"
      @close="modpackOpen = false"
      @installed="onModpackInstalled"
    />

    <div v-if="archiveOpen" class="archive-overlay" @click.self="archiveOpen = false">
      <section class="archive-dialog">
        <h3>{{ t("instances.importArchiveTitle") }}</h3>
        <p class="mono archive-path">{{ archivePath }}</p>
        <label>
          <span>{{ t("instances.archiveName") }}</span>
          <input v-model="archiveName" spellcheck="false" @keyup.enter="confirmArchive" />
        </label>
        <p v-if="archiveError" class="error">{{ archiveError }}</p>
        <div class="archive-actions">
          <button class="ghost" @click="archiveOpen = false">{{ t("action.cancel") }}</button>
          <button class="primary" :disabled="archiveBusy || !archiveName.trim()" @click="confirmArchive">
            {{ archiveBusy ? t("editor.working") : t("instances.import") }}
          </button>
        </div>
      </section>
    </div>

    <div v-if="accountOpen" class="archive-overlay" @click.self="closeAccountPrompt">
      <section class="archive-dialog">
        <h3>{{ t("accounts.neededTitle") }}</h3>
        <p class="hint">{{ t("accounts.neededHint") }}</p>
        <label>
          <span>{{ t("accounts.name") }}</span>
          <input
            v-model="accountName"
            spellcheck="false"
            maxlength="16"
            :placeholder="t('accounts.namePlaceholder')"
            @keyup.enter="addAccountAndPlay"
          />
        </label>
        <p class="hint">{{ t("accounts.offlineHint") }}</p>
        <p v-if="accountError" class="error">{{ accountError }}</p>
        <div class="archive-actions">
          <button class="ghost" @click="closeAccountPrompt">{{ t("action.cancel") }}</button>
          <button class="ghost" @click="closeAccountPrompt(); goPage('accounts')">
            {{ t("app.accounts") }}
          </button>
          <button class="primary" :disabled="accountBusy || !accountName.trim()" @click="addAccountAndPlay">
            {{ accountBusy ? t("editor.working") : t("accounts.neededPlay") }}
          </button>
        </div>
      </section>
    </div>

    <JavaPrompt @installed="refresh" />

    <OnboardingDialog
      v-if="onboardingOpen"
      :settings="settings"
      @close="onboardingOpen = false"
      @settings="onSettings"
      @install="goPage('install')"
      @import-mc="openImport()"
    />

    <Transition name="toast">
      <div v-if="toast" class="toast">{{ toast }}</div>
      <div v-if="removedFolder" class="removed-banner">
        <span>{{ t("app.removedKept", { name: removedFolder.name }) }}</span>
        <code :title="removedFolder.dir">{{ removedFolder.dir }}</code>
        <button class="ghost" type="button" @click="openRemovedFolder">{{ t("app.openFolder") }}</button>
        <button class="ghost" type="button" @click="closeRemovedFolder">{{ t("action.close") }}</button>
      </div>
    </Transition>
  </div>
</template>

<style scoped>
.removed-banner {
  position: fixed;
  left: 18px;
  right: 18px;
  bottom: 18px;
  z-index: 30;
  display: flex;
  gap: 10px;
  align-items: center;
  flex-wrap: wrap;
  padding: 12px 14px;
  border-radius: 14px;
  border: 1px solid var(--border-strong);
  background: var(--dialog-b, #1b1f23);
  box-shadow: 0 18px 40px rgba(0, 0, 0, 0.4);
  font-size: 12.5px;
}

.removed-banner code {
  flex: 1 1 240px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--muted);
}

.wallpaper {
  position: fixed;
  inset: 0;
  z-index: 0;
  background-size: cover;
  background-position: center;
  background-repeat: no-repeat;
  image-rendering: pixelated;
}

.wallpaper::after {
  content: "";
  position: absolute;
  inset: 0;
  background: var(--wall-scrim);
}

.shell {
  display: grid;
  grid-template-columns: 92px 1fr;
  height: 100%;
  position: relative;
  z-index: 1;
}

.rail {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 16px;
  padding: 16px 0 14px;
  border-inline-end: 2px solid var(--border);
  background-color: var(--rail-bg);
  background-image: var(--rail-texture);
  box-shadow: inset -2px 0 0 rgba(0, 0, 0, 0.35), inset 2px 0 0 rgba(255, 255, 255, 0.05);
}

.logo {
  display: grid;
  place-items: center;
  width: 48px;
  height: 48px;
  overflow: hidden;
  border: 2px solid var(--border);
  background: var(--slot);
  box-shadow: inset 2px 2px 0 var(--bevel-hi), inset -2px -2px 0 var(--bevel-lo);
}

.logo img {
  width: 42px;
  height: 42px;
  object-fit: contain;
  image-rendering: pixelated;
}

nav {
  display: flex;
  flex-direction: column;
  gap: 8px;
  width: 100%;
  padding: 0 9px;
}

.nav-item {
  flex-direction: column;
  gap: 4px;
  width: 100%;
  padding: 9px 2px 7px;
  border: 2px solid transparent;
  background: transparent;
  box-shadow: none;
  color: var(--muted);
  font-size: 11px;
  letter-spacing: 0.02em;
  text-shadow: 1px 1px 0 rgba(0, 0, 0, 0.4);
}

.nav-item:hover:not(:disabled) {
  color: var(--text);
  background: var(--surface-3);
  border-color: var(--border);
  box-shadow: inset 2px 2px 0 var(--bevel-hi), inset -2px -2px 0 var(--bevel-lo);
}

.nav-item.active {
  color: var(--text);
  background: var(--slot);
  border-color: var(--gold);
  box-shadow: inset 2px 2px 0 rgba(255, 255, 255, 0.08), inset -2px -2px 0 rgba(0, 0, 0, 0.4);
}

.nav-item.active svg {
  color: var(--gold);
}

.rail-foot {
  margin-top: auto;
}

.account-pill {
  width: 46px;
  height: 46px;
  padding: 0;
  display: grid;
  place-items: center;
  border: 2px solid var(--border);
  background: var(--slot);
  box-shadow: inset 2px 2px 0 var(--bevel-hi), inset -2px -2px 0 var(--bevel-lo);
  overflow: hidden;
}

.account-pill:hover {
  border-color: var(--gold);
}

main {
  display: flex;
  flex-direction: column;
  padding: 24px 28px 20px;
  min-width: 0;
  overflow: hidden;
}

.topbar {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 18px;
  margin-bottom: 18px;
}

main > .error {
  margin-bottom: 14px;
}

h1 {
  font-size: 24px;
  line-height: 1.15;
  margin: 0;
  font-weight: 400;
  letter-spacing: 0.01em;
  text-shadow: var(--heading-shadow);
}

.sub {
  margin: 4px 0 0;
  font-size: 12px;
  color: var(--muted);
}

.top-actions {
  display: flex;
  align-items: center;
  gap: 10px;
  padding-top: 4px;
}

.root-wrap {
  position: relative;
}

.root {
  cursor: pointer;
  font-family: var(--font);
  display: inline-flex;
  align-items: center;
  gap: 7px;
  max-width: 330px;
  font-size: 11px;
  color: var(--muted);
  background: var(--slot);
  border: 2px solid var(--border);
  box-shadow: inset 2px 2px 0 var(--bevel-hi), inset -2px -2px 0 var(--bevel-lo);
  padding: 6px 9px;
  overflow: hidden;
  white-space: nowrap;
}

.root svg {
  flex: none;
  color: var(--muted);
}

.root:disabled {
  cursor: default;
  opacity: 0.6;
}

.menu-scrim {
  position: fixed;
  inset: 0;
  z-index: 29;
}

.root-menu {
  position: absolute;
  right: 0;
  top: calc(100% + 6px);
  z-index: 30;
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 280px;
  max-width: 440px;
  padding: 6px;
  border: 2px solid var(--border);
  background: var(--panel);
  box-shadow: inset 2px 2px 0 var(--bevel-hi), inset -2px -2px 0 var(--bevel-lo), var(--shadow);
}

.root-menu button {
  overflow: hidden;
  padding: 7px 9px;
  border: 2px solid transparent;
  background: transparent;
  box-shadow: none;
  color: var(--muted);
  font: inherit;
  font-size: 11px;
  text-align: left;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.root-menu button:hover {
  background: var(--surface-3);
  border-color: var(--border);
  color: var(--text);
}

.root-menu button.active {
  background: var(--accent-soft);
  border-color: var(--gold);
  color: var(--accent);
}

.root-row {
  display: flex;
  align-items: center;
  gap: 2px;
}

.root-row > button:first-child {
  flex: 1;
  min-width: 0;
}

.root-menu .forget-root {
  flex: none;
  display: grid;
  place-items: center;
  width: 24px;
  height: 26px;
  padding: 0;
  color: var(--muted-2);
}

.root-menu .forget-root:hover {
  color: var(--danger);
  background: var(--danger-soft);
}

.root-menu .add-root {
  display: flex;
  align-items: center;
  gap: 6px;
  margin-top: 2px;
  padding-top: 8px;
  border-top: 2px solid var(--border);
}

.running-pill {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  font-size: 11.5px;
  color: var(--accent);
  background: var(--accent-soft);
  border: 2px solid var(--border);
  box-shadow: inset 2px 2px 0 rgba(255, 255, 255, 0.08), inset -2px -2px 0 rgba(0, 0, 0, 0.35);
  padding: 5px 11px;
}

.dot {
  width: 8px;
  height: 8px;
  background: var(--accent);
  animation: pulse 1.4s steps(2, end) infinite;
}

.toast {
  position: fixed;
  left: 50%;
  bottom: 26px;
  transform: translateX(-50%);
  background: var(--toast-bg);
  border: 2px solid var(--border);
  color: var(--text);
  padding: 9px 16px;
  font-size: 12.5px;
  box-shadow: inset 2px 2px 0 var(--bevel-hi), inset -2px -2px 0 var(--bevel-lo), var(--shadow);
  z-index: 30;
}

.page-enter-active,
.page-leave-active {
  transition: opacity 90ms linear;
}

.page-enter-from,
.page-leave-to {
  opacity: 0;
}

.toast-enter-active,
.toast-leave-active {
  transition: opacity 90ms linear;
}

.toast-enter-from,
.toast-leave-to {
  opacity: 0;
}

.archive-overlay {
  position: fixed;
  inset: 0;
  z-index: 24;
  display: grid;
  place-items: center;
  padding: 36px;
  background: var(--overlay);
}

.archive-dialog {
  width: min(480px, 100%);
  display: flex;
  flex-direction: column;
  gap: 12px;
  padding: 18px;
  border: 2px solid var(--border);
  background: var(--panel);
  box-shadow: inset 2px 2px 0 var(--bevel-hi), inset -2px -2px 0 var(--bevel-lo), var(--shadow);
}

.archive-dialog h3 {
  margin: 0;
  font-size: 16px;
  font-weight: 400;
  letter-spacing: 0.04em;
  color: var(--text);
  text-shadow: var(--heading-shadow);
}

.archive-dialog label {
  display: flex;
  flex-direction: column;
  gap: 6px;
  font-size: 12px;
  color: var(--muted);
}

.archive-path {
  margin: 0;
  font-size: 11px;
  color: var(--muted-2);
  word-break: break-all;
}

.archive-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
}

.hint {
  margin: 0;
  font-size: 11.5px;
  color: var(--muted-2);
  line-height: 1.6;
}
</style>
