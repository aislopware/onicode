import { describe, expect, it } from "@effect/vitest";
import * as Schema from "effect/Schema";

import { WorkflowRunDetail, WorkflowRunsSubscribeInput, WorkflowRunsUpdate } from "./workflow.ts";

const decodeUpdate = Schema.decodeUnknownSync(WorkflowRunsUpdate);
const decodeInput = Schema.decodeUnknownSync(WorkflowRunsSubscribeInput);
const encodeDetail = Schema.encodeSync(WorkflowRunDetail);
const decodeDetail = Schema.decodeUnknownSync(WorkflowRunDetail);
const encodeInput = Schema.encodeSync(WorkflowRunsSubscribeInput);
// The inputs of servers from before `diff` (no fields, or v0.25/v0.26 `{ changes?: boolean }`).
const decodeOlderInput = Schema.decodeUnknownSync(Schema.Struct({}));
const decodeLegacyChangesInput = Schema.decodeUnknownSync(
  Schema.Struct({ changes: Schema.optional(Schema.Boolean) }),
);

describe("workflows.subscribe across versions", () => {
  it("reads a whole list from a server that does not send changes", () => {
    const list = { workflows: [], runs: [], questions: [] };
    expect(decodeUpdate(list)).toEqual(list);
  });

  it("asks for diffs in a way older servers ignore and fall back to whole lists", () => {
    const encoded = encodeInput({ diff: true });
    expect(() => decodeOlderInput(encoded)).not.toThrow();
    expect(decodeLegacyChangesInput(encoded)).toEqual({});
  });

  it("accepts legacy changes: true from v0.25/v0.26 clients", () => {
    expect(decodeInput({ changes: true })).toEqual({ changes: true });
  });
});

// `WorkflowStep` as builds from before the `queue` kind decode it.
const OlderWorkflowStep = Schema.Struct({
  name: Schema.String,
  kind: Schema.Literals(["step", "wait", "agent", "ask", "sleep"]),
  status: Schema.Literals(["running", "completed", "failed"]),
  attempts: Schema.Number,
  detail: Schema.NullOr(Schema.String),
  threadId: Schema.NullOr(Schema.String),
  environmentId: Schema.NullOr(Schema.String),
  result: Schema.NullOr(Schema.Json),
  said: Schema.NullOr(Schema.String),
  startedAt: Schema.String,
  endedAt: Schema.NullOr(Schema.String),
});
const decodeOlderSteps = Schema.decodeUnknownSync(
  Schema.Struct({ steps: Schema.Array(OlderWorkflowStep) }),
);

describe("workflows.get across versions", () => {
  const run = {
    runId: "run-1",
    workflow: "triage",
    version: 1,
    title: "Triage",
    status: "running",
    detail: null,
    result: null,
    pauses: 0,
    retired: false,
    resumesAt: null,
    parentRunId: null,
    parentStep: null,
    startedAt: "2026-11-27T00:00:00.000Z",
    updatedAt: "2026-11-27T00:00:00.000Z",
    endedAt: null,
  } as const;
  const queued = {
    name: "build",
    kind: "queue",
    status: "running",
    attempts: 1,
    detail: "builders",
    threadId: null,
    environmentId: null,
    result: null,
    said: null,
    ahead: 2,
    startedAt: "2026-11-27T00:00:01.000Z",
    endedAt: null,
  } as const;
  const encoded = encodeDetail({ run, steps: [queued], references: [] });

  it("sends a queue step as a wait to older builds and as a queue step to newer ones", () => {
    expect(decodeOlderSteps(encoded).steps[0]?.kind).toBe("wait");
    expect(decodeDetail(encoded).steps).toEqual([queued]);
  });
});
