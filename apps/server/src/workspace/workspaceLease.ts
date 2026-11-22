import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import type * as FileSystem from "effect/FileSystem";
import type * as Path from "effect/Path";
import * as Semaphore from "effect/Semaphore";

const leases = new Map<string, { semaphore: Semaphore.Semaphore; users: number }>();

const holdLease = <A, E, R>(key: string, effect: Effect.Effect<A, E, R>): Effect.Effect<A, E, R> =>
  Effect.suspend(() => {
    const lease = leases.get(key) ?? { semaphore: Semaphore.makeUnsafe(1), users: 0 };
    leases.set(key, lease);
    lease.users++;
    return lease.semaphore.withPermit(effect).pipe(
      Effect.ensuring(
        Effect.sync(() => {
          lease.users--;
          if (lease.users === 0) leases.delete(key);
        }),
      ),
    );
  });

/** Keyed by the real path, so callers that name one checkout through a symlink share its lease. */
export const withWorkspaceLease = <A, E, R>(
  fileSystem: FileSystem.FileSystem,
  path: Path.Path,
  cwd: string,
  effect: Effect.Effect<A, E, R>,
): Effect.Effect<A, E, R> =>
  canonicalize(fileSystem, path, path.resolve(cwd)).pipe(
    Effect.flatMap((key) => holdLease(key, effect)),
  );

/**
 * Every lease in `cwds`, each once. They are taken in sorted order, so two callers that hold
 * several at once cannot each wait on one the other has; a caller holding one lease must not
 * ask for more.
 */
export const withWorkspaceLeases = <A, E, R>(
  fileSystem: FileSystem.FileSystem,
  path: Path.Path,
  cwds: Iterable<string>,
  effect: Effect.Effect<A, E, R>,
): Effect.Effect<A, E, R> =>
  Effect.forEach([...cwds], (cwd) => canonicalize(fileSystem, path, path.resolve(cwd))).pipe(
    Effect.flatMap((keys) =>
      [...new Set(keys)].toSorted().reduceRight((inner, key) => holdLease(key, inner), effect),
    ),
  );

// A restore or a ref switch rewrites files under its directory, so it must exclude provider
// startup in any overlapping workspace: symlink aliases, parents, and children. Kept apart
// from the non-reentrant withWorkspaceLease, which a restore's Git work takes. Reserve before
// leasing: a worktree turn start holds its reservation while it waits for its lease.
export type ReservationKind = "restore" | "checkout-switch" | "turn-start";

interface Reservation {
  readonly cwd: string;
  readonly kind: ReservationKind;
}

const reservations = new Set<Reservation>();
let reservationReleased = Deferred.makeUnsafe<void>();

// path.relative folds win32 drive-letter case and treats a POSIX backslash as
// part of a name, unlike a raw prefix check.
export const contains = (path: Path.Path, parent: string, child: string) => {
  const relative = path.relative(parent, child);
  return (
    relative === "" ||
    (!path.isAbsolute(relative) && relative !== ".." && !relative.startsWith(`..${path.sep}`))
  );
};

const conflicts = (path: Path.Path, a: Reservation, b: Reservation) =>
  (a.kind !== "turn-start" || b.kind !== "turn-start") &&
  (contains(path, a.cwd, b.cwd) || contains(path, b.cwd, a.cwd));

const reserve = (path: Path.Path, reservation: Reservation): Effect.Effect<void> =>
  Effect.suspend(() => {
    for (const held of reservations) {
      if (conflicts(path, held, reservation)) {
        return Deferred.await(reservationReleased).pipe(Effect.andThen(reserve(path, reservation)));
      }
    }
    reservations.add(reservation);
    return Effect.void;
  });

const release = (reservation: Reservation) =>
  Effect.sync(() => {
    if (!reservations.delete(reservation)) return;
    const released = reservationReleased;
    reservationReleased = Deferred.makeUnsafe<void>();
    Deferred.doneUnsafe(released, Effect.void);
  });

// A worktree removed by storage cleanup is recreated inside the reserved
// section, so resolve its nearest existing ancestor: a symlink alias above it
// must still canonicalize the same on both sides. Walks at most the path depth.
export const canonicalize = (
  fileSystem: FileSystem.FileSystem,
  path: Path.Path,
  target: string,
): Effect.Effect<string> =>
  fileSystem.realPath(target).pipe(
    Effect.catchReason("PlatformError", "NotFound", () => {
      const parent = path.dirname(target);
      return parent === target
        ? Effect.succeed(target)
        : canonicalize(fileSystem, path, parent).pipe(
            Effect.map((resolved) => path.join(resolved, path.basename(target))),
          );
    }),
    Effect.orElseSucceed(() => target),
  );

const withReservation =
  (kind: Reservation["kind"]) =>
  <A, E, R>(
    fileSystem: FileSystem.FileSystem,
    path: Path.Path,
    cwd: string,
    effect: Effect.Effect<A, E, R>,
  ): Effect.Effect<A, E, R> =>
    canonicalize(fileSystem, path, cwd).pipe(
      Effect.flatMap((canonical) => {
        const reservation: Reservation = { cwd: canonical, kind };
        return reserve(path, reservation).pipe(
          Effect.andThen(effect),
          Effect.ensuring(release(reservation)),
        );
      }),
    );

/** Exclusive over every overlapping workspace while checkpoint files are restored. */
export const withCheckpointRestoreReservation = withReservation("restore");

/** Exclusive over every overlapping workspace while a checkout changes its ref. */
export const withCheckoutSwitchReservation = withReservation("checkout-switch");

/**
 * As withCheckoutSwitchReservation, but runs `onBusy` with the kind of the reservation in the
 * way instead of waiting, so a caller that already holds a lease can take it without deadlocking.
 */
export const withCheckoutSwitchReservationOrElse = <A, E, R, A2, E2, R2>(
  fileSystem: FileSystem.FileSystem,
  path: Path.Path,
  cwd: string,
  effect: Effect.Effect<A, E, R>,
  onBusy: (held: ReservationKind) => Effect.Effect<A2, E2, R2>,
): Effect.Effect<A | A2, E | E2, R | R2> =>
  canonicalize(fileSystem, path, cwd).pipe(
    Effect.flatMap((canonical) => {
      const reservation: Reservation = { cwd: canonical, kind: "checkout-switch" };
      return Effect.suspend((): Effect.Effect<A | A2, E | E2, R | R2> => {
        for (const held of reservations) {
          if (conflicts(path, held, reservation)) return onBusy(held.kind);
        }
        reservations.add(reservation);
        return effect;
      }).pipe(Effect.ensuring(release(reservation)));
    }),
  );

/** Waits out overlapping restores and ref switches; turn startups never wait on each other. */
export const withTurnStartReservation = withReservation("turn-start");
