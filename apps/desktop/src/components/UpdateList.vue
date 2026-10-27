<script setup lang="ts">
import type { ModUpdate } from "../api";
import { t } from "../i18n";

const props = defineProps<{ updates: ModUpdate[]; busy: string | null }>();

defineEmits<{ apply: [files: string[]] }>();
</script>

<template>
  <div v-if="props.updates.length" class="updates">
    <div class="updates-head">
      <span>{{ t("editor.updatesAvailable", { count: props.updates.length }) }}</span>
      <button class="primary small" :disabled="props.busy !== null" @click="$emit('apply', [])">
        {{ props.busy === "*" ? t("editor.updating") : t("editor.updateAll") }}
      </button>
    </div>
    <ul>
      <li v-for="update in props.updates" :key="update.file">
        <div class="text">
          <strong>{{ update.name }}</strong>
          <small class="mono">{{ update.current_version }} → {{ update.latest_version }}</small>
        </div>
        <button class="ghost small" :disabled="props.busy !== null" @click="$emit('apply', [update.file])">
          {{ props.busy === update.file ? t("editor.updating") : t("editor.update") }}
        </button>
      </li>
    </ul>
  </div>
</template>

<style scoped>
.updates {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 10px 12px;
  border: 1px solid rgba(111, 220, 111, 0.32);
  border-radius: 12px;
  background: rgba(111, 220, 111, 0.06);
}

.updates-head {
  display: flex;
  align-items: center;
  gap: 10px;
  font-size: 11.5px;
  color: var(--accent);
}

.updates-head .primary.small {
  margin-inline-start: auto;
  padding: 4px 12px;
  font-size: 11px;
}

ul {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 6px;
}

li {
  display: flex;
  align-items: center;
  gap: 12px;
}

li .ghost {
  margin-inline-start: auto;
  padding: 4px 10px;
  font-size: 11px;
}

.text {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.text strong {
  font-size: 12.5px;
  font-weight: 600;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.text small {
  font-size: 10.5px;
  color: var(--muted-2);
}
</style>
