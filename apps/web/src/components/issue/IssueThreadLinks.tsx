import { useAtomValue } from "@effect/atom-react";
import { squashAtomCommandFailure } from "@onicode/client-runtime/state/runtime";
import { visibleLinkedThreads } from "@onicode/client-runtime/state/threads";
import { AuthOrchestrationOperateScope, threadIssueKeysEqual } from "@onicode/contracts";
import type {
  EnvironmentId,
  IssueDetail,
  IssueRef,
  ScopedThreadRef,
  ThreadId,
} from "@onicode/contracts";
import { Link2Icon, Link2OffIcon, MessageSquareIcon } from "lucide-react";
import { msg, T, useGT } from "gt-react";
import { useState } from "react";

import { openCommandPalette } from "~/commandPaletteBus";
import { m } from "~/i18n/messages";
import { appAtomRegistry } from "~/rpc/atomRegistry";
import {
  useAnyEnvironmentHiding,
  useEnvironmentThreadRefs,
  useServerConfigs,
  useThreadShell,
} from "~/state/entities";
import { issueEnvironment } from "~/state/issues";
import { useEnvironmentQuery } from "~/state/query";
import { useEnvironmentScope } from "~/state/session";
import { environmentThreadShells, threadEnvironment } from "~/state/threads";
import { useAtomCommand } from "~/state/use-atom-command";

import { readableFailure } from "../pullRequest/pullRequestDetail.logic";
import { ThreadLinkPicker } from "../ThreadLinkPicker";
import { Button } from "../ui/button";
import { Dialog, DialogPopup, DialogTitle } from "../ui/dialog";
import { toastManager } from "../ui/toast";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../ui/tooltip";

/**
 * The threads an issue is linked to, and the control that links it to one more: the thread it is
 * shown beside, or one picked from the environment when it is shown on its own.
 */
