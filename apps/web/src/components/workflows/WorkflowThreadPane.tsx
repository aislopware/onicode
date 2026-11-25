import { scopedThreadKey } from "@onicode/client-runtime/environment";
import { selectEnvironmentQueryData } from "@onicode/client-runtime/state/runtime";
import { type EnvironmentId, type ScopedThreadRef } from "@onicode/contracts";
import { useNavigate } from "@tanstack/react-router";
import { useGT } from "gt-react";
import { useEffect } from "react";

import { ComposerHandleContext } from "../../composerHandleContext";
import type { SideChatParentPanel } from "../../lib/sideChatPane";
import { useQuietThreadShell } from "../../state/entities";
import { useEnvironmentQuerySelector } from "../../state/query";
import { useEnvironmentShellStatus } from "../../state/shell";
import { threadFamilies, useEnvironmentThread } from "../../state/threads";
import { linkedThreadRouteParams, linkedThreadSurface } from "../../linkedThreadSurface";
import {
  buildThreadRouteParams,
  isThreadRouteSnapshotAuthoritative,
  resolveThreadRouteRenderState,
} from "../../threadRoutes";
import ChatView from "../ChatView";
import { useOpenThreadFamily } from "../ThreadRouteView";
import { BackgroundThreadContext } from "../chat/backgroundThread";
import { closeThreadFindIn } from "../chat/threadFindStore";
import { LinkedThread } from "../chat/LinkedThreadPane";
import { useWorkflowThreadOpen } from "./useWorkflowThreadOpen";

/** Opens the thread as a full thread view; `linked` reads it through `viaEnvironmentId`. */
export function useOpenThreadPage(): (
  threadRef: ScopedThreadRef,
  viaEnvironmentId?: EnvironmentId,
) => void {
  const navigate = useNavigate();
  return (threadRef, viaEnvironmentId) =>
    void (viaEnvironmentId === undefined
      ? navigate({ to: "/$environmentId/$threadId", params: buildThreadRouteParams(threadRef) })
      : navigate({
          to: "/linked/$viaEnvironmentId/$linkedEnvironmentId/$linkedThreadId",
          params: linkedThreadRouteParams({ viaEnvironmentId, ...threadRef }),
        }));
}

/** What a transcript's tab is called: the step its agent did, or the thread's own title. */
export function WorkflowThreadTabLabel({ threadRef }: { readonly threadRef: ScopedThreadRef }) {
  const gt = useGT();
  const shell = useQuietThreadShell(threadRef);
  // The shell list leaves an idle step's thread out; its family read, one per open tab, has it.
  const family = useEnvironmentQuerySelector(
    shell === null ? threadFamilies.familyAtom(threadRef.environmentId, threadRef.threadId) : null,
    selectEnvironmentQueryData,
  );
  const row = shell ?? family?.threads.find((thread) => thread.id === threadRef.threadId);
  const step = row?.workflowRun?.step;
  return step === undefined
    ? (row?.title ?? gt("Agent", { $context: "fallback title of a coding agent's thread" }))
    : gt("{step} · transcript", { step, $context: "tab: the agent thread of a workflow step" });
}

/**
 * An agent step's thread beside its run: the same chat as anywhere, composer included, so a
 * person can read what the agent did and talk to it without leaving the run.
 */
export default function WorkflowThreadPane({
  viaEnvironmentId,
  threadRef,
}: {
  /** The environment running the workflow. */
  readonly viaEnvironmentId: EnvironmentId;
  readonly threadRef: ScopedThreadRef;
}) {
  const threadOpen = useWorkflowThreadOpen(viaEnvironmentId, threadRef.environmentId);
  if (threadOpen.open === "linked" && threadOpen.label !== null) {
    const surface = linkedThreadSurface({
      viaEnvironmentId,
      ...threadRef,
      label: threadOpen.label,
      title: null,
    });
    return <LinkedThread key={surface.id} panelRef={null} surface={surface} visible />;
  }
  return <ConnectedWorkflowThreadPane threadRef={threadRef} />;
}

function ConnectedWorkflowThreadPane({ threadRef }: { readonly threadRef: ScopedThreadRef }) {
  const gt = useGT();
  const openThreadPage = useOpenThreadPage();
  const threadKey = scopedThreadKey(threadRef);
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- find opened in this transcript closes with its tab, as a side chat's does with its pane
  useEffect(() => () => closeThreadFindIn(threadKey), [threadKey]);
  const shell = useQuietThreadShell(threadRef);
  const live = isThreadRouteSnapshotAuthoritative(
    useEnvironmentShellStatus(threadRef.environmentId),
  );
  // The shell list leaves an idle step thread out; its family brings the row back, and its own
  // detail says whether it is gone.
  useOpenThreadFamily(threadRef, live && shell === null);
  const detail = useEnvironmentThread(
    shell === null ? threadRef.environmentId : null,
    shell === null ? threadRef.threadId : null,
  );
  const renderState = resolveThreadRouteRenderState({
    bootstrapComplete: live,
    serverThreadExists: shell !== null,
    serverThreadDeleted: shell?.deletedAt != null,
    unlistedThreadDetail: detail,
    draftThreadExists: false,
  });
  const openFull = () => openThreadPage(threadRef);
  // The diff panel and the terminal drawer belong to the full thread view, so both lead there.
  const panel: SideChatParentPanel = {
    hostRef: threadRef,
    openToolDiff: openFull,
    runShellCommand: openFull,
  };
  return renderState !== "ready" ? (
    <p className="p-3 text-xs text-muted-foreground" role="status">
      {renderState === "missing"
        ? gt("This thread is no longer there.")
        : gt("Loading the thread…")}
    </p>
  ) : (
    // The attribute is how the embedded chat knows the keyboard is meant for it.
    <div data-side-chat-pane className="flex min-h-0 flex-1 flex-col">
      <BackgroundThreadContext value>
        <ComposerHandleContext value={null}>
          <ChatView
            environmentId={threadRef.environmentId}
            threadId={threadRef.threadId}
            routeKind="server"
            embedded={panel}
          />
        </ComposerHandleContext>
      </BackgroundThreadContext>
    </div>
  );
}
