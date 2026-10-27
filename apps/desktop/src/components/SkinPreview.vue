<script setup lang="ts">
/**
 * Software rasteriser for the player model. Every face of every box is a
 * textured quad drawn with an affine triangle mapping, so the model has real
 * depth and a back side without WebGL.
 *
 * The model always spins around its own vertical axis and is scaled to fit the
 * canvas at every reachable yaw and pitch, so no angle can push it out of view.
 */
import { onMounted, onUnmounted, ref, watch } from "vue";
import { t } from "../i18n";

const props = defineProps<{
  skin: string | null;
  cape?: string | null;
  model: "wide" | "slim";
}>();

const canvas = ref<HTMLCanvasElement | null>(null);
const dragging = ref(false);

/** Camera distance in model units; the player model is 33 units tall. */
const CAMERA = 150;
/** Camera tilt limits in degrees. */
const MIN_PITCH = -28;
const MAX_PITCH = 32;
/** Room between the model and the canvas edge, CSS pixels. */
const MARGIN = 10;
/** Ground shadow, model units below the centre. */
const SHADOW_Y = 16.4;
const SHADOW_RX = 9;
const SHADOW_RY = 2.4;
/** Skin pixels per texture unit; HD skins are 128 wide. */
const TEXTURE = 64;
/** Idle spin speed, degrees per second. */
const SPIN_SPEED = 20;

type Vec3 = [number, number, number];

interface Uv {
  u: number;
  v: number;
  w: number;
  h: number;
}

interface FaceUvs {
  top: Uv;
  bottom: Uv;
  right: Uv;
  front: Uv;
  left: Uv;
  back: Uv;
}

interface Face {
  corners: [Vec3, Vec3, Vec3, Vec3];
  normal: Vec3;
  uv: Uv;
  source: "skin" | "cape";
  /** Brightness for the fake directional light: 1, 0.88, 0.82 or 0.62. */
  shade: number;
}

/** Prepared textures, one canvas per brightness. */
interface Texture {
  shades: Map<number, HTMLCanvasElement>;
  scale: number;
}

const SHADES = [1, 0.88, 0.82, 0.62];

const skinTexture = ref<Texture | null>(null);
const capeTexture = ref<Texture | null>(null);

let faces: Face[] = [];
let angle = -26;
let pitch = 8;
let spin = true;
let frame = 0;
let lastFrame = 0;
let lastX = 0;
let lastY = 0;
let skinUrl: string | null = null;
let capeUrl: string | null = null;
let observer: ResizeObserver | null = null;

function uv(u: number, v: number, w: number, h: number): Uv {
  return { u, v, w, h };
}

function faceUvs(
  top: [number, number, number, number],
  bottom: [number, number, number, number],
  right: [number, number, number, number],
  front: [number, number, number, number],
  left: [number, number, number, number],
  back: [number, number, number, number],
): FaceUvs {
  return {
    top: uv(...top),
    bottom: uv(...bottom),
    right: uv(...right),
    front: uv(...front),
    left: uv(...left),
    back: uv(...back),
  };
}

/**
 * One box, skin pixels. Corners follow the texture: top-left, top-right,
 * bottom-right, bottom-left. The face labelled `right` is the player's right,
 * which is the -x side because the camera looks down +z at the front.
 */
