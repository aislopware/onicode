import {
  type ChatAttachment,
  isCompactionCommandMessage,
  parseGoalCommand,
  type OrchestrationV2ProjectedTurnItem,
  type OrchestrationV2ProviderCapabilities,
  type ProviderInstanceId,
} from "@onicode/contracts";
import { copySorted } from "@onicode/shared/Array";
import { mFallback, msg } from "gt-i18n";
import type { MFunctionType } from "gt-i18n/types";

import { commandNeverSteers, compactionBlocksSteering } from "./composerDispatch.ts";
import type { ThreadStructure } from "./models.ts";

type Projection = ThreadStructure;
type Run = Projection["runs"][number];
type Message = Projection["messages"][number];
type ProviderSession = Projection["providerSessions"][number];

const ACTIVE_RUN_STATUSES = new Set<Run["status"]>(["preparing", "starting", "running", "waiting"]);
const MERGE_BACK_RUN_STATUSES = new Set<Run["status"]>(["waiting", "completed"]);
const MERGE_BACK_BLOCKING_RUN_STATUSES = new Set<Run["status"]>([
  "preparing",
  "starting",
  "running",
]);

export interface QueuedThreadRun {
  readonly run: Run;
  readonly text: string;
  readonly attachments: ReadonlyArray<ChatAttachment>;
  readonly messageId: Message["id"];
  readonly context?: Message["context"];
  /** Carries the attribution a sent message shows: automation or agent sender. */
  readonly message?: Message;
  /**
   * Held as a steer for the active turn while that turn compacts. A compaction that completes
   * sends it into the turn; one that fails or is stopped leaves it an ordinary queued message.
   */
  readonly awaitsCompaction: boolean;
  /** Whether the provider it runs on runs /goal itself; absent when unknown. */
  readonly nativeGoals?: boolean;
}

export interface ThreadQueueWorkflowState {
  readonly activeRun: Run | null;
  readonly queuedRuns: ReadonlyArray<QueuedThreadRun>;
  readonly isHeld: boolean;
  readonly canPromoteToSteer: boolean;
  /** The active run is a compaction, so promoting a queued message stops it instead of steering. */
  readonly promoteSkipsCompaction: boolean;
  readonly canSendNow: boolean;
  /** Whether the session's provider runs /goal itself; absent when its server does not say. */
  readonly nativeGoals?: ThreadNativeGoals;
}

export interface ThreadNativeGoals {
  readonly instanceId: ProviderInstanceId;
  readonly supported: boolean;
}

/**
 * Whether a message to `instanceId` runs /goal natively. Only the session's own provider is
 * known; a message switching providers leaves it to the server.
 */
export function nativeGoalsFor(
  nativeGoals: ThreadNativeGoals | undefined,
  instanceId: ProviderInstanceId,
): boolean | undefined {
  return nativeGoals !== undefined && nativeGoals.instanceId === instanceId
    ? nativeGoals.supported
    : undefined;
}

export function resolveActiveThreadRun(projection: Pick<Projection, "runs">): Run | null {
  return projection.runs.findLast((run) => ACTIVE_RUN_STATUSES.has(run.status)) ?? null;
}

/**
 * A successfully finished provider turn remains in `waiting` while its
 * checkpoint is captured. Keep that newest turn available for merge-back
 * instead of falling through to an older fully checkpointed run.
 */
export function resolveLatestMergeBackRun(projection: Projection): Run | null {
  const latestProviderFinishedRun = projection.runs.reduce<Run | null>(
    (latest, run) =>
      MERGE_BACK_RUN_STATUSES.has(run.status) && (latest === null || run.ordinal > latest.ordinal)
        ? run
        : latest,
    null,
  );
  if (latestProviderFinishedRun === null) return null;

  const hasNewerActiveRun = projection.runs.some(
    (run) =>
      run.ordinal > latestProviderFinishedRun.ordinal &&
      MERGE_BACK_BLOCKING_RUN_STATUSES.has(run.status),
  );
  return hasNewerActiveRun ? null : latestProviderFinishedRun;
}

function resolveThreadProviderSession(projection: Projection): ProviderSession | null {
  const activeRun = resolveActiveThreadRun(projection);
  const providerThreadId = activeRun?.providerThreadId ?? projection.thread.activeProviderThreadId;
  const activeProviderThread =
    providerThreadId === null
      ? null
      : (projection.providerThreads.find((thread) => thread.id === providerThreadId) ?? null);
  const attachedProviderThread =
    activeProviderThread ??
    projection.providerThreads.find(
      (thread) => thread.appThreadId === projection.thread.id && thread.providerSessionId !== null,
    ) ??
    null;
  const sessionId = attachedProviderThread?.providerSessionId ?? null;
  if (sessionId !== null) {
    return projection.providerSessions.find((session) => session.id === sessionId) ?? null;
  }
  return (
    projection.providerSessions.findLast(
      (session) => session.status !== "stopped" && session.status !== "error",
    ) ?? null
  );
}

