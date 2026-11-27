import type { EnvironmentId } from "@onicode/contracts";

const INTENT_DELAY_MS = 150;
// A tap leaves the row before its click opens the thread, so the hold outlives the pointer.
const RELEASE_DELAY_MS = 1_000;

/**
 * Each row a hover rests on pulls a thread snapshot and a live stream, which a sweep down a long
 * sidebar cannot afford over Tailscale or Funnel. Rows of another machine load at pointerdown.
 */
export function prefetchesOnHover(input: {
  readonly pointerType: string;
  readonly environmentId: EnvironmentId;
  readonly localEnvironmentId: EnvironmentId | null;
}): boolean {
  return input.pointerType === "mouse" && input.environmentId === input.localEnvironmentId;
}

/** A row's hold on its thread, from hover intent or pointerdown until the pointer has left. */
export function createThreadIntentPrefetch() {
  let timer: ReturnType<typeof setTimeout> | undefined;
  let release: (() => void) | null = null;
  const stop = () => {
    clearTimeout(timer);
    release?.();
    release = null;
  };
  const start = (hold: () => () => void) => {
    clearTimeout(timer);
    if (release === null) release = hold();
  };
  return {
    pointerEnter: (hoverPrefetches: boolean, hold: () => () => void) => {
      // A held row stays held while the pointer is back on it.
      clearTimeout(timer);
      if (release === null && hoverPrefetches) {
        timer = setTimeout(() => start(hold), INTENT_DELAY_MS);
      }
    },
    pointerLeave: () => {
      clearTimeout(timer);
      if (release !== null) timer = setTimeout(stop, RELEASE_DELAY_MS);
    },
    pointerDown: start,
    dispose: stop,
  };
}
