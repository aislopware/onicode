import type { OrchestrationV2ThreadProjection } from "@onicode/contracts";
import { assert, it } from "vite-plus/test";

import {
  contextBoundaryOrdinal,
  runContextBoundaryOrdinal,
  scheduledFreshRunOrdinals,
} from "./contextBoundary.ts";

type Projection = Pick<OrchestrationV2ThreadProjection, "runs" | "turnItems">;

it("reads turn items once however many fresh-session scheduled runs a thread has", () => {
  const scheduledRuns = 200;
  const clearOrdinal = 120;
  // A steer restarted this scheduled run, so the run now names the steer as its message.
  const steeredOrdinal = 150;
  const runs = Array.from({ length: scheduledRuns + 1 }, (_, index) => ({
    id: `run-${index + 1}`,
    ordinal: index + 1,
    status: "completed",
    startedAt: "2026-11-19T00:00:00.000Z",
    freshContext: true,
    userMessageId: index + 1 === steeredOrdinal ? "steer" : `message-${index + 1}`,
  }));
  const items = [
    ...runs.map((run) => ({
      type: "user_message",
      runId: run.id,
      ordinal: run.ordinal * 10,
      messageId: `message-${run.ordinal}`,
      ...(run.ordinal === clearOrdinal ? {} : { scheduledTaskId: "task-hourly" }),
    })),
    {
      type: "user_message",
      runId: `run-${steeredOrdinal}`,
      ordinal: steeredOrdinal * 10 + 1,
      messageId: "steer",
    },
  ];
  let itemReads = 0;
  const turnItems = new Proxy(items, {
    get(target, property, receiver) {
      if (typeof property === "string" && /^\d+$/.test(property)) itemReads += 1;
      return Reflect.get(target, property, receiver);
    },
  });
  const projection = { runs, turnItems } as unknown as Projection;

  assert.equal(contextBoundaryOrdinal(projection), clearOrdinal);
  assert.isAtMost(itemReads, items.length);

  itemReads = 0;
  const scheduledOrdinals = scheduledFreshRunOrdinals(projection);
  assert.equal(scheduledOrdinals.size, scheduledRuns);
  assert.isFalse(scheduledOrdinals.has(clearOrdinal));
  assert.isTrue(scheduledOrdinals.has(steeredOrdinal));
  assert.isAtMost(itemReads, items.length);

  // Most threads never start fresh, and their dispatches should not read turn items at all.
  itemReads = 0;
  const continuing = {
    runs: runs.map((run) => ({ ...run, freshContext: false })),
    turnItems,
  } as unknown as Projection;
  assert.equal(contextBoundaryOrdinal(continuing), 0);
  assert.equal(scheduledFreshRunOrdinals(continuing).size, 0);
  assert.equal(itemReads, 0);
});

it("ignores fresh runs that never reached a provider, also after a rollback rewound them", () => {
  const run = (ordinal: number, status: string, startedAt: string | null) => ({
    ordinal,
    status,
    startedAt,
    freshContext: true,
    userMessageId: `message-${ordinal}`,
  });
  const boundary = (runs: ReadonlyArray<ReturnType<typeof run>>) =>
    contextBoundaryOrdinal({ runs, turnItems: [] } as unknown as Projection);

  assert.equal(boundary([run(2, "cancelled", null)]), 0);
  assert.equal(boundary([run(2, "rolled_back", null)]), 0);
  assert.equal(boundary([run(2, "rolled_back", "2026-11-19T00:00:00.000Z")]), 2);
});

it("starts a scheduled fresh session's conversation, its wakes included, at the schedule's run", () => {
  const run = (ordinal: number, providerThreadId: string, freshContext = false) => ({
    id: `run-${ordinal}`,
    ordinal,
    status: "completed",
    startedAt: "2026-11-19T00:00:00.000Z",
    providerThreadId,
    userMessageId: `message-${ordinal}`,
    ...(freshContext ? { freshContext: true } : {}),
  });
  const runs = [
    run(1, "main"),
    run(2, "main"),
    run(3, "schedule", true),
    run(4, "main"),
    // A background task of the schedule's session settled and woke it.
    run(5, "schedule"),
  ];
  const turnItems = runs.map((source) => ({
    type: "user_message",
    runId: source.id,
    ordinal: source.ordinal * 10,
    messageId: source.userMessageId,
    ...(source.ordinal === 3 ? { scheduledTaskId: "task-hourly" } : {}),
  }));
  const projection = { runs, turnItems } as unknown as Projection;
  const boundaryAt = (ordinal: number) =>
    runContextBoundaryOrdinal(
      projection,
      projection.runs.find((source) => source.ordinal === ordinal),
    );

  assert.equal(boundaryAt(5), 3);
  assert.equal(boundaryAt(3), 3);
  // The main conversation continues past the schedule from its own start.
  assert.equal(boundaryAt(4), 0);
});
