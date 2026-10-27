<script setup lang="ts">
/** Square avatar cropped from the skin's head textures. */
import { computed, onUnmounted, ref, watch } from "vue";

const props = defineProps<{ bytes: ArrayBuffer | null; size?: number }>();

const url = ref<string | null>(null);

watch(
  () => props.bytes,
  (bytes) => {
    if (url.value) URL.revokeObjectURL(url.value);
    url.value = bytes && bytes.byteLength ? URL.createObjectURL(new Blob([bytes], { type: "image/png" })) : null;
  },
  { immediate: true },
);

onUnmounted(() => {
  if (url.value) URL.revokeObjectURL(url.value);
});

const headStyle = computed(() => {
  const size = props.size ?? 48;
  const unit = size / 8;
  const sheet = 64 * unit;
  return {
    width: `${size}px`,
    height: `${size}px`,
    backgroundImage: url.value ? `url("${url.value}"), url("${url.value}")` : "none",
    backgroundSize: `${sheet}px ${sheet}px`,
    // Hat layer on top of the face, the way the game stacks it.
    backgroundPosition: `-${40 * unit}px -${8 * unit}px, -${8 * unit}px -${8 * unit}px`,
  };
});
</script>

<template>
  <div class="skin-head" :style="headStyle" />
</template>

<style scoped>
.skin-head {
  flex: none;
  border-radius: 10px;
  background-color: var(--surface-3);
  background-repeat: no-repeat;
  border: 1px solid var(--border);
  image-rendering: pixelated;
}
</style>
