import { pullRequestFilterProjects } from "@onicode/client-runtime/pull-request-project-filter";
import type { EnvironmentProject } from "@onicode/client-runtime/state/models";
import {
  ISSUE_LIST_MAX_EXCLUDED_PROJECTS,
  repositoryGroupingKeyOf,
  type EnvironmentId,
  type IssueListInput,
  type ProjectId,
} from "@onicode/contracts";

import { sourceControlClients } from "@onicode/client-runtime/source-control-clients";
import type { IssueListTarget } from "@onicode/client-runtime/state/issues";

import type { IssueSurfaceTarget } from "~/rightPanelStore";

import type { IssuesSearch } from "./IssuesPage";

// The router reads a bare `key=42` as a number.
const text = (value: unknown): string | undefined => {
  const raw = typeof value === "number" ? String(value) : value;
  return typeof raw === "string" && raw.trim() ? raw.trim() : undefined;
};
// Only free text is capped: a shortened id or issue key names something else.
const bounded = (value: unknown) => text(value)?.slice(0, 200);

const labels = (raw: unknown): ReadonlyArray<string> | undefined => {
  const names = (Array.isArray(raw) ? raw : [raw]).flatMap((name) => bounded(name) ?? []);
  return names.length === 0 ? undefined : names.slice(0, 20);
};

const projectPairs = (raw: unknown): IssuesSearch["hideProjects"] => {
  const pairs = (Array.isArray(raw) ? raw : []).flatMap((pair) => {
    const environmentId = Array.isArray(pair) ? text(pair[0]) : undefined;
    const projectId = Array.isArray(pair) ? text(pair[1]) : undefined;
    return environmentId && projectId
      ? [[environmentId as EnvironmentId, projectId as ProjectId] as const]
      : [];
  });
  return pairs.length === 0 ? undefined : pairs.slice(0, ISSUE_LIST_MAX_EXCLUDED_PROJECTS);
};

// Every optional key is an own key, even when undefined: the router lays these over the parent
// route's unvalidated search, where a key left out would keep its raw value.
export const issuesSearchFromUrl = (raw: Record<string, unknown>): IssuesSearch => ({
  who:
    raw.who === "assigned" || raw.who === "authored" || raw.who === "mentioned" ? raw.who : "all",
  status: raw.status === "closed" || raw.status === "all" ? raw.status : "open",
  hideProjects: projectPairs(raw.hideProjects),
  host: bounded(raw.host),
  q: bounded(raw.q),
  label: labels(raw.label),
  assignee: bounded(raw.assignee),
  order: raw.order === "created" ? "created" : undefined,
  selectedEnvironmentId: text(raw.selectedEnvironmentId) as EnvironmentId | undefined,
  selectedProjectId: text(raw.selectedProjectId) as ProjectId | undefined,
  selectedHost: text(raw.selectedHost),
  tracker: text(raw.tracker),
  scope: text(raw.scope),
  key: text(raw.key),
  selectedIssueUrl: text(raw.selectedIssueUrl),
});

/** The panel tab a link into the page names, before any row was pressed. */
export const issueSurfaceTargetFromSearch = (
  search: Pick<
    IssuesSearch,
    | "selectedEnvironmentId"
    | "selectedProjectId"
    | "selectedHost"
    | "tracker"
    | "scope"
    | "key"
    | "selectedIssueUrl"
  >,
): IssueSurfaceTarget | null =>
  !search.tracker || !search.scope || !search.key || !search.selectedProjectId
    ? null
    : {
        ...(search.selectedEnvironmentId === undefined
          ? {}
          : { environmentId: search.selectedEnvironmentId }),
        projectId: search.selectedProjectId,
        ...(search.selectedHost === undefined ? {} : { host: search.selectedHost }),
        tracker: search.tracker,
        scope: search.scope,
        key: search.key,
        ...(search.selectedIssueUrl === undefined ? {} : { url: search.selectedIssueUrl }),
      };

/**
 * One choice per project: projects on one repository can read different Linear teams. Scratch
 * projects stay, since Settings can bind one to a Linear team.
 */
export const issueFilterProjects = (
  projects: ReadonlyArray<EnvironmentProject>,
  environmentLabels: ReadonlyMap<EnvironmentId, string>,
) =>
  pullRequestFilterProjects(
    projects.map((project) => ({ ...project, repositoryIdentity: null })),
    environmentLabels,
  );

/** The same key as `pullRequestProjectKey`. */
const projectKey = (project: { readonly id: ProjectId; readonly environmentId: EnvironmentId }) =>
  JSON.stringify([project.environmentId, project.id]);

