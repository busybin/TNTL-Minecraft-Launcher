<script setup lang="ts">
import { computed } from "vue";

const props = defineProps<{ name: string; size?: number }>();

const icons: Record<string, string> = {
  play: '<path d="M8 5.2v13.6L19 12z" fill="currentColor" stroke="none"/>',
  stop: '<rect x="6" y="6" width="12" height="12" fill="currentColor" stroke="none"/>',
  grid: '<rect x="3.5" y="3.5" width="7" height="7"/><rect x="13.5" y="3.5" width="7" height="7"/><rect x="3.5" y="13.5" width="7" height="7"/><rect x="13.5" y="13.5" width="7" height="7"/>',
  download: '<path d="M12 4v10"/><path d="M8 11l4 4 4-4"/><path d="M5 19h14"/>',
  settings:
    '<path d="M4 7h8"/><circle cx="15" cy="7" r="2.2"/><path d="M18.2 7H20"/><path d="M4 17h3"/><circle cx="10" cy="17" r="2.2"/><path d="M13.2 17H20"/>',
  terminal: '<rect x="3" y="4.5" width="18" height="15"/><path d="M7 10l2.6 2L7 14"/><path d="M12.5 14.5h4"/>',
  edit: '<path d="M4 20h4.2L19 9.2a2.6 2.6 0 0 0-3.7-3.7L4.5 16.3z"/><path d="M13.5 7.2l3.7 3.7"/>',
  trash: '<path d="M4 7h16"/><path d="M9.5 7V4.8h5V7"/><path d="M6.5 7l1 12.2h9L17.5 7"/>',
  refresh: '<path d="M20 12a8 8 0 1 1-2.6-5.9"/><path d="M20 4v4.2h-4.2"/>',
  search: '<circle cx="10.8" cy="10.8" r="6.3"/><path d="M15.4 15.4L20 20"/>',
  plus: '<path d="M12 5v14"/><path d="M5 12h14"/>',
  user: '<circle cx="12" cy="8.5" r="3.6"/><path d="M4.8 20c1-3.6 3.8-5.4 7.2-5.4s6.2 1.8 7.2 5.4"/>',
  cpu: '<rect x="7" y="7" width="10" height="10"/><path d="M10 3v3"/><path d="M14 3v3"/><path d="M10 18v3"/><path d="M14 18v3"/><path d="M3 10h3"/><path d="M3 14h3"/><path d="M18 10h3"/><path d="M18 14h3"/>',
  chevron: '<path d="M9.5 6.5L15 12l-5.5 5.5"/>',
  "chevron-up": '<path d="M6.5 14.5L12 9l5.5 5.5"/>',
  "chevron-down": '<path d="M6.5 9.5L12 15l5.5-5.5"/>',
  check: '<path d="M5 12.5l4.5 4.5L19 6.5"/>',
  alert: '<path d="M12 4.5l8.5 15h-17z"/><path d="M12 10v4"/><path d="M12 17h.01"/>',
  cube: '<path d="M12 3.2l8 4.4v9L12 21l-8-4.4v-9z"/><path d="M4.2 7.7l7.8 4.3 7.8-4.3"/><path d="M12 12v9"/>',
  folder: '<path d="M3.6 8.2a2.2 2.2 0 0 1 2.2-2.2h3.6l2 2.2h7a2.2 2.2 0 0 1 2.2 2.2v7.4a2.2 2.2 0 0 1-2.2 2.2H5.8a2.2 2.2 0 0 1-2.2-2.2z"/>',
  package: '<rect x="3.5" y="4" width="17" height="5"/><path d="M5 9v9.4A1.6 1.6 0 0 0 6.6 20h10.8a1.6 1.6 0 0 0 1.6-1.6V9"/><path d="M10 13h4"/>',
  spark: '<path d="M12 3.4l1.9 5.3 5.3 1.9-5.3 1.9-1.9 5.3-1.9-5.3L4.8 10.6l5.3-1.9z"/>',
  close: '<path d="M6.5 6.5l11 11"/><path d="M17.5 6.5l-11 11"/>',
  info: '<circle cx="12" cy="12" r="8.2"/><path d="M12 11v5"/><path d="M12 7.8h.01"/>',
  copy: '<rect x="9" y="9" width="11" height="11"/><path d="M15 6.5A2.5 2.5 0 0 0 12.5 4h-6A2.5 2.5 0 0 0 4 6.5v6A2.5 2.5 0 0 0 6.5 15"/>',
  image:
    '<rect x="3.5" y="5" width="17" height="14"/><circle cx="9" cy="10.5" r="1.6"/><path d="M5 17l4.5-4 3 2.5 3.1-3L20 17"/>',
  server:
    '<rect x="3.5" y="4.5" width="17" height="6"/><rect x="3.5" y="13.5" width="17" height="6"/><path d="M7 7.5h.01"/><path d="M7 16.5h.01"/>',
  clock: '<circle cx="12" cy="12" r="8.2"/><path d="M12 7.5V12l3 2"/>',
  diagnosis: '<path d="M3 12h3.5l2-5 3 10 2.5-6 1.5 3H21"/>',
  external: '<path d="M14 4h6v6"/><path d="M20 4l-9 9"/><path d="M19 14v4.5A1.5 1.5 0 0 1 17.5 20h-12A1.5 1.5 0 0 1 4 18.5v-12A1.5 1.5 0 0 1 5.5 5H10"/>',
};

const body = computed(() => icons[props.name] ?? icons.cube);
</script>

<template>
  <svg
    :width="size ?? 18"
    :height="size ?? 18"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="2.2"
    stroke-linecap="square"
    stroke-linejoin="miter"
    shape-rendering="crispEdges"
    aria-hidden="true"
    v-html="body"
  />
</template>
