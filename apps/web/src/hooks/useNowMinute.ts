import { minuteTick } from "@onicode/client-runtime/clock";
import { useSyncExternalStore } from "react";

const noSubscription = () => () => {};

/** The current minute. While `live` is false it is read on render but does not re-render each minute. */
export function useNowMinute(live = true): string {
  return useSyncExternalStore(
    live ? minuteTick.subscribe : noSubscription,
    minuteTick.getSnapshot,
    minuteTick.getSnapshot,
  );
}
