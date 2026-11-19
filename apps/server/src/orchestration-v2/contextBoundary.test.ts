import type { OrchestrationV2ThreadProjection } from "@onicode/contracts";
import { assert, it } from "vite-plus/test";

import { contextBoundaryOrdinal } from "./contextBoundary.ts";

type Projection = Pick<OrchestrationV2ThreadProjection, "runs" | "turnItems">;

it("reads turn items once however many fresh-session scheduled runs a thread has", () => {
  const scheduledRuns = 200;
  const clearOrdinal = 120;
  const runs = Array.from({ length: scheduledRuns + 1 }, (_, index) => ({
    ordinal: index + 1,
    status: "completed",
    startedAt: "2026-11-19T00:00:00.000Z",
    freshContext: true,
    userMessageId: `message-${index + 1}`,
  }));
  const items = runs.map((run) => ({
    type: "user_message",
    messageId: run.userMessageId,
    ...(run.ordinal === clearOrdinal ? {} : { scheduledTaskId: "task-hourly" }),
  }));
  let itemReads = 0;
  const turnItems = new Proxy(items, {
    get(target, property, receiver) {
      if (typeof property === "string" && /^\d+$/.test(property)) itemReads += 1;
      return Reflect.get(target, property, receiver);
    },
  });

  const boundary = contextBoundaryOrdinal({ runs, turnItems } as unknown as Projection);

  assert.equal(boundary, clearOrdinal);
  assert.isAtMost(itemReads, items.length);
});
