import type { ScopedThreadRef } from "@onicode/contracts";
import { useEffect, useRef } from "react";

import { appAtomRegistry } from "~/rpc/atomRegistry";
import { localEnvironmentIdAtom } from "~/state/primaryEnvironment";
import { environmentThreadStateAtom } from "~/state/threads";

import { prefetchesOnHover } from "./threadIntentPrefetch.logic";

const INTENT_DELAY_MS = 150;
// A tap leaves the row before its click opens the thread, so the hold outlives the pointer.
const RELEASE_DELAY_MS = 1_000;

/**
 * Loads a thread's detail once the pointer rests on its row (this computer's threads only), or at
 * pointerdown, so the click renders the timeline in its first frame instead of an empty one. The
 * opened thread holds its own subscription, so the row lets go soon after the pointer leaves.
 */
export function useThreadIntentPrefetch(threadRef: ScopedThreadRef) {
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const release = useRef<(() => void) | null>(null);
  const stop = () => {
    clearTimeout(timer.current);
    release.current?.();
    release.current = null;
  };
  const start = () => {
    clearTimeout(timer.current);
    if (release.current !== null) return;
    release.current = appAtomRegistry.mount(
      environmentThreadStateAtom(threadRef.environmentId, threadRef.threadId),
    );
  };
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- releases a held thread subscription when the row unmounts
  useEffect(
    () => () => {
      clearTimeout(timer.current);
      release.current?.();
      release.current = null;
    },
    [],
  );
  return {
    onPointerEnter: (event: React.PointerEvent) => {
      // A held row stays held while the pointer is back on it.
      clearTimeout(timer.current);
      if (release.current !== null) return;
      if (
        prefetchesOnHover({
          pointerType: event.pointerType,
          environmentId: threadRef.environmentId,
          localEnvironmentId: appAtomRegistry.get(localEnvironmentIdAtom),
        })
      ) {
        timer.current = setTimeout(start, INTENT_DELAY_MS);
      }
    },
    onPointerLeave: () => {
      clearTimeout(timer.current);
      if (release.current !== null) timer.current = setTimeout(stop, RELEASE_DELAY_MS);
    },
    onPointerDown: start,
  };
}
