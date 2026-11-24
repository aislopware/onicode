// @effect-diagnostics nodeBuiltinImport:off - Update leases coordinate Node server processes.
import * as NodeCrypto from "node:crypto";
import * as NodeFSP from "node:fs/promises";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";
import * as KeyedLock from "@onicode/shared/KeyedLock";
import { lock } from "proper-lockfile";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as Ref from "effect/Ref";
import * as Schedule from "effect/Schedule";
import * as Schema from "effect/Schema";

export interface ProviderMaintenanceCommandCoordinatorShape<E> {
  readonly withCommandLock: <A, R>(input: {
    readonly targetKey: string;
    readonly lockKey: string;
    readonly onQueued?: Effect.Effect<void, E, R>;
    readonly onInterrupt?: (cancelled: boolean) => Effect.Effect<void, never, R>;
    readonly run: Effect.Effect<A, E, R>;
  }) => Effect.Effect<A, E, R>;
  /** Interrupts the target's command, queued or running, and waits for its finalizers. */
  readonly cancel: (targetKey: string) => Effect.Effect<void>;
}

interface RunningTarget {
  readonly fiber: Fiber.Fiber<unknown, unknown>;
  cancelled: boolean;
}

class MachineLockError extends Schema.TaggedError<MachineLockError>()("MachineLockError", {
  held: Schema.Boolean,
  cause: Schema.Defect(),
}) {}

// The updated CLI belongs to one OS user, so its servers share this lock whatever TMPDIR or
// Onicode home each was started with; a shared temp directory can belong to another user.
const lockDirectory = () => NodePath.join(NodeOS.homedir(), ".onicode", "provider-updates");

/** @internal */
export const machineLockFile = (lockKey: string) =>
  NodePath.join(
    lockDirectory(),
    NodeCrypto.createHash("sha256").update(lockKey).digest("hex").slice(0, 32),
  );

// Other Onicode servers on this machine can run the same installation's updater; one replacing its own
// files makes the other's spawn or capability check fail. Without a usable lock directory, run anyway.
const withMachineLock = <A, E, R>(
  lockKey: string,
  run: Effect.Effect<A, E, R>,
): Effect.Effect<A, E, R> => {
  const directory = lockDirectory();
  const file = machineLockFile(lockKey);
  const acquire = Effect.acquireRelease(
    Effect.tryPromise({
      try: async () => {
        await NodeFSP.mkdir(directory, { recursive: true });
        return await lock(file, {
          realpath: false,
          stale: 60_000,
          update: 10_000,
          onCompromised: () => {},
        });
      },
      catch: (cause) =>
        new MachineLockError({
          held: (cause as { code?: unknown } | null)?.code === "ELOCKED",
          cause,
        }),
    }),
    // A compromised lease rejects on release; the update has already finished by then.
    (release) => Effect.promise(() => release().catch(() => undefined)),
  ).pipe(
    Effect.retry({ while: (error) => error.held, schedule: Schedule.spaced("1 second") }),
    Effect.catch((error) =>
      Effect.logWarning("Could not lock provider update across servers", {
        lockKey,
        cause: String(error.cause),
      }),
    ),
  );
  return Effect.scoped(acquire.pipe(Effect.andThen(run)));
};

export const makeProviderMaintenanceCommandCoordinator = Effect.fn(
  "makeProviderMaintenanceCommandCoordinator",
)(function* <E>(input: { readonly makeAlreadyRunningError: (targetKey: string) => E }) {
  const runningTargetsRef = yield* Ref.make<ReadonlyMap<string, RunningTarget>>(new Map());
  const locks = yield* KeyedLock.make<string>();

  const acquireTarget = Effect.fn("acquireTarget")(function* (
    targetKey: string,
    fiber: Fiber.Fiber<unknown, unknown>,
  ) {
    return yield* Ref.modify(
      runningTargetsRef,
      (
        runningTargets,
      ): readonly [RunningTarget | undefined, ReadonlyMap<string, RunningTarget>] => {
        if (runningTargets.has(targetKey)) {
          return [undefined, runningTargets];
        }
        const entry: RunningTarget = { fiber, cancelled: false };
        return [entry, new Map(runningTargets).set(targetKey, entry)];
      },
    );
  });

  const releaseTarget = (targetKey: string, entry: RunningTarget) =>
    Ref.update(runningTargetsRef, (runningTargets) => {
      if (runningTargets.get(targetKey) !== entry) {
        return runningTargets;
      }
      const next = new Map(runningTargets);
      next.delete(targetKey);
      return next;
    });

  const withCommandLock: ProviderMaintenanceCommandCoordinatorShape<E>["withCommandLock"] =
    Effect.fn("providerMaintenanceCommandCoordinator.withCommandLock")(function* ({
      targetKey,
      lockKey,
      onQueued,
      onInterrupt,
      run,
    }) {
      return yield* Effect.uninterruptibleMask((restore) =>
        Effect.gen(function* () {
          const entry = yield* Effect.withFiber((fiber) => acquireTarget(targetKey, fiber));
          if (!entry) {
            return yield* Effect.fail(input.makeAlreadyRunningError(targetKey));
          }

          return yield* restore(
            (onQueued ?? Effect.void).pipe(
              Effect.andThen(locks.withLock(lockKey, withMachineLock(lockKey, run))),
            ),
          ).pipe(
            Effect.onInterrupt(() => (onInterrupt ? onInterrupt(entry.cancelled) : Effect.void)),
            Effect.ensuring(releaseTarget(targetKey, entry)),
          );
        }),
      );
    });

  const cancel = Effect.fn("providerMaintenanceCommandCoordinator.cancel")(function* (
    targetKey: string,
  ) {
    const entry = (yield* Ref.get(runningTargetsRef)).get(targetKey);
    if (entry === undefined) return;
    entry.cancelled = true;
    yield* Fiber.interrupt(entry.fiber);
  });

  return {
    withCommandLock,
    cancel,
  } satisfies ProviderMaintenanceCommandCoordinatorShape<E>;
});
