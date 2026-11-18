import * as DateTime from "effect/DateTime";
import { describe, expect, it } from "vite-plus/test";

import { nextQueuedRun, queuedRunsInDeliveryOrder } from "./QueuedRunOrder.ts";

const at = (time: string) => DateTime.makeUnsafe(`2026-01-01T${time}Z`);
const delegatedCompletion = {
  generation: 1,
  parentRunId: "run:parent",
  taskIds: ["task:child"],
};

describe("queued run delivery order", () => {
  it("delivers completions and queued messages in request order, keeping manual queue order", () => {
    const projection = {
      messages: [
        { id: "message:old" },
        { id: "message:moved" },
        { id: "message:earlier-completion", delegatedCompletion },
        { id: "message:later-completion", delegatedCompletion },
      ],
      runs: [
        {
          id: "run:old",
          ordinal: 3,
          queuePosition: 2,
          requestedAt: at("10:00:00"),
          status: "queued",
          userMessageId: "message:old",
        },
        {
          id: "run:moved",
          ordinal: 4,
          queuePosition: 1,
          requestedAt: at("10:02:00"),
          status: "queued",
          userMessageId: "message:moved",
        },
        {
          id: "run:earlier-completion",
          ordinal: 2,
          queuePosition: 3,
          requestedAt: at("09:59:00"),
          status: "queued",
          userMessageId: "message:earlier-completion",
        },
        {
          id: "run:later-completion",
          ordinal: 5,
          queuePosition: 4,
          requestedAt: at("10:03:00"),
          status: "queued",
          userMessageId: "message:later-completion",
        },
      ],
    } as never;

    expect(queuedRunsInDeliveryOrder(projection).map((run) => run.id)).toEqual([
      "run:earlier-completion",
      "run:moved",
      "run:old",
      "run:later-completion",
    ]);
  });

  it.each([
    { label: "skips a held run", first: { queueHeld: true }, next: "run:second" },
    // Started before its release, a steer would run without the follow-up task the release binds.
    {
      label: "waits for a steer that is not released yet",
      first: { steerWhenReadyRunId: "run:ended" },
      next: undefined,
    },
    {
      label: "skips a held steer the user must resume",
      first: { queueHeld: true, steerWhenReadyRunId: "run:ended" },
      next: "run:second",
    },
  ])("starts the next run: $label", ({ first, next }) => {
    const queued = (id: string, ordinal: number, fields: object) => ({
      id: `run:${id}`,
      ordinal,
      requestedAt: at("10:00:00"),
      status: "queued",
      userMessageId: `message:${id}`,
      ...fields,
    });
    const projection = {
      messages: [{ id: "message:first" }, { id: "message:second" }],
      runs: [queued("first", 1, first), queued("second", 2, {})],
    } as never;

    expect(nextQueuedRun(projection)?.id).toBe(next);
  });
});