function box(
  cx: number,
  cy: number,
  cz: number,
  w: number,
  h: number,
  d: number,
  grow: number,
  uvs: FaceUvs,
  source: Face["source"],
): Face[] {
  const hw = (w + grow) / 2;
  const hh = (h + grow) / 2;
  const hd = (d + grow) / 2;
  const x0 = cx - hw;
  const x1 = cx + hw;
  const y0 = cy - hh;
  const y1 = cy + hh;
  const z0 = cz - hd;
  const z1 = cz + hd;
  const face = (
    uvRect: Uv,
    corners: [Vec3, Vec3, Vec3, Vec3],
    normal: Vec3,
    shade: number,
  ): Face => ({ corners, normal, uv: uvRect, source, shade });
  return [
    face(uvs.front, [[x0, y0, z1], [x1, y0, z1], [x1, y1, z1], [x0, y1, z1]], [0, 0, 1], 1),
    face(uvs.back, [[x1, y0, z0], [x0, y0, z0], [x0, y1, z0], [x1, y1, z0]], [0, 0, -1], 0.82),
    face(uvs.right, [[x0, y0, z1], [x0, y0, z0], [x0, y1, z0], [x0, y1, z1]], [-1, 0, 0], 0.88),
    face(uvs.left, [[x1, y0, z0], [x1, y0, z1], [x1, y1, z1], [x1, y1, z0]], [1, 0, 0], 0.88),
    face(uvs.top, [[x0, y0, z0], [x1, y0, z0], [x1, y0, z1], [x0, y0, z1]], [0, -1, 0], 1),
    face(uvs.bottom, [[x0, y1, z1], [x1, y1, z1], [x1, y1, z0], [x0, y1, z0]], [0, 1, 0], 0.62),
  ];
}

function buildModel() {
  const slim = props.model === "slim";
  const armWidth = slim ? 3 : 4;
  const armX = 4 + armWidth / 2;
  const list: Face[] = [];

  list.push(
    ...box(0, -12, 0, 8, 8, 8, 0,
      faceUvs([8, 0, 8, 8], [16, 0, 8, 8], [0, 8, 8, 8], [8, 8, 8, 8], [16, 8, 8, 8], [24, 8, 8, 8]), "skin"),
    ...box(0, -2, 0, 8, 12, 4, 0,
      faceUvs([20, 16, 8, 4], [28, 16, 8, 4], [16, 20, 4, 12], [20, 20, 8, 12], [28, 20, 4, 12], [32, 20, 8, 12]), "skin"),
    ...box(armX, -2, 0, armWidth, 12, 4, 0,
      slim
        ? faceUvs([44, 16, 3, 4], [47, 16, 3, 4], [40, 20, 3, 12], [43, 20, 3, 12], [46, 20, 3, 12], [49, 20, 3, 12])
        : faceUvs([44, 16, 4, 4], [48, 16, 4, 4], [40, 20, 4, 12], [44, 20, 4, 12], [48, 20, 4, 12], [52, 20, 4, 12]), "skin"),
    ...box(-armX, -2, 0, armWidth, 12, 4, 0,
      slim
        ? faceUvs([36, 48, 3, 4], [39, 48, 3, 4], [32, 52, 3, 12], [35, 52, 3, 12], [38, 52, 3, 12], [41, 52, 3, 12])
        : faceUvs([36, 48, 4, 4], [40, 48, 4, 4], [32, 52, 4, 12], [36, 52, 4, 12], [40, 52, 4, 12], [44, 52, 4, 12]), "skin"),
    ...box(2, 10, 0, 4, 12, 4, 0,
      faceUvs([4, 16, 4, 4], [8, 16, 4, 4], [0, 20, 4, 12], [4, 20, 4, 12], [8, 20, 4, 12], [12, 20, 4, 12]), "skin"),
    ...box(-2, 10, 0, 4, 12, 4, 0,
      faceUvs([20, 48, 4, 4], [24, 48, 4, 4], [16, 52, 4, 12], [20, 52, 4, 12], [24, 52, 4, 12], [28, 52, 4, 12]), "skin"),
  );

  // Overlay layers (hat, jacket, sleeves, pants) sit one pixel further out.
  list.push(
    ...box(0, -12, 0, 8, 8, 8, 1,
      faceUvs([40, 0, 8, 8], [48, 0, 8, 8], [32, 8, 8, 8], [40, 8, 8, 8], [48, 8, 8, 8], [56, 8, 8, 8]), "skin"),
    ...box(0, -2, 0, 8, 12, 4, 1,
      faceUvs([20, 32, 8, 4], [28, 32, 8, 4], [16, 36, 4, 12], [20, 36, 8, 12], [28, 36, 4, 12], [32, 36, 8, 12]), "skin"),
    ...box(armX, -2, 0, armWidth, 12, 4, 1,
      slim
        ? faceUvs([44, 32, 3, 4], [47, 32, 3, 4], [40, 36, 3, 12], [43, 36, 3, 12], [46, 36, 3, 12], [49, 36, 3, 12])
        : faceUvs([44, 32, 4, 4], [48, 32, 4, 4], [40, 36, 4, 12], [44, 36, 4, 12], [48, 36, 4, 12], [52, 36, 4, 12]), "skin"),
    ...box(-armX, -2, 0, armWidth, 12, 4, 1,
      slim
        ? faceUvs([52, 48, 3, 4], [55, 48, 3, 4], [48, 52, 3, 12], [51, 52, 3, 12], [54, 52, 3, 12], [57, 52, 3, 12])
        : faceUvs([52, 48, 4, 4], [56, 48, 4, 4], [48, 52, 4, 12], [52, 52, 4, 12], [56, 52, 4, 12], [60, 52, 4, 12]), "skin"),
    ...box(2, 10, 0, 4, 12, 4, 1,
      faceUvs([4, 32, 4, 4], [8, 32, 4, 4], [0, 36, 4, 12], [4, 36, 4, 12], [8, 36, 4, 12], [12, 36, 4, 12]), "skin"),
    ...box(-2, 10, 0, 4, 12, 4, 1,
      faceUvs([4, 48, 4, 4], [8, 48, 4, 4], [0, 52, 4, 12], [4, 52, 4, 12], [8, 52, 4, 12], [12, 52, 4, 12]), "skin"),
  );

  if (capeTexture.value) {
    list.push(
      {
        corners: [[5, -8, -3.6], [-5, -8, -3.6], [-5, 8, -3.6], [5, 8, -3.6]],
        normal: [0, 0, -1],
        uv: uv(1, 1, 10, 16),
        source: "cape",
        shade: 0.88,
      },
      {
        corners: [[-5, -8, -2.4], [5, -8, -2.4], [5, 8, -2.4], [-5, 8, -2.4]],
        normal: [0, 0, 1],
        uv: uv(12, 1, 10, 16),
        source: "cape",
        shade: 0.82,
      },
    );
  }

  faces = list;
}

