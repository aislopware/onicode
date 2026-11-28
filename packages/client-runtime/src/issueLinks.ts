import {
  IssueTrackerKind,
  issueTrackerBindingScopes,
  type IssueRef,
  type IssueTrackerConnectionList,
  type RepositoryIdentity,
} from "@onicode/contracts";
import {
  JIRA_HOST,
  JIRA_KIND,
  parseJiraIssueUrl,
} from "@onicode/issue-tracker-jira/shared/issueUrl";
import {
  LINEAR_HOST,
  LINEAR_KIND,
  parseLinearIssueUrl,
} from "@onicode/issue-tracker-linear/shared/issueUrl";

import { sourceControlClients } from "./sourceControlClients.ts";
import type { EnvironmentProject } from "./state/shell.ts";

type LinkProject = Pick<EnvironmentProject, "id" | "environmentId" | "repositoryIdentity">;

const issueBindingsByRepository = new WeakMap<
  RepositoryIdentity,
  ReturnType<typeof sourceControlClients.issueBindings>
>();

/** The hosts and scopes a project's git-host tracker is bound to, the way the server resolves them. */
function projectIssueBindings(project: LinkProject) {
  const identity = project.repositoryIdentity;
  if (identity == null) return [];
  const cached = issueBindingsByRepository.get(identity);
  if (cached !== undefined) return cached;
  const bindings = sourceControlClients
    .issueBindings(identity)
    .map((binding) => ({ ...binding, scope: binding.scope.toLowerCase() }));
  issueBindingsByRepository.set(identity, bindings);
  return bindings;
}

/**
 * Names the host a host-less git-host reference is read on, the project's own as the server's
 * `IssueService.requireProject` picks it, so one issue keys the same whether its link named the
 * host or not. A named host is kept: a fork's upstream can live elsewhere.
 */
export function withProjectIssueHost<
  Ref extends { readonly tracker: string; readonly host?: string | undefined },
>(ref: Ref, project: LinkProject | null | undefined): Ref {
  if (ref.host !== undefined || project == null) return ref;
  const host = projectIssueBindings(project).find(
    (binding) => binding.tracker === ref.tracker,
  )?.host;
  return host === undefined ? ref : { ...ref, host };
}

function linkAuthority(url: string): string | null {
  try {
    return new URL(url).host.toLowerCase();
  } catch {
    return null;
  }
}

function indexIssueBindings<Project extends LinkProject>(projects: ReadonlyArray<Project>) {
  const bindings = projects.flatMap((project) =>
    projectIssueBindings(project).map((binding) => ({ project, binding })),
  );
  return { bindings, hosts: new Set(bindings.map((candidate) => candidate.binding.host)) };
}

/** Each standalone tracker's link parser, as the server's `IssueService` tries them. */
const STANDALONE_ISSUE_LINKS = [
  { tracker: LINEAR_KIND, parse: parseLinearIssueUrl },
  { tracker: JIRA_KIND, parse: parseJiraIssueUrl },
];

function parseStandaloneIssueLink(url: string) {
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    return null;
  }
  for (const { tracker, parse } of STANDALONE_ISSUE_LINKS) {
    const link = parse(parsed);
    if (link !== null) return { tracker, link };
  }
  return null;
}

/** The workspace a standalone tracker's issue link names, which a reference read through it keeps. */
export function issueLinkWorkspace(url: string | undefined): string | undefined {
  return url === undefined
    ? undefined
    : parseStandaloneIssueLink(url)?.link.workspace.toLowerCase();
}

/** A link to a place in an issue (`#issuecomment-1`, `#note_1`, `#comment-…`, Jira's `focusedCommentId`). */
function namesPlaceInIssue(url: string): boolean {
  try {
    const parsed = new URL(url);
    return parsed.hash !== "" || parsed.searchParams.has("focusedCommentId");
  } catch {
    return false;
  }
}

/** Whether `url` is a standalone tracker's issue link, which only bindings can resolve. */
export function isStandaloneIssueLink(url: string): boolean {
  return parseStandaloneIssueLink(url) !== null;
}

const escapeRegExp = (text: string) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const STANDALONE_LINK_HINT_PATTERN = new RegExp(
  `${escapeRegExp(LINEAR_HOST)}(?::\\d*)?/|\\.${escapeRegExp(JIRA_HOST)}(?::\\d*)?/`,
  "i",
);

/** A cheap pre-check, so text without such a link never asks for the bindings. */
export function mayContainStandaloneIssueLink(text: string): boolean {
  return STANDALONE_LINK_HINT_PATTERN.test(text);
}

/**
 * A standalone tracker link resolves through the project bound to its scope, else any bound to
 * the tracker, and only through an account whose credential reaches the workspace it names: team
 * keys repeat across workspaces, so a link elsewhere stays an ordinary link.
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
  const accounts = new Set(
    [...trackers.trackers, ...(trackers.accounts ?? [])]
      .filter(
        (connection) =>
          connection.tracker === tracker &&
          connection.account !== null &&
          connection.workspace?.toLowerCase() === link.workspace.toLowerCase(),
      )
      .map((connection) => connection.accountId ?? null),
  );
  if (accounts.size === 0) return null;
  const bindings = { ...trackers.bindings, ...trackers.accountBindings };
  const onTracker = projects.flatMap((project) => {
    const binding = bindings[project.id];
    return binding?.tracker === tracker && accounts.has(binding.accountId ?? null)
      ? [{ project, binding }]
      : [];
  });
  const preferFirst = (candidates: typeof onTracker) =>
    candidates.find((candidate) => candidate.project.id === preferredProjectId) ?? candidates[0];
  const scope = link.scope.toLowerCase();
  const boundScope = (candidate: (typeof onTracker)[number]) =>
    issueTrackerBindingScopes(candidate.binding).find((key) => key.toLowerCase() === scope);
  const own = preferFirst(onTracker.filter((candidate) => boundScope(candidate) !== undefined));
  const lender = own ?? preferFirst(onTracker);
  if (lender === undefined) return null;
  return {
    project: lender.project,
    ref: {
      projectId: lender.project.id,
      tracker: lender.binding.tracker,
      host: link.host,
      scope: (own && boundScope(own)) ?? link.scope,
      key: link.key,
      workspace: link.workspace.toLowerCase(),
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

  const resolve = (
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
        tracker: IssueTrackerKind.make(lender.binding.tracker),
        host: lender.binding.host,
        scope: own === undefined ? link.scope : lender.binding.scope,
        key: link.key,
      },
    };
  };

  return (
    url: string,
    preferredProjectId?: string,
  ): {
    readonly project: Project;
    readonly ref: IssueRef;
    /**
     * The link names a place inside the issue, which the panel cannot show: it opens at the top,
     * so a click is the browser's. A GitHub link may still name a pull request's comment.
     */
    readonly namesPlace: boolean;
  } | null => {
    const target = resolve(url, preferredProjectId);
    return target === null ? null : { ...target, namesPlace: namesPlaceInIssue(url) };
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
}) {
  return createIssueLinkResolver(input.projects, input.trackers)(
    input.url,
    input.preferredProjectId,
  );
}
