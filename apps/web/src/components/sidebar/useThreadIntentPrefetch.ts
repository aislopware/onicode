import type { ScopedThreadRef } from "@onicode/contracts";
import { useEffect, useState } from "react";

import { appAtomRegistry } from "~/rpc/atomRegistry";
import { localEnvironmentIdAtom } from "~/state/primaryEnvironment";
import { environmentThreadStateAtom } from "~/state/threads";

import { createThreadIntentPrefetch, prefetchesOnHover } from "./threadIntentPrefetch.logic";

/**
 * Loads a thread's detail once the pointer rests on its row (this computer's threads only), or at
 * pointerdown, so the click renders the timeline in its first frame instead of an empty one. The
 * opened thread holds its own subscription, so the row lets go soon after the pointer leaves.
 */
export function useThreadIntentPrefetch(threadRef: ScopedThreadRef) {
  const [prefetch] = useState(createThreadIntentPrefetch);
  const hold = () =>
    appAtomRegistry.mount(environmentThreadStateAtom(threadRef.environmentId, threadRef.threadId));
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- releases a held thread subscription when the row unmounts
  useEffect(() => prefetch.dispose, [prefetch]);
  return {
    onPointerEnter: (event: React.PointerEvent) =>
      prefetch.pointerEnter(
        prefetchesOnHover({
          pointerType: event.pointerType,
          environmentId: threadRef.environmentId,
          localEnvironmentId: appAtomRegistry.get(localEnvironmentIdAtom),
        }),
        hold,
      ),
    onPointerLeave: prefetch.pointerLeave,
    onPointerDown: () => prefetch.pointerDown(hold),
  };
}
