import type { KeyboardLayoutLetters } from "@onicode/contracts";
import type { ShortcutKeyboardLayoutMap } from "@onicode/shared/keybindings";
import { EVENT_CODE_SHORTCUT_KEYS } from "@onicode/shared/keybindingMatching";
import { useSyncExternalStore } from "react";

interface KeyboardLayoutSource extends Partial<
  Pick<EventTarget, "addEventListener" | "removeEventListener">
> {
  getLayoutMap(): Promise<ShortcutKeyboardLayoutMap>;
}

let keyboardLayoutMap: ShortcutKeyboardLayoutMap | undefined;
let keyboardLayoutLetters: KeyboardLayoutLetters | undefined;
const letterListeners = new Set<() => void>();

export function getKeyboardLayoutMap() {
  return keyboardLayoutMap;
}

function lettersOf(layout: ShortcutKeyboardLayoutMap | undefined) {
  if (!layout) return undefined;
  const letters: Record<string, string> = {};
  const codes = [
    ...Array.from({ length: 26 }, (_, index) => `Key${String.fromCharCode(65 + index)}`),
    ...Object.keys(EVENT_CODE_SHORTCUT_KEYS),
  ];
  for (const code of codes) {
    const letter = layout.get(code)?.toLowerCase();
    if (letter && /^[a-z]$/.test(letter)) letters[code] = letter;
  }
  return letters;
}

function setKeyboardLayoutMap(layout: ShortcutKeyboardLayoutMap | undefined) {
  keyboardLayoutMap = layout;
  const letters = lettersOf(layout);
  // Focus refreshes return a new map for the same layout; only a real change notifies.
  if (JSON.stringify(letters) === JSON.stringify(keyboardLayoutLetters)) return;
  keyboardLayoutLetters = letters;
  for (const listener of letterListeners) listener();
}

function subscribeKeyboardLayoutLetters(listener: () => void) {
  letterListeners.add(listener);
  return () => void letterListeners.delete(listener);
}

/** The layout's letters for preview processes that match shortcuts without `navigator.keyboard`. */
export function getKeyboardLayoutLetters() {
  return keyboardLayoutLetters;
}

export function useKeyboardLayoutLetters() {
  return useSyncExternalStore(subscribeKeyboardLayoutLetters, getKeyboardLayoutLetters);
}

// Option changes event.key to a symbol on macOS; the layout map recovers the
// letter printed on that key without assuming a US physical layout.
export function trackKeyboardLayout(
  keyboard = (navigator as Navigator & { keyboard?: KeyboardLayoutSource }).keyboard,
  focusTarget: EventTarget | undefined = typeof window === "undefined" ? undefined : window,
) {
  let revision = 0;
  const refresh = async (clearMap: boolean) => {
    const request = ++revision;
    if (clearMap) setKeyboardLayoutMap(undefined);
    try {
      const layout = await keyboard?.getLayoutMap();
      if (request === revision) setKeyboardLayoutMap(layout);
    } catch {
      // Unsupported or permission-blocked browsers keep the existing fallback.
      if (request === revision) setKeyboardLayoutMap(undefined);
    }
  };
  const onLayoutChange = () => void refresh(true);
  const onFocus = () => void refresh(false);
  void refresh(true);
  keyboard?.addEventListener?.("layoutchange", onLayoutChange);
  focusTarget?.addEventListener("focus", onFocus);
  return () => {
    revision++;
    setKeyboardLayoutMap(undefined);
    keyboard?.removeEventListener?.("layoutchange", onLayoutChange);
    focusTarget?.removeEventListener("focus", onFocus);
  };
}
