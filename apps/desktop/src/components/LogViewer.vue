<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import type { Finding, LogAnalysis, LogLevel } from "../api";
import Icon from "./Icon.vue";
import FindingCard from "./FindingCard.vue";
import { findingsReport } from "../findings";
import { t } from "../i18n";

const props = defineProps<{
  lines: string[];
  analysis: LogAnalysis | null;
  loading: boolean;
  running: boolean;
}>();
defineEmits<{ fix: [kind: "deps" | "versions"] }>();

type Tab = "all" | "problems" | "diagnosis";
const tab = ref<Tab>("all");
const search = ref("");
const showInfo = ref(true);
const showWarn = ref(true);
const showError = ref(true);
const showDebug = ref(false);
const follow = ref(true);
const body = ref<HTMLElement | null>(null);
const highlighted = ref<number | null>(null);
const copied = ref(false);

const counts = computed(
  () =>
    props.analysis?.counts ?? {
      total: props.lines.length,
      debug: 0,
      info: 0,
      warn: 0,
      error: 0,
      context: 0,
    },
);
const problemCount = computed(() => counts.value.warn + counts.value.error);
const findings = computed<Finding[]>(() => props.analysis?.findings ?? []);

function fallback(line: string): LogLevel {
  if (/\/ERROR\]|\/FATAL\]|\bERROR\b|Exception|Caused by:/i.test(line)) return "error";
  if (/\/WARN\]|\bWARN\b/i.test(line)) return "warn";
  if (/\/DEBUG\]|\/TRACE\]/i.test(line)) return "debug";
  return "info";
}

function levelOf(index: number): LogLevel {
  return props.analysis?.levels[index] ?? fallback(props.lines[index] ?? "");
}

function levelShown(level: LogLevel): boolean {
  if (level === "context") return showWarn.value || showError.value;
  if (level === "info") return showInfo.value;
  if (level === "warn") return showWarn.value;
  if (level === "error") return showError.value;
  return showDebug.value;
}

const visible = computed(() => {
  if (tab.value === "diagnosis") return [];
  const needle = search.value.trim().toLowerCase();
  const items: { index: number; text: string; level: LogLevel }[] = [];
  for (let index = 0; index < props.lines.length; index += 1) {
    const text = props.lines[index] ?? "";
    const level = levelOf(index);
    if (tab.value === "problems" && level !== "warn" && level !== "error" && level !== "context") continue;
    if (!levelShown(level)) continue;
    if (needle && !text.toLowerCase().includes(needle)) continue;
    items.push({ index, text, level });
  }
  return items;
});

const filteredFindings = computed<Finding[]>(() => {
  const needle = search.value.trim().toLowerCase();
  if (!needle) return findings.value;
  return findings.value.filter((finding) =>
    [finding.code, finding.subject, finding.evidence, ...Object.values(finding.params)]
      .filter(Boolean)
      .some((value) => String(value).toLowerCase().includes(needle)),
  );
});

function lineClass(level: LogLevel): string {
  if (level === "error") return "err";
  if (level === "warn") return "warn";
  if (level === "context") return "context";
  if (level === "debug") return "debug";
  return "";
}

function onScroll() {
  const element = body.value;
  if (!element) return;
  follow.value = element.scrollHeight - element.scrollTop - element.clientHeight < 40;
}

async function jump(line: number) {
  tab.value = "all";
  search.value = "";
  showInfo.value = true;
  showWarn.value = true;
  showError.value = true;
  showDebug.value = true;
  follow.value = false;
  highlighted.value = line;
  await nextTick();
  body.value?.querySelector<HTMLElement>(`[data-line="${line}"]`)?.scrollIntoView({ block: "center" });
  window.setTimeout(() => {
    if (highlighted.value === line) highlighted.value = null;
  }, 3200);
}

async function copyReport() {
  try {
    await navigator.clipboard.writeText(findingsReport(findings.value));
    copied.value = true;
    window.setTimeout(() => (copied.value = false), 2000);
  } catch {
    copied.value = false;
  }
}

