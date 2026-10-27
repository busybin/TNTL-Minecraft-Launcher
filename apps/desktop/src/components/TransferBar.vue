<script setup lang="ts">
import { computed } from "vue";
import { formatBytes, formatSpeed } from "../api";
import { t } from "../i18n";

const props = withDefaults(
  defineProps<{
    label: string;
    done?: number;
    total?: number | null;
    speed?: number;
    filesDone?: number | null;
    filesTotal?: number | null;
    detail?: string;
    indeterminate?: boolean;
    /// Show the cancel button while the transfer can still be stopped.
    busy?: boolean;
  }>(),
  { done: 0, total: null, speed: 0, filesDone: null, filesTotal: null, detail: "", busy: false },
);

const emit = defineEmits<{ cancel: [] }>();

const percent = computed(() => {
  if (props.total && props.total > 0) return Math.min(100, (props.done / props.total) * 100);
  if (props.filesTotal && props.filesTotal > 0 && props.filesDone !== null) {
    return Math.min(100, (props.filesDone / props.filesTotal) * 100);
  }
  return 0;
});

const pulse = computed(() => {
  if (props.indeterminate) return true;
  return !(props.total && props.total > 0) && !(props.filesTotal && props.filesTotal > 0);
});

const bytesText = computed(() => {
  if (!props.done && !props.total) return "";
  const size = formatBytes(props.done);
  return props.total ? `${size} / ${formatBytes(props.total)}` : size;
});

const speedText = computed(() => formatSpeed(props.speed));
</script>

<template>
  <div class="progress-block">
    <div class="progress-head">
      <span>{{ label }}</span>
      <span v-if="!pulse" class="mono">{{ Math.round(percent) }}%</span>
      <button v-if="busy" class="cancel" type="button" @click="emit('cancel')">
        {{ t("action.cancel") }}
      </button>
    </div>
    <div class="bar">
      <div class="fill" :class="{ pulse }" :style="pulse ? undefined : { width: `${percent}%` }" />
    </div>
    <div class="progress-foot mono">
      <span class="detail">{{ detail }}</span>
      <span v-if="bytesText">{{ bytesText }}</span>
      <span v-if="speedText" class="speed">{{ speedText }}</span>
    </div>
  </div>
</template>

<style scoped>
.progress-block {
  display: flex;
  flex-direction: column;
  gap: 7px;
  padding: 12px;
  border: 2px solid var(--border);
  background: var(--panel);
  box-shadow: inset 2px 2px 0 var(--bevel-hi), inset -2px -2px 0 var(--bevel-lo);
}
.progress-head {
  display: flex;
  justify-content: space-between;
  gap: 8px;
  font-size: 11.5px;
  color: var(--muted);
}
.progress-head > span:first-child {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.cancel {
  border: 2px solid var(--border);
  background: var(--panel-2);
  color: var(--muted);
  font-size: 11px;
  padding: 1px 7px;
  cursor: pointer;
}
.cancel:hover {
  color: var(--text);
  border-color: var(--accent);
}
.bar {
  height: 14px;
  padding: 2px;
  background: var(--slot-inset);
  border: 2px solid var(--border);
  box-shadow: inset 2px 2px 0 rgba(0, 0, 0, 0.55);
  overflow: hidden;
}
.fill {
  height: 100%;
  background: var(--accent);
  box-shadow: inset 0 2px 0 rgba(255, 255, 255, 0.4), inset 0 -2px 0 rgba(0, 0, 0, 0.35);
  transition: width 0.15s steps(4, end);
}
.fill.pulse {
  width: 35%;
  animation: slide 1.2s ease-in-out infinite;
}
@keyframes slide {
  0% {
    margin-left: -35%;
  }
  100% {
    margin-left: 100%;
  }
}
.progress-foot {
  display: flex;
  justify-content: space-between;
  gap: 8px;
  font-size: 11px;
  color: var(--muted);
}
.progress-foot .detail {
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.progress-foot .speed {
  color: var(--text);
}
</style>
