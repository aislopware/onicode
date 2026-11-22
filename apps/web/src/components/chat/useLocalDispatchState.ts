import { type EnvironmentId, type MessageId, type RuntimeRequestId } from "@onicode/contracts";
import { useState } from "react";
import { type ComposerSubmissionIntent } from "../../composer-logic";
import { type SessionPhase, type Thread } from "../../types";
import {
  createLocalDispatchSnapshot,
  hasServerAcknowledgedLocalDispatch,
  type LocalDispatchSnapshot,
} from "../ChatView.logic";

export function useLocalDispatchState(input: {
  threadKey: string;
  environmentId: EnvironmentId;
  activeThread: Thread | undefined;
  activeLatestRun: Thread["latestRun"] | null;
  latestUserMessageId: MessageId | null;
  phase: SessionPhase;
  activePendingApproval: RuntimeRequestId | null;
  activePendingUserInput: RuntimeRequestId | null;
  threadError: string | null | undefined;
}) {
  const { threadKey } = input;
  // ChatView stays mounted across threads, and a detached send still holds its own thread's
  // setters, so each thread keeps its own snapshot.
  const [dispatches, setDispatches] = useState<ReadonlyMap<string, LocalDispatchSnapshot>>(
    () => new Map(),
  );
  const localDispatch = dispatches.get(threadKey) ?? null;

  const resetLocalDispatch = () => {
    setDispatches((current) => {
      if (!current.has(threadKey)) return current;
      const next = new Map(current);
      next.delete(threadKey);
      return next;
    });
  };

  const serverAcknowledgedLocalDispatch = hasServerAcknowledgedLocalDispatch({
    localDispatch,
    phase: input.phase,
    latestRun: input.activeLatestRun,
    latestUserMessageId: input.latestUserMessageId,
    runtime: input.activeThread?.runtime ?? null,
    hasPendingApproval: input.activePendingApproval !== null,
    hasPendingUserInput: input.activePendingUserInput !== null,
    threadError: input.threadError,
  });
  const activeLocalDispatch = serverAcknowledgedLocalDispatch ? null : localDispatch;
  const beginLocalDispatch = (options?: {
    preparingWorktree?: boolean;
    submissionIntent?: ComposerSubmissionIntent;
  }) => {
    const preparingWorktree = Boolean(options?.preparingWorktree);
    setDispatches((current) => {
      const active = serverAcknowledgedLocalDispatch ? undefined : current.get(threadKey);
      if (active) {
        const submissionIntent = options?.submissionIntent ?? active.submissionIntent;
        return active.preparingWorktree === preparingWorktree &&
          active.submissionIntent === submissionIntent
          ? current
          : new Map(current).set(threadKey, { ...active, preparingWorktree, submissionIntent });
      }
      return new Map(current).set(
        threadKey,
        createLocalDispatchSnapshot(input.activeThread, {
          ...options,
          environmentId: input.environmentId,
          latestUserMessageId: input.latestUserMessageId,
        }),
      );
    });
  };

  return {
    beginLocalDispatch,
    resetLocalDispatch,
    localDispatchStartedAt: activeLocalDispatch?.startedAt ?? null,
    isPreparingWorktree: activeLocalDispatch?.preparingWorktree ?? false,
    isSendBusy: activeLocalDispatch !== null,
    backgroundSubmissionPending: activeLocalDispatch?.submissionIntent === "background",
  };
}
