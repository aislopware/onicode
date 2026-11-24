import { decodeOptions, getReactI18nCache, gtFallback, type useMessages } from "gt-react-native";

import { DEFAULT_LOCALE } from "./locales";

export type MessageResolver = ReturnType<typeof useMessages>;

let activeLocale = DEFAULT_LOCALE;

/** I18nProvider calls this once the locale's catalog is in GT's cache. */
export function setActiveLocale(locale: string): void {
  activeLocale = locale;
}

function lookup(message: string, variables: Parameters<MessageResolver>[1]) {
  const decoded = decodeOptions(message);
  const options = typeof decoded?.$_source === "string" ? decoded : variables;
  const source = typeof decoded?.$_source === "string" ? decoded.$_source : message;
  const target =
    activeLocale === DEFAULT_LOCALE
      ? undefined
      : getReactI18nCache().lookupTranslation(activeLocale, source, { ...options, $format: "ICU" });
  return { source, options, target };
}

function resolve(message: string, variables: Parameters<MessageResolver>[1] = {}): string {
  const { source, options, target } = lookup(message, variables);
  // Untranslated text is English, so it takes English plural rules.
  return typeof target === "string"
    ? gtFallback(target, { ...options, $locale: activeLocale, $_fallback: source })
    : gtFallback(source, { ...options, $locale: DEFAULT_LOCALE });
}

/**
 * Resolves a `msg()` string in the current UI language, at call time. For code outside React
 * (alerts, stores, notifications). Components use `useMessages()` so they re-render on a switch.
 */
export const m = ((message, variables) =>
  typeof message === "string" ? resolve(message, variables) : message) as MessageResolver;

/**
 * A `msg()` string's ICU pattern in the current UI language, its variables unfilled. For native
 * code that fills them in while JS is not running.
 */
export function messageTemplate(message: string): string {
  const { source, target } = lookup(message, {});
  return typeof target === "string" ? target : source;
}

/**
 * Keeps `msg()` text unresolved, for helpers whose result is cached past a language switch.
 * Only for helpers that pass a whole `msg()` string; display sites resolve the result.
 */
export const keepMarked = ((message: string) => message) as MessageResolver;