export function threadSteersDuringCompaction(projection: Projection | null | undefined): boolean {
  if (projection == null) return false;
  return (
    resolveThreadProviderSession(projection)?.capabilities.turns.activeSteeringDuringCompaction ===
    true
  );
}

function supportsSteering(turns: OrchestrationV2ProviderCapabilities["turns"]): boolean {
  return (
    turns.supportsActiveSteering ||
    (turns.supportsInterrupt && turns.supportsSteeringByInterruptRestart)
  );
}

export function threadCanSteer(
  projection: Projection | null,
  input: {
    readonly compactCommandIsActive: boolean;
    readonly compactionRunning: boolean;
    readonly activeRunId: Run["id"] | null | undefined;
    /** The server holds a steer sent into an automatic compaction until it ends. */
    readonly steersAfterCompaction?: boolean;
  },
): boolean {
  const turns = projection && resolveThreadProviderSession(projection)?.capabilities.turns;
  // A steer submitted before the session attaches can wait for its provider turn.
  if (turns && !supportsSteering(turns)) return false;
  const run = projection?.runs.find((candidate) => candidate.id === input.activeRunId);
  // Its provider turn has ended and only the checkpoint is pending: nothing takes a steer.
  if (run?.status === "waiting") return false;
  return !compactionBlocksSteering({
    compactCommandIsActive: input.compactCommandIsActive,
    compactionRunning: input.compactionRunning,
    steersDuringCompaction:
      turns?.activeSteeringDuringCompaction === true || input.steersAfterCompaction === true,
    run,
    messages: projection?.messages,
  });
}

/** Whether the session's provider runs /goal itself; undefined when its server does not say. */
export function threadNativeGoals(
  projection: Projection | null | undefined,
): ThreadNativeGoals | undefined {
  const session = projection == null ? null : resolveThreadProviderSession(projection);
  const supported = session?.capabilities.turns.supportsNativeGoals;
  return session == null || supported === undefined
    ? undefined
    : { instanceId: session.providerInstanceId, supported };
}

export function threadSupportsProviderHandoff(projection: Projection | null | undefined): boolean {
  if (projection == null) return false;
  const session = resolveThreadProviderSession(projection);
  if (session !== null) {
    return session.capabilities.sessions.supportsProviderSwitchingViaHandoff;
  }
  if (resolveActiveThreadRun(projection) !== null) return false;
  if (projection.thread.historyOrigin === "v1_import" || projection.runs.length === 0) return true;

  // Detaching a stopped session removes it from the projection. The server still
  // hands off from saved app history, which needs the source provider thread but
  // not its native ref.
  return projection.providerThreads.some(
    (thread) =>
      thread.id === projection.thread.activeProviderThreadId &&
      thread.appThreadId === projection.thread.id &&
      thread.providerInstanceId === projection.thread.modelSelection.instanceId,
  );
}

export function getUserQueuedThreadRuns(
  projection: Pick<Projection, "runs" | "messages">,
): ReadonlyArray<Run> {
  if (!projection.runs.some((run) => run.status === "queued")) return [];

  const automaticCompletionMessageIds = new Set(
    projection.messages
      .filter(
        (message) =>
          message.delegatedCompletion !== undefined || message.notification !== undefined,
      )
      .map((message) => message.id),
  );
  return projection.runs.filter(
    (run) => run.status === "queued" && !automaticCompletionMessageIds.has(run.userMessageId),
  );
}

/** Delivery order: the queue position the server set, else the order sent. */
export function compareQueuedRuns(
  left: Pick<Run, "queuePosition" | "ordinal">,
  right: Pick<Run, "queuePosition" | "ordinal">,
): number {
  return (
    (left.queuePosition ?? left.ordinal) - (right.queuePosition ?? right.ordinal) ||
    left.ordinal - right.ordinal
  );
}

