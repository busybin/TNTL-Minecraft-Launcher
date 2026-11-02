<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import {
  api,
  events,
  type BisectEvent,
  type BisectModView,
  type BisectPlanView,
  type BisectReport,
  type BisectTestView,
  type UnlistenFn,
} from "../api";
import Icon from "./Icon.vue";
import { t } from "../i18n";

const props = defineProps<{ instance: string }>();
const emit = defineEmits<{ close: []; changed: [] }>();

const plan = ref<BisectPlanView | null>(null);
const error = ref<string | null>(null);
const running = ref(false);
const report = ref<BisectReport | null>(null);
const tests = ref<BisectTestView[]>([]);
const current = ref<{ index: number; kind: string; enabled: number; disabled: number; total: number } | null>(null);
const stage = ref<"plan" | "running" | "restoring" | "done">("plan");
const timeout = ref(180);
const disabled = ref(false);
let unlisten: UnlistenFn | undefined;

const estimate = computed(() => {
  const count = plan.value?.unproven || plan.value?.total || 0;
  return Math.max(2, Math.ceil(Math.log2(Math.max(2, count)))) + 2;
});
const KIND_KEYS: Record<string, string> = {
  reproduce: "reproduce",
  "new mods only": "pool",
  "first half": "halfA",
  "second half": "halfB",
  verify: "verify",
};

function kindLabel(kind: string): string {
  const key = KIND_KEYS[kind];
  return key ? t(`bisect.kind.${key}`) : kind;
}

const canStart = computed(
  () => Boolean(plan.value) && !running.value && (plan.value?.total ?? 0) >= 2 && !plan.value?.blocked.length && !plan.value?.missing.length,
);

async function loadPlan() {
  try {
    plan.value = await api.bisectPlan(props.instance);
  } catch (cause) {
    error.value = String(cause);
  }
}

function onEvent(event: BisectEvent) {
  if (event.type === "plan") {
    plan.value = event.plan;
  } else if (event.type === "test") {
    stage.value = "running";
    current.value = {
      index: event.index,
      kind: event.kind,
      enabled: event.enabled,
      disabled: event.disabled,
      total: event.total,
    };
  } else if (event.type === "result") {
    tests.value = [
      ...tests.value,
      {
        index: event.index,
        kind: current.value?.kind ?? "",
        enabled: current.value?.enabled ?? 0,
        disabled: current.value?.disabled ?? 0,
        crashed: event.crashed,
        duration_ms: event.duration_ms,
        summary: event.summary,
      },
    ];
  } else if (event.type === "restoring") {
    stage.value = "restoring";
    current.value = null;
  } else if (event.type === "finished") {
    report.value = event.report;
    stage.value = "done";
    running.value = false;
  }
}

async function start() {
  if (!canStart.value) return;
  error.value = null;
  report.value = null;
  tests.value = [];
  current.value = null;
  disabled.value = false;
  running.value = true;
  stage.value = "running";
  try {
    const result = await api.startBisect(props.instance, timeout.value);
    report.value = result;
    stage.value = "done";
  } catch (cause) {
    error.value = String(cause);
    stage.value = "plan";
  } finally {
    running.value = false;
  }
}

async function stop() {
  try {
    await api.stopBisect();
  } catch (cause) {
    error.value = String(cause);
  }
}

async function close() {
  if (running.value) {
    if (!window.confirm(t("bisect.closeRunning"))) return;
    await stop();
  }
  emit("close");
}

async function disableCulprit() {
  const culprit = report.value?.culprit;
  if (!culprit) return;
  const files = [culprit.file, ...(report.value?.also_disabled ?? []).map((module) => module.file)];
  try {
    await api.setModsEnabled(props.instance, files, false);
    disabled.value = true;
    emit("changed");
  } catch (cause) {
    error.value = String(cause);
  }
}

function moduleLabel(module: BisectModView): string {
  return module.version ? `${module.name} ${module.version}` : module.name;
}

onMounted(async () => {
  try {
    unlisten = await events.onBisectProgress(onEvent);
  } catch {
    // Progress is an optimisation; the final report still arrives.
  }
  await loadPlan();
});

onBeforeUnmount(() => {
  unlisten?.();
  if (running.value) void api.stopBisect().catch(() => undefined);
});
</script>