/** Cut the skin out (alpha test) and pre-darken it for each light level. */
function prepare(image: HTMLImageElement): Texture {
  const base = document.createElement("canvas");
  base.width = image.naturalWidth;
  base.height = image.naturalHeight;
  const context = base.getContext("2d");
  if (!context) {
    return { shades: new Map(), scale: 1 };
  }
  context.imageSmoothingEnabled = false;
  context.drawImage(image, 0, 0);
  const pixels = context.getImageData(0, 0, base.width, base.height);
  for (let index = 3; index < pixels.data.length; index += 4) {
    pixels.data[index] = pixels.data[index] >= 100 ? 255 : 0;
  }
  context.putImageData(pixels, 0, 0);

  const shades = new Map<number, HTMLCanvasElement>();
  for (const shade of SHADES) {
    const canvas = document.createElement("canvas");
    canvas.width = base.width;
    canvas.height = base.height;
    const target = canvas.getContext("2d");
    if (!target) continue;
    target.drawImage(base, 0, 0);
    if (shade < 1) {
      target.globalCompositeOperation = "source-atop";
      target.fillStyle = `rgba(0, 0, 0, ${1 - shade})`;
      target.fillRect(0, 0, canvas.width, canvas.height);
    }
    shades.set(shade, canvas);
  }
  return { shades, scale: base.width / TEXTURE };
}

async function load(url: string | null): Promise<Texture | null> {
  if (!url) return null;
  const image = new Image();
  image.src = url;
  try {
    await image.decode();
  } catch {
    return null;
  }
  return prepare(image);
}

