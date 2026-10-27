import { computed, ref } from "vue";

/**
 * Byte counters from a progress event turned into a visible rate. Events can be
 * as frequent as one per chunk, so the rate is sampled over a short window
 * instead of trusting two consecutive events.
 */
export function useTransfer() {
  const bytes = ref(0);
  const total = ref<number | null>(null);
  const speed = ref(0);
  let lastTime = 0;
  let lastBytes = 0;

  function track(done: number, sum: number | null = null) {
    const value = Math.max(0, Math.round(done));
    bytes.value = value;
    total.value = sum && sum > 0 ? sum : null;
    const now = performance.now();
    if (!lastTime || value < lastBytes) {
      lastTime = now;
      lastBytes = value;
      if (value === 0) speed.value = 0;
      return;
    }
    const elapsed = now - lastTime;
    if (elapsed < 400) return;
    const rate = ((value - lastBytes) * 1000) / elapsed;
    speed.value = speed.value > 0 ? speed.value * 0.5 + rate * 0.5 : rate;
    lastTime = now;
    lastBytes = value;
  }

  function reset() {
    bytes.value = 0;
    total.value = null;
    speed.value = 0;
    lastTime = 0;
    lastBytes = 0;
  }

  const percent = computed(() => {
    const sum = total.value;
    return sum && sum > 0 ? Math.min(100, (bytes.value / sum) * 100) : 0;
  });

  return { bytes, total, speed, percent, track, reset };
}
