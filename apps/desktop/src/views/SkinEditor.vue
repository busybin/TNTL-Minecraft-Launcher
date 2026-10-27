<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import { api, type AccountView } from "../api";
import SkinHead from "../components/SkinHead.vue";
import SkinPreview from "../components/SkinPreview.vue";
import SkinGallery from "../components/SkinGallery.vue";
import { t } from "../i18n";

const props = defineProps<{ account: AccountView }>();
const emit = defineEmits<{ close: []; changed: [] }>();

const current = ref<AccountView>({ ...props.account });
const skin = ref<ArrayBuffer | null>(null);
const cape = ref<ArrayBuffer | null>(null);
const skinUrl = ref<string | null>(null);
const capeUrl = ref<string | null>(null);
const busy = ref(false);
const error = ref<string | null>(null);
const status = ref<string | null>(null);
const defaults = ref<Array<{ key: string; bytes: ArrayBuffer | null }>>([]);
const tab = ref<"appearance" | "library">("appearance");
const preview = ref<{ nickname: string | null; slim: boolean; source: string; bytes: ArrayBuffer } | null>(null);
const previewUrl = ref<string | null>(null);

async function loadDefaults() {
  try {
    const keys = await api.defaultSkins();
    defaults.value = await Promise.all(
      keys.map(async (key) => {
        try {
          return { key, bytes: await api.defaultSkinImage(key) };
        } catch {
          return { key, bytes: null };
        }
      }),
    );
  } catch {
    defaults.value = [];
  }
}

async function loadImages() {
  if (skinUrl.value) URL.revokeObjectURL(skinUrl.value);
  if (capeUrl.value) URL.revokeObjectURL(capeUrl.value);
  skinUrl.value = null;
  capeUrl.value = null;
  try {
    const bytes = await api.accountSkin(current.value.id);
    skin.value = bytes.byteLength ? bytes : null;
    skinUrl.value = skin.value ? URL.createObjectURL(new Blob([skin.value], { type: "image/png" })) : null;
  } catch {
    skin.value = null;
  }
  try {
    const bytes = await api.accountCape(current.value.id);
    cape.value = bytes.byteLength ? bytes : null;
    capeUrl.value = cape.value ? URL.createObjectURL(new Blob([cape.value], { type: "image/png" })) : null;
  } catch {
    cape.value = null;
  }
}

async function apply(next: Promise<AccountView>, message?: string) {
  busy.value = true;
  error.value = null;
  status.value = null;
  try {
    current.value = await next;
    await loadImages();
    emit("changed");
    if (message) status.value = message;
  } catch (cause) {
    error.value = String(cause);
  } finally {
    busy.value = false;
  }
}

async function chooseSkin() {
  if (busy.value) return;
  const path = await api.pickFile(t("skin.chooseFile"), ["png"]);
  if (!path) return;
  await apply(api.setAccountSkin(current.value.id, path, current.value.skin_model), t("skin.uploaded"));
}

async function chooseDefault(key: string) {
  if (busy.value) return;
  await apply(api.setDefaultSkin(current.value.id, key), t("skin.defaultApplied"));
}

function onGalleryPreview(item: { nickname: string | null; skin: ArrayBuffer; slim: boolean; source: string }) {
  if (previewUrl.value) URL.revokeObjectURL(previewUrl.value);
  previewUrl.value = URL.createObjectURL(new Blob([item.skin], { type: "image/png" }));
  preview.value = { nickname: item.nickname, slim: item.slim, source: item.source, bytes: item.skin };
}

async function usePreview() {
  if (busy.value) return;
  const item = preview.value;
  if (!item) return;
  // Wear exactly the texture that was previewed: re-resolving a nickname can
  // answer from another source and install a different skin.
  const next = api.applyGallerySkin(current.value.id, Array.from(new Uint8Array(item.bytes)), item.slim);
  clearPreview();
  await apply(next, t("skin.onlineApplied"));
}

function clearPreview() {
  if (previewUrl.value) URL.revokeObjectURL(previewUrl.value);
  previewUrl.value = null;
  preview.value = null;
}

async function chooseModel(model: "wide" | "slim") {
  if (busy.value || model === current.value.skin_model) return;
  await apply(api.updateAccount(current.value.id, model, null));
}

async function toggleApply() {
  if (busy.value) return;
  await apply(api.updateAccount(current.value.id, null, !current.value.apply_skin));
}

async function clearSkin() {
  if (busy.value) return;
  if (!window.confirm(t("skin.clearConfirm", { name: current.value.name }))) return;
  await apply(api.clearAccountSkin(current.value.id), t("skin.cleared"));
}

async function refresh() {
  if (busy.value) return;
  await apply(api.refreshAccount(current.value.id), t("skin.refreshed"));
}