watch(
  () => props.lines.length,
  async () => {
    if (!follow.value || tab.value !== "all") return;
    await nextTick();
    body.value?.scrollTo({ top: body.value.scrollHeight });
  },
);

watch(tab, async (next) => {
  await nextTick();
  const element = body.value;
  if (!element) return;
  if (next === "all" && follow.value) element.scrollTo({ top: element.scrollHeight });
  else element.scrollTo({ top: 0 });
});
</script>

<template>
  <div class="viewer">
    <div class="tabs">
      <button type="button" :class="{ active: tab === 'all' }" @click="tab = 'all'">
        {{ t("log.all") }}<span>{{ counts.total }}</span>
      </button>
      <button type="button" :class="{ active: tab === 'problems' }" @click="tab = 'problems'">
        {{ t("log.problems") }}<span :class="{ hot: problemCount }">{{ problemCount }}</span>
      </button>
      <button type="button" :class="{ active: tab === 'diagnosis' }" @click="tab = 'diagnosis'">
        {{ t("log.diagnosis") }}<span :class="{ hot: findings.length }">{{ findings.length }}</span>
      </button>
      <span v-if="running" class="live"><i />{{ t("log.live") }}</span>
    </div>

    <div class="tools">
      <label class="search">
        <Icon name="search" :size="13" />
        <input v-model="search" spellcheck="false" :placeholder="t('log.search')" />
      </label>
      <div class="levels">
        <button type="button" :class="{ on: showInfo }" @click="showInfo = !showInfo">
          {{ t("log.levelInfo") }} {{ counts.info }}
        </button>
        <button type="button" class="warn" :class="{ on: showWarn }" @click="showWarn = !showWarn">
          {{ t("log.levelWarn") }} {{ counts.warn }}
        </button>
        <button type="button" class="err" :class="{ on: showError }" @click="showError = !showError">
          {{ t("log.levelError") }} {{ counts.error }}
        </button>
        <button type="button" class="debug" :class="{ on: showDebug }" @click="showDebug = !showDebug">
          {{ t("log.levelDebug") }} {{ counts.debug }}
        </button>
      </div>
      <span v-if="search.trim() || tab === 'problems'" class="shown">
        {{ t("log.shown", { shown: visible.length, total: counts.total }) }}
      </span>
    </div>

    <div v-if="tab === 'diagnosis'" class="diagnosis scroll">
      <div class="diagnosis-head">
        <p class="note">{{ t("log.diagnosisHint") }}</p>
        <button type="button" class="ghost small" :disabled="!findings.length" @click="copyReport">
          <Icon name="copy" :size="12" />{{ copied ? t("log.copied") : t("log.copyReport") }}
        </button>
      </div>
      <p v-if="!analysis || loading" class="empty">{{ t("log.analyzing") }}</p>
      <p v-else-if="!filteredFindings.length && search.trim()" class="empty">{{ t("log.noMatch") }}</p>
      <p v-else-if="!findings.length" class="empty ok">{{ t("log.noFindings") }}</p>
      <div v-else class="cards">
        <FindingCard
          v-for="finding in filteredFindings"
          :key="`${finding.code}-${finding.subject}-${finding.line}`"
          :finding="finding"
          @jump="jump"
          @fix="$emit('fix', $event)"
        />
      </div>
    </div>

    <div v-else ref="body" class="log scroll" @scroll="onScroll">
      <p v-if="loading" class="empty">{{ t("log.loading") }}</p>
      <p v-else-if="!visible.length" class="empty">
        {{ tab === "problems" && !search.trim() ? t("log.noProblems") : t("log.noMatch") }}
      </p>
      <template v-else>
        <div
          v-for="item in visible"
          :key="item.index"
          class="row"
          :class="[lineClass(item.level), { hit: highlighted === item.index }]"
          :data-line="item.index"
        >
          <span class="no">{{ item.index + 1 }}</span>
          <span class="text">{{ item.text || " " }}</span>
        </div>
      </template>
    </div>
  </div>
</template>

