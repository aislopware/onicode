import { RunId, ThreadId } from "@onicode/contracts";
import { assert, it } from "@effect/vitest";

import * as ThreadManagement from "../orchestration-v2/ThreadManagementService.ts";
import { interruptFailure } from "./WorkflowAgentThreads.ts";

const threadId = ThreadId.make("thread-1");
const runId = RunId.make("turn-1");

it.each([
  [new ThreadManagement.ThreadManagementRunNotFoundError({ threadId, runId }), true],
  [new ThreadManagement.ThreadManagementThreadNotInterruptibleError({ threadId, runId }), true],
  [new Error("database is locked"), undefined],
])("counts a stop refused with %s as final: %s", (cause, final) => {
  assert.strictEqual(interruptFailure(cause).final, final);
});
