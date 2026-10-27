<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import { api, events, type UnlistenFn } from "../api";
import Icon from "../components/Icon.vue";
import TransferBar from "../components/TransferBar.vue";
import { t } from "../i18n";
import { closeJavaPrompt, javaPrompt } from "../javaPrompt";
import { useTransfer } from "../progress";

const emit = defineEmits<{ installed: [] }>();

const busy = ref(false);
const error = ref<string | null>(null);
const { bytes, total, speed, track, reset } = useTransfer();
let unlisten: UnlistenFn | null = null;

const major = computed(() => javaPrompt.value?.major ?? 0);

onMounted(async () => {
  unlisten = await events.onJavaProgress((event) => {
    if (!busy.value) return;
    if (event.stage === "manifest") reset();
    if (event.stage === "downloading") track(event.done, event.total);
  });
});

onUnmounted(() => unlisten?.());

async function install() {
  const prompt = javaPrompt.value;
  if (!prompt || busy.value) return;
  busy.value = true;
  error.value = null;
  reset();
  try {
    await api.installJava(`adoptium-${prompt.major}`);
    emit("installed");
    closeJavaPrompt();
    prompt.retry?.();
  } catch (cause) {
    error.value = String(cause);
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <Teleport to="body">
    <div v-if="javaPrompt" class="overlay" @click.self="!busy && closeJavaPrompt()">
      <section class="dialog">
        <header>
          <span class="chip"><Icon name="download" :size="16" /></span>
          <div class="head-text">
            <h2>{{ t("java.needTitle", { major }) }}</h2>
            <p>{{ t("java.needBody", { major }) }}</p>
          </div>
        </header>

        <TransferBar
          v-if="busy"
          :label="t('java.downloading', { major })"
          :done="bytes"
          :total="total"
          :speed="speed"
        />
        <p v-if="error" class="error">{{ error }}</p>
        <p v-else class="hint">{{ t("java.hint") }}</p>

        <footer>
          <button class="ghost" :disabled="busy" @click="closeJavaPrompt()">{{ t("java.later") }}</button>
          <button class="primary" :disabled="busy" @click="install">
            <Icon name="download" :size="15" />
            {{ busy ? t("java.downloading", { major }) : t("java.download", { major }) }}
          </button>
        </footer>
      </section>
    </div>
  </Teleport>
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
  backdrop-filter: blur(3px);
}
.dialog {
  width: min(520px, 100%);
  max-height: calc(100vh - 56px);
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  gap: 14px;
  padding: 18px 20px 14px;
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
  background: var(--accent-soft);
  border: 1px solid rgba(111, 220, 111, 0.24);
}
.head-text h2 {
  margin: 0;
  font-size: 16px;
}
.head-text p {
  margin: 4px 0 0;
  font-size: 12px;
  color: var(--muted);
}
.hint {
  margin: 0;
  font-size: 11.5px;
  color: var(--muted-2);
}
.error {
  margin: 0;
  font-size: 11.5px;
  color: var(--danger, #ff7b72);
}
footer {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
}
</style>
