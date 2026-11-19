import { msg } from "gt-react";
import { thrownErrorMessage } from "@onicode/client-runtime/errors";
import { m } from "~/i18n/messages";
import type { ModelSelection, RuntimeMode, ScopedThreadRef } from "@onicode/contracts";
import { scopeThreadRef } from "@onicode/client-runtime/environment";
import type { StartThreadTurnInput } from "@onicode/client-runtime/operations";
import {
  type AtomCommandResult,
  isAtomCommandInterrupted,
  squashAtomCommandFailure,
} from "@onicode/client-runtime/state/runtime";
import { useRef } from "react";

import { newMessageId, newThreadId } from "~/lib/utils";
import { useComposerDraftStore } from "../../composerDraftStore";
import { useStableCallback } from "../../hooks/useStableCallback";
import { useRightPanelStore } from "../../rightPanelStore";
import { readThreadShell, waitForThreadShell } from "../../state/entities";
import { threadEnvironment } from "../../state/threads";
import { useAtomCommand } from "../../state/use-atom-command";
import { useOrchestrationCommand } from "../../state/use-orchestration-command";
import { stackedThreadToast, toastManager } from "../ui/toast";

export type SideChatMessage = Pick<
  StartThreadTurnInput["message"],
  "text" | "attachments" | "context"
> & { readonly titleSeed: string };

/** The thread a side chat forks and the parent composer's settings, read at Send. */
export interface SideChatSource {
  readonly threadRef: ScopedThreadRef;
  readonly runtimeMode: RuntimeMode;
  readonly modelSelection: ModelSelection | undefined;
}

/**
 * Starts side chats from `threadRef`, or offers none when it is null. `open` forks the
 * source thread's latest stable point, sends `message` there with the source's settings
 * (`threadRef` and `readSettings()` when none is given), and shows the tab. It resolves
 * false when the side chat did not start, or another one is still starting.
 */
export function useSideChat(input: {
  readonly threadRef: ScopedThreadRef | null;
  readonly readSettings: () => {
    readonly runtimeMode: RuntimeMode;
    readonly modelSelection: ModelSelection | undefined;
  };
  /** Reports a failed start on the thread it started from, which may no longer be on screen. */
  readonly onError: (message: string, threadRef: ScopedThreadRef) => void;
}) {
  const pendingRef = useRef(false);
  const forkSideChat = useAtomCommand(threadEnvironment.forkSideChat, { reportFailure: false });
  const deleteThread = useAtomCommand(threadEnvironment.delete, { reportFailure: false });
  const setThreadRuntimeMode = useOrchestrationCommand(threadEnvironment.setRuntimeMode, {
    reportFailure: false,
  });
  const startThreadTurn = useOrchestrationCommand(threadEnvironment.startTurn, {
    reportFailure: false,
  });
  const start = async (source: SideChatSource, message: SideChatMessage | undefined) => {
    const { threadRef, runtimeMode, modelSelection } = source;
    const reportFailure = (
      failure: Extract<AtomCommandResult<unknown, unknown>, { readonly _tag: "Failure" }>,
      fallback: string,
    ) => {
      if (isAtomCommandInterrupted(failure)) return;
      const error = squashAtomCommandFailure(failure);
      input.onError(thrownErrorMessage(error, m) ?? fallback, threadRef);
    };
    const targetThreadId = newThreadId();
    const fork = await forkSideChat({
      environmentId: threadRef.environmentId,
      // oxlint-disable-next-line onicode/no-untranslated-copy -- stored on the server as the thread title, shared by every viewer
      input: { sourceThreadId: threadRef.threadId, targetThreadId, title: "Side chat" },
    });
    if (fork._tag === "Failure") {
      reportFailure(fork, m(msg("Could not start a side chat.")));
      return false;
    }
    // Drops the empty side chat a failed start leaves behind; one that got its message stays.
    const discard = () =>
      void deleteThread({
        environmentId: threadRef.environmentId,
        input: { threadId: targetThreadId, onlyIfUnused: true },
      });
    const targetRef = scopeThreadRef(threadRef.environmentId, targetThreadId);
    if (!(await waitForThreadShell(targetRef))) {
      input.onError(
        m(msg("The side chat was created, but it did not reach this client.")),
        threadRef,
      );
      discard();
      return false;
    }
    const sideChat = readThreadShell(targetRef);
    if (sideChat === null) {
      input.onError(m(msg("The side chat is no longer available.")), threadRef);
      discard();
      return false;
    }
    if (runtimeMode !== sideChat.runtimeMode) {
      const mode = await setThreadRuntimeMode({
        environmentId: threadRef.environmentId,
        input: { threadId: targetThreadId, runtimeMode, createdAt: new Date().toISOString() },
      });
      if (mode._tag === "Failure") {
        reportFailure(mode, m(msg("Could not set the side chat's permission mode.")));
        discard();
        return false;
      }
    }
    // Send before showing the tab: closing a tab whose side chat has no run deletes it.
    if (message) {
      const { titleSeed, ...content } = message;
      const turn = await startThreadTurn({
        environmentId: threadRef.environmentId,
        input: {
          threadId: targetThreadId,
          message: { messageId: newMessageId(), role: "user", ...content },
          ...(modelSelection === undefined ? {} : { modelSelection }),
          titleSeed,
          runtimeMode,
          interactionMode: "default",
          createdAt: new Date().toISOString(),
        },
      });
      if (turn._tag === "Failure") {
        reportFailure(turn, m(msg("Could not send the side chat message.")));
        discard();
        return false;
      }
    } else if (modelSelection !== undefined) {
      useComposerDraftStore.getState().setModelSelection(targetRef, modelSelection);
    }
    useRightPanelStore.getState().openSideChat(threadRef, targetThreadId);
    return true;
  };
  const open = async (message?: SideChatMessage, sent?: SideChatSource) => {
    const source =
      sent ?? (input.threadRef ? { threadRef: input.threadRef, ...input.readSettings() } : null);
    if (!source) return false;
    if (pendingRef.current) {
      if (sent) {
        toastManager.add(
          stackedThreadToast({
            type: "warning",
            title: m(msg("Side chat not started")),
            description: m(msg("Another side chat is still starting. Send again once it opens.")),
          }),
        );
      }
      return false;
    }
    pendingRef.current = true;
    return start(source, message).finally(() => {
      pendingRef.current = false;
    });
  };
  return { available: input.threadRef !== null, open: useStableCallback(open) };
}
