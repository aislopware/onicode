import {
  type WorkflowCounts,
  WorkflowQuestion,
  type WorkflowRunReference,
  type WorkflowRunsSnapshot,
  type WorkflowRunsUpdate,
} from "@onicode/contracts";
import * as Context from "effect/Context";
import type * as Duration from "effect/Duration";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Schema from "effect/Schema";
import * as Stream from "effect/Stream";

import type * as WorkflowStore from "./WorkflowStore.ts";

export class WorkflowNotRegisteredError extends Schema.TaggedError<WorkflowNotRegisteredError>()(
  "WorkflowNotRegisteredError",
  { workflow: Schema.String, version: Schema.optional(Schema.Number) },
) {
  override get message(): string {
    return this.version === undefined
      ? `No workflow named ${this.workflow} is registered.`
      : `Version ${this.version} of workflow ${this.workflow} is not registered.`;
  }
}

export class WorkflowRunNotFoundError extends Schema.TaggedError<WorkflowRunNotFoundError>()(
  "WorkflowRunNotFoundError",
  { runId: Schema.String },
) {
  override get message(): string {
    return `Workflow run ${this.runId} was not found.`;
  }
}

export class WorkflowInputError extends Schema.TaggedError<WorkflowInputError>()(
  "WorkflowInputError",
  { problem: Schema.String },
) {
  override get message(): string {
    return this.problem;
  }
}

export { WorkflowQuestion };

export type WorkflowError =
  | WorkflowNotRegisteredError
  | WorkflowRunNotFoundError
  | WorkflowInputError;

export class Workflows extends Context.Service<
  Workflows,
  {
    /** Start a run. A second start with the same key is the same run. */
    readonly start: (
      workflow: string,
      input: unknown,
      options: {
        readonly idempotencyKey: string;
        /** The run that waits for this one. A run started with a key has one parent or none. */
        readonly parent?: WorkflowStore.RunParent;
        /** The version to run, when the caller holds a definition. Without it, the newest. */
        readonly version?: number;
      },
    ) => Effect.Effect<{ readonly runId: string }, WorkflowError>;
    /**
     * Say that something a run waits on happened. `delivered` woke a wait; `buffered` is kept for
     * a wait that has not opened yet, for `ttl` (seven days by default).
     */
    readonly signal: (
      address: WorkflowStore.SignalAddress,
      value: Schema.Json,
      options: { readonly id: string; readonly ttl?: Duration.Input },
    ) => Effect.Effect<"delivered" | "buffered", WorkflowInputError>;
    /**
     * Answer the question a run asks a person. One answer per time it is asked: a second, a
     * double click, wakes nothing and is not kept for later.
     */
    readonly answer: (
      runId: string,
      name: string,
      value: Schema.Json,
    ) => Effect.Effect<void, WorkflowInputError>;
    /** Run a paused run's failed step again. */
    readonly resume: (runId: string) => Effect.Effect<void, WorkflowError>;
    readonly cancel: (runId: string, reason?: string) => Effect.Effect<void, WorkflowError>;
    readonly get: (runId: string) => Effect.Effect<
      {
        readonly run: WorkflowStore.WorkflowRun;
        readonly steps: ReadonlyArray<WorkflowStore.WorkflowStep>;
        readonly references: ReadonlyArray<WorkflowRunReference>;
      },
      WorkflowRunNotFoundError | WorkflowInputError
    >;
    readonly list: (filter?: {
      readonly status?: WorkflowStore.WorkflowRunStatus;
    }) => Effect.Effect<ReadonlyArray<WorkflowStore.WorkflowRun>, WorkflowInputError>;
    /** The run once `done` holds for it. Looks now, then on every change to the run. */
    readonly waitFor: (
      runId: string,
      done: (run: WorkflowStore.WorkflowRun) => boolean,
    ) => Effect.Effect<WorkflowStore.WorkflowRun>;
    /** The questions runs are waiting on a person to answer, oldest first. */
    readonly questions: Effect.Effect<ReadonlyArray<WorkflowQuestion>, WorkflowInputError>;
    /** What each run that has not ended is a run of, however many: no list limit applies. */
    readonly unfinished: Effect.Effect<
      ReadonlyArray<{ readonly workflow: string; readonly version: number }>
    >;
    /** What workflows keep in the store under keys starting with `prefix`, for a person to see. */
    readonly stored: (
      prefix: string,
    ) => Effect.Effect<
      ReadonlyArray<{ readonly key: string; readonly value: Schema.Json }>,
      WorkflowInputError
    >;
    /** Set a key of the store from outside a run, as a person flipping a switch does. */
    readonly storeSet: (key: string, value: Schema.Json) => Effect.Effect<void, WorkflowInputError>;
    readonly storeDelete: (key: string) => Effect.Effect<void, WorkflowInputError>;
    /** Recent runs and open questions, now and again after every change. */
    readonly watch: Stream.Stream<WorkflowRunsSnapshot>;
    /** The same snapshot first, then only the runs each change wrote. */
    readonly watchChanges: Stream.Stream<WorkflowRunsUpdate>;
    /** How much is going on, now and again whenever a count changes. */
    readonly counts: Stream.Stream<WorkflowCounts>;
    /**
     * Deliver timeouts to waits past their deadline, drop expired mail, and resume paused runs
     * whose rest is over. Runs by itself every few seconds.
     */
    readonly sweep: Effect.Effect<void>;
  }
