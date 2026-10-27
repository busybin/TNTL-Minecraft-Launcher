import { ref } from "vue";

/** A launch or install failure that a runtime download would fix. */
export interface JavaPrompt {
  major: number;
  /** Run again once the runtime is installed, when the caller can retry. */
  retry: (() => void) | null;
}

export const javaPrompt = ref<JavaPrompt | null>(null);

const PATTERNS = [/needs Java (\d+)/i, /requires Java (\d+)/i, /Java (\d+) runtime/i];

function requiredMajor(message: string): number {
  for (const pattern of PATTERNS) {
    const major = Number(pattern.exec(message)?.[1]);
    if (major) return major;
  }
  return 0;
}

/**
 * Open the runtime prompt when `message` says a Java version is missing.
 * Returns true when the error was handled, so callers can skip their banner.
 */
export function maybePromptJava(message: string, retry?: () => void): boolean {
  const major = requiredMajor(message);
  if (!major) return false;
  javaPrompt.value = { major, retry: retry ?? null };
  return true;
}

export function closeJavaPrompt() {
  javaPrompt.value = null;
}
