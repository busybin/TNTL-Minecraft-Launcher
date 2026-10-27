<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import { api, type MemoryAdvice } from "../api";
import { t } from "../i18n";

/**
 * One track, two handles. The backend advice turns the track into red/green/
 * yellow bands; the bright overlay between the handles is the allocation the
 * instance will actually ask the JVM for. The top of the track stops at
 * installed RAM minus the OS reserve, so it can never promise the whole
 * machine.
 */
const props = defineProps<{ min: number; max: number }>();
const emit = defineEmits<{ update: [min_mb: number, max_mb: number] }>();

const STEP_MB = 256;
const FLOOR_MB = 256;

const advice = ref<MemoryAdvice | null>(null);
onMounted(async () => {
  try {
    advice.value = await api.memoryAdvice();
  } catch {
    // Falls back to the static bands below.
  }
});

const bands = computed<MemoryAdvice>(
  () =>
    advice.value ?? {
      total_mb: null,
      green_min_mb: 2 * 1024,
      green_max_mb: 6 * 1024,
      warn_max_mb: 12 * 1024,
      limit_mb: 14 * 1024,
    },
);
const limit = computed(() => Math.max(2 * FLOOR_MB, bands.value.limit_mb));
const limitGb = computed(() => Math.round(limit.value / 1024));

const totalText = computed(() =>
  advice.value?.total_mb ? t("memory.total", { gb: Math.round(advice.value.total_mb / 1024) }) : t("memory.totalUnknown"),
);

function clampValue(value: number): number {
  const snapped = Math.round(value / STEP_MB) * STEP_MB;
  return Math.min(Math.max(snapped, FLOOR_MB), limit.value);
}

/** Values from older configs may sit above the cap; show and report the capped one. */
const valueMin = computed(() => Math.min(clampValue(props.min), clampValue(props.max)));
const valueMax = computed(() => Math.max(clampValue(props.min), clampValue(props.max)));
const pctMin = computed(() => (valueMin.value / limit.value) * 100);
const pctMax = computed(() => (valueMax.value / limit.value) * 100);

watch(
  [() => props.min, () => props.max, limit],
  () => {
    // Before the real advice arrives the fallback cap is only a guess; do not
    // shrink a saved value against it.
    if (!advice.value) return;
    if (props.min !== valueMin.value || props.max !== valueMax.value) {
      emit("update", valueMin.value, valueMax.value);
    }
  },
);

const bandStyle = computed(() => {
  const g0 = (bands.value.green_min_mb / limit.value) * 100;
  const g1 = (bands.value.green_max_mb / limit.value) * 100;
  const w = (bands.value.warn_max_mb / limit.value) * 100;
  return {
    background: `linear-gradient(to right, var(--danger) 0%, var(--danger) ${g0}%, #6fdc6f ${g0}%, #6fdc6f ${g1}%, var(--warning) ${g1}%, var(--warning) ${w}%, var(--danger) ${w}%, var(--danger) 100%)`,
  };
});

const selectionStyle = computed(() => ({
  insetInlineStart: `${pctMin.value}%`,
  width: `${Math.max(0, pctMax.value - pctMin.value)}%`,
}));

/** Red when too small to run, green inside the advice, yellow on the headroom. */
const zone = computed(() => {
  if (valueMax.value < bands.value.green_min_mb) return "danger";
  if (valueMax.value > bands.value.warn_max_mb) return "danger";
  if (valueMax.value > bands.value.green_max_mb) return "warn";
  return "ok";
});

const track = ref<HTMLDivElement | null>(null);
let dragging: "min" | "max" | null = null;

function valueAt(clientX: number): number {
  const element = track.value;
  if (!element) return valueMin.value;
  const rect = element.getBoundingClientRect();
  if (rect.width <= 0) return valueMin.value;
  const ratio = Math.min(1, Math.max(0, (clientX - rect.left) / rect.width));
  return clampValue(ratio * limit.value);
}

function apply(which: "min" | "max", value: number): void {
  if (which === "min") {
    emit("update", Math.min(value, valueMax.value), valueMax.value);
  } else {
    emit("update", valueMin.value, Math.max(value, valueMin.value));
  }
}

function startDrag(which: "min" | "max", event: PointerEvent): void {
  dragging = which;
  track.value?.setPointerCapture(event.pointerId);
  apply(which, valueAt(event.clientX));
}

function onPointerDown(event: PointerEvent): void {
  if (event.button !== 0) return;
  const value = valueAt(event.clientX);
  const which =
    Math.abs(value - valueMin.value) <= Math.abs(value - valueMax.value) ? "min" : "max";
  startDrag(which, event);
  event.preventDefault();
}

function onPointerMove(event: PointerEvent): void {
  if (!dragging) return;
  apply(dragging, valueAt(event.clientX));
}

function endDrag(event: PointerEvent): void {
  if (!dragging) return;
  dragging = null;
  if (track.value?.hasPointerCapture(event.pointerId)) {
    track.value.releasePointerCapture(event.pointerId);
  }
}

