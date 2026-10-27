<script setup lang="ts">
import { computed } from "vue";
import type { DiagnosticReport } from "../api";
import Icon from "./Icon.vue";
import FindingCard from "./FindingCard.vue";
import { findingsReport } from "../findings";
import { t } from "../i18n";

const props = defineProps<{
  name: string;
  report: DiagnosticReport | null;
  busy: boolean;
  error: string | null;
  actionBusy: string | null;
}>();
defineEmits<{
  close: [];
  rerun: [];
  action: [kind: "deps" | "versions" | "repair" | "mods" | "log"];
}>();

const errors = computed(() => props.report?.findings.filter((f) => f.severity === "error").length ?? 0);
const warnings = computed(() => props.report?.findings.filter((f) => f.severity === "warning").length ?? 0);
const hasMissing = computed(
  () =>
    props.report?.findings.some((f) =>
      f.code === "missing_dependency",
    ) ?? false,
);
const hasVersionMismatch = computed(
  () => props.report?.findings.some((f) => f.code === "dependency_version") ?? false,
);
const hasFiles = computed(() => props.report?.findings.some((f) => f.code === "files_missing") ?? false);
const summary = computed(() =>
  errors.value
    ? t("doctor.summary", { errors: errors.value, warnings: warnings.value })
    : warnings.value
      ? t("doctor.summaryWarnings", { warnings: warnings.value })
      : t("doctor.summaryOk"),
);
const loader = computed(() => {
  const report = props.report;
  if (!report) return "";
  return report.loader
    ? `${report.loader} ${report.loader_version ?? ""}`.trim()
    : t("doctor.vanilla");
});

function copy() {
  if (props.report) void navigator.clipboard.writeText(findingsReport(props.report.findings));
}
</script>

<template>
  <div class="overlay" @click.self="$emit('close')">
    <section class="doctor">
      <header>
        <span class="badge-icon"><Icon name="diagnosis" :size="17" /></span>
        <div class="head-text">
          <strong>{{ t("doctor.title") }}</strong>
          <small>{{ name }}</small>
        </div>
        <button class="icon" type="button" :title="t('action.close')" @click="$emit('close')">
          <Icon name="close" />
        </button>
      </header>

      <div v-if="report" class="meta">
        <span class="chip">{{ summary }}</span>
        <span class="chip">{{ report.game_version }}</span>
        <span class="chip">{{ loader }}</span>
        <span class="chip">{{ t("doctor.mods", { enabled: report.enabled_mods, total: report.mods }) }}</span>
        <span class="chip">{{ t("doctor.memory", { memory: report.memory_mb }) }}</span>
        <span v-if="report.java_major" class="chip">{{ t("doctor.java", { major: report.java_major }) }}</span>
      </div>

      <div class="body scroll">
        <p v-if="error" class="error">{{ error }}</p>
        <p v-else-if="busy && !report" class="empty">{{ t("doctor.checking") }}</p>
        <template v-else-if="report">
          <p v-if="!report.findings.length" class="empty ok">
            <Icon name="check" :size="15" />{{ t("doctor.noFindings") }}
          </p>
          <div v-else class="cards">
            <FindingCard
              v-for="(finding, index) in report.findings"
              :key="index"
              :finding="finding"
              @fix="$emit('action', $event)"
            />
          </div>
          <p class="note">{{ t("doctor.offlineHint") }}</p>
        </template>
      </div>

      <footer>
        <button
          v-if="hasMissing"
          class="ghost small"
          type="button"
          :disabled="busy || actionBusy !== null"
          @click="$emit('action', 'deps')"
        >
          <Icon name="download" :size="13" />
          {{ actionBusy === "deps" ? t("doctor.working") : t("doctor.installDeps") }}
        </button>
        <button
          v-if="hasVersionMismatch"
          class="ghost small"
          type="button"
          :disabled="busy || actionBusy !== null"
          @click="$emit('action', 'versions')"
        >
          <Icon name="refresh" :size="13" />
          {{ actionBusy === "versions" ? t("doctor.working") : t("doctor.fixVersions") }}
        </button>
        <button
          v-if="hasFiles"
          class="ghost small"
          type="button"
          :disabled="busy || actionBusy !== null"
          @click="$emit('action', 'repair')"
        >
          <Icon name="refresh" :size="13" />
          {{ actionBusy === "repair" ? t("doctor.working") : t("doctor.repair") }}
        </button>
        <button class="ghost small" type="button" @click="$emit('action', 'mods')">
          <Icon name="folder" :size="13" />{{ t("doctor.openMods") }}
        </button>
        <button class="ghost small" type="button" @click="$emit('action', 'log')">
          <Icon name="terminal" :size="13" />{{ t("doctor.viewLog") }}
        </button>
        <span class="grow" />
        <button class="ghost small" type="button" :disabled="!report" @click="copy">
          <Icon name="copy" :size="13" />{{ t("doctor.copy") }}
        </button>
        <button class="primary small" type="button" :disabled="busy" @click="$emit('rerun')">
          <Icon name="refresh" :size="13" />{{ busy ? t("doctor.checking") : t("doctor.rerun") }}
        </button>
      </footer>
    </section>
  </div>