export const issueProjectKeys = (pairs: IssuesSearch["hideProjects"]): ReadonlySet<string> =>
  new Set((pairs ?? []).map((pair) => JSON.stringify(pair)));

/**
 * A server older than `excludeProjectIds` reads hidden projects too, and tags a scope's rows with
 * one of the projects that read it, so a row stays while a shown project in its environment reads
 * the same scope. Trackers bound in settings and errors name no repository scope, so those stay
 * while a shown project shares the hidden one's repository.
 */
export function issueRowHidden(
  hidden: ReadonlySet<string>,
  projects: ReadonlyArray<Pick<EnvironmentProject, "id" | "environmentId" | "repositoryIdentity">>,
): (
  environmentId: EnvironmentId,
  row: {
    readonly projectId: ProjectId;
    readonly host?: string | undefined;
    readonly scope?: string | undefined;
  },
) => boolean {
  if (hidden.size === 0) return () => false;
  type Project = (typeof projects)[number];
  const scopeKey = (environmentId: EnvironmentId, host: string, scope: string) =>
    JSON.stringify([environmentId, host.toLowerCase(), scope.toLowerCase()]);
  const scopes = new Map<Project, ReadonlyArray<string>>();
  const scopesOf = (project: Project) => {
    let keys = scopes.get(project);
    if (!keys) {
      keys = sourceControlClients
        .issueBindings(project.repositoryIdentity)
        .map(({ host, scope }) => scopeKey(project.environmentId, host, scope));
      scopes.set(project, keys);
    }
    return keys;
  };
  const repositoryOf = (project: Project) =>
    project.repositoryIdentity
      ? JSON.stringify([
          project.environmentId,
          repositoryGroupingKeyOf(project.repositoryIdentity).toLowerCase(),
        ])
      : null;
  const byKey = new Map(projects.map((project) => [projectKey(project), project]));
  // Built when a hidden row first needs its environment; a server that honours exclusions sends none.
  const shownIn = new Map<EnvironmentId, { scopes: Set<string>; repositories: Set<string> }>();
  const shown = (environmentId: EnvironmentId) => {
    let index = shownIn.get(environmentId);
    if (!index) {
      const inEnvironment = projects.filter(
        (project) => project.environmentId === environmentId && !hidden.has(projectKey(project)),
      );
      index = {
        scopes: new Set(inEnvironment.flatMap(scopesOf)),
        repositories: new Set(inEnvironment.flatMap((project) => repositoryOf(project) ?? [])),
      };
      shownIn.set(environmentId, index);
    }
    return index;
  };
  return (environmentId, row) => {
    const key = projectKey({ environmentId, id: row.projectId });
    if (!hidden.has(key)) return false;
    const project = byKey.get(key);
    if (!project) return true;
    if (row.host !== undefined && row.scope !== undefined) {
      const rowScope = scopeKey(environmentId, row.host, row.scope);
      if (shown(environmentId).scopes.has(rowScope)) return false;
      if (scopesOf(project).includes(rowScope)) return true;
    }
    const repository = repositoryOf(project);
    return repository === null || !shown(environmentId).repositories.has(repository);
  };
}

/**
 * Each environment leaves out its hidden projects, named from the saved pairs so a listing sent
 * before the project list loads still leaves them out. One with every project hidden is not read.
 * Rows past the request limit are hidden on the client.
 */
export function issueListTargets(
  environmentIds: ReadonlyArray<EnvironmentId>,
  projects: ReadonlyArray<{ readonly id: ProjectId; readonly environmentId: EnvironmentId }>,
  hidden: IssuesSearch["hideProjects"],
  input: IssueListInput,
): ReadonlyArray<IssueListTarget> {
  const keys = issueProjectKeys(hidden);
  return environmentIds.flatMap((environmentId) => {
    const own = projects.filter((project) => project.environmentId === environmentId);
    if (own.length > 0 && own.every((project) => keys.has(projectKey(project)))) return [];
    const excludeProjectIds = (
      own.length > 0
        ? own.filter((project) => keys.has(projectKey(project))).map((project) => project.id)
        : (hidden ?? [])
            .filter(([environment]) => environment === environmentId)
            .map(([, projectId]) => projectId)
    ).slice(0, ISSUE_LIST_MAX_EXCLUDED_PROJECTS);
    return [
      {
        environmentId,
        input: excludeProjectIds.length ? { ...input, excludeProjectIds } : input,
      },
    ];
  });
}
