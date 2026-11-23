import { parsePullRequestAnchor } from "@onicode/shared/pullRequestAnchor";
import { resolveChangeRequestLinkTarget } from "@onicode/client-runtime/change-request-links";
import { sourceControlClients } from "@onicode/client-runtime/source-control-clients";
import type { EnvironmentProject } from "@onicode/client-runtime/state/shell";
import { EnvironmentId, ProjectId, type IssueRef } from "@onicode/contracts";

import {
  fileRoutePathSegments,
  resolveFileLinkPath,
  resolveWorkspaceFilePath,
} from "../features/files/filePath";
import { resolveMarkdownLinkPresentation } from "./markdownLinks";

/**
 * The file route for a path a change request's markdown links, in the checkout the server read
 * it through. Null for any other link, and for a checkout of another repository that only lent
 * its credentials: its paths name none of the change request's files.
 */
export function pullRequestFileLinkRoute(
  href: string,
  checkout: {
    readonly environmentId: string;
    readonly workspaceRoot: string;
    readonly borrowedCheckout?: boolean | undefined;
  } | null,
) {
  if (checkout === null || checkout.borrowedCheckout === true) return null;
  const link = resolveMarkdownLinkPresentation(href);
  if (link.kind !== "file") return null;
  const path = resolveFileLinkPath(checkout.workspaceRoot, link.path);
  if (path === null) return null;
  return {
    environmentId: checkout.environmentId,
    path: fileRoutePathSegments(resolveWorkspaceFilePath(checkout.workspaceRoot, path)),
    cwd: checkout.workspaceRoot,
    ...(link.line ? { line: String(link.line) } : {}),
  };
}

/** The PullRequest route for a change request link from a project, or null for the browser. */
export function nativePullRequestDestination(
  url: string,
  context: { environmentId: string; projectId: string } | null,
  projects: ReadonlyArray<Pick<EnvironmentProject, "id" | "environmentId" | "repositoryIdentity">>,
  configLoaded: (environmentId: EnvironmentId) => boolean,
) {
  if (!context) return null;
  const target = resolveChangeRequestLinkTarget({
    url,
    projects,
    configLoaded,
    environmentId: EnvironmentId.make(context.environmentId),
    projectId: ProjectId.make(context.projectId),
    lendHostProject: true,
  });
  if (target === null) return null;
  return {
    environmentId: context.environmentId,
    projectId: String(target.project.id),
    repository: target.repository,
    number: target.number,
    ...(parsePullRequestAnchor(url) ? { navigationUrl: url } : {}),
    ...(target.host === undefined ? {} : { host: target.host }),
  };
}

/**
 * The pull request an issue link may name instead, on a host that numbers both in one sequence
 * and writes every `#12` through its issue route. Null elsewhere. Only a candidate to read.
 */
export function pullRequestCandidateForIssueLink(
  url: string,
  ref: Pick<IssueRef, "projectId" | "host" | "scope" | "key">,
) {
  const candidateUrl = sourceControlClients.changeRequestUrlFromReferenceAutolink(url);
  const number = Number(ref.key);
  if (candidateUrl === null || !Number.isInteger(number) || number <= 0) return null;
  return {
    target: {
      projectId: ref.projectId,
      ...(ref.host === undefined ? {} : { host: ref.host }),
      repository: ref.scope,
      number,
    },
    navigationUrl: parsePullRequestAnchor(candidateUrl) ? candidateUrl : undefined,
  };
}
