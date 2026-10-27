<script setup lang="ts">
import { computed } from "vue";
import type { Finding } from "../api";
import Icon from "./Icon.vue";
import { findingDetail, findingHint, findingTitle, severityLabel } from "../findings";
import { t } from "../i18n";

const props = defineProps<{ finding: Finding }>();
defineEmits<{ jump: [line: number]; fix: [kind: "deps" | "versions"] }>();

const title = computed(() => findingTitle(props.finding));
const detail = computed(() => findingDetail(props.finding));
const hint = computed(() => findingHint(props.finding));
const fixKind = computed<"deps" | "versions" | null>(() => {
  if (props.finding.code === "missing_dependency") return "deps";
  if (props.finding.code === "dependency_version") return "versions";
  return null;
});
const fixLabel = computed(() => (fixKind.value === "versions" ? t("editor.fixVersions") : t("editor.fixDeps")));
</script>

<template>
  <article class="finding" :class="finding.severity">
    <header>
      <span class="severity">{{ severityLabel(finding.severity) }}</span>
      <strong>{{ title }}</strong>
      <button v-if="fixKind" class="fix" type="button" @click="$emit('fix', fixKind)">
        <Icon name="download" :size="12" />{{ fixLabel }}
      </button>
      <button
        v-if="finding.line !== null"
        class="jump"
        type="button"
        :title="t('log.showLine')"
        @click="$emit('jump', finding.line)"
      >
        <Icon name="terminal" :size="12" />{{ t("log.showLine") }}
      </button>
    </header>
    <p v-if="finding.subject" class="subject">{{ finding.subject }}</p>
    <p v-if="detail" class="detail">{{ detail }}</p>
    <p v-if="finding.evidence" class="evidence mono">{{ finding.evidence }}</p>
    <p class="hint"><Icon name="info" :size="13" />{{ hint }}</p>
  </article>
</template>

<style scoped>
.finding {
  border: 1px solid var(--border);
  border-left: 3px solid var(--muted-2);
  border-radius: 10px;
  padding: 10px 12px;
  background: var(--surface-1);
  display: grid;
  gap: 5px;
}

.finding.error { border-left-color: var(--danger); }
.finding.warning { border-left-color: var(--warning); }
.finding.info { border-left-color: #6fdc6f; }

header {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

header strong { font-size: 12.5px; }

.severity {
  font-size: 10px;
  letter-spacing: 0.04em;
  text-transform: uppercase;
  padding: 2px 6px;
  border-radius: 5px;
  background: var(--surface-2);
  color: var(--muted);
}

.error .severity { color: var(--danger); background: var(--danger-soft); }
.warning .severity { color: var(--warning); background: rgba(255, 184, 77, 0.14); }
.info .severity { color: #8bf0b0; background: rgba(111, 220, 111, 0.12); }

.fix {
  border: 1px solid rgba(111, 220, 111, 0.35);
  background: rgba(111, 220, 111, 0.12);
  color: var(--accent);
  font-size: 11px;
  border-radius: 6px;
  padding: 3px 8px;
  display: inline-flex;
  align-items: center;
  gap: 5px;
  cursor: pointer;
}

.jump {
  margin-left: auto;
  border: 0;
  background: transparent;
  color: var(--accent);
  font-size: 11px;
  display: inline-flex;
  align-items: center;
  gap: 5px;
  cursor: pointer;
  padding: 2px 4px;
}

.subject {
  margin: 0;
  font-size: 12.5px;
  font-weight: 600;
}

.detail,
.hint {
  margin: 0;
  font-size: 11.5px;
  line-height: 1.5;
  color: var(--muted);
}

.evidence {
  margin: 0;
  font-size: 10.5px;
  line-height: 1.45;
  color: var(--log-warn);
  background: var(--console-bg);
  border-radius: 6px;
  padding: 6px 8px;
  overflow-wrap: anywhere;
}

.hint {
  display: flex;
  gap: 6px;
  align-items: flex-start;
  color: var(--muted-2);
}
</style>
