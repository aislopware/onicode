import type { EnvironmentId } from "@onicode/contracts";

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
