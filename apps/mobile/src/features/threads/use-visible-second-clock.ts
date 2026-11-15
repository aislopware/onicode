import { useIsFocused } from "@react-navigation/native";
import { secondTick } from "@onicode/client-runtime/clock";
import { useSyncExternalStore } from "react";

import { useAppActive } from "../../lib/appState";
import { usePaneRole } from "../../components/ui/Pane";

const subscribeNever = () => () => undefined;

/**
 * Retained routes stay mounted when hidden, so an ungated tick would keep
 * re-rendering them. Every visible label shares one module-level tick.
 */
export function useVisibleSecondClock(enabled: boolean): number {
  return useVisibleSecondClockValue(enabled, currentSecond);
}

const currentSecond = (nowMs: number) => nowMs;

/**
 * A primitive read from the visible second clock. The caller renders only when
 * it changes, so a label that shows whole minutes does not render each second.
 */
export function useVisibleSecondClockValue<A extends string | number>(
  enabled: boolean,
  read: (nowMs: number) => A,
): A {
  const focused = useIsFocused();
  // The sidebar's route stays unfocused under the page beside it.
  const inSidebar = usePaneRole() === "sidebar";
  const appActive = useAppActive();
  const ticking = enabled && (focused || inSidebar) && appActive;
  // While paused the snapshot still reads the shared tick, so resuming shows
  // the current second rather than the one the clock stopped on.
  const getSnapshot = () => read(secondTick.getSnapshot());
  return useSyncExternalStore(
    ticking ? secondTick.subscribe : subscribeNever,
    getSnapshot,
    getSnapshot,
  );
}
