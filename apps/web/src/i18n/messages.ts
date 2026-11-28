import { decodeOptions, getReactI18nCache, gtFallback, type useMessages } from "gt-react";

import { DEFAULT_LOCALE } from "./locales";

export type MessageResolver = ReturnType<typeof useMessages>;

let activeLocale = DEFAULT_LOCALE;

/** The runtime calls this once the locale's catalog is in GT's cache. */
export function setActiveLocale(locale: string): void {
  activeLocale = locale;
}

/** The UI language for Intl formatting outside React; components use GT's `useLocale()`. */
export function getActiveLocale(): string {
  return activeLocale;
}

function resolve(message: string, variables: Parameters<MessageResolver>[1] = {}): string {
  const decoded = decodeOptions(message);
  const options = typeof decoded?.$_source === "string" ? decoded : variables;
  const source = typeof decoded?.$_source === "string" ? decoded.$_source : message;
  // English never touches GT, so this also runs in unit tests where GT was never initialized.
  const target =
    activeLocale === DEFAULT_LOCALE
      ? undefined
      : getReactI18nCache().lookupTranslation(activeLocale, source, { ...options, $format: "ICU" });
  // Untranslated text is English, so it takes English plural rules.
  return typeof target === "string"
    ? gtFallback(target, { ...options, $locale: activeLocale, $_fallback: source })
    : gtFallback(source, { ...options, $locale: DEFAULT_LOCALE });
}

/**
 * Resolves a `msg()` string in the current UI language, at call time. For code outside React
 * (toasts, stores, announcements). Components use `useMessages()` so they re-render on a switch.
 */
export const m = ((message, variables) =>
  typeof message === "string" ? resolve(message, variables) : message) as MessageResolver;

/** A `msg()` template in the current UI language, still unformatted, for libraries that format ICU themselves. */
export function template(message: string): string {
  const decoded = decodeOptions(message);
  const source = typeof decoded?.$_source === "string" ? decoded.$_source : message;
  if (activeLocale === DEFAULT_LOCALE) return source;
  const target = getReactI18nCache().lookupTranslation(activeLocale, source, {
    ...(typeof decoded?.$_source === "string" ? decoded : {}),
    $format: "ICU",
  });
  return typeof target === "string" ? target : source;
}

/**
 * Keeps `msg()` text unresolved, for helpers whose result is cached past a language switch.
 * Only for helpers that pass a whole `msg()` string; display sites resolve the result.
 */
export const keepMarked = ((message: string) => message) as MessageResolver;
