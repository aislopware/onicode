import type {
  OrchestrationV2ExecutionNode,
  OrchestrationV2RunAttempt,
  OrchestrationV2TurnItem,
} from "@onicode/contracts";
import * as DateTime from "effect/DateTime";

type AttemptRecord = Pick<
  OrchestrationV2RunAttempt,
  "id" | "runId" | "attemptOrdinal" | "rootNodeId" | "startedAt"
>;

/**
 * Finds the run attempt an item belongs to by walking its execution nodes. Older history pages
 * and bounded snapshots carry items without their leaf nodes, so an unknown node falls back to
 * the run's latest attempt that started by the item's start.
 */
export function makeTurnItemAttemptResolver<A extends AttemptRecord>(
  attempts: ReadonlyArray<A>,
  nodes: ReadonlyArray<Pick<OrchestrationV2ExecutionNode, "id" | "rootNodeId" | "parentNodeId">>,
): (item: OrchestrationV2TurnItem) => A | undefined {
  const attemptByRootNodeId = new Map(attempts.map((attempt) => [attempt.rootNodeId, attempt]));
  const nodeById = new Map(nodes.map((node) => [node.id, node]));
  const attemptsByRunId = new Map<string, A[]>();
  for (const attempt of attempts) {
    const runAttempts = attemptsByRunId.get(attempt.runId) ?? [];
    runAttempts.push(attempt);
    attemptsByRunId.set(attempt.runId, runAttempts);
  }
  for (const runAttempts of attemptsByRunId.values()) {
    runAttempts.sort((left, right) => left.attemptOrdinal - right.attemptOrdinal);
  }

  const attemptStartedBy = (item: OrchestrationV2TurnItem): A | undefined => {
    const runAttempts = item.runId === null ? undefined : attemptsByRunId.get(item.runId);
    if (runAttempts === undefined) return undefined;
    const itemStartedAt = DateTime.toEpochMillis(item.startedAt ?? item.updatedAt);
    return (
      runAttempts.findLast(
        (attempt) =>
          attempt.startedAt !== null && DateTime.toEpochMillis(attempt.startedAt) <= itemStartedAt,
      ) ?? runAttempts[0]
    );
  };

  return (item) => {
    if (item.nodeId === null || item.runId === null) return undefined;
    let nodeId: OrchestrationV2ExecutionNode["id"] | null = item.nodeId;
    const visited = new Set<OrchestrationV2ExecutionNode["id"]>();
    while (nodeId !== null && !visited.has(nodeId)) {
      visited.add(nodeId);
      const directAttempt = attemptByRootNodeId.get(nodeId);
      if (directAttempt?.runId === item.runId) return directAttempt;
      const node = nodeById.get(nodeId);
      if (node === undefined) return attemptStartedBy(item);
      const rootAttempt = attemptByRootNodeId.get(node.rootNodeId);
      if (rootAttempt?.runId === item.runId) return rootAttempt;
      nodeId = node.parentNodeId;
    }
    return undefined;
  };
}