async function chooseCape(id: string | null) {
  if (busy.value) return;
  await apply(api.setAccountCape(current.value.id, id));
}

const kindLabel = computed(() =>
  current.value.microsoft ? t("accounts.microsoft") : t("accounts.offline"),
);

onMounted(() => {
  void loadImages();
  void loadDefaults();
});
onUnmounted(() => {
  if (skinUrl.value) URL.revokeObjectURL(skinUrl.value);
  if (capeUrl.value) URL.revokeObjectURL(capeUrl.value);
  clearPreview();
});
</script>

<template>
  <div class="dialog-overlay" @click.self="emit('close')">
    <section class="skin-dialog">
      <header class="skin-head">
        <div>
          <h3>{{ t("skin.title", { name: current.name }) }}</h3>
          <p class="hint">{{ kindLabel }}</p>
        </div>
        <button class="icon" :title="t('action.close')" @click="emit('close')">✕</button>
      </header>

      <div class="skin-body">
        <div class="skin-preview">
          <SkinPreview
            :skin="previewUrl ?? skinUrl"
            :cape="preview ? null : capeUrl"
            :model="preview ? (preview.slim ? 'slim' : 'wide') : current.skin_model"
          />
          <div v-if="preview" class="preview-bar">
            <span class="hint">
              {{ preview.nickname || t("skin.gallery") }} · {{ t("skin.fromSource", { source: preview.source }) }}
            </span>
            <div class="row-actions">
              <button class="ghost small" @click="clearPreview">{{ t("skin.previewClear") }}</button>
              <button class="primary small" :disabled="busy" @click="usePreview">{{ t("skin.galleryApply") }}</button>
            </div>
          </div>
        </div>

        <div class="skin-right">
          <div class="segmented tabs">
            <button :class="{ active: tab === 'appearance' }" @click="tab = 'appearance'">
              {{ t("skin.tabAppearance") }}
            </button>
            <button :class="{ active: tab === 'library' }" @click="tab = 'library'">
              {{ t("skin.tabLibrary") }}
            </button>
          </div>

          <div v-show="tab === 'appearance'" class="skin-controls">
          <div class="skin-row">
            <span class="label">{{ t("skin.file") }}</span>
            <div class="row-actions">
              <button class="ghost small" :disabled="busy" @click="chooseSkin">
                {{ current.has_skin ? t("skin.replace") : t("skin.upload") }}
              </button>
              <button v-if="current.has_skin" class="ghost small danger" :disabled="busy" @click="clearSkin">
                {{ t("skin.clear") }}
              </button>
            </div>
          </div>

          <div v-if="!current.microsoft && defaults.length" class="skin-row column">
            <span class="label">{{ t("skin.defaults") }}</span>
            <div class="default-grid">
              <button
                v-for="entry in defaults"
                :key="entry.key"
                class="default-chip"
                :disabled="busy"
                :title="t(`skin.default.${entry.key}`)"
                @click="chooseDefault(entry.key)"
              >
                <SkinHead :bytes="entry.bytes" :size="34" />
                <span>{{ t(`skin.default.${entry.key}`) }}</span>
              </button>
            </div>
            <p class="hint">{{ t("skin.defaultsHint") }}</p>
          </div>

          <div class="skin-row">
            <span class="label">{{ t("skin.model") }}</span>
            <div class="segmented">
              <button :class="{ active: current.skin_model === 'wide' }" :disabled="busy" @click="chooseModel('wide')">
                {{ t("skin.wide") }}
              </button>
              <button :class="{ active: current.skin_model === 'slim' }" :disabled="busy" @click="chooseModel('slim')">
                {{ t("skin.slim") }}
              </button>
            </div>
          </div>

          <div v-if="!current.microsoft" class="skin-row column">
            <div class="toggle-line">
              <div>
                <span class="label">{{ t("skin.applyOffline") }}</span>
                <p class="hint">{{ t("skin.applyOfflineHint") }}</p>
              </div>
              <button class="toggle-btn" :class="{ on: current.apply_skin }" :disabled="busy" @click="toggleApply">
                {{ current.apply_skin ? t("editor.enabled") : t("editor.disabled") }}
              </button>
            </div>
          </div>

          <div v-if="current.microsoft" class="skin-row column">
            <span class="label">{{ t("skin.cape") }}</span>
            <div class="cape-list">
              <button class="cape-chip" :class="{ active: !current.cape }" :disabled="busy" @click="chooseCape(null)">
                {{ t("skin.noCape") }}
              </button>
              <button
                v-for="entry in current.capes"
                :key="entry.id"
                class="cape-chip"
                :class="{ active: entry.active }"
                :disabled="busy"
                @click="chooseCape(entry.id)"
              >
                {{ entry.alias || entry.id.slice(0, 8) }}
              </button>
              <span v-if="!current.capes.length" class="hint">{{ t("skin.noCapes") }}</span>
            </div>
          </div>

          <div v-if="current.microsoft" class="skin-row">
            <span class="label">{{ t("skin.account") }}</span>
            <button class="ghost small" :disabled="busy" @click="refresh">{{ t("skin.refresh") }}</button>
          </div>

          <p v-if="error" class="error">{{ error }}</p>
          <p v-if="status" class="hint">{{ status }}</p>
          </div>

          <div v-show="tab === 'library'" class="skin-controls">
            <SkinGallery @preview="onGalleryPreview" />
          </div>
        </div>
      </div>
    </section>
  </div>
