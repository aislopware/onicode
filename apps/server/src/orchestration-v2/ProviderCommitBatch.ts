import * as Cause from "effect/Cause";
import * as Clock from "effect/Clock";
import * as Config from "effect/Config";
import * as Effect from "effect/Effect";
import * as Context from "effect/Context";
import * as Deferred from "effect/Deferred";
import * as Exit from "effect/Exit";
import * as Queue from "effect/Queue";
import * as Schema from "effect/Schema";
import * as Semaphore from "effect/Semaphore";
import * as Stream from "effect/Stream";

const MAX_EVENTS = 64;
const MAX_BYTES = 1024 * 1024;
// While another write waits, a shared transaction stops taking jobs after this long.
const MAX_HOLD_NANOS = 100_000_000n;
// A stream waiting this long ranks like a main thread, so subagents cannot starve.
const STARVATION_MILLIS = 30_000;
// Bounds snapshots held across stream queues; a full backlog makes streams wait to read more.
const MAX_QUEUED = 4 * MAX_EVENTS;
const encodeQueue = Schema.encodeEffect(Schema.fromJsonString(Schema.Unknown));

export const enabled = Config.Boolean("ONICODE_PROVIDER_COMMIT_BATCHING").pipe(
  Config.withDefault(true),
  Effect.orDie,
);

type Snapshot =
  | {
      readonly type: "node.updated";
      readonly node: { readonly id: string; readonly threadId: string };
    }
  | {
      readonly type: "message.updated";
      readonly message: { readonly id: string; readonly threadId: string };
    }
  | {
      readonly type: "turn_item.updated";
      readonly turnItem: { readonly id: string; readonly threadId: string };
    }
  | {
      readonly type: "plan.updated";
      readonly plan: { readonly id: string; readonly threadId: string; readonly kind: string };
    };
const isSnapshot = (event: { readonly type: string }): event is Snapshot =>
  event.type === "node.updated" ||
  event.type === "message.updated" ||
  event.type === "turn_item.updated" ||
  event.type === "plan.updated";
const snapshotEntity = (event: Snapshot) => {
  const entity =
    event.type === "node.updated"
      ? event.node
      : event.type === "message.updated"
        ? event.message
        : event.type === "turn_item.updated"
          ? event.turnItem
          : event.plan;
  return {
    key: `${event.type}:${entity.id}`,
    threadId: entity.threadId,
    // Step durations are derived from each todo list state in turn, so none may be skipped.
    combinable: event.type !== "plan.updated" || event.plan.kind !== "todo_list",
  };
};

/** Who a provider stream writes for; ranks its snapshots against other streams'. */
export interface StreamPriority {
  readonly threadId: string;
  /** A subagent or delegated task, which yields to main threads. */
  readonly delegated: boolean;
}

export class CommitBatchError extends Schema.TaggedError<CommitBatchError>()("CommitBatchError", {
  cause: Schema.Defect(),
}) {}
export const isCommitBatchError = Schema.is(CommitBatchError);

interface StreamState {
  readonly owner: { active: boolean };
  /** What the snapshot that stopped this stream failed with; its later snapshots fail with it. */
  failure: Exit.Exit<void, CommitBatchError> | undefined;
  readonly priority: StreamPriority | undefined;
  /** Queued snapshots not yet taken into a batch, in stream order. */
  readonly jobs: Array<Job>;
  unsettled: number;
  /** Jobs of this stream that a write, control or EOF elsewhere is waiting on. */
  awaited: number;
  servedAt: number;
  /** A control, failure or EOF of this stream waits for its queued snapshots. */
  finishing: boolean;
}
interface Job {
  readonly stream: StreamState;
  /** Still in its stream's queue, holding one of the backlog's `MAX_QUEUED` slots. */
  queued: boolean;
  readonly key: string;
  readonly combinable: boolean;
  readonly threadId: string;
  readonly queuedAt: number;
  readonly bytes: number;
  readonly execute: Effect.Effect<void, CommitBatchError>;
  readonly settle: (
    exit: Exit.Exit<void, CommitBatchError>,
    preserveFailure: boolean,
  ) => Effect.Effect<void>;
  /** Settles a job a later snapshot of the same entity replaced, with that snapshot's outcome. */
  readonly skip: (exit: Exit.Exit<void, CommitBatchError>) => Effect.Effect<void>;
  readonly cancel: Effect.Effect<void>;
  readonly settled: Effect.Effect<void>;
}
interface Reservation {
  readonly threadIds: ReadonlyArray<string> | undefined;
  readonly done: Deferred.Deferred<void>;
}
const overlaps = (
  threadIds: ReadonlyArray<string> | undefined,
  others: ReadonlyArray<string> | undefined,
) =>
  threadIds === undefined ||
  others === undefined ||
  threadIds.some((threadId) => others.includes(threadId));
