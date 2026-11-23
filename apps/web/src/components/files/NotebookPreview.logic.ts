/** Saved HTML outputs mounted beyond the visible ones, while fewer than this are mounted. */
export const NOTEBOOK_OUTPUT_PRELOAD_CAP = 16;

/**
 * The outputs to keep mounted: every visible one, then those already mounted inside the preload
 * margin, so an output at the viewport edge does not remount on every pass, then preloads.
 */
export function selectMountedNotebookOutputs<T>(
  inView: ReadonlySet<T>,
  nearby: ReadonlySet<T>,
  mounted: ReadonlySet<T>,
): Set<T> {
  const next = new Set(inView);
  for (const node of mounted) {
    if (nearby.has(node)) next.add(node);
  }
  for (const node of nearby) {
    if (next.size >= NOTEBOOK_OUTPUT_PRELOAD_CAP) break;
    next.add(node);
  }
  return next;
}
