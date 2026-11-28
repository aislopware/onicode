import {
  type EnvironmentId,
  type MessageId,
  type ProviderInstanceId,
  type RuntimeRequestId,
} from "@onicode/contracts";
import { type RefObject, useState } from "react";
import { type ComposerSubmissionIntent } from "../../composer-logic";
import { type SessionPhase, type Thread } from "../../types";
import {
  createLocalDispatchSnapshot,
  hasServerAcknowledgedLocalDispatch,
  type LocalDispatchSnapshot,
} from "../ChatView.logic";

export function useLocalDispatchState(input: {
  environmentId: EnvironmentId;
  routeThreadKey: string;
  currentRouteThreadKeyRef: RefObject<string | null>;
  activeThread: Thread | undefined;
  activeLatestRun: Thread["latestRun"] | null;
  latestUserMessageId: MessageId | null;
  phase: SessionPhase;
  activePendingApproval: RuntimeRequestId | null;
  activePendingUserInput: RuntimeRequestId | null;
  threadError: string | null | undefined;
}) {
  const key = input.routeThreadKey;
  const [stored, setLocalDispatch] = useState<{
    key: string;
    snapshot: LocalDispatchSnapshot;
  } | null>(null);
  // ChatView stays mounted across threads, and a detached send still holds its own thread's setters.
  const localDispatch = stored?.key === key ? stored.snapshot : null;

  const resetLocalDispatch = () => {
    setLocalDispatch((current) => (current?.key === key ? null : current));
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
    providerInstanceId?: ProviderInstanceId;
  }) => {
    // A send that finishes after the user left its thread has no busy state on screen to show.
    if (input.currentRouteThreadKeyRef.current !== key) return;
    const preparingWorktree = Boolean(options?.preparingWorktree);
    setLocalDispatch((current) => {
      const active =
        serverAcknowledgedLocalDispatch || current?.key !== key ? null : current.snapshot;
      if (active) {
        const submissionIntent = options?.submissionIntent ?? active.submissionIntent;
        const providerInstanceId = options?.providerInstanceId ?? active.providerInstanceId;
        return active.preparingWorktree === preparingWorktree &&
          active.submissionIntent === submissionIntent &&
          active.providerInstanceId === providerInstanceId
          ? current
          : {
              key,
              snapshot: { ...active, preparingWorktree, submissionIntent, providerInstanceId },
            };
      }
      return {
        key,
        snapshot: createLocalDispatchSnapshot(input.activeThread, {
          ...options,
          environmentId: input.environmentId,
          latestUserMessageId: input.latestUserMessageId,
        }),
      };
    });
  };

  return {
    beginLocalDispatch,
    resetLocalDispatch,
    localDispatchStartedAt: activeLocalDispatch?.startedAt ?? null,
    isPreparingWorktree: activeLocalDispatch?.preparingWorktree ?? false,
    isSendBusy: activeLocalDispatch !== null,
    localDispatchProviderInstanceId: activeLocalDispatch?.providerInstanceId ?? null,
    backgroundSubmissionPending: activeLocalDispatch?.submissionIntent === "background",
  };
}