interface Unit {
  readonly job: Job;
  readonly superseded: ReadonlyArray<Job>;
}
interface Batch {
  readonly units: ReadonlyArray<Unit>;
  readonly reason: "ready" | "limit";
  readonly dequeued: number;
}

/**
 * Snapshots carry an entity's full state, so an earlier one is dropped when a later one of the
 * same entity follows it in this stream with no first appearance of another entity in between.
 * Kept jobs stay in stream order, and entities first appear in the same order as before.
 */
function coalesce(jobs: ReadonlyArray<Job>): Array<Unit> {
  const firstSeen = new Map<string, number>();
  jobs.forEach((job, index) => {
    if (!firstSeen.has(job.key)) firstSeen.set(job.key, index);
  });
  const survivor: Array<number> = Array.from({ length: jobs.length }, (_, index) => index);
  const nextOfKey = new Map<string, number>();
  for (let index = jobs.length - 1; index >= 0; index--) {
    const job = jobs[index]!;
    const next = nextOfKey.get(job.key);
    nextOfKey.set(job.key, index);
    if (next === undefined || !job.combinable || !jobs[next]!.combinable) continue;
    let replaced = true;
    for (let between = index + 1; replaced && between < next; between++) {
      if (firstSeen.get(jobs[between]!.key)! > index) replaced = false;
    }
    if (replaced) survivor[index] = survivor[next]!;
  }
  const superseded = new Map<number, Array<Job>>();
  jobs.forEach((job, index) => {
    const kept = survivor[index]!;
    if (kept !== index) superseded.set(kept, [...(superseded.get(kept) ?? []), job]);
  });
  return jobs.flatMap((job, index) =>
    survivor[index] === index ? [{ job, superseded: superseded.get(index) ?? [] }] : [],
  );
}

const CurrentWorker = Context.Reference<number | undefined>(
  "onicode/ProviderCommitBatch/CurrentWorker",
  {
    defaultValue: () => undefined,
  },
);

