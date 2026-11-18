import type { IssueRef } from "@onicode/contracts";

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

/**
 * Resolves issue links by the server's `IssueService.resolveUrl` rule: the project bound to the
 * link's host and scope, else one on that tracker and host lending read-only credentials, so a
 * lookalike hostname stays an ordinary link. Bindings are read once, at the first link.
 */
export function createIssueLinkResolver<Project extends LinkProject>(
  /** The projects of the environment the link is read in. */
  projects: ReadonlyArray<Project>,
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
    if (parsed === null) return null;
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
}): { readonly project: Project; readonly ref: IssueRef } | null {
  return createIssueLinkResolver(input.projects)(input.url, input.preferredProjectId);
}