function onKey(which: "min" | "max", event: KeyboardEvent): void {
  const current = which === "min" ? valueMin.value : valueMax.value;
  let next: number | null = null;
  switch (event.key) {
    case "ArrowLeft":
    case "ArrowDown":
      next = current - STEP_MB;
      break;
    case "ArrowRight":
    case "ArrowUp":
      next = current + STEP_MB;
      break;
    case "PageDown":
      next = current - 1024;
      break;
    case "PageUp":
      next = current + 1024;
      break;
    case "Home":
      next = FLOOR_MB;
      break;
    case "End":
      next = limit.value;
      break;
    default:
      return;
  }
  event.preventDefault();
  if (next === null) return;
  apply(which, clampValue(next));
}
</script>

<template>
  <div class="memory" :class="`zone-${zone}`">
    <div class="memory-head">
      <span>{{ t("memory.title") }}</span>
      <small class="mono">
        {{ totalText }}<template v-if="advice?.total_mb"> · {{ t("memory.limit", { gb: limitGb }) }}</template>
      </small>
    </div>

    <div class="track-wrap">
      <div
        ref="track"
        class="track"
        role="group"
        :aria-label="t('memory.title')"
        @pointerdown="onPointerDown"
        @pointermove="onPointerMove"
        @pointerup="endDrag"
        @pointercancel="endDrag"
      >
        <div class="bands" :style="bandStyle" />
        <div class="selection" :style="selectionStyle" />
        <div
          class="thumb min"
          role="slider"
          tabindex="0"
          :aria-label="t('memory.min')"
          :aria-valuemin="FLOOR_MB"
          :aria-valuemax="limit"
          :aria-valuenow="valueMin"
          :style="{ insetInlineStart: `${pctMin}%` }"
          @keydown="onKey('min', $event)"
        />
        <div
          class="thumb max"
          role="slider"
          tabindex="0"
          :aria-label="t('memory.max')"
          :aria-valuemin="FLOOR_MB"
          :aria-valuemax="limit"
          :aria-valuenow="valueMax"
          :style="{ insetInlineStart: `${pctMax}%` }"
          @keydown="onKey('max', $event)"
        />
      </div>
    </div>

    <div class="memory-foot">
      <span class="mono">
        <span class="tag">{{ t("memory.min") }}</span>
        {{ valueMin }} MiB
      </span>
      <span v-if="zone === 'ok'" class="zone-text ok">{{ t("memory.zoneOk") }}</span>
      <span v-else-if="zone === 'warn'" class="zone-text warn">{{ t("memory.zoneWarn") }}</span>
      <span v-else class="zone-text danger">{{ t("memory.zoneDanger") }}</span>
      <span class="mono">
        <span class="tag">{{ t("memory.max") }}</span>
        {{ valueMax }} MiB
      </span>
    </div>

    <p class="memory-hint">{{ t("memory.advice", { min: bands.green_min_mb, max: bands.green_max_mb }) }}</p>
  </div>
</template>

<style scoped>
.memory {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.memory-head {
  display: flex;
  justify-content: space-between;
  align-items: baseline;
  gap: 8px;
  font-size: 12px;
}

.memory-head small {
  color: var(--muted-2);
}

.track-wrap {
  padding-inline: 10px;
}

.track {
  position: relative;
  block-size: 14px;
  border-radius: 999px;
  border: 1px solid var(--border);
  background: var(--field);
  cursor: pointer;
  touch-action: none;
  direction: ltr;
}

.bands {
  position: absolute;
  inset: 0;
  border-radius: inherit;
  opacity: 0.55;
}

.selection {
  position: absolute;
  inset-block: 0;
  border-radius: 999px;
  background: linear-gradient(180deg, rgba(255, 255, 255, 0.32), rgba(255, 255, 255, 0.12));
  box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.22);
  pointer-events: none;
}

.thumb {
  position: absolute;
  inset-block-start: 50%;
  inline-size: 18px;
  block-size: 18px;
  border-radius: 50%;
  background: var(--surface-1);
  border: 2px solid var(--text);
  box-shadow: 0 2px 6px rgba(0, 0, 0, 0.45);
  transform: translate(-50%, -50%);
  pointer-events: none;
  outline: none;
}

.thumb::after {
  content: "";
  position: absolute;
  inset: 5px;
  border-radius: 50%;
  background: currentColor;
}

.thumb.max {
  border-color: var(--accent);
  color: var(--accent);
}

.thumb.min {
  color: var(--text);
}

.thumb:focus-visible {
  box-shadow: 0 0 0 3px rgba(111, 220, 111, 0.35);
}

.memory-foot {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 8px;
  font-size: 11px;
}

.tag {
  color: var(--muted-2);
  margin-inline-end: 3px;
}

.zone-text {
  font-size: 11px;
}

.zone-text.ok {
  color: var(--accent);
}

.zone-text.warn {
  color: var(--warning);
}

.zone-text.danger {
  color: var(--danger);
}

.memory-hint {
  margin: 0;
  font-size: 10.5px;
  color: var(--muted-2);
}
</style>
