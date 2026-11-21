import { requireNativeModule } from "expo";
import { useEffect, useMemo, useState } from "react";
import { AppState, Platform } from "react-native";
import { createMessageTimeFormatter } from "./messageTimestamp";

function systemUses24HourClock() {
  if (Platform.OS !== "android") return undefined;
  return requireNativeModule<{ is24HourFormat(): boolean }>("OniNativeControls").is24HourFormat();
}

/** Android's 24-hour clock setting, which Intl ignores; undefined elsewhere, where the locale has it. */
export function useSystem24HourClock(): boolean | undefined {
  const [is24Hour, setIs24Hour] = useState(systemUses24HourClock);
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- Refresh the native clock preference when Android returns to the foreground.
  useEffect(() => {
    if (Platform.OS !== "android") return;
    const subscription = AppState.addEventListener("change", (state) => {
      if (state === "active") setIs24Hour(systemUses24HourClock());
    });
    return () => subscription.remove();
  }, []);
  return is24Hour;
}

export function useMessageTimeFormatter() {
  const is24Hour = useSystem24HourClock();
  return useMemo(() => createMessageTimeFormatter(is24Hour), [is24Hour]);
}