<style scoped>
.viewer {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
}

.tabs {
  display: flex;
  align-items: center;
  gap: 4px;
  padding: 0 14px;
  border-bottom: 1px solid var(--border);
  background: var(--surface-1);
}

.tabs > button {
  border: 0;
  background: transparent;
  color: var(--muted);
  font-size: 12px;
  padding: 9px 11px 8px;
  border-bottom: 2px solid transparent;
  display: inline-flex;
  align-items: center;
  gap: 7px;
  cursor: pointer;
}

.tabs > button.active {
  color: var(--text);
  border-bottom-color: var(--accent);
}

.tabs > button span {
  font-size: 10px;
  min-width: 18px;
  padding: 1px 5px;
  border-radius: 999px;
  background: var(--surface-2);
  color: var(--muted);
  text-align: center;
}

.tabs > button span.hot {
  color: var(--danger);
  background: rgba(255, 107, 107, 0.14);
}

.live {
  margin-left: auto;
  display: inline-flex;
  align-items: center;
  gap: 5px;
  font-size: 10.5px;
  color: var(--accent);
}

.live i {
  width: 5px;
  height: 5px;
  border-radius: 50%;
  background: currentColor;
  animation: pulse 1.8s ease-in-out infinite;
}

.tools {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 8px 14px;
  border-bottom: 1px solid var(--border);
  background: var(--surface-1);
  flex-wrap: wrap;
}

.search {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 4px 8px;
  border: 1px solid var(--border);
  border-radius: 7px;
  color: var(--muted);
  flex: 1;
  min-width: 160px;
  max-width: 340px;
}

.search input {
  border: 0;
  background: transparent;
  color: var(--text);
  font-size: 11.5px;
  outline: none;
  width: 100%;
}

.levels {
  display: flex;
  gap: 4px;
  flex-wrap: wrap;
}

.levels button {
  border: 1px solid var(--border);
  background: transparent;
  color: var(--muted-2);
  border-radius: 6px;
  font-size: 10.5px;
  padding: 3px 7px;
  cursor: pointer;
  opacity: 0.5;
}

.levels button.on {
  opacity: 1;
  color: var(--text);
  border-color: var(--border-strong);
}

.levels button.warn.on { color: var(--warning); }
.levels button.err.on { color: var(--danger); }
.levels button.debug.on { color: var(--muted); }

.shown {
  margin-left: auto;
  font-size: 10.5px;
  color: var(--muted-2);
}

.log,
.diagnosis {
  flex: 1;
  min-height: 0;
  overflow: auto;
  background: var(--console-bg);
  padding: 10px 14px 16px;
  font-size: 11.5px;
  line-height: 1.6;
}

.diagnosis {
  background: var(--surface-1);
  padding: 12px 16px 18px;
}

.diagnosis-head {
  display: flex;
  align-items: center;
  gap: 12px;
  justify-content: space-between;
  margin-bottom: 10px;
}

.diagnosis-head .note {
  margin: 0;
  font-size: 11.5px;
  color: var(--muted);
}

.cards {
  display: grid;
  gap: 8px;
}

.empty {
  margin: 18px 4px;
  color: var(--muted);
  font-size: 12px;
}

.empty.ok {
  color: var(--accent);
}

.row {
  display: flex;
  gap: 10px;
  padding: 0 4px;
  border-radius: 4px;
  color: var(--log-text);
}

.row .no {
  flex: 0 0 38px;
  text-align: right;
  color: #46505c;
  user-select: none;
  font-size: 10px;
}

.row .text {
  white-space: pre-wrap;
  word-break: break-all;
  min-width: 0;
}

.row.err { color: var(--log-err); }
.row.warn { color: var(--log-warn); }
.row.context { color: #9ba7b4; padding-left: 22px; }
.row.debug { color: #6b7682; }

.row.hit {
  background: rgba(111, 220, 111, 0.16);
  outline: 1px solid rgba(111, 220, 111, 0.35);
}

.small {
  font-size: 11px;
  padding: 4px 8px;
}
</style>
