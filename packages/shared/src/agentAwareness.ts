import {
  isListedThreadLineage,
  isProviderNativeSubagentThread,
  type EnvironmentId,
  type OrchestrationV2ThreadShell,
  type Project,
  type ThreadId,
} from "@onicode/contracts";
import * as DateTime from "effect/DateTime";

import {
  backgroundWorkHoldsCompletion,
  monitorHoldsWakeCompletion,
} from "./orchestrationV2PendingBackgroundWork.ts";

export type AgentAwarenessPhase =
  | "starting"
  | "running"
  | "waiting_for_approval"
  | "waiting_for_input"
  | "completed"
  | "failed"
  | "stale";

/**
 * Turn items that can start or end a subagent's user-only wait: its secret or link card,
 * or a user message that moves past a link card. Filters that drop subagent transcript
 * updates must keep these, or the parent's "Input needed" never appears or never clears.
 */
export const turnItemCanChangeUserWait = (item: { readonly type: string }) =>
  item.type === "secret_request" || item.type === "link_request" || item.type === "user_message";

export interface AgentAwarenessState {
  readonly environmentId: EnvironmentId;
  readonly threadId: ThreadId;
  readonly projectTitle: string;
  readonly threadTitle: string;
  readonly phase: AgentAwarenessPhase;
  readonly headline: string;
  readonly detail?: string;
  /** A result someone has opened since it came in: it waits for no one, so nothing shows it. */
  readonly seen?: true;
  readonly modelTitle: string;
  readonly updatedAt: string;
  readonly deepLink: string;
}

function buildAgentAwarenessDeepLink(input: {
  readonly environmentId: EnvironmentId;
  readonly threadId: ThreadId;
}): string {
  return `/threads/${encodeURIComponent(input.environmentId)}/${encodeURIComponent(input.threadId)}`;
}

/** What a thread shown under another asks of the person. */
export type ChildThreadRequest = "approval" | "user_input";

/**
 * What a subagent or side chat asks that the listed thread above it shows, by the rule the thread
 * lists roll requests up with. The agent above answers a subagent's question itself, a provider's
 * own subagent asks on its parent, and once a subagent's run ends nothing waits on the answer.
 */
export function childThreadRequest(
  thread: Pick<
    OrchestrationV2ThreadShell,
    "activityRunStatus" | "creationSource" | "lineage" | "pendingRuntimeRequest" | "status"
  >,
): ChildThreadRequest | null {
  const request = thread.pendingRuntimeRequest;
  if (request === null || request.kind === "auth_refresh") return null;
  const asks = request.kind === "user_input" ? "user_input" : "approval";
  if (thread.lineage.sideChat === true) return asks;
  if (
    thread.lineage.relationshipToParent !== "subagent" ||
    isProviderNativeSubagentThread(thread) ||
    (thread.activityRunStatus ?? null) === null ||
    (asks === "user_input" && request.userOnly !== true)
  ) {
    return null;
  }
  return asks;
}

export interface ProjectThreadAwarenessV2Input {
  readonly environmentId: EnvironmentId;
  readonly project: Pick<Project, "title">;
  /** The most pressing thing a thread shown under this one asks. */
  readonly childRequest?: ChildThreadRequest | null;
  readonly thread: Pick<
    OrchestrationV2ThreadShell,
    | "activityRunStatus"
    | "id"
    | "lineage"
    | "lastVisitedAt"
    | "latestRunBackgroundWake"
    | "latestRunCompletedAt"
    | "latestRunStopRequested"
    | "modelSelection"
    | "pendingBackgroundTasks"
    | "pendingRuntimeRequest"
    | "settledOverride"
    | "status"
    | "title"
    | "updatedAt"
  >;
}

export function projectThreadAwarenessV2(
  input: ProjectThreadAwarenessV2Input,
): AgentAwarenessState | null {
  const { environmentId, project, thread } = input;
  if (!isListedThreadLineage(thread.lineage)) return null;
  const phase = resolveThreadAwarenessPhaseV2(thread, input.childRequest ?? null);
  if (phase === null) {
    return null;
  }
  if (phase === "completed" && thread.latestRunStopRequested === true) return null;
  // Resolve the phase first so live or waiting work stays visible after settlement.
  const runStatus = thread.activityRunStatus ?? thread.status;
  if (
    thread.settledOverride === "settled" &&
    (phase === "completed" ||
      phase === "failed" ||
      (phase === "running" && runStatus === "completed")) &&
    !backgroundWorkHoldsCompletion(thread.pendingBackgroundTasks ?? [])
  ) {
    return null;
  }
  // The thread lists call a result read by the same rule.
  const seen =
    (phase === "completed" || phase === "failed") &&
    thread.lastVisitedAt != null &&
    thread.latestRunCompletedAt != null &&
    DateTime.toEpochMillis(thread.lastVisitedAt) >=
      DateTime.toEpochMillis(thread.latestRunCompletedAt);
  const detail =
    phase === "completed"
      ? "Review the completed task."
      : phase === "failed"
        ? "The agent run failed."
        : undefined;
  return {
    environmentId,
    threadId: thread.id,
    projectTitle: project.title,
    threadTitle: thread.title,
    phase,
    headline: headlineForPhase(phase),
    ...(detail === undefined ? {} : { detail }),
    ...(seen ? { seen } : {}),
    modelTitle: thread.modelSelection.model,
    updatedAt: DateTime.formatIso(thread.updatedAt),
    deepLink: buildAgentAwarenessDeepLink({ environmentId, threadId: thread.id }),
  };
}

function resolveThreadAwarenessPhaseV2(
  thread: ProjectThreadAwarenessV2Input["thread"],
  childRequest: ChildThreadRequest | null,
): AgentAwarenessPhase | null {
  const own = thread.pendingRuntimeRequest;
  const asked = own !== null && own.kind !== "auth_refresh";
  if ((asked && own.kind !== "user_input") || childRequest === "approval") {
    return "waiting_for_approval";
  }
  if (asked || childRequest === "user_input") return "waiting_for_input";
  switch (thread.activityRunStatus ?? thread.status) {
    case "preparing":
    case "starting":
      return "starting";
    case "running":
    case "waiting":
      return "running";
    case "completed":
      return backgroundWorkHoldsCompletion(thread.pendingBackgroundTasks ?? []) ||
        monitorHoldsWakeCompletion(thread)
        ? "running"
        : "completed";
    case "failed":
      return "failed";
    case "idle":
    case "queued":
    case "interrupted":
    case "cancelled":
    case "rolled_back":
      return null;
  }
}

function headlineForPhase(phase: AgentAwarenessPhase): string {
  switch (phase) {
    case "starting":
      return "Starting agent";
    case "running":
      return "Agent is working";
    case "waiting_for_approval":
      return "Approval needed";
    case "waiting_for_input":
      return "Waiting for input";
    case "completed":
      return "Agent finished";
    case "failed":
      return "Agent failed";
    case "stale":
      return "Update delayed";
  }
}