<template>
  <Teleport to="body">
    <div class="overlay" @click.self="close">
      <section class="dialog">
        <header>
          <span class="chip"><Icon name="diagnosis" :size="16" /></span>
          <div class="head-text">
            <h2>{{ t("bisect.title") }}</h2>
            <p>{{ t("bisect.subtitle") }}</p>
          </div>
          <button class="icon" type="button" :title="t('action.close')" @click="close">
            <Icon name="close" />
          </button>
        </header>

        <p v-if="error" class="error">{{ error }}</p>

        <div v-if="!plan && !error" class="empty">{{ t("bisect.loading") }}</div>

        <template v-else-if="plan && stage === 'plan'">
          <p class="summary">
            {{ t("bisect.plan", { total: plan.total, proven: plan.proven, unproven: plan.unproven }) }}
          </p>
          <p v-if="plan.unproven" class="hint">{{ t("bisect.testFirst") }}</p>
          <p v-else class="hint">{{ t("bisect.noNew") }}</p>

          <div v-if="plan.suspects.length" class="mods">
            <span class="label">{{ t("bisect.suspects") }}</span>
            <div class="chips">
              <span v-for="module in plan.suspects.slice(0, 14)" :key="module.file" class="mod-chip">
                <strong>{{ moduleLabel(module) }}</strong>
                <small class="mono">{{ module.file }}</small>
              </span>
              <span v-if="plan.suspects.length > 14" class="mod-chip more">+{{ plan.suspects.length - 14 }}</span>
            </div>
          </div>

          <p v-if="plan.blocked.length" class="warn">
            <Icon name="alert" :size="15" />
            {{ t("bisect.blocked") }}
            <span class="mono">{{ plan.blocked.map((module) => module.file).join(", ") }}</span>
          </p>
          <p v-if="plan.missing.length" class="warn">
            <Icon name="alert" :size="15" />
            {{ t("bisect.missing") }}
            <span class="mono">{{ plan.missing.join("; ") }}</span>
          </p>

          <div class="run-row">
            <label>
              <span>{{ t("bisect.timeout") }}</span>
              <select v-model.number="timeout" class="mono">
                <option :value="60">60s</option>
                <option :value="120">120s</option>
                <option :value="180">180s</option>
                <option :value="300">300s</option>
                <option :value="600">600s</option>
              </select>
            </label>
            <span class="hint">{{ t("bisect.estimate", { count: estimate }) }}</span>
            <button class="primary" type="button" :disabled="!canStart" @click="start">
              <Icon name="play" :size="14" />{{ t("bisect.start") }}
            </button>
          </div>
        </template>

        <template v-else-if="stage === 'running' || stage === 'restoring'">
          <p class="summary">
            {{
              stage === "restoring"
                ? t("bisect.restoring")
                : current
                  ? t("bisect.testing", { index: current.index, kind: kindLabel(current.kind) })
                  : t("bisect.starting")
            }}
          </p>
          <p v-if="current" class="hint">
            {{ t("bisect.modsOn", { enabled: current.enabled, total: current.total }) }}
          </p>
          <div class="tests">
            <div v-for="entry in tests" :key="entry.index" class="test-row">
              <span class="mono">#{{ entry.index }}</span>
              <span class="kind">{{ kindLabel(entry.kind) }}</span>
              <span :class="entry.crashed ? 'bad' : 'ok'">
                {{ entry.crashed ? t("bisect.crashed") : t("bisect.passed") }}
              </span>
              <span class="hint">{{ Math.round(entry.duration_ms / 1000) }}s</span>
            </div>
          </div>
          <div class="run-row">
            <button class="danger" type="button" :disabled="stage === 'restoring'" @click="stop">
              <Icon name="stop" :size="13" />{{ t("bisect.stop") }}
            </button>
          </div>
        </template>

        <template v-else-if="report">
          <p class="summary">
            <template v-if="report.outcome === 'culprit' && report.culprit">
              <strong>{{ t("bisect.culprit", { name: moduleLabel(report.culprit) }) }}</strong>
            </template>
            <template v-else-if="report.outcome === 'interaction'">
              <strong>{{ t("bisect.interaction") }}</strong>
            </template>
            <template v-else>
              <strong>{{ t(`bisect.outcome.${report.outcome}`) }}</strong>
            </template>
          </p>
          <p v-if="report.outcome === 'culprit'" class="hint">
            {{ t("bisect.foundIn", { count: report.tests.length }) }}
          </p>
          <p v-else-if="report.outcome === 'inconclusive'" class="hint">{{ report.message }}</p>

          <div v-if="report.culprit" class="culprit">
            <div class="mod-chip">
              <strong>{{ moduleLabel(report.culprit) }}</strong>
              <small class="mono">{{ report.culprit.file }}</small>
            </div>
            <template v-if="report.also_disabled.length">
              <p class="hint">{{ t("bisect.alsoDisabled") }}</p>
              <div class="chips">
                <span v-for="module in report.also_disabled" :key="module.file" class="mod-chip">
                  <strong>{{ moduleLabel(module) }}</strong>
                  <small class="mono">{{ module.file }}</small>
                </span>
              </div>
            </template>
            <button class="primary" type="button" :disabled="disabled" @click="disableCulprit">
              <Icon name="check" :size="14" />{{ disabled ? t("bisect.disabled") : t("bisect.disable") }}
            </button>
          </div>

          <div v-if="report.outcome === 'interaction' && report.suspects.length" class="culprit">
            <p class="hint">{{ t("bisect.interactionHint") }}</p>
            <div class="chips">
              <span v-for="module in report.suspects" :key="module.file" class="mod-chip">
                <strong>{{ moduleLabel(module) }}</strong>
                <small class="mono">{{ module.file }}</small>
              </span>
            </div>
          </div>

          <div class="tests">
            <div v-for="entry in report.tests" :key="entry.index" class="test-row">
              <span class="mono">#{{ entry.index }}</span>
              <span class="kind">{{ kindLabel(entry.kind) }}</span>
              <span :class="entry.crashed ? 'bad' : 'ok'">
                {{ entry.crashed ? t("bisect.crashed") : t("bisect.passed") }}
              </span>
              <span class="hint">{{ Math.round(entry.duration_ms / 1000) }}s</span>
            </div>
          </div>

          <div class="run-row">
            <button class="ghost" type="button" :disabled="running" @click="start">
              <Icon name="refresh" :size="13" />{{ t("bisect.again") }}
            </button>
          </div>
        </template>

        <footer>
          <button class="ghost" type="button" @click="close">{{ t("action.close") }}</button>
        </footer>
      </section>
    </div>
  </Teleport>