export const make = Effect.fn("ProviderCommitBatch.make")(function* (
  commit: (work: Effect.Effect<void, CommitBatchError>) => Effect.Effect<void, CommitBatchError>,
  enabled: boolean,
  isolate: (
    work: Effect.Effect<void, CommitBatchError>,
  ) => Effect.Effect<void, CommitBatchError> = (work) => work,
  /** Whether a client has the thread open; its streams rank first. */
  isViewed: (threadId: string) => boolean = () => false,
) {
  const wake = yield* Queue.sliding<void>(1);
  const backlog = yield* Semaphore.make(MAX_QUEUED);
  const streams = new Set<StreamState>();
  const pending = new Set<Job>();
  // Writes in `barrier`; a snapshot for their threads that arrives later waits for them.
  const reservations = new Set<Reservation>();
  let closed = false;
  let servings = 0;
  // Writes in `barrier` that have not finished; they wait on the batch or the writer it holds.
  let waitingWriters = 0;
  const shutdown = Effect.gen(function* () {
    closed = true;
    yield* Queue.shutdown(wake);
    for (const job of pending) yield* job.cancel;
    yield* backlog.releaseAll;
  });
  yield* Effect.addFinalizer(() => shutdown);
  // Waits for snapshots and writes registered so far on `threadIds`; omitted, on every thread.
  // Streams holding those snapshots rank first meanwhile, so a busier stream cannot starve them.
  const earlierWork = (threadIds?: ReadonlyArray<string>) => {
    const jobs = Array.from(pending).filter((job) => overlaps(threadIds, [job.threadId]));
    const writes = Array.from(reservations)
      .filter((reservation) => overlaps(threadIds, reservation.threadIds))
      .map((reservation) => Deferred.await(reservation.done));
    if (jobs.length === 0) return Effect.all(writes, { discard: true });
    return Effect.acquireUseRelease(
      Effect.sync(() => {
        for (const job of jobs) job.stream.awaited++;
      }),
      () => Effect.all([...jobs.map((job) => job.settled), ...writes], { discard: true }),
      () =>
        Effect.sync(() => {
          for (const job of jobs) job.stream.awaited--;
        }),
    );
  };
  // These helpers run once per raw event; the shared transaction owns the tracing span.
  const enqueue = Effect.fnUntraced(function* <A, E, R>(
    effect: Effect.Effect<A, E, R>,
    stream: StreamState,
    entity: { readonly key: string; readonly threadId: string; readonly combinable: boolean },
    bytes: number,
    skipped: A,
  ) {
    const caller = yield* Effect.context<R>();
    const reply = yield* Deferred.make<A, E | CommitBatchError>();
    let ownFailure: Cause.Cause<E> | undefined;
    let result: A;
    const release = () => {
      if (pending.delete(job)) stream.unsettled--;
    };
    const job: Job = {
      stream,
      queued: false,
      key: entity.key,
      combinable: entity.combinable,
      threadId: entity.threadId,
      queuedAt: yield* Clock.currentTimeMillis,
      bytes,
      execute: effect.pipe(
        Effect.tapCause((cause) =>
          Effect.sync(() => {
            ownFailure = cause;
          }),
        ),
        Effect.mapError((cause) => new CommitBatchError({ cause })),
        Effect.provideContext(caller),
        Effect.tap((value) =>
          Effect.sync(() => {
            result = value;
          }),
        ),
        Effect.asVoid,
      ),
      settle: Effect.fnUntraced(function* (
        exit: Exit.Exit<void, CommitBatchError>,
        preserveFailure: boolean,
      ) {
        release();
        if (Exit.isFailure(exit)) {
          stream.owner.active = false;
          stream.failure ??= exit;
          yield* Deferred.failCause(
            reply,
            preserveFailure && ownFailure !== undefined ? ownFailure : exit.cause,
          );
        } else yield* Deferred.succeed(reply, result);
      }),
      skip: (exit) =>
        Exit.isFailure(exit)
          ? job.settle(exit, false)
          : Effect.suspend(() => {
              release();
              return Deferred.succeed(reply, skipped);
            }),
      cancel: Effect.suspend(() => {
        release();
        const dequeued = job.queued;
        job.queued = false;
        return Deferred.interrupt(reply).pipe(
          Effect.andThen(dequeued ? backlog.release(1) : Effect.void),
        );
      }),
      settled: Deferred.await(reply).pipe(Effect.exit, Effect.asVoid),
    };
    yield* Effect.uninterruptibleMask((restore) =>
      Effect.gen(function* () {
        if (closed) return yield* Deferred.interrupt(reply);
        const earlier = Array.from(reservations)
          .filter((reservation) => overlaps([job.threadId], reservation.threadIds))
          .map((reservation) => Deferred.await(reservation.done));
        // Pending before it waits, so a write registered meanwhile waits for it in turn.
        pending.add(job);
        stream.unsettled++;
        yield* restore(
          Effect.all(earlier, { discard: true }).pipe(Effect.andThen(backlog.take(1))),
        ).pipe(Effect.onError(() => job.cancel));
        if (closed) {
          yield* backlog.release(1);
          return yield* job.cancel;
        }
        job.queued = true;
        stream.jobs.push(job);
        yield* Queue.offer(wake, undefined);
      }),
    );
    return Deferred.await(reply);
  });
  const rank = (stream: StreamState, now: number) => {
    const tier =
      stream.priority === undefined
        ? 1
        : isViewed(stream.priority.threadId)
          ? 0
          : stream.priority.delegated
            ? 2
            : 1;
    const starved = now - stream.jobs[0]!.queuedAt >= STARVATION_MILLIS;
    return [
      stream.awaited > 0 ? -1 : starved ? Math.min(tier, 1) : tier,
      stream.finishing ? 0 : 1,
      stream.servedAt,
    ];
  };
  // Streams a waiter needs first, then viewed streams, main threads, then subagents; within a
  // tier, streams finishing a control first, then the stream served longest ago.
  const nextBatch = (now: number): Batch | undefined => {
    const ready = Array.from(streams, (stream) =>
      stream.jobs.length === 0 ? [] : [{ stream, rank: rank(stream, now) }],
    )
      .flat()
      .sort((left, right) => {
        for (let index = 0; index < left.rank.length; index++) {
          const order = left.rank[index]! - right.rank[index]!;
          if (order !== 0) return order;
        }
        return 0;
      });
    if (ready.length === 0) return undefined;
    const units: Array<Unit> = [];
    let bytes = 0;
    let dequeued = 0;
    let reason: Batch["reason"] = "ready";
    for (const { stream } of ready) {
      let take = 0;
      while (take < stream.jobs.length && take < MAX_EVENTS - units.length) {
        const next = stream.jobs[take]!.bytes;
        if (units.length + take > 0 && bytes + next > MAX_BYTES) break;
        bytes += next;
        take++;
      }
      if (take < stream.jobs.length) reason = "limit";
      if (take === 0) break;
      const taken = stream.jobs.splice(0, take);
      for (const job of taken) job.queued = false;
      dequeued += take;
      units.push(...coalesce(taken));
      stream.servedAt = ++servings;
      if (units.length >= MAX_EVENTS || bytes >= MAX_BYTES) break;
    }
    return { units, reason, dequeued };
  };
  // Commits a prefix of `units` in one transaction and returns how many it took.
  const commitPrefix = (units: ReadonlyArray<Unit>, reason: Batch["reason"]) =>
    Effect.uninterruptibleMask((restore) =>
      Effect.gen(function* () {
        const outcomes = new Map<Job, Exit.Exit<void, CommitBatchError>>();
        // Units of a stream that stopped; they settle once the transaction's outcome is known.
        const stopped = new Set<Unit>();
        let taken = 0;
        const work = Effect.gen(function* () {
          // A retried transaction runs this again from the first unit.
          outcomes.clear();
          stopped.clear();
          taken = 0;
          const startedAt = yield* Clock.monotonicTimeNanos;
          for (const unit of units) {
            const { job } = unit;
            if (
              taken > 0 &&
              waitingWriters > 0 &&
              (yield* Clock.monotonicTimeNanos) - startedAt >= MAX_HOLD_NANOS
            )
              return;
            taken++;
            if (!job.stream.owner.active) {
              stopped.add(unit);
              continue;
            }
            const outcome = yield* (units.length > 1 ? isolate(job.execute) : job.execute).pipe(
              Effect.exit,
            );
            outcomes.set(job, outcome);
            if (Exit.isFailure(outcome)) {
              // Its failure is recorded once settled, when the transaction's outcome is known.
              job.stream.owner.active = false;
              if (units.length === 1) return yield* Effect.failCause(outcome.cause);
            }
          }
        });
        const exit = yield* restore(
          commit(work).pipe(Effect.provideService(CurrentWorker, yield* Effect.fiberId)),
        ).pipe(Effect.exit);
        const committed = taken === 0 ? units : units.slice(0, taken);
        const jobs = committed.flatMap((unit) => [unit.job, ...unit.superseded]);
        yield* Effect.annotateCurrentSpan({
          "orchestration_v2.work_item_count": committed.length,
          "orchestration_v2.coalesced_work_item_count": jobs.length - committed.length,
          "orchestration_v2.deferred_work_item_count": units.length - committed.length,
          "orchestration_v2.consumer_count": new Set(jobs.map((job) => job.stream)).size,
          "orchestration_v2.raw_json_bytes": jobs.reduce((bytes, job) => bytes + job.bytes, 0),
          "orchestration_v2.batch_flush_reason": reason,
          "orchestration_v2.backlog_batching": true,
          "orchestration_v2.batch_wait_ms":
            (yield* Clock.currentTimeMillis) - Math.min(...jobs.map((job) => job.queuedAt)),
        });
        for (const unit of committed) {
          const { job, superseded } = unit;
          if (stopped.has(unit)) {
            // A superseded snapshot arrived before the failure, so its reply must carry it too.
            const failure = job.stream.failure;
            for (const each of [job, ...superseded])
              yield* failure === undefined ? each.cancel : each.skip(failure);
            continue;
          }
          const outcome = outcomes.get(job) ?? Exit.void;
          const final = Exit.isSuccess(exit) ? outcome : exit;
          yield* job.settle(
            final,
            Exit.isSuccess(exit) ||
              (Exit.isFailure(exit) &&
                Exit.isFailure(outcome) &&
                Cause.squash(exit.cause) === Cause.squash(outcome.cause)),
          );
          for (const replaced of superseded) yield* replaced.skip(final);
        }
        return committed.length;
      }),
    ).pipe(Effect.withSpan("orchestrationV2.ProviderCommitBatch.flush"));
  if (enabled) {
    yield* Effect.forever(
      Effect.gen(function* () {
        const batch = nextBatch(yield* Clock.currentTimeMillis);
        if (batch === undefined) return yield* Queue.take(wake);
        yield* backlog.release(batch.dequeued);
        let units = batch.units;
        while (units.length > 0) units = units.slice(yield* commitPrefix(units, batch.reason));
      }),
    ).pipe(Effect.ensuring(shutdown), Effect.forkScoped);
  }
  /**
   * Runs `effect` after snapshots and writes queued for `threadIds` commit, and before ones that
   * arrive later; omitted, every thread. Other threads' work never delays it.
   */
  const barrier = Effect.fnUntraced(function* <A, E, R>(
    effect: Effect.Effect<A, E, R>,
    threadIds?: ReadonlyArray<string>,
  ) {
    if (closed) return yield* Effect.interrupt;
    if (!enabled || (yield* CurrentWorker) === (yield* Effect.fiberId)) return yield* effect;
    return yield* Effect.uninterruptibleMask((restore) => {
      const reservation: Reservation = { threadIds, done: Deferred.makeUnsafe() };
      const earlier = earlierWork(threadIds);
      reservations.add(reservation);
      waitingWriters++;
      return restore(
        Effect.gen(function* () {
          yield* earlier;
          if (closed) return yield* Effect.interrupt;
          return yield* effect;
        }),
      ).pipe(
        Effect.ensuring(
          Effect.suspend(() => {
            waitingWriters--;
            reservations.delete(reservation);
            return Deferred.done(reservation.done, Exit.void);
          }),
        ),
      );
    });
  });
  const consume = Effect.fnUntraced(function* <A extends { readonly type: string }, E, R, E2, R2>(
    source: Stream.Stream<A, E, R>,
    process: (event: A) => Effect.Effect<void, E2, R2>,
    after: Effect.Effect<boolean, E2, R2>,
    canBatch: Effect.Effect<boolean, never, R2> = Effect.succeed(true),
    priority?: StreamPriority,
  ) {
    if (!enabled)
      return yield* source.pipe(
        Stream.mapEffect((event) => process(event).pipe(Effect.andThen(after))),
        Stream.takeUntil(Boolean),
        Stream.runDrain,
      );
    const stream: StreamState = {
      owner: { active: true },
      failure: undefined,
      priority,
      jobs: [],
      unsettled: 0,
      awaited: 0,
      servedAt: 0,
      finishing: false,
    };
    streams.add(stream);
    const signals = source.pipe(
      Stream.map((event) => ({ _tag: "Event" as const, event })),
      Stream.catchCause((cause) => Stream.succeed({ _tag: "Failure" as const, cause })),
      Stream.concat(Stream.succeed({ _tag: "End" as const })),
    );
    let resume: Effect.Effect<void> = Effect.void;
    // Its own thread and those it has queued snapshots for; its controls read them.
    const threadIds = new Set(priority === undefined ? [] : [priority.threadId]);
    // Replies run in order after this stream's earlier snapshots. EOF, failures and controls also
    // wait for the other work queued before them that they may read.
    yield* signals.pipe(
      Stream.mapEffect((signal) =>
        Effect.gen(function* () {
          yield* resume;
          const processed = yield* Deferred.make<void>();
          let reply: Effect.Effect<boolean, E | E2 | CommitBatchError, R2>;
          let check = false;
          const snapshot =
            signal._tag === "Event" && isSnapshot(signal.event) && (yield* canBatch)
              ? signal.event
              : undefined;
          if (snapshot !== undefined) {
            const bytes = Buffer.byteLength(yield* encodeQueue(snapshot));
            const entity = snapshotEntity(snapshot);
            threadIds.add(entity.threadId);
            reply = yield* enqueue(
              process(snapshot).pipe(Effect.as(false)),
              stream,
              entity,
              bytes,
              false,
            );
          } else {
            if (stream.unsettled > 0) stream.finishing = true;
            if (signal._tag !== "Event") {
              // Cleanup after a stream ends reads other threads' work, so let what they queued land.
              const end =
                signal._tag === "End" ? Effect.succeed(true) : Effect.failCause(signal.cause);
              reply = earlierWork().pipe(Effect.andThen(end));
            } else {
              const event = signal.event;
              const earlier =
                threadIds.size === 0 ? Effect.void : earlierWork(Array.from(threadIds));
              reply = earlier.pipe(
                Effect.andThen(
                  Effect.sync(() => {
                    stream.finishing = false;
                  }),
                ),
                Effect.andThen(process(event)),
                Effect.as(false),
              );
              check = true;
              resume = Deferred.await(processed);
            }
          }
          return { reply, processed, check };
        }),
      ),
      Stream.buffer({ capacity: MAX_EVENTS }),
      Stream.mapEffect(({ reply, processed, check }) =>
        reply.pipe(
          Effect.flatMap((end) =>
            end ? Effect.succeed(true) : check ? after : Effect.succeed(false),
          ),
          Effect.tap((stop) =>
            Effect.sync(() => {
              if (stop) stream.owner.active = false;
            }),
          ),
          Effect.ensuring(Deferred.succeed(processed, undefined)),
        ),
      ),
      Stream.takeUntil(Boolean),
      Stream.runDrain,
      Effect.ensuring(
        Effect.gen(function* () {
          stream.owner.active = false;
          streams.delete(stream);
          for (const job of stream.jobs.splice(0)) yield* job.cancel;
        }),
      ),
    );
  });
  return { barrier, consume };
});
