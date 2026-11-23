const RECHECK_MIN_MS = 30 * 60 * 1_000;
const RECHECK_MAX_MS = 24 * 60 * 60 * 1_000;
const RECHECK_IDLE_DIVISOR = 12;
const RECHECKS_PER_SWEEP = 4;

interface BranchRead {
  readonly readAtMs: number;
  readonly found: boolean;
  /** Read for an expired thread that retention keeps only while its branch has an open PR. */
  readonly retained: boolean;
  readonly failingSinceMs: number | null;
}

/**
 * Schedules a sweep's branch lookups, so their git work does not grow with the number of idle
 * threads. GitManager's caches cannot: they expire in minutes and evict past a few thousand branches.
 */
export function makeNoPullRequestRechecks() {
  const reads = new Map<string, BranchRead>();
  return {
    /**
     * The branches a scheduled sweep looks up now: every one that last had a pull request, and
     * the few longest unread that had none and are due. One idle a day is due every two hours.
     */
    select: (
      nowMs: number,
      branches: ReadonlyArray<{
        readonly key: string;
        readonly activityAtMs: number;
        readonly retained?: boolean;
      }>,
    ): ReadonlySet<string> => {
      const selected = new Set<string>();
      const due: Array<{ key: string; activityAtMs: number; retained: boolean; readAtMs: number }> =
        [];
      for (const branch of branches) {
        const read = reads.get(branch.key);
        if (read?.found === true) {
          selected.add(branch.key);
          continue;
        }
        // A thread that just expired settles on its next read, so an earlier read does not count.
        const retained = branch.retained === true;
        const readAtMs =
          read === undefined || (retained && !read.retained)
            ? Number.NEGATIVE_INFINITY
            : read.readAtMs;
        const delayMs = Math.min(
          Math.max((nowMs - branch.activityAtMs) / RECHECK_IDLE_DIVISOR, RECHECK_MIN_MS),
          RECHECK_MAX_MS,
        );
        if (nowMs - readAtMs < delayMs) continue;
        due.push({ key: branch.key, activityAtMs: branch.activityAtMs, retained, readAtMs });
      }
      due.sort((left, right) =>
        left.readAtMs === right.readAtMs
          ? right.activityAtMs - left.activityAtMs
          : left.readAtMs - right.readAtMs,
      );
      // Stamped before the lookup, so one that keeps failing cannot hold a place in every sweep.
      for (const { key, retained } of due.slice(0, RECHECKS_PER_SWEEP)) {
        selected.add(key);
        reads.set(key, {
          readAtMs: nowMs,
          found: false,
          retained,
          failingSinceMs: reads.get(key)?.failingSinceMs ?? null,
        });
      }
      return selected;
    },
    found: (key: string): boolean => reads.get(key)?.found === true,
    record: (key: string, nowMs: number, found: boolean, retained = false): void => {
      reads.set(key, { readAtMs: nowMs, found, retained, failingSinceMs: null });
    },
    succeeded: (key: string): void => {
      const read = reads.get(key);
      if (read?.failingSinceMs != null) reads.set(key, { ...read, failingSinceMs: null });
    },
    /** A forced refresh that keeps failing retries at doubling gaps, at most RECHECK_MIN_MS apart. */
    refreshDue: (key: string, nowMs: number): boolean => {
      const read = reads.get(key);
      if (read === undefined || read.failingSinceMs === null) return true;
      return nowMs - read.readAtMs >= Math.min(read.readAtMs - read.failingSinceMs, RECHECK_MIN_MS);
    },
    /** A failed lookup waits like one that found none; true once lookups have failed for a day. */
    failed: (key: string, nowMs: number, retained: boolean): boolean => {
      const failingSinceMs = reads.get(key)?.failingSinceMs ?? nowMs;
      reads.set(key, { readAtMs: nowMs, found: false, retained, failingSinceMs });
      return nowMs - failingSinceMs >= RECHECK_MAX_MS;
    },
    retain: (keys: { readonly has: (key: string) => boolean }): void => {
      for (const key of reads.keys()) {
        if (!keys.has(key)) reads.delete(key);
      }
    },
  };
}