</template>

<style scoped>
.overlay {
  position: fixed;
  inset: 0;
  z-index: 70;
  display: grid;
  place-items: center;
  padding: 24px;
  background: rgba(0, 0, 0, 0.66);
}

.dialog {
  width: min(760px, 100%);
  max-height: calc(100vh - 48px);
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  gap: 12px;
  padding: 18px;
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: linear-gradient(180deg, var(--dialog-a), var(--dialog-b));
  box-shadow: var(--shadow);
}

header {
  display: flex;
  align-items: flex-start;
  gap: 10px;
}

.chip {
  display: grid;
  place-items: center;
  width: 34px;
  height: 34px;
  border: 1px solid var(--border);
  background: var(--surface-3);
  color: var(--accent);
}

.head-text {
  flex: 1;
  min-width: 0;
}

h2 {
  margin: 0;
  font-size: 15px;
}

.head-text p {
  margin: 3px 0 0;
  color: var(--muted-2);
  font-size: 11.5px;
}

.summary {
  margin: 0;
  font-size: 13px;
}

.hint {
  margin: 0;
  color: var(--muted-2);
  font-size: 11.5px;
  line-height: 1.5;
}

.error,
.warn {
  margin: 0;
  padding: 8px 10px;
  border: 1px solid var(--border);
  font-size: 11.5px;
  line-height: 1.5;
}

.error {
  color: #ffc9c9;
  background: var(--error-bg);
}

.warn {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 6px;
  color: var(--warning);
  background: rgba(255, 200, 87, 0.08);
  border-color: rgba(255, 200, 87, 0.25);
}

.mods {
  display: flex;
  flex-direction: column;
  gap: 5px;
}

.label {
  color: var(--muted);
  font-size: 10.5px;
  text-transform: uppercase;
  letter-spacing: 0.08em;
}

.chips {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}

.mod-chip {
  display: flex;
  flex-direction: column;
  gap: 1px;
  padding: 5px 8px;
  border: 1px solid var(--border);
  background: var(--row-bg);
  min-width: 0;
}

.mod-chip strong {
  font-size: 11.5px;
}

.mod-chip small {
  color: var(--muted-2);
  font-size: 9.5px;
}

.mod-chip.more {
  justify-content: center;
  color: var(--muted-2);
}

.run-row {
  display: flex;
  flex-wrap: wrap;
  align-items: flex-end;
  gap: 10px;
}

.run-row label {
  display: flex;
  flex-direction: column;
  gap: 3px;
  font-size: 10.5px;
  color: var(--muted);
}

.run-row .primary {
  margin-inline-start: auto;
}

.tests {
  display: flex;
  flex-direction: column;
  gap: 4px;
  max-height: 220px;
  overflow-y: auto;
}

.test-row {
  display: grid;
  grid-template-columns: 44px minmax(0, 1fr) auto auto;
  gap: 8px;
  align-items: center;
  padding: 5px 8px;
  border: 1px solid var(--border);
  background: var(--row-bg);
  font-size: 11px;
}

.kind {
  color: var(--muted);
}

.ok {
  color: var(--accent);
}

.bad {
  color: var(--danger);
}

.culprit {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 10px;
  border: 1px solid var(--border);
  background: var(--surface-1);
}

.culprit .primary {
  align-self: flex-start;
}

footer {
  display: flex;
  justify-content: flex-end;
}

.empty {
  padding: 18px 0;
  color: var(--muted-2);
  text-align: center;
}

@media (max-width: 620px) {
  .overlay {
    padding: 10px;
  }

  .test-row {
    grid-template-columns: 38px minmax(0, 1fr) auto;
  }

  .test-row .hint {
    display: none;
  }
}
</style>