/** Colour of a face: the nearest prepared brightness. */
function shadeOf(texture: Texture, shade: number): HTMLCanvasElement | null {
  let best: number | null = null;
  for (const level of texture.shades.keys()) {
    if (best === null || Math.abs(level - shade) < Math.abs(best - shade)) best = level;
  }
  return best === null ? null : texture.shades.get(best) ?? null;
}

function texturedTriangle(
  context: CanvasRenderingContext2D,
  image: HTMLCanvasElement,
  source: [number, number][],
  target: [number, number][],
) {
  const [sx0, sy0] = source[0];
  const [sx1, sy1] = source[1];
  const [sx2, sy2] = source[2];
  const [dx0, dy0] = target[0];
  const [dx1, dy1] = target[1];
  const [dx2, dy2] = target[2];
  const denom = sx0 * (sy2 - sy1) - sx1 * sy2 + sx2 * sy1 + (sx1 - sx2) * sy0;
  if (!denom) return;
  const m11 = -(sy0 * (dx2 - dx1) - sy1 * dx2 + sy2 * dx1 + (sy1 - sy2) * dx0) / denom;
  const m12 = (sy1 * dy2 + sy0 * (dy1 - dy2) - sy2 * dy1 + (sy2 - sy1) * dy0) / denom;
  const m21 = (sx0 * (dx2 - dx1) - sx1 * dx2 + sx2 * dx1 + (sx1 - sx2) * dx0) / denom;
  const m22 = -(sx1 * dy2 + sx0 * (dy1 - dy2) - sx2 * dy1 + (sx2 - sx1) * dy0) / denom;
  const tx = (sx0 * (sy2 * dx1 - sy1 * dx2) + sy0 * (sx1 * dx2 - sx2 * dx1) + (sx2 * sy1 - sx1 * sy2) * dx0) / denom;
  const ty = (sx0 * (sy2 * dy1 - sy1 * dy2) + sy0 * (sx1 * dy2 - sx2 * dy1) + (sx2 * sy1 - sx1 * sy2) * dy0) / denom;
  context.save();
  context.beginPath();
  context.moveTo(dx0, dy0);
  context.lineTo(dx1, dy1);
  context.lineTo(dx2, dy2);
  context.closePath();
  context.clip();
  context.transform(m11, m12, m21, m22, tx, ty);
  context.drawImage(image, 0, 0);
  context.restore();
}

/** Grow a triangle by `amount` pixels so neighbouring faces do not seam. */
function spread(points: [number, number][], amount: number): [number, number][] {
  const cx = (points[0][0] + points[1][0] + points[2][0]) / 3;
  const cy = (points[0][1] + points[1][1] + points[2][1]) / 3;
  return points.map(([x, y]) => {
    const dx = x - cx;
    const dy = y - cy;
    const length = Math.hypot(dx, dy) || 1;
    return [x + (dx / length) * amount, y + (dy / length) * amount];
  });
}

/** Rotate a point into view space for the given angles, in degrees. */
function rotate(point: Vec3, yawDeg: number, pitchDeg: number): Vec3 {
  const yaw = (yawDeg * Math.PI) / 180;
  const tilt = (pitchDeg * Math.PI) / 180;
  const cosYaw = Math.cos(yaw);
  const sinYaw = Math.sin(yaw);
  const cosTilt = Math.cos(tilt);
  const sinTilt = Math.sin(tilt);
  const x = point[0] * cosYaw + point[2] * sinYaw;
  const z = -point[0] * sinYaw + point[2] * cosYaw;
  return [x, point[1] * cosTilt - z * sinTilt, point[1] * sinTilt + z * cosTilt];
}

/** Screen offset from the centre, per unit of scale. */
function offset(point: Vec3, yawDeg: number, pitchDeg: number): [number, number] {
  const rotated = rotate(point, yawDeg, pitchDeg);
  const factor = CAMERA / (CAMERA - rotated[2]);
  return [rotated[0] * factor, rotated[1] * factor];
}

/**
 * Largest horizontal and vertical offsets, per unit of scale, that the model
 * can reach anywhere in the allowed angle range. Fitting with these means no
 * rotation can ever push part of the model past the canvas edge.
 */
