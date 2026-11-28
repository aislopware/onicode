import {
  createIssueEnvironmentAtoms,
  createMergedIssueCandidatesAtoms,
  createMergedIssueListAtoms,
  type IssueCandidatesTarget,
  type IssueListTarget,
} from "@onicode/client-runtime/state/issues";
import { useAtomValue } from "@effect/atom-react";
import type { EnvironmentId } from "@onicode/contracts";
import { useMessages } from "gt-react-native";

import { connectionAtomRuntime } from "../connection/runtime";
import { appAtomRegistry } from "./atom-registry";
import { formatEnvironmentQueryError } from "./query";
import { composerPullRequests } from "./pull-requests";

export const issueEnvironment = createIssueEnvironmentAtoms(
  connectionAtomRuntime,
  composerPullRequests.linkChanges,
);

// The atoms keep the failure; hooks word it at render so it follows a language switch.
const mergedIssueLists = createMergedIssueListAtoms(issueEnvironment, (cause) => cause);

/** One listing read across every environment asked. */
export function useIssueList(targets: ReadonlyArray<IssueListTarget>) {
  const view = useAtomValue(mergedIssueLists.atom(targets));
  const messages = useMessages();
  return {
    ...view,
    error: view.error === null ? null : formatEnvironmentQueryError(view.error, messages),
    refresh: () => mergedIssueLists.refresh(appAtomRegistry, targets, "manual"),
  };
}

/** After a write the list cannot see, such as a new issue filed from a sheet. */
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

/** Asks again only the scopes that failed; the ones that answered keep their answer. */
export function refreshIssueFilterCandidates(targets: ReadonlyArray<IssueCandidatesTarget>) {
  mergedIssueCandidates.retry(appAtomRegistry, targets);
}
