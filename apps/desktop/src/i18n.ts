/**
 * Tiny message table. A string key is looked up in the active locale and falls
 * back to English; `{name}` placeholders are replaced from `values`.
 */
import { computed, ref } from "vue";
import ar from "./locales/ar";
import en from "./locales/en";
import es from "./locales/es";
import fr from "./locales/fr";
import ja from "./locales/ja";
import pt from "./locales/pt";
import ru from "./locales/ru";
import zh from "./locales/zh-CN";

export type Locale = "en" | "zh-CN" | "es" | "fr" | "ar" | "pt" | "ru" | "ja";
/** `system` follows the operating system language. */
export type LocaleChoice = "system" | Locale;

export const LOCALES: Locale[] = ["en", "zh-CN", "es", "fr", "ar", "pt", "ru", "ja"];
export const languageChoices: LocaleChoice[] = ["system", ...LOCALES];

const tables: Record<Locale, Record<string, string>> = {
  en,
  "zh-CN": zh,
  es,
  fr,
  ar,
  pt,
  ru,
  ja,
};

/** Languages written right to left. */
const RTL: Locale[] = ["ar"];

const LOCALE_KEY = "tntl.locale";
/** The key used before the rename; read once so the choice survives. */
const LEGACY_LOCALE_KEY = "tnt.locale";

function match(tag: string): Locale | null {
  const lower = tag.toLowerCase();
  if (lower.startsWith("zh")) return "zh-CN";
  for (const locale of LOCALES) {
    if (lower === locale.toLowerCase() || lower.startsWith(`${locale.toLowerCase()}-`)) return locale;
  }
  if (lower.startsWith("pt")) return "pt";
  if (lower.startsWith("es")) return "es";
  return null;
}

function systemLocale(): Locale {
  const tags = typeof navigator === "undefined" ? [] : (navigator.languages ?? [navigator.language]);
  for (const tag of tags) {
    const locale = tag ? match(tag) : null;
    if (locale) return locale;
  }
  return "en";
}

function storedChoice(): LocaleChoice {
  try {
    const saved = localStorage.getItem(LOCALE_KEY) ?? localStorage.getItem(LEGACY_LOCALE_KEY);
    if (!saved) return "system";
    if (saved === "system") return "system";
    return match(saved) ?? "system";
  } catch {
    return "system";
  }
}

/** What the settings page shows as selected. */
export const choice = ref<LocaleChoice>(storedChoice());

/** The language actually in use. */
export const locale = computed<Locale>(() => (choice.value === "system" ? systemLocale() : choice.value));

function apply(next: Locale) {
  document.documentElement.lang = next;
  document.documentElement.dir = RTL.includes(next) ? "rtl" : "ltr";
}

export function setLocale(next: LocaleChoice) {
  choice.value = next;
  apply(locale.value);
  try {
    localStorage.setItem(LOCALE_KEY, next);
  } catch {
    // Storage can be unavailable; the session still uses `next`.
  }
}

export function t(key: string, values?: Record<string, string | number>): string {
  let text = tables[locale.value]?.[key] ?? tables.en[key] ?? key;
  if (values) {
    for (const [name, value] of Object.entries(values)) {
      text = text.replaceAll(`{${name}}`, String(value));
    }
  }
  return text;
}
