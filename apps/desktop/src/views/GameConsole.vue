<script setup lang="ts">
import { onBeforeUnmount, ref, watch } from "vue";
import { api, formatDuration, type GameExit, type LogAnalysis } from "../api";
import Icon from "../components/Icon.vue";
import LogViewer from "../components/LogViewer.vue";
import { t } from "../i18n";

const props = defineProps<{
  title: string;
  /** Instance the log belongs to, used for saving it to disk. */
  instance?: string | null;
  lines: string[];
  exit: GameExit | null;
  loading: boolean;
  running: boolean;
}>();

defineEmits<{ close: []; stop: [] }>();

const analysis = ref<LogAnalysis | null>(null);
let timer: number | undefined;
let request = 0;
let lastAnalyze = 0;
/** A new line arrived that has not been analysed yet. */
let pending = false;

async function analyze() {
  pending = false;
  lastAnalyze = Date.now();
  const id = ++request;
  try {
    const result = await api.analyzeLog(props.lines.join("\n"));
    if (id === request) analysis.value = result;
  } catch {
    // Keep the previous report; the raw log stays readable.
  }
}

// The old debounce restarted its 700 ms timer on every line, so a game that
// kept logging never reached the analysis at all. Throttle instead: run at
// most once per interval, and still once after the burst ends.
const ANALYZE_EVERY = 1500;

function schedule() {
  pending = true;
  const wait = Math.max(0, ANALYZE_EVERY - (Date.now() - lastAnalyze));
  if (timer) window.clearTimeout(timer);
  timer = window.setTimeout(() => {
    timer = undefined;
    if (pending) void analyze();
  }, wait);
}

watch(() => props.lines, schedule);
watch(() => props.lines.length, schedule);

onBeforeUnmount(() => {
  if (timer) window.clearTimeout(timer);
  request += 1;
});

const status = ref<string | null>(null);

function flash(message: string) {
  status.value = message;
  window.setTimeout(() => {
    if (status.value === message) status.value = null;
  }, 2600);
}

const fixing = ref(false);

async function fixDependencies(kind: "deps" | "versions") {
  if (!props.instance || props.running || fixing.value) return;
  fixing.value = true;
  flash(t("doctor.working"));
  try {
    if (kind === "versions") {
      const fixed = await api.fixDependencyVersions(props.instance);
      flash(
        fixed.unresolved.length
          ? t("editor.versionsUnresolved", { list: fixed.unresolved.join(", ") })
          : fixed.updated.length
            ? t("editor.versionsFixed", { count: fixed.updated.length })
            : t("editor.versionsNone"),
      );
    } else {
      const report = await api.installMissingDeps(props.instance);
      flash(
        report.unresolved.length
          ? t("editor.depsUnresolved", { list: report.unresolved.join(", ") })
          : t("editor.depsInstalled", { count: report.installed.length }),
      );
    }
  } catch (cause) {
    flash(String(cause));
  } finally {
    fixing.value = false;
  }
}

async function copyLog() {
  try {
    await navigator.clipboard.writeText(props.lines.join("\n"));
    flash(t("console.copied"));
  } catch (cause) {
    flash(String(cause));
  }
}

async function saveLog() {
  if (!props.instance) return;
  const target = await api.pickSaveFile(t("console.export"), `${props.instance}.log`, ["log"]);
  if (!target) return;
  try {
    await api.exportLog(props.instance, target);
    flash(t("console.exported", { file: target.split(/[\\/]/).pop() ?? target }));
  } catch (cause) {
    flash(String(cause));
  }
}

</script>