export function IssueThreadLinks({
  environmentId,
  reference,
  issue,
  threadRef,
}: {
  environmentId: EnvironmentId;
  reference: IssueRef & { readonly host: string };
  issue: Pick<IssueDetail, "url" | "displayKey" | "title">;
  threadRef: ScopedThreadRef | null;
}) {
  const gt = useGT();
  const thread = useThreadShell(threadRef?.environmentId === environmentId ? threadRef : null);
  const canOperate = useEnvironmentScope(environmentId, AuthOrchestrationOperateScope);
  const listsThreads =
    useServerConfigs().get(environmentId)?.environment.capabilities.issueLinkedThreads === true;
  const link = useAtomCommand(threadEnvironment.linkIssue, { reportFailure: false });
  const unlink = useAtomCommand(threadEnvironment.unlinkIssue, { reportFailure: false });
  const hiddenKeys = useAtomValue(environmentThreadShells.hiddenThreadKeysAtom);
  const hidingAnywhere = useAnyEnvironmentHiding();
  const loadedThreadRefs = useEnvironmentThreadRefs(environmentId);
  const key = {
    tracker: reference.tracker,
    host: reference.host,
    scope: reference.scope,
    key: reference.key,
  };
  const relationsAtom = issueEnvironment.linkedThreads({
    environmentId,
    input: { ...key, url: issue.url },
  });
  const relations = useEnvironmentQuery(listsThreads ? relationsAtom : null);
  // Retain the last count while refresh temporarily clears query data.
  const [lastRelations, setLastRelations] = useState(relations.data);
  if (relations.data !== null && relations.data !== lastRelations) {
    setLastRelations(relations.data);
  }
  const [pending, setPending] = useState(false);
  const [pickerOpen, setPickerOpen] = useState(false);

  const hereLink = thread?.issues?.find((existing) => threadIssueKeysEqual(existing, key));
  const linkedHere = hereLink !== undefined;
  const changeLink = async (threadId: ThreadId, remove: boolean) => {
    if (pending) return;
    setPending(true);
    // The stored spelling: older servers match an unlink's key case-sensitively.
    const unlinkKey = hereLink === undefined ? key : { ...key, key: hereLink.key };
    const result = await (remove
      ? unlink({ environmentId, input: { threadId, ...unlinkKey } })
      : link({
          environmentId,
          input: {
            threadId,
            ...key,
            projectId: reference.projectId,
            url: issue.url,
            displayKey: issue.displayKey,
            title: issue.title,
            source: "manual",
          },
        }));
    setPending(false);
    if (result._tag === "Failure") {
      toastManager.add({
        type: "error",
        title: remove ? m(msg("Could not unlink the issue")) : m(msg("Could not link the issue")),
        description: readableFailure(
          squashAtomCommandFailure(result),
          m(msg("The server refused it.")),
        ),
      });
      return;
    }
    if (listsThreads) appAtomRegistry.refresh(relationsAtom);
    setPickerOpen(false);
  };

  const linkedThreads = visibleLinkedThreads(
    (relations.data ?? lastRelations)?.threads ?? [],
    environmentId,
    loadedThreadRefs,
    hiddenKeys,
    hidingAnywhere,
  );
  const linkedThreadsLabel =
    linkedThreads.length > 0
      ? gt(
          "{count, plural, one {Linked from {count} thread} other {Linked from {count} threads}}",
          { count: linkedThreads.length },
        )
      : gt("Linked threads");
  // A first read that failed shows "?", which reads again: thread search does not index issue links.
  const relationsKnown = (relations.data ?? lastRelations) !== null;
  const readFailed = !relationsKnown && relations.error !== null;
  return (
    <>
      {readFailed ? (
        <Tooltip>
          <TooltipTrigger
            render={
              <Button
                size="xs"
                variant="ghost"
                aria-label={gt("Could not load linked threads. Click to try again.")}
                onClick={relations.refresh}
              >
                <MessageSquareIcon aria-hidden className="size-3.5" />
                <span aria-hidden>?</span>
              </Button>
            }
          />
          <TooltipPopup>{gt("Could not load linked threads. Click to try again.")}</TooltipPopup>
        </Tooltip>
      ) : linkedThreads.length > 0 ? (
        <Tooltip>
          <TooltipTrigger
            render={
              <Button
                size="xs"
                variant="ghost"
                aria-label={linkedThreadsLabel}
                onClick={() =>
                  openCommandPalette({
                    query: issue.url,
                    linkedThreads: { environmentId, threads: linkedThreads },
                  })
                }
              >
                <MessageSquareIcon aria-hidden className="size-3.5" />
                <span aria-hidden>{linkedThreads.length}</span>
              </Button>
            }
          />
          <TooltipPopup>
            {gt("{label}. Search in the command palette.", { label: linkedThreadsLabel })}
          </TooltipPopup>
        </Tooltip>
      ) : null}
      {canOperate ? (
        <Tooltip>
          <TooltipTrigger
            render={
              <Button
                size="icon-xs"
                variant="ghost"
                aria-label={linkedHere ? gt("Unlink from thread") : gt("Link to thread")}
                aria-pressed={thread === null ? undefined : linkedHere}
                disabled={pending}
                onClick={() => {
                  if (thread === null) setPickerOpen(true);
                  else void changeLink(thread.id, linkedHere);
                }}
              />
            }
          >
            {linkedHere ? (
              <Link2OffIcon aria-hidden className="size-3.5" />
            ) : (
              <Link2Icon aria-hidden className="size-3.5" />
            )}
          </TooltipTrigger>
          <TooltipPopup>
            {linkedHere ? (
              <T>Unlink from this thread</T>
            ) : thread === null ? (
              gt("Link to thread")
            ) : (
              <T>Link to this thread</T>
            )}
          </TooltipPopup>
        </Tooltip>
      ) : null}
      {pickerOpen ? (
        <Dialog open onOpenChange={setPickerOpen}>
          <DialogPopup className="max-w-md" showCloseButton={false}>
            <DialogTitle className="sr-only">
              <T>Link issue to a thread</T>
            </DialogTitle>
            <ThreadLinkPicker
              environmentId={environmentId}
              pending={pending}
              isLinked={(candidate) =>
                (candidate.issues ?? []).some((existing) => threadIssueKeysEqual(existing, key))
              }
              onSelect={(threadId) => void changeLink(threadId, false)}
            />
          </DialogPopup>
        </Dialog>
      ) : null}
    </>
  );
}
