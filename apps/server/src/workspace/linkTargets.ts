// @effect-diagnostics nodeBuiltinImport:off
import * as NodeFSP from "node:fs/promises";
import * as NodePath from "node:path";

import * as Duration from "effect/Duration";
import * as Effect from "effect/Effect";
import type * as Path from "effect/Path";

import { isWithinRoot } from "./KnownWorkspaces.ts";
import { networkPathReachedBy } from "./networkLinks.ts";

// A link to an unresponsive network share must not hold up the local entries beside it.
const LINK_TARGET_TIMEOUT = Duration.seconds(1);
// A timed-out stat cannot be cancelled and keeps a libuv worker (4 by default) busy, so cap
// target probes in flight to leave a worker for other fs work. Three, so links to healthy
// folders still resolve while two probes hang on a stalled mount.
export const MAX_PENDING_LINK_TARGET_PROBES = 3;
/** The abort reason of a caller whose timeout fired; a cancelled caller says nothing of the probe. */
export const LINK_PROBE_TIMED_OUT = "link-probe-timed-out";
/** A timed-out caller that only waited behind probes still expected to answer. */
export const LINK_PROBE_CROWDED = "crowded";

const isAtOrUnder = (target: string, root: string) =>
  target === root || target.startsWith(root.endsWith(NodePath.sep) ? root : root + NodePath.sep);

/**
 * Runs `probe` with at most `max` in flight, one per `target` (a link's first hop). A full pool
 * queues callers until their signal aborts: `undefined`, or `LINK_PROBE_CROWDED` on a timeout. A
 * probe that outlived a timeout answers `undefined` at once for its target and anything below it.
 */