</template>

<style scoped>
.overlay {
  position: fixed;
  inset: 0;
  background: var(--overlay);
  backdrop-filter: blur(6px);
  display: grid;
  place-items: center;
  padding: 36px;
  z-index: 22;
}

.doctor {
  width: min(780px, 100%);
  max-height: min(720px, 100%);
  background: linear-gradient(180deg, var(--dialog-a), var(--dialog-b));
  border: 1px solid var(--border-strong);
  border-radius: 16px;
  display: flex;
  flex-direction: column;
  overflow: hidden;
  box-shadow: 0 30px 80px rgba(0, 0, 0, 0.55);
  animation: rise 180ms ease both;
}

header {
  display: flex;
  align-items: center;
  gap: 11px;
  padding: 13px 16px;
  border-bottom: 1px solid var(--border);
}

.badge-icon {
  display: grid;
  place-items: center;
  width: 34px;
  height: 34px;
  border-radius: 10px;
  color: var(--accent);
  background: rgba(111, 220, 111, 0.1);
  border: 1px solid rgba(111, 220, 111, 0.2);
}

.head-text { flex: 1; min-width: 0; }
.head-text strong { display: block; font-size: 13.5px; }
.head-text small { color: var(--muted); font-size: 11.5px; }

.meta {
  display: flex;
  gap: 6px;
  flex-wrap: wrap;
  padding: 9px 16px;
  border-bottom: 1px solid var(--border);
}

.chip {
  font-size: 10.5px;
  padding: 3px 8px;
  border-radius: 999px;
  background: var(--surface-2);
  color: var(--muted);
}

.body {
  flex: 1;
  min-height: 120px;
  overflow: auto;
  padding: 12px 16px;
}

.cards { display: grid; gap: 8px; }

.empty {
  margin: 26px 4px;
  color: var(--muted);
  font-size: 12px;
  display: flex;
  align-items: center;
  gap: 7px;
}

.empty.ok { color: var(--accent); }

.note {
  margin: 12px 2px 0;
  font-size: 10.5px;
  color: var(--muted-2);
}

footer {
  display: flex;
  align-items: center;
  gap: 7px;
  flex-wrap: wrap;
  padding: 10px 16px;
  border-top: 1px solid var(--border);
  background: var(--surface-1);
}

footer .grow { flex: 1; }

.small { font-size: 11px; padding: 5px 9px; }

.error {
  margin: 4px 0;
  padding: 9px 11px;
  border-radius: 8px;
  background: rgba(255, 107, 107, 0.1);
  color: var(--danger);
  font-size: 12px;
}
</style>
