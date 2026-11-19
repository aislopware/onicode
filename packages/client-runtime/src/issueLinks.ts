import type { IssueRef, IssueTrackerConnectionList } from "@onicode/contracts";
import {
  LINEAR_HOST,
  LINEAR_KIND,
  parseLinearIssueUrl,
} from "@onicode/issue-tracker-linear/shared/issueUrl";

import { sourceControlClients } from "./sourceControlClients.ts";
import type { EnvironmentProject } from "./state/shell.ts";

type LinkProject = Pick<EnvironmentProject, "id" | "environmentId" | "repositoryIdentity">;

/** The host and scope a project's git-host tracker is bound to, the way the server resolves it. */
function projectIssueBinding(project: LinkProject): {
  readonly tracker: string;
  readonly host: string;
  readonly scope: string;
} | null {
  const identity = project.repositoryIdentity;
  const scope = sourceControlClients.issueScope(identity);
  const kind = identity?.provider;
  if (!identity || kind === undefined || kind === "unknown" || scope === null) return null;
  return {
    tracker: kind,
    host: sourceControlClients.changeRequestHost(identity, kind),
    scope: scope.toLowerCase(),
  };
}

function linkAuthority(url: string): string | null {
  try {
    return new URL(url).host.toLowerCase();
  } catch {
    return null;
  }
}

function indexIssueBindings<Project extends LinkProject>(projects: ReadonlyArray<Project>) {
  const bindings = projects.flatMap((project) => {
    const binding = projectIssueBinding(project);
    return binding === null ? [] : [{ project, binding }];
  });
  return { bindings, hosts: new Set(bindings.map((candidate) => candidate.binding.host)) };
}

function parseStandaloneIssueLink(url: string) {
  try {
    const link = parseLinearIssueUrl(new URL(url));
    return link === null ? null : { tracker: LINEAR_KIND, link };
  } catch {
    return null;
  }
}

/** Whether `url` is a standalone tracker's issue link, which only bindings can resolve. */
export function isStandaloneIssueLink(url: string): boolean {
  return parseStandaloneIssueLink(url) !== null;
}

/** A cheap pre-check, so text without such a link never asks for the bindings. */
export function mayContainStandaloneIssueLink(text: string): boolean {
  return text.includes(`${LINEAR_HOST}/`);
}

/**
 * A standalone tracker link resolves through the project bound to its scope, else any bound to
 * the tracker, and only when it names the workspace the credential reaches: team keys repeat
 * across workspaces, so a link elsewhere stays an ordinary link.
 */
function resolveStandaloneIssueLink<Project extends LinkProject>(
  url: string,
  projects: ReadonlyArray<Project>,
  trackers: IssueTrackerConnectionList | undefined,
  preferredProjectId: string | undefined,
): { readonly project: Project; readonly ref: IssueRef } | null {
  const parsed = trackers === undefined ? null : parseStandaloneIssueLink(url);
  if (parsed === null || trackers === undefined) return null;
  const { tracker, link } = parsed;
  const workspace = trackers.trackers.find(
    (connection) => connection.tracker === tracker && connection.account !== null,
  )?.workspace;
  if (workspace?.toLowerCase() !== link.workspace.toLowerCase()) return null;
  const { bindings } = trackers;
  const onTracker = projects.flatMap((project) => {
    const binding = bindings[project.id];
    return binding?.tracker === tracker ? [{ project, binding }] : [];
  });
  const preferFirst = (candidates: typeof onTracker) =>
    candidates.find((candidate) => candidate.project.id === preferredProjectId) ?? candidates[0];
  const scope = link.scope.toLowerCase();
  const own = preferFirst(
    onTracker.filter((candidate) => candidate.binding.scope.toLowerCase() === scope),
  );
  const lender = own ?? preferFirst(onTracker);
  if (lender === undefined) return null;
  return {
    project: lender.project,
    ref: {
      projectId: lender.project.id,
      tracker: lender.binding.tracker,
      host: link.host,
      scope: own === undefined ? link.scope : lender.binding.scope,
      key: link.key,
    },
  };
}

/**
 * Resolves issue links by the server's `IssueService.resolveUrl` rule: the project bound to the
 * link's host and scope, else one on that tracker and host lending read-only credentials, so a
 * lookalike hostname stays an ordinary link. Bindings are read once, at the first link.
 */
export function createIssueLinkResolver<Project extends LinkProject>(
  /** The projects of the environment the link is read in. */
  projects: ReadonlyArray<Project>,
  /** The environment's standalone trackers and bindings; without them only git-host links resolve. */
  trackers?: IssueTrackerConnectionList,
) {
  let index: ReturnType<typeof indexIssueBindings<Project>> | undefined;
  const bindingsRead = () => (index ??= indexIssueBindings(projects));

  /** A host whose hostname does not name it, such as GitHub Enterprise Server, is vouched for by a project on it. */
  const parseIssueLink = (url: string, authority: string | null) => {
    const parsed = sourceControlClients.parseIssueUrl(url);
    if (parsed !== null || authority === null) return parsed;
    const hostname = new URL(url).hostname.toLowerCase();
    const { bindings, hosts } = bindingsRead();
    if (!hosts.has(hostname) && !hosts.has(authority)) return null;
    for (const { project, binding } of bindings) {
      if (binding.host !== hostname && binding.host !== authority) continue;
      const known = sourceControlClients.parseIssueUrl(url, project.repositoryIdentity?.provider);
      if (known !== null) return known;
    }
    return null;
  };

  return (
    url: string,
    preferredProjectId?: string,
  ): { readonly project: Project; readonly ref: IssueRef } | null => {
    // A link names its server's port; a binding does only when its remote or web URL did.
    const authority = linkAuthority(url);
    const parsed = parseIssueLink(url, authority);
    if (parsed === null)
      return resolveStandaloneIssueLink(url, projects, trackers, preferredProjectId);
    const { definition, link } = parsed;
    const onHost = bindingsRead().bindings.filter(
      ({ binding }) =>
        binding.tracker === definition.kind &&
        (binding.host === link.host || binding.host === authority),
    );
    const onAuthority = onHost.filter((candidate) => candidate.binding.host === authority);
    const bound = onAuthority.length > 0 ? onAuthority : onHost;
    const preferFirst = (candidates: typeof bound) =>
      candidates.find((candidate) => candidate.project.id === preferredProjectId) ?? candidates[0];
    const own =
      preferFirst(bound.filter((candidate) => candidate.binding.scope === link.scope)) ??
      preferFirst(bound.filter((candidate) => candidate.binding.scope === link.scopeAlias));
    const lender = own ?? preferFirst(bound);
    if (lender === undefined) return null;
    return {
      project: lender.project,
      ref: {
        projectId: lender.project.id,
        tracker: lender.binding.tracker as IssueRef["tracker"],
        host: lender.binding.host,
        scope: own === undefined ? link.scope : lender.binding.scope,
        key: link.key,
      },
    };
  };
}

/** One link resolved on its own, for a click or press; see `createIssueLinkResolver`. */
export function resolveIssueLinkTarget<Project extends LinkProject>(input: {
  readonly url: string;
  /** The projects of the environment the link is read in. */
  readonly projects: ReadonlyArray<Project>;
  readonly preferredProjectId?: string | undefined;
  /** The environment's standalone trackers and bindings; without them only git-host links resolve. */
  readonly trackers?: IssueTrackerConnectionList | undefined;
}): { readonly project: Project; readonly ref: IssueRef } | null {
  return createIssueLinkResolver(input.projects, input.trackers)(
    input.url,
    input.preferredProjectId,
  );
}