<template>
  <div class="overlay" @click.self="$emit('close')">
    <section class="console">
      <header>
        <div class="head-left">
          <span class="terminal-badge"><Icon name="terminal" :size="16" /></span>
          <div>
            <strong>{{ title }}</strong>
            <span v-if="running" class="badge ok"><i class="live" />{{ t("instances.running") }}</span>
          </div>
        </div>
        <div class="buttons">
          <span v-if="status" class="saved">{{ status }}</span>
          <button class="ghost small" :disabled="loading || !lines.length" @click="copyLog">
            <Icon name="copy" :size="13" />{{ t("console.copy") }}
          </button>
          <button class="ghost small" :disabled="!instance" @click="saveLog">
            <Icon name="download" :size="13" />{{ t("console.export") }}
          </button>
          <button v-if="running" class="danger" @click="$emit('stop')">
            <Icon name="stop" :size="13" />{{ t("console.stopGame") }}
          </button>
          <button class="icon" :title="t('action.close')" @click="$emit('close')"><Icon name="close" /></button>
        </div>
      </header>

      <LogViewer :lines="lines" :analysis="analysis" :loading="loading" :running="running" @fix="fixDependencies" />

      <footer>
        <template v-if="exit">
          <span v-if="exit.crash" class="badge danger">{{ exit.crash.kind }}</span>
          <span v-else class="badge ok">{{ t("console.exit", { code: exit.exit_code ?? "?" }) }}</span>
          <span class="duration mono">{{ formatDuration(exit.duration_ms) }}</span>
        </template>

        <div v-if="exit?.crash" class="crash">
          <p>{{ exit.crash.summary }}</p>
          <p v-for="hint in exit.crash.hints" :key="hint">• {{ hint }}</p>
        </div>
      </footer>
    </section>
  </div>
</template>

<style scoped>
.saved {
  font-size: 11px;
  color: var(--accent);
}

.small {
  font-size: 11px;
  padding: 5px 9px;
}

.overlay {
  position: fixed;
  inset: 0;
  background: var(--overlay);
  backdrop-filter: blur(6px);
  display: grid;
  place-items: center;
  padding: 36px;
  z-index: 20;
}

.console {
  width: min(980px, 100%);
  height: min(640px, 100%);
  background: var(--console-bg);
  border: 1px solid var(--border-strong);
  border-radius: 18px;
  display: flex;
  flex-direction: column;
  overflow: hidden;
  box-shadow: 0 30px 80px rgba(0, 0, 0, 0.6);
  animation: rise 180ms ease both;
}

header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 14px 18px;
  gap: 12px;
  border-bottom: 1px solid var(--border);
  background: var(--surface-1);
}

.head-left {
  display: flex;
  align-items: center;
  gap: 11px;
  min-width: 0;
}

.terminal-badge {
  display: grid;
  place-items: center;
  width: 34px;
  height: 34px;
  border-radius: 10px;
  color: var(--accent);
  background: rgba(111, 220, 111, 0.1);
  border: 1px solid rgba(111, 220, 111, 0.2);
}

header strong {
  display: block;
  font-size: 13.5px;
  margin-bottom: 3px;
  word-break: break-all;
}

.live {
  width: 5px;
  height: 5px;
  border-radius: 50%;
  background: currentColor;
  animation: pulse 1.8s ease-in-out infinite;
}

.buttons {
  display: flex;
  gap: 8px;
  align-items: center;
}

.buttons .danger {
  font-size: 12px;
  padding: 7px 12px;
}

.body {
  flex: 1;
  min-height: 0;
  padding: 14px 18px;
  font-size: 11.5px;
  line-height: 1.62;
  background: var(--console-bg);
}

.line {
  margin: 0;
  white-space: pre-wrap;
  word-break: break-all;
  color: var(--log-text);
}

.line.err {
  color: var(--log-err);
}

.line.warn {
  color: var(--log-warn);
}

.line.ok {
  color: var(--log-ok);
}

.note {
  color: var(--muted);
}

footer {
  border-top: 1px solid var(--border);
  padding: 11px 18px;
  display: flex;
  gap: 10px;
  align-items: center;
  flex-wrap: wrap;
  background: var(--surface-1);
}

.follow {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 11px;
  color: var(--accent);
}

.follow i {
  width: 5px;
  height: 5px;
  border-radius: 50%;
  background: currentColor;
}

.follow.paused {
  color: var(--muted-2);
}

.duration {
  font-size: 11px;
  color: var(--muted);
}

.crash {
  width: 100%;
  color: var(--danger);
  font-size: 12px;
  line-height: 1.55;
}

.crash p {
  margin: 3px 0;
}
</style>