</template>

<style scoped>
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

.skin-dialog {
  width: min(760px, 100%);
  max-height: calc(100vh - 72px);
  display: flex;
  flex-direction: column;
  gap: 16px;
  padding: 20px;
  border-radius: 20px;
  border: 1px solid var(--border-strong);
  background: linear-gradient(180deg, var(--dialog-a), var(--dialog-b));
  box-shadow: 0 30px 80px rgba(0, 0, 0, 0.6);
}

.skin-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 12px;
}

.skin-head h3 {
  margin: 0;
  font-size: 15px;
}

.skin-body {
  display: grid;
  grid-template-columns: 1.05fr 1fr;
  grid-template-rows: minmax(0, 1fr);
  gap: 18px;
  min-height: 0;
}

.skin-preview {
  display: flex;
  flex-direction: column;
  min-height: 320px;
  border-radius: 16px;
  border: 1px solid var(--border);
  background:
    radial-gradient(circle at 50% 20%, var(--surface-4), transparent 70%),
    var(--field-soft);
  overflow: hidden;
}

.skin-right {
  display: flex;
  flex-direction: column;
  gap: 12px;
  min-width: 0;
  min-height: 0;
  overflow-y: auto;
}

.skin-controls {
  display: flex;
  flex-direction: column;
  gap: 14px;
}

.tabs {
  align-self: flex-start;
}

.preview-bar {
  flex: 0 0 auto;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  padding: 8px 10px;
  border-top: 1px solid var(--border);
  background: var(--field-soft);
}

.skin-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
}

.skin-row.column {
  flex-direction: column;
  align-items: stretch;
  gap: 8px;
}

.toggle-line {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
}

.label {
  font-size: 12px;
  color: var(--muted);
}

.row-actions {
  display: flex;
  gap: 8px;
}

.segmented {
  display: inline-flex;
  padding: 3px;
  gap: 3px;
  border-radius: 10px;
  background: var(--field);
  border: 1px solid var(--border);
}

.segmented button {
  border: 0;
  background: transparent;
  padding: 5px 12px;
  border-radius: 7px;
  font-size: 12px;
}

.segmented button.active {
  background: var(--accent-soft);
  color: var(--accent);
}

.cape-list {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}

.default-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(78px, 1fr));
  gap: 6px;
}

.default-chip {
  flex-direction: column;
  gap: 5px;
  padding: 7px 4px;
  border-radius: 12px;
  border: 1px solid var(--border);
  background: var(--surface-2);
  color: var(--muted);
  font-size: 10.5px;
  line-height: 1.2;
}

.online-row {
  display: flex;
  gap: 8px;
}

.online-row input {
  flex: 1;
  min-width: 0;
  padding: 7px 10px;
  font-size: 12px;
}

.online-found {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 8px;
  border-radius: 12px;
  border: 1px solid rgba(111, 220, 111, 0.3);
  background: var(--accent-soft);
}

.online-meta {
  display: flex;
  flex-direction: column;
  gap: 2px;
  flex: 1;
  min-width: 0;
  font-size: 12px;
}

.online-meta .hint {
  font-size: 10.5px;
}

.cape-chip {
  font-size: 11.5px;
  padding: 5px 10px;
  border-radius: 999px;
  border: 1px solid var(--border);
  background: var(--surface-2);
  color: var(--muted);
}

.hint {
  margin: 0;
  font-size: 11.5px;
  color: var(--muted-2);
  line-height: 1.6;
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

.cape-chip.active {
  border-color: rgba(111, 220, 111, 0.4);
  color: var(--accent);
  background: var(--accent-soft);
}

@media (max-width: 760px) {
  .skin-body {
    grid-template-columns: 1fr;
    grid-template-rows: auto auto;
    overflow-y: auto;
  }

  .skin-preview {
    min-height: 300px;
  }
}
.online-source {
  max-width: 132px;
  font-size: 11.5px;
}

.browse {
  align-self: flex-start;
}
</style>