export function deriveThreadQueueWorkflowState(
  projection: Projection,
  resolve: MFunctionType = mFallback,
): ThreadQueueWorkflowState {
  const activeRun = resolveActiveThreadRun(projection);
  const session = resolveThreadProviderSession(projection);
  const capabilities = session?.capabilities.turns;
  const canSteer = capabilities !== undefined && supportsSteering(capabilities);
  const activeMessage = projection.messages.find(
    (message) => message.id === activeRun?.userMessageId,
  );
  // Waiting for its checkpoint, the compaction is over.
  const promoteSkipsCompaction =
    activeRun?.status !== "waiting" &&
    activeMessage !== undefined &&
    isCompactionCommandMessage(activeMessage);
  const hasSteerableProviderTurn =
    activeRun?.status === "running" &&
    activeRun.activeAttemptId !== null &&
    projection.providerTurns.some(
      (turn) => turn.runAttemptId === activeRun.activeAttemptId && turn.status === "running",
    );
  const nativeGoals =
    session === null || capabilities?.supportsNativeGoals === undefined
      ? undefined
      : { instanceId: session.providerInstanceId, supported: capabilities.supportsNativeGoals };
  const queuedRuns = copySorted(getUserQueuedThreadRuns(projection), compareQueuedRuns).map(
    (run) => {
      const message = projection.messages.find((candidate) => candidate.id === run.userMessageId);
      // A message that named no model runs on the thread's pick when it starts, as the server has it.
      const runNativeGoals = nativeGoalsFor(
        nativeGoals,
        message?.followsThreadSelection === true
          ? projection.thread.modelSelection.instanceId
          : run.providerInstanceId,
      );
      return {
        ...(runNativeGoals === undefined ? {} : { nativeGoals: runNativeGoals }),
        run,
        // The server keeps the field after the turn or its compaction ends, so the running
        // compaction decides, not the field alone.
        awaitsCompaction:
          run.steerWhenReadyRunId !== undefined &&
          activeRun?.status === "running" &&
          run.steerWhenReadyRunId === activeRun.id &&
          projection.turnItems.some(
            (item) =>
              item.type === "compaction" &&
              item.runId === activeRun.id &&
              item.status === "running",
          ),
        text: message?.text ?? resolve(msg("Queued message")),
        attachments: message?.attachments ?? [],
        messageId: run.userMessageId,
        ...(message?.context ? { context: message.context } : {}),
        ...(message ? { message } : {}),
      };
    },
  );

  return {
    activeRun,
    queuedRuns,
    isHeld: projection.runs.some((run) => run.status === "queued" && run.queueHeld === true),
    canSendNow:
      activeRun === null &&
      projection.thread.archivedAt == null &&
      projection.thread.deletedAt == null,
    canPromoteToSteer:
      promoteSkipsCompaction ||
      ((activeRun?.status === "preparing" || activeRun?.status === "starting") &&
        (capabilities === undefined || canSteer)) ||
      (hasSteerableProviderTurn && canSteer),
    promoteSkipsCompaction,
    ...(nativeGoals === undefined ? {} : { nativeGoals }),
  };
}

/** A fresh message, or one asking for a result, needs its own turn: it may skip compaction only. */
export function canPromoteQueuedRun(
  workflow: Pick<ThreadQueueWorkflowState, "canPromoteToSteer" | "promoteSkipsCompaction">,
  run: Pick<Run, "freshContext" | "outputSchema">,
): boolean {
  return (
    workflow.canPromoteToSteer &&
    (workflow.promoteSkipsCompaction ||
      (run.freshContext !== true && run.outputSchema === undefined))
  );
}

export type QueuedRunSendNow =
  | { readonly type: "start" }
  | { readonly type: "steer"; readonly targetRunId: Run["id"] }
  | { readonly type: "move-first"; readonly beforeRunId: Run["id"] };

/**
 * What a queued row's Send now does, or null when it can't. While a turn runs, a command that
 * never steers is only moved to the front, which needs no steering capability either; nor does
 * /goal on a provider that runs it, which the server moves first, stopping the turn if it must.
 */
export function resolveQueuedRunSendNow(
  workflow: ThreadQueueWorkflowState,
  runId: Run["id"],
  /** The text just saved for it, which the projection may not show yet. */
  saved?: { readonly text: string; readonly attachments: ReadonlyArray<unknown> },
): QueuedRunSendNow | null {
  const queued = workflow.queuedRuns.find(({ run }) => run.id === runId);
  if (queued === undefined) return null;
  if (workflow.canSendNow) return { type: "start" };
  if (workflow.activeRun === null) return null;
  const message = saved ?? {
    text: queued.message?.text ?? queued.text,
    attachments: queued.attachments,
  };
  const { nativeGoals } = queued;
  if (!workflow.promoteSkipsCompaction && commandNeverSteers(message, nativeGoals)) {
    const first = workflow.queuedRuns[0]!.run.id;
    return first === runId ? null : { type: "move-first", beforeRunId: first };
  }
  const nativeGoal =
    !workflow.promoteSkipsCompaction && nativeGoals === true && parseGoalCommand(message) !== null;
  return canPromoteQueuedRun(
    nativeGoal ? { ...workflow, canPromoteToSteer: true } : workflow,
    queued.run,
  )
    ? { type: "steer", targetRunId: workflow.activeRun.id }
    : null;
}

export function canForkProjectedAssistantItem(input: {
  readonly projectedItem: OrchestrationV2ProjectedTurnItem;
  readonly capabilities?: OrchestrationV2ProviderCapabilities | undefined;
}): boolean {
  const item = input.projectedItem.item;
  if (item.type !== "assistant_message" || item.runId === null || item.status !== "completed") {
    return false;
  }
  if (input.capabilities === undefined) {
    // Historical and inherited rows may outlive their provider-session record.
    // Keep the portable server-side fallback available when capability evidence
    // is absent; a known incapable provider is rejected below.
    return true;
  }
  const capabilities = input.capabilities;
  const canForkNatively =
    capabilities.threads.canForkThread &&
    capabilities.threads.canForkFromTurn &&
    capabilities.identity.nativeThreadIds === "strong";
  return canForkNatively || capabilities.context.supportsFullThreadHandoff;
}

export function canDetachThreadProviderSession(projection: Projection): boolean {
  const session = resolveThreadProviderSession(projection);
  return session !== null && session.status !== "stopped" && session.status !== "error";
}
