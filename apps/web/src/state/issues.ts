import {
  createIssueEnvironmentAtoms,
  createMergedIssueCandidatesAtoms,
  createMergedIssueListAtoms,
  type IssueCandidatesTarget,
  type IssueListTarget,
  type MergedIssueList,
} from "@onicode/client-runtime/state/issues";
import { useAtomValue } from "@effect/atom-react";
import { issueLinkWorkspace } from "@onicode/client-runtime/issue-links";
import { selectEnvironmentQueryData } from "@onicode/client-runtime/state/runtime";
import type { EnvironmentId, IssueRef, IssueSummary, ThreadIssueLink } from "@onicode/contracts";
import { Atom } from "effect/reactivity";
import { useMessages } from "gt-react";

import { connectionAtomRuntime } from "../connection/runtime";
import { appAtomRegistry } from "../rpc/atomRegistry";
import { useServerConfigs } from "./entities";
import { formatEnvironmentQueryError, useEnvironmentQuery } from "./query";
import { pullRequestEnvironment } from "./pullRequests";

export type { IssueListTarget };

export const issueEnvironment = createIssueEnvironmentAtoms(
  connectionAtomRuntime,
  pullRequestEnvironment.linkChanges,
);

type IssueReadScope = "sidebar" | "thread-details" | "linked-items" | "sidebar-tooltip";

/**
 * An issue's live summary, for rows and icons that show its state rather than its conversation.
 * A server without `issues.summary` serves the detail instead, which carries the same fields.
 */
export function useIssueSummary(
  environmentId: EnvironmentId | null,
  input: IssueRef | null,
  mountScope?: IssueReadScope,
): IssueSummary | null {
  const configs = useServerConfigs();
  const capabilities =
    environmentId === null || input === null
      ? undefined
      : configs.get(environmentId)?.environment.capabilities;
  const target =
    environmentId === null || input === null || capabilities?.issues !== true
      ? null
      : { environmentId, input };
  const summary = useEnvironmentQuery(
    target !== null && capabilities?.issueSummaries === true
      ? issueEnvironment.summary(target, mountScope)
      : null,
  ).data;
  const detail = useEnvironmentQuery(
    target !== null && capabilities?.issueSummaries !== true
      ? issueEnvironment.detail(target, mountScope)
      : null,
  ).data;
  return summary ?? detail;
}

/** A link's reference, read in the workspace it kept, or for older links the one its URL names. */
function linkedIssueRef(link: ThreadIssueLink): IssueRef {
  const workspace = link.workspace ?? issueLinkWorkspace(link.url);
  return {
    projectId: link.projectId,
    tracker: link.tracker,
    host: link.host,
    scope: link.scope,
    key: link.key,
    ...(workspace === undefined ? {} : { workspace }),
  };
}

/**
 * A linked issue's live row. A URL can name a renamed workspace's old key, which costs the
 * server a lookup.
 */
export function useLinkedIssueSummary(
  environmentId: EnvironmentId,
  link: ThreadIssueLink | undefined,
  mountScope?: IssueReadScope,
) {
  return useIssueSummary(
    environmentId,
    link === undefined ? null : linkedIssueRef(link),
    mountScope,
  );
}

const emptySidebarIssue = Atom.make<IssueSummary | null>(null);

// A visible row reads each counted issue, so a thread linking dozens costs dozens of reads.
const SIDEBAR_GROUP_STATUS_LINKS = 10;

/** The summary a sidebar row shows for its first linked issues: an open one first, else a closed one. */
export function useSidebarLinkedIssue(
  environmentId: EnvironmentId,
  links: ReadonlyArray<ThreadIssueLink> | undefined,
) {
  const configs = useServerConfigs();
  const capabilities = configs.get(environmentId)?.environment.capabilities;
  // Without summaries a group would read every issue's full detail just to pick an icon.
  const readable =
    capabilities?.issues === true && (capabilities.issueSummaries === true || links?.length === 1);
  const aggregate =
    links?.length && readable
      ? Atom.make((get): IssueSummary | null => {
          const summaries = links.slice(0, SIDEBAR_GROUP_STATUS_LINKS).map((link) => {
            const target = { environmentId, input: linkedIssueRef(link) };
            return selectEnvironmentQueryData(
              get(
                capabilities?.issueSummaries === true
                  ? issueEnvironment.summary(target, "sidebar")
                  : issueEnvironment.detail(target, "sidebar"),
              ),
            );
          });
          const open = summaries.find((summary) => summary?.state.category === "open");
          if (open) return open;
          if (summaries.includes(null)) return null;
          return (
            summaries.find(
              (summary) => summary?.state.closeReason && summary.state.closeReason !== "completed",
            ) ??
            summaries[0] ??
            null
          );
        })
      : emptySidebarIssue;
  return useAtomValue(aggregate);
}

// The atoms keep the failure; hooks word it at render so it follows a language switch.
const mergedIssueLists = createMergedIssueListAtoms(issueEnvironment, (cause) => cause);

/** The same listing read across every connected environment, as the pull-request page does. */
export function useIssueList(targets: ReadonlyArray<IssueListTarget>): MergedIssueList & {
  readonly refresh: () => void;
  readonly reload: () => void;
  readonly retry: () => void;
} {
  const view = useAtomValue(mergedIssueLists.atom(targets));
  const messages = useMessages();
  return {
    ...view,
    error: view.error === null ? null : formatEnvironmentQueryError(view.error, messages),
    refresh: () => mergedIssueLists.refresh(appAtomRegistry, targets, "manual"),
    reload: () => mergedIssueLists.refresh(appAtomRegistry, targets, "background"),
    retry: () => mergedIssueLists.retry(appAtomRegistry, targets),
  };
}

/** After a write made beside the list, such as a state change in the detail panel. */
export function refreshIssueLists(environmentId: EnvironmentId) {
  mergedIssueLists.refreshEnvironment(appAtomRegistry, environmentId);
}

const mergedIssueCandidates = createMergedIssueCandidatesAtoms(
  issueEnvironment.candidates,
  (cause) => cause,
);

/** The labels and people of every scope a list filter covers; no targets reads nothing. */
export function useIssueFilterCandidates(targets: ReadonlyArray<IssueCandidatesTarget>) {
  const view = useAtomValue(mergedIssueCandidates.atom(targets));
  const messages = useMessages();
  return {
    ...view,
    error: view.error === null ? null : formatEnvironmentQueryError(view.error, messages),
  };
}
