<script setup lang="ts">
import { computed, useId } from "vue";

const props = defineProps<{ seed: string; rounded?: boolean }>();

const uid = useId().replace(/[^a-zA-Z0-9]/g, "");

const hash = computed(() => {
  let value = 0;
  for (const char of props.seed) value = (value * 31 + char.charCodeAt(0)) % 100000;
  return value;
});

/** Flat, block-built hills: every column is a stack of 10px tiles. */
function hills(offset: number, spread: number, top: number): number[] {
  const columns: number[] = [];
  let height = 2 + ((hash.value + offset) % 3);
  for (let index = 0; index < 20; index += 1) {
    const noise = ((hash.value + index * 37 + offset) % spread) - spread / 2;
    height = Math.max(2, Math.min(9, Math.round(2.5 + top / 10 + noise / 12)));
    columns.push(height);
  }
  return columns;
}

const far = computed(() => hills(11, 5, 24));
const near = computed(() => hills(53, 7, 48));

type Tile = { x: number; y: number; top: boolean };

function tiles(columns: number[]): Tile[] {
  const out: Tile[] = [];
  columns.forEach((height, index) => {
    for (let row = 0; row < height; row += 1) {
      out.push({ x: index * 10, y: 200 - (row + 1) * 10, top: row === height - 1 });
    }
  });
  return out;
}

const farTiles = computed(() => tiles(far.value));
const nearTiles = computed(() => tiles(near.value));
const nearDots = computed(() => nearTiles.value.filter((tile, index) => index % 7 === 0 && !tile.top));

/** A small blocky tree sits on the near hills. */
const treeX = computed(() => 24 + (hash.value % 40));
</script>

<template>
  <div class="cover" :class="{ rounded }">
    <svg viewBox="0 0 200 200" preserveAspectRatio="xMidYMid slice" shape-rendering="crispEdges" aria-hidden="true">
      <defs>
        <linearGradient :id="`sky${uid}`" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0" stop-color="#6d9fe8" />
          <stop offset="0.55" stop-color="#96bdf2" />
          <stop offset="1" stop-color="#c7defa" />
        </linearGradient>
        <clipPath :id="`clip${uid}`">
          <rect width="200" height="200" />
        </clipPath>
      </defs>

      <g :clip-path="`url(#clip${uid})`">
        <rect width="200" height="200" :fill="`url(#sky${uid})`" />

        <!-- square sun -->
        <rect x="150" y="30" width="26" height="26" fill="#ffe98a" />
        <rect x="154" y="34" width="18" height="18" fill="#fff8cf" />

        <!-- blocky clouds -->
        <g fill="#f4f9ff" opacity="0.85">
          <rect x="18" y="34" width="42" height="10" />
          <rect x="28" y="24" width="22" height="10" />
          <rect x="118" y="62" width="34" height="10" />
          <rect x="126" y="52" width="16" height="10" />
        </g>
        <g fill="#dfeeff" opacity="0.7">
          <rect x="60" y="78" width="26" height="8" />
          <rect x="8" y="104" width="30" height="8" />
          <rect x="164" y="112" width="26" height="8" />
        </g>

        <!-- far hills -->
        <rect
          v-for="(tile, index) in farTiles"
          :key="`f${index}`"
          :x="tile.x"
          :y="tile.y"
          width="10"
          height="10"
          :fill="tile.top ? '#6f9c72' : '#5c8a63'"
        />

        <!-- near hills: grass on top, dirt underneath -->
        <rect
          v-for="(tile, index) in nearTiles"
          :key="`n${index}`"
          :x="tile.x"
          :y="tile.y"
          width="10"
          height="10"
          :fill="tile.top ? '#4f8a3e' : '#6b4b31'"
        />
        <g fill="#3f7031">
          <rect
            v-for="(tile, index) in nearDots"
            :key="`nd${index}`"
            :x="tile.x + 3"
            :y="tile.y + 3"
            width="4"
            height="4"
          />
        </g>

        <!-- tree -->
        <g :transform="`translate(${treeX} 0)`">
          <rect x="30" y="112" width="8" height="26" fill="#5b3d24" />
          <rect x="30" y="112" width="2" height="26" fill="#755233" />
          <rect x="16" y="86" width="36" height="16" fill="#3f7f35" />
          <rect x="22" y="76" width="24" height="12" fill="#4c9440" />
          <rect x="12" y="96" width="44" height="12" fill="#356b2c" />
          <rect x="22" y="82" width="6" height="6" fill="#5fae50" />
          <rect x="36" y="94" width="6" height="6" fill="#2c5925" />
        </g>

        <!-- floating grass block -->
        <g transform="translate(0 6)">
          <polygon points="100,54 152,84 100,114 48,84" fill="#7cc45e" />
          <polygon points="48,84 100,114 100,170 48,140" fill="#6b4b31" />
          <polygon points="152,84 100,114 100,170 152,140" fill="#573c26" />
          <polygon points="48,84 100,114 100,124 48,94" fill="#4f8a3e" />
          <polygon points="152,84 100,114 100,124 152,94" fill="#3d6e31" />
          <polygon points="100,54 152,84 100,114 48,84" fill="none" stroke="#2c1e12" stroke-width="2" />
          <polygon points="48,84 100,114 100,170 48,140" fill="none" stroke="#2c1e12" stroke-width="2" />
          <polygon points="152,84 100,114 100,170 152,140" fill="none" stroke="#2c1e12" stroke-width="2" />
          <g fill="#8fd06d">
            <rect x="76" y="78" width="6" height="6" />
            <rect x="112" y="92" width="6" height="6" />
            <rect x="94" y="96" width="6" height="6" />
          </g>
          <g fill="#3f2b1a">
            <rect x="58" y="106" width="6" height="6" />
            <rect x="126" y="116" width="6" height="6" />
            <rect x="70" y="130" width="6" height="6" />
            <rect x="112" y="140" width="6" height="6" />
          </g>
        </g>
      </g>
    </svg>
    <div class="vignette" />
  </div>
</template>

<style scoped>
.cover {
  position: relative;
  overflow: hidden;
  background: var(--bg-soft);
  width: 100%;
  height: 100%;
}

svg {
  display: block;
  width: 100%;
  height: 100%;
}

.vignette {
  position: absolute;
  inset: 0;
  background: linear-gradient(180deg, transparent 62%, rgba(0, 0, 0, 0.32));
}
</style>
