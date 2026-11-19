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
import type { EnvironmentId, IssueRef, IssueSummary, ThreadIssueLink } from "@onicode/contracts";

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
    environmentId === null ? undefined : configs.get(environmentId)?.environment.capabilities;
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

/** A linked issue's live row, read in the workspace its link named. */
export function useLinkedIssueSummary(
  environmentId: EnvironmentId,
  link: ThreadIssueLink | undefined,
  mountScope?: IssueReadScope,
) {
  const workspace = issueLinkWorkspace(link?.url);
  return useIssueSummary(
    environmentId,
    link === undefined
      ? null
      : {
          projectId: link.projectId,
          tracker: link.tracker,
          host: link.host,
          scope: link.scope,
          key: link.key,
          ...(workspace === undefined ? {} : { workspace }),
        },
    mountScope,
  );
}

const mergedIssueLists = createMergedIssueListAtoms(
  issueEnvironment.list,
  formatEnvironmentQueryError,
);

/** The same listing read across every connected environment, as the pull-request page does. */
export function useIssueList(
  targets: ReadonlyArray<IssueListTarget>,
): MergedIssueList & { readonly refresh: () => void; readonly retry: () => void } {
  const view = useAtomValue(mergedIssueLists.atom(targets));
  return {
    ...view,
    refresh: () => mergedIssueLists.refresh(appAtomRegistry, targets),
    retry: () => mergedIssueLists.retry(appAtomRegistry, targets),
  };
}

/** After a write made beside the list, such as a state change in the detail panel. */
export function refreshIssueLists(environmentId: EnvironmentId) {
  mergedIssueLists.refreshEnvironment(appAtomRegistry, environmentId);
}

const mergedIssueCandidates = createMergedIssueCandidatesAtoms(
  issueEnvironment.candidates,
  formatEnvironmentQueryError,
);

/** The labels and people of every scope a list filter covers; no targets reads nothing. */
export function useIssueFilterCandidates(targets: ReadonlyArray<IssueCandidatesTarget>) {
  return useAtomValue(mergedIssueCandidates.atom(targets));
}
