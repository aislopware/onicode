import { useState } from "react";

import { observeIntersection } from "~/lib/observeIntersection";

/**
 * Whether an element has been on screen yet, and the ref that watches it. A row whose read costs
 * a request waits for it, so revealing a long list reads only what scrolls into view.
 */
export function useSeenOnce(deferred: boolean) {
  const [seen, setSeen] = useState(() => !deferred || typeof IntersectionObserver === "undefined");
  const ref = seen
    ? undefined
    : (node: Element | null) => {
        if (!node) return;
        return observeIntersection(node, (entry) => {
          if (entry.isIntersecting) setSeen(true);
        });
      };
  return { seen, ref };
}
