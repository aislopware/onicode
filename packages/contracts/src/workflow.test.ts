import { describe, expect, it } from "@effect/vitest";
import * as Schema from "effect/Schema";

import { WorkflowRunsSubscribeInput, WorkflowRunsUpdate } from "./workflow.ts";

const decodeUpdate = Schema.decodeUnknownSync(WorkflowRunsUpdate);
const decodeInput = Schema.decodeUnknownSync(WorkflowRunsSubscribeInput);
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