let fitCache: { model: string; x: number; y: number } | null = null;

function fit(): { x: number; y: number } {
  if (!faces.length) return { x: SHADOW_RX, y: SHADOW_Y + SHADOW_RY };
  if (fitCache && fitCache.model === props.model) return fitCache;
  let x = 0;
  let y = 0;
  const points: Vec3[] = [];
  for (const face of faces) points.push(...face.corners);
  for (let yawDeg = 0; yawDeg < 360; yawDeg += 4) {
    for (let pitchDeg = MIN_PITCH; pitchDeg <= MAX_PITCH; pitchDeg += 3) {
      for (const point of points) {
        const [ox, oy] = offset(point, yawDeg, pitchDeg);
        x = Math.max(x, Math.abs(ox));
        y = Math.max(y, Math.abs(oy));
      }
    }
  }
  fitCache = { model: props.model, x: Math.max(x, SHADOW_RX), y: Math.max(y, SHADOW_Y + SHADOW_RY) };
  return fitCache;
}

function render() {
  const element = canvas.value;
  if (!element) return;
  const width = element.clientWidth;
  const height = element.clientHeight;
  if (width < 24 || height < 24) return;
  const ratio = Math.min(window.devicePixelRatio || 1, 2);
  const pixelWidth = Math.round(width * ratio);
  const pixelHeight = Math.round(height * ratio);
  if (element.width !== pixelWidth || element.height !== pixelHeight) {
    element.width = pixelWidth;
    element.height = pixelHeight;
  }
  const context = element.getContext("2d");
  if (!context) return;
  context.setTransform(ratio, 0, 0, ratio, 0, 0);
  context.clearRect(0, 0, width, height);
  context.imageSmoothingEnabled = false;

  const bounds = fit();
  const scale = Math.max(
    0.5,
    Math.min(
      (width / 2 - MARGIN) / bounds.x,
      (height / 2 - MARGIN) / bounds.y,
    ),
  );
  const originX = width / 2;
  const originY = height / 2;

  // Soft shadow on the ground plane, always inside the canvas.
  context.save();
  const shadowY = originY + SHADOW_Y * scale;
  const shadow = context.createRadialGradient(
    originX, shadowY, 2,
    originX, shadowY, SHADOW_RX * scale,
  );
  shadow.addColorStop(0, "rgba(0, 0, 0, 0.35)");
  shadow.addColorStop(1, "rgba(0, 0, 0, 0)");
  context.fillStyle = shadow;
  context.beginPath();
  context.ellipse(originX, shadowY, SHADOW_RX * scale, SHADOW_RY * scale, 0, 0, Math.PI * 2);
  context.fill();
  context.restore();

  const project = (point: Vec3): [number, number] => {
    const rotated = rotate(point, angle, pitch);
    const factor = CAMERA / (CAMERA - rotated[2]);
    return [originX + rotated[0] * scale * factor, originY + rotated[1] * scale * factor];
  };

  const visible = faces
    .map((face) => {
      const normal = rotate(face.normal, angle, pitch);
      const center = rotate(
        [
          (face.corners[0][0] + face.corners[1][0] + face.corners[2][0] + face.corners[3][0]) / 4,
          (face.corners[0][1] + face.corners[1][1] + face.corners[2][1] + face.corners[3][1]) / 4,
          (face.corners[0][2] + face.corners[1][2] + face.corners[2][2] + face.corners[3][2]) / 4,
        ],
        angle,
        pitch,
      );
      const facing =
        normal[0] * -center[0] + normal[1] * -center[1] + normal[2] * (CAMERA - center[2]);
      return { face, depth: center[2], facing };
    })
    .filter((entry) => entry.facing > 0)
    .sort((left, right) => left.depth - right.depth);

  for (const { face } of visible) {
    const texture = face.source === "cape" ? capeTexture.value : skinTexture.value;
    if (!texture) continue;
    const image = shadeOf(texture, face.shade);
    if (!image) continue;
    const { u, v, w, h } = face.uv;
    const s = texture.scale;
    const source: [number, number][] = [
      [u * s, v * s],
      [(u + w) * s, v * s],
      [(u + w) * s, (v + h) * s],
      [u * s, (v + h) * s],
    ];
    const target = face.corners.map(project) as [number, number][];
    texturedTriangle(context, image, [source[0], source[1], source[2]], spread([target[0], target[1], target[2]], 0.45));
    texturedTriangle(context, image, [source[0], source[2], source[3]], spread([target[0], target[2], target[3]], 0.45));
  }
}

