import { expect, it } from "@effect/vitest";
import * as Schema from "effect/Schema";

import { WorkflowToolkit } from "./tools.ts";

it("keeps exact wait names, store keys and channels", () => {
  const decode = Schema.decodeUnknownSync(WorkflowToolkit.tools.oni_workflow.parametersSchema);
  expect(decode({ action: "signal", runId: "run-1", wait: "review " }).wait).toBe("review ");
  expect(() => decode({ action: "signal", runId: "run-1", wait: " " })).toThrow();
  expect(decode({ action: "store", key: "team " }).key).toBe("team ");
  expect(() => decode({ action: "store_set", key: " ", value: 1 })).toThrow();
  expect(decode({ action: "signal", workflow: "w", channel: "review " }).channel).toBe("review ");
  expect(() => decode({ action: "signal", workflow: "w", channel: " " })).toThrow();
});

it("shows agents a queue step as a queue step, not the wire's wait", () => {
  const encode = Schema.encodeSync(WorkflowToolkit.tools.oni_workflow.successSchema);
  const step = {
    name: "pool reviewers",
    kind: "queue",
    status: "running",
    attempts: 1,
    detail: null,
    threadId: null,
    environmentId: null,
    result: null,
    said: null,
    ahead: 2,
    startedAt: "2026-11-27T00:00:00.000Z",
    endedAt: null,
  } as const;
  expect(encode({ steps: [step] }).steps).toEqual([step]);
});
