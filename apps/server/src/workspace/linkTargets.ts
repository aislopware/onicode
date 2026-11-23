// @effect-diagnostics nodeBuiltinImport:off
import * as NodeFSP from "node:fs/promises";

import * as Duration from "effect/Duration";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as Option from "effect/Option";

// A link to an unresponsive network share must not hold up the local entries beside it.
const LINK_TARGET_TIMEOUT = Duration.seconds(1);
// A timed-out stat cannot be cancelled and keeps a libuv worker (4 by default) busy, so cap
// target probes in flight to leave a worker for other fs work. Three, so links to healthy
// folders still resolve while two probes hang on a stalled mount.
export const MAX_PENDING_LINK_TARGET_PROBES = 3;
/** The abort reason of a caller whose timeout fired; a cancelled caller says nothing of the probe. */
export const LINK_PROBE_TIMED_OUT = "link-probe-timed-out";

/**
 * Runs `probe` with at most `max` in flight. Overlapping callers, as typing makes, share one
 * probe per path, queued callers included. A full pool makes callers wait for a slot until their
 * signal aborts, which answers `undefined`; a probe that outlived a caller's timeout (an abort
 * for `LINK_PROBE_TIMED_OUT`), or a pool held only by such probes, answers that at once.
 */
export function makeLinkTargetProbe<A>(
  probe: (fullPath: string) => Promise<A>,
  max = MAX_PENDING_LINK_TARGET_PROBES,
) {
  type Entry = { readonly result: Promise<A | undefined>; stuck: boolean };
  const pending = new Map<string, Entry>();
  // First in, first woken for a free slot; a woken caller that starts no probe passes its wake
  // on. Callers queued for a path whose probe starts join it instead.
  type Waiter = { readonly fullPath: string; reason?: "slot" | "joined"; wake?: () => void };
  const waiters: Waiter[] = [];
  const release = (waiter: Waiter, reason: "slot" | "joined") => {
    waiter.reason = reason;
    waiter.wake?.();
  };
  const wakeNext = () => {
    const waiter = waiters.shift();
    if (waiter) release(waiter, "slot");
  };
  const untilAborted = <B>(promise: Promise<B>, signal: AbortSignal, onAbort: () => void) =>
    new Promise<B | undefined>((resolve) => {
      const abort = () => {
        onAbort();
        resolve(undefined);
      };
      signal.addEventListener("abort", abort, { once: true });
      void promise.then((value) => {
        signal.removeEventListener("abort", abort);
        resolve(value);
      });
    });
  const heldByStuck = () => [...pending.values()].every((entry) => entry.stuck);
  const awaitProbe = (entry: Entry, signal: AbortSignal) =>
    untilAborted(entry.result, signal, () => {
      if (signal.reason !== LINK_PROBE_TIMED_OUT) return;
      entry.stuck = true;
      // Queued callers then answer at once too; each woken one wakes the next.
      if (pending.size >= max && heldByStuck()) wakeNext();
    });
  return async (fullPath: string, signal: AbortSignal): Promise<A | undefined> => {
    let woken = false;
    const passOn = () => {
      if (woken) wakeNext();
    };
    for (;;) {
      if (signal.aborted) {
        passOn();
        return undefined;
      }
      const shared = pending.get(fullPath);
      if (shared) {
        passOn();
        return shared.stuck ? undefined : awaitProbe(shared, signal);
      }
      if (pending.size < max) break;
      if (heldByStuck()) {
        passOn();
        return undefined;
      }
      const wake = Promise.withResolvers<void>();
      const waiter: Waiter = { fullPath, wake: wake.resolve };
      waiters.push(waiter);
      await untilAborted(wake.promise, signal, () => {
        const queued = waiters.indexOf(waiter);
        if (queued !== -1) waiters.splice(queued, 1);
      });
      // A slot given before or after an abort is handed on once, by the next pass of the loop.
      woken = waiter.reason === "slot";
    }
    const entry: Entry = {
      result: probe(fullPath)
        .catch(() => undefined)
        .finally(() => {
          pending.delete(fullPath);
          wakeNext();
        }),
      stuck: false,
    };
    pending.set(fullPath, entry);
    for (const waiter of waiters.filter((queued) => queued.fullPath === fullPath)) {
      waiters.splice(waiters.indexOf(waiter), 1);
      release(waiter, "joined");
    }
    return awaitProbe(entry, signal);
  };
}

interface LinkTarget {
  /** Undefined when only `realpath` failed, as on some network volumes; the kind still holds. */
  readonly realPath: string | undefined;
  readonly kind: "directory" | "file";
}

// One pool for the process: browsing, the Files list and every search index share libuv's workers.
const probeLinkTarget = makeLinkTargetProbe(async (fullPath): Promise<LinkTarget | null> => {
  const stats = await NodeFSP.stat(fullPath).catch(() => undefined);
  const kind = stats?.isDirectory() ? "directory" : stats?.isFile() ? "file" : undefined;
  if (kind === undefined) return null;
  return { realPath: await NodeFSP.realpath(fullPath).catch(() => undefined), kind };
});

/**
 * What `fullPath` resolves to through any links: `null` when it is missing or neither a file nor
 * a folder, `undefined` when no answer came in time.
 */
export const resolveLinkTarget = (fullPath: string) =>
  Effect.suspend(() => {
    const caller = new AbortController();
    return Effect.promise(() => probeLinkTarget(fullPath, caller.signal)).pipe(
      Effect.timeoutOption(LINK_TARGET_TIMEOUT),
      Effect.onExit((exit) =>
        Effect.sync(() =>
          caller.abort(
            Exit.isSuccess(exit) && Option.isNone(exit.value) ? LINK_PROBE_TIMED_OUT : undefined,
          ),
        ),
      ),
      Effect.map(Option.getOrUndefined),
    );
  });