function tick(now: number) {
  if (spin && !dragging.value) {
    const delta = lastFrame ? Math.min(now - lastFrame, 120) : 16;
    angle = (angle + (SPIN_SPEED * delta) / 1000) % 360;
    render();
  }
  lastFrame = now;
  frame = requestAnimationFrame(tick);
}

function stopDrag() {
  dragging.value = false;
}

function onPointerDown(event: PointerEvent) {
  dragging.value = true;
  spin = false;
  lastX = event.clientX;
  lastY = event.clientY;
  (event.target as HTMLElement).setPointerCapture?.(event.pointerId);
}

function onPointerMove(event: PointerEvent) {
  if (!dragging.value) return;
  angle = (angle + (event.clientX - lastX) * 0.6) % 360;
  pitch = Math.max(MIN_PITCH, Math.min(MAX_PITCH, pitch - (event.clientY - lastY) * 0.4));
  lastX = event.clientX;
  lastY = event.clientY;
  render();
}

async function refreshTextures() {
  // Decoding is async: a slower request for an older texture must not
  // overwrite the result of a newer one.
  if (props.skin !== skinUrl) {
    const requested = props.skin;
    skinUrl = requested;
    const texture = await load(requested);
    if (skinUrl === requested) skinTexture.value = texture;
  }
  if (props.cape !== capeUrl) {
    const requested = props.cape ?? null;
    capeUrl = requested;
    const texture = await load(requested);
    if (capeUrl === requested) capeTexture.value = texture;
  }
  fitCache = null;
  buildModel();
  render();
}

onMounted(async () => {
  await refreshTextures();
  observer = new ResizeObserver(() => render());
  if (canvas.value) observer.observe(canvas.value);
  window.addEventListener("pointerup", stopDrag);
  window.addEventListener("pointercancel", stopDrag);
  window.addEventListener("blur", stopDrag);
  frame = requestAnimationFrame(tick);
});

watch(
  () => [props.skin, props.cape, props.model],
  () => {
    void refreshTextures();
  },
);

onUnmounted(() => {
  cancelAnimationFrame(frame);
  observer?.disconnect();
  window.removeEventListener("pointerup", stopDrag);
  window.removeEventListener("pointercancel", stopDrag);
  window.removeEventListener("blur", stopDrag);
});

defineExpose({ render });
</script>

<template>
  <div class="skin-stage" :class="{ dragging }">
    <canvas
      ref="canvas"
      class="skin-canvas"
      @pointerdown="onPointerDown"
      @pointermove="onPointerMove"
      @pointerup="stopDrag"
      @pointercancel="stopDrag"
      @dblclick="spin = true"
    />
    <p v-if="!skin" class="skin-empty">{{ t("skin.none") }}</p>
  </div>
</template>

<style scoped>
.skin-stage {
  position: relative;
  flex: 1 1 auto;
  align-self: stretch;
  width: 100%;
  min-height: 220px;
  overflow: hidden;
}

.skin-canvas {
  position: absolute;
  inset: 0;
  display: block;
  width: 100%;
  height: 100%;
  cursor: grab;
  touch-action: none;
  user-select: none;
}

.skin-stage.dragging .skin-canvas {
  cursor: grabbing;
}

.skin-empty {
  position: absolute;
  bottom: 14px;
  left: 0;
  right: 0;
  text-align: center;
  margin: 0;
  font-size: 11.5px;
  color: var(--muted-2);
}
</style>
