import { requireNativeModule } from "expo";
import { useMemo, useSyncExternalStore } from "react";
import { AppState, Platform } from "react-native";
import { createMessageTimeFormatter } from "./messageTimestamp";

function systemUses24HourClock() {
  if (Platform.OS !== "android") return undefined;
  return requireNativeModule<{ is24HourFormat(): boolean }>("OniNativeControls").is24HourFormat();
}

// Work log rows read the clock too, so every reader shares one native read and one listener.
let is24Hour: boolean | undefined;
let stale = true;
let appStateSubscription: { remove(): void } | null = null;
const listeners = new Set<() => void>();

function subscribe(listener: () => void) {
  listeners.add(listener);
  if (Platform.OS === "android" && appStateSubscription === null) {
    appStateSubscription = AppState.addEventListener("change", (state) => {
      if (state !== "active") return;
      const next = systemUses24HourClock();
      if (next === is24Hour) return;
      is24Hour = next;
      for (const notify of listeners) notify();
    });
  }
  return () => {
    listeners.delete(listener);
    if (listeners.size > 0) return;
    appStateSubscription?.remove();
    appStateSubscription = null;
    stale = true;
  };
}

function snapshot() {
  // Unwatched, the setting may have changed since the last read.
  if (stale) {
    is24Hour = systemUses24HourClock();
    stale = false;
  }
  return is24Hour;
}

/** Android's 24-hour clock setting, which Intl ignores; undefined elsewhere, where the locale has it. */
export function useSystem24HourClock(): boolean | undefined {
  return useSyncExternalStore(subscribe, snapshot);
}

export function useMessageTimeFormatter() {
  const is24Hour = useSystem24HourClock();
  return useMemo(() => createMessageTimeFormatter(is24Hour), [is24Hour]);
}