>()("onicode/workflows/Workflows") {}

const refuse = Effect.fn("Workflows.disabled")(function* () {
  return yield* new WorkflowInputError({
    problem:
      "This server defines no workflows yet. Add a file to the workflows folder of its Onicode home.",
  });
});

/** The service with nothing registered and no engine behind it. */
export const disabled = Workflows.of({
  start: refuse,
  signal: refuse,
  answer: refuse,
  resume: refuse,
  cancel: refuse,
  get: Effect.fn("Workflows.get")(function* (runId) {
    return yield* new WorkflowRunNotFoundError({ runId });
  }),
  list: Effect.fn("Workflows.list")(function* () {
    return yield* Effect.succeed<ReadonlyArray<WorkflowStore.WorkflowRun>>([]);
  }),
  waitFor: Effect.fn("Workflows.waitFor")(function* () {
    return yield* Effect.never;
  }),
  questions: Effect.succeed([]),
  unfinished: Effect.succeed([]),
  stored: Effect.fn("Workflows.stored")(function* () {
    return yield* Effect.succeed<ReadonlyArray<{ key: string; value: Schema.Json }>>([]);
  }),
  storeSet: refuse,
  storeDelete: refuse,
  watch: Stream.concat(Stream.make({ workflows: [], runs: [], questions: [] }), Stream.never),
  watchChanges: Stream.concat(
    Stream.make({ workflows: [], runs: [], questions: [] }),
    Stream.never,
  ),
  counts: Stream.concat(Stream.make({ needsYou: 0, active: 0, total: 0 }), Stream.never),
  sweep: Effect.void,
});
export const layerDisabled = Layer.succeed(Workflows, disabled);

export const OFF =
  'Workflows are off on this server. Turn them on in its Settings → Labs → Workflows, or without a desktop, set "workflowsEnabled": true in userdata/settings.json in its Onicode home (~/.onicode by default).';

const refuseOff = Effect.fn("Workflows.off")(function* () {
  return yield* new WorkflowInputError({ problem: OFF });
});

/** The service while workflows are off: every request is refused. */
export const off = Workflows.of({
  ...disabled,
  start: refuseOff,
  signal: refuseOff,
  answer: refuseOff,
  resume: refuseOff,
  cancel: refuseOff,
  get: refuseOff,
  list: refuseOff,
  questions: refuseOff(),
  stored: refuseOff,
  storeSet: refuseOff,
  storeDelete: refuseOff,
});
