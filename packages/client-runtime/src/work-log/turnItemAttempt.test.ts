import {
  NodeId,
  RunAttemptId,
  RunId,
  ThreadId,
  TurnItemId,
  type OrchestrationV2TurnItem,
} from "@onicode/contracts";
import * as DateTime from "effect/DateTime";
import { describe, expect, it } from "vite-plus/test";

import { makeTurnItemAttemptResolver } from "./turnItemAttempt.ts";

const runId = RunId.make("run-1");
const rootNodeId = NodeId.make("node-root");
const retryRootNodeId = NodeId.make("node-retry-root");
const at = (second: number) => DateTime.makeUnsafe(Date.UTC(2026, 10, 19, 0, 0, second));

const attempt = (ordinal: number, root: NodeId, startedAt: number | null) => ({
  id: RunAttemptId.make(`attempt-${ordinal}`),
  runId,
  attemptOrdinal: ordinal,
  rootNodeId: root,
  startedAt: startedAt === null ? null : at(startedAt),
});

const command = (nodeId: string, startedAt: number): OrchestrationV2TurnItem => ({
  id: TurnItemId.make(`item-${nodeId}`),
  threadId: ThreadId.make("thread-1"),
  runId,
  nodeId: NodeId.make(nodeId),
  providerThreadId: null,
  providerTurnId: null,
  nativeItemRef: null,
  parentItemId: null,
  ordinal: startedAt,
  status: "completed",
  title: null,
  startedAt: at(startedAt),
  completedAt: at(startedAt),
  updatedAt: at(startedAt),
  type: "command_execution",
  input: "vp check",
  output: "ok",
  exitCode: 0,
});

describe("makeTurnItemAttemptResolver", () => {
  it("finds an item's attempt through its node, or by time when the node was not loaded", () => {
    const first = attempt(1, rootNodeId, 1);
    const retry = attempt(2, retryRootNodeId, 10);
    const resolve = makeTurnItemAttemptResolver(
      [retry, first],
      [{ id: NodeId.make("node-known"), rootNodeId: retryRootNodeId, parentNodeId: null }],
    );

    expect(resolve(command("node-known", 2))).toBe(retry);
    // A history page brought these items without their tool nodes.
    expect(resolve(command("node-older-tool", 5))).toBe(first);
    expect(resolve(command("node-later-tool", 12))).toBe(retry);
    expect(resolve({ ...command("node-other-run", 5), runId: RunId.make("run-2") })).toBe(
      undefined,
    );
  });
});