export function makeLinkTargetProbe<A>(
  probe: (fullPath: string) => Promise<A>,
  max = MAX_PENDING_LINK_TARGET_PROBES,
) {
  type Entry = { readonly result: Promise<A | undefined>; stuck: boolean };
  const pending = new Map<string, Entry>();
  // First in, first woken for a free slot; a woken caller that starts no probe passes its wake
  // on. Callers queued for a target whose probe starts join it, and ones at or below a target
  // whose probe gets stuck answer at once; neither takes a slot.
  type Waiter = { readonly target: string; reason?: "slot" | "target"; wake?: () => void };
  const waiters: Waiter[] = [];
  const release = (waiter: Waiter, reason: "slot" | "target") => {
    waiter.reason = reason;
    waiter.wake?.();
  };
  const wakeNext = () => {
    const waiter = waiters.shift();
    if (waiter) release(waiter, "slot");
  };
  const releaseFor = (matches: (waiterTarget: string) => boolean) => {
    for (const waiter of waiters.filter((queued) => matches(queued.target))) {
      waiters.splice(waiters.indexOf(waiter), 1);
      release(waiter, "target");
    }
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
  const heldOnlyByStuck = () => pending.size >= max && heldByStuck();
  const behindStuck = (target: string) =>
    [...pending].some(([stuckTarget, entry]) => entry.stuck && isAtOrUnder(target, stuckTarget));
  const awaitProbe = (target: string, entry: Entry, signal: AbortSignal) =>
    untilAborted(entry.result, signal, () => {
      if (signal.reason !== LINK_PROBE_TIMED_OUT) return;
      entry.stuck = true;
      releaseFor((waiterTarget) => isAtOrUnder(waiterTarget, target));
      // Every queued caller then answers at once too; each woken one wakes the next.
      if (heldOnlyByStuck()) wakeNext();
    });
  return async (
    fullPath: string,
    signal: AbortSignal,
    target = fullPath,
  ): Promise<A | undefined | typeof LINK_PROBE_CROWDED> => {
    let woken = false;
    let queued = false;
    const passOn = () => {
      if (woken) wakeNext();
    };
    for (;;) {
      if (signal.aborted) {
        passOn();
        return queued &&
          signal.reason === LINK_PROBE_TIMED_OUT &&
          !heldOnlyByStuck() &&
          !behindStuck(target)
          ? LINK_PROBE_CROWDED
          : undefined;
      }
      const shared = pending.get(target);
      if (shared) {
        passOn();
        return shared.stuck ? undefined : awaitProbe(target, shared, signal);
      }
      if (behindStuck(target)) {
        passOn();
        return undefined;
      }
      if (pending.size < max) break;
      if (heldByStuck()) {
        passOn();
        return undefined;
      }
      queued = true;
      const wake = Promise.withResolvers<void>();
      const waiter: Waiter = { target, wake: wake.resolve };
      waiters.push(waiter);
      await untilAborted(wake.promise, signal, () => {
        const index = waiters.indexOf(waiter);
        if (index !== -1) waiters.splice(index, 1);
      });
      // A slot given before or after an abort is handed on once, by the next pass of the loop.
      woken = waiter.reason === "slot";
    }
    const entry: Entry = {
      result: probe(fullPath)
        .catch(() => undefined)
        .finally(() => {
          pending.delete(target);
          wakeNext();
        }),
      stuck: false,
    };
    pending.set(target, entry);
    releaseFor((waiterTarget) => waiterTarget === target);
    return awaitProbe(target, entry, signal);
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

// Links into one share key to the same target. Reading a link in a folder already read does not
// touch where it leads; any other path is its own key, since reading it may reach a stalled mount.
const firstHopTarget = (fullPath: string, link: boolean, signal: AbortSignal) =>
  link
    ? new Promise<string | undefined>((resolve) => {
        signal.addEventListener("abort", () => resolve(undefined), { once: true });
        void NodeFSP.readlink(fullPath).then(
          (target) => resolve(NodePath.resolve(NodePath.dirname(fullPath), target)),
          () => resolve(NodePath.resolve(fullPath)),
        );
      })
    : Promise.resolve(NodePath.resolve(fullPath));

interface LinkTargetOptions {
  /** A symlink listed in a folder already read, whose first hop can key the probe. */
  readonly link?: boolean;
  /** A target on a network share outside `root` resolves to null, before anything touches it. */
  readonly within?: { readonly path: Path.Path; readonly root: string };
}

// Undefined once aborted.
const reachesNetworkPath = (
  fullPath: string,
  within: LinkTargetOptions["within"],
  signal: AbortSignal,
) =>
  within === undefined
    ? Promise.resolve(false)
    : new Promise<boolean | undefined>((resolve) => {
        signal.addEventListener("abort", () => resolve(undefined), { once: true });
        const inside = isWithinRoot(within.path, fullPath, within.root);
        void networkPathReachedBy(
          within.path,
          inside ? within.path.relative(within.root, fullPath) : fullPath,
          inside ? { from: within.root, allowedRoot: within.root } : { allowedRoot: within.root },
        ).then(
          (reached) => resolve(reached !== null),
          () => resolve(true),
        );
      });

/**
 * Like `resolveLinkTarget`, but a caller whose time ran out while it only waited behind probes
 * still expected to answer gets `LINK_PROBE_CROWDED`: its target was never asked.
 */
export const resolveLinkTargetOrCrowded = (fullPath: string, options?: LinkTargetOptions) =>
  Effect.suspend(() => {
    const caller = new AbortController();
    // Every wait inside settles once the caller aborts, so the timeout can read the answer.
    const answer = reachesNetworkPath(fullPath, options?.within, caller.signal).then((refused) =>
      refused === undefined
        ? undefined
        : refused
          ? null
          : firstHopTarget(fullPath, options?.link === true, caller.signal).then((target) =>
              target === undefined || caller.signal.aborted
                ? undefined
                : probeLinkTarget(fullPath, caller.signal, target),
            ),
    );
    return Effect.promise(() => answer).pipe(
      Effect.timeoutOrElse({
        duration: LINK_TARGET_TIMEOUT,
        orElse: () =>
          Effect.promise(() => {
            caller.abort(LINK_PROBE_TIMED_OUT);
            return answer;
          }),
      }),
      Effect.onInterrupt(() => Effect.sync(() => caller.abort())),
    );
  });

/**
 * What `fullPath` resolves to through any links: `null` when it is missing or neither a file nor
 * a folder, `undefined` when no answer came in time.
 */
export const resolveLinkTarget = (fullPath: string, options?: LinkTargetOptions) =>
  resolveLinkTargetOrCrowded(fullPath, options).pipe(
    Effect.map((target) => (target === LINK_PROBE_CROWDED ? undefined : target)),
  );

// A crowded answer says nothing of the folder, so it queues again a bounded number of times.
const resolveUncrowded = (fullPath: string) =>
  Effect.repeat(resolveLinkTargetOrCrowded(fullPath), {
    while: (target) => target === LINK_PROBE_CROWDED,
    times: 2,
  }).pipe(Effect.map((target) => (target === LINK_PROBE_CROWDED ? undefined : target)));

/**
 * A folder's state and real path, from probes bounded like a link's: `undefined` when the folder
 * or its `.git` did not answer in time, as on a stalled mount.
 */
export const probeFolder = Effect.fn("linkTargets.probeFolder")(function* (folder: string) {
  const target = yield* resolveUncrowded(folder);
  if (target === undefined) return undefined;
  if (target === null) return { state: "missing" as const, realPath: folder };
  const git = yield* resolveUncrowded(NodePath.join(folder, ".git"));
  if (git === undefined) return undefined;
  return {
    state: git === null ? ("plain" as const) : ("git" as const),
    realPath: target.realPath ?? folder,
  };
});
