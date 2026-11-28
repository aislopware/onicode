// A project bound to a standalone tracker (Linear) in settings lists it beside its git host's.
// References outside the project's own repositories (upstream and fork) may be read on the host,
// but every write must pass through requireWritable.
import * as Cache from "effect/Cache";
import * as Context from "effect/Context";
import * as Duration from "effect/Duration";
import * as Effect from "effect/Effect";
import * as Equal from "effect/Equal";
import * as Exit from "effect/Exit";
import * as Hash from "effect/Hash";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import {
  IssueOperationError,
  IssueTrackerKind,
  IssueUnavailableError,
  type IssueTrackerAccountId,
  SourceControlProviderKind,
  type IssueCandidates,
  type IssueCandidatesInput,
  type IssueCommentInput,
  type IssueCommentsPage,
  type IssueCommentsPageInput,
  type IssueCreateInput,
  type IssueCreateResult,
  type IssueDetail,
  type IssueDetailInput,
  type IssueError,
  type IssueListEntry,
  type IssueListInput,
  type IssueListProjectError,
  type IssueListResult,
  type IssueReactionInput,
  type IssueRef,
  type IssueScopeRef,
  type IssueSetStateInput,
  type IssueSummary,
  type IssueTemplateList,
  type IssueTemplatesInput,
  type IssueTrackerSummary,
  type IssueTrackerProjectBinding,
  issueTrackerBindingScopes,
  type IssueUpdateInput,
  type OrchestrationProjectShell,
  type ProjectId,
} from "@onicode/contracts";
import * as SourceControlRateLimit from "@onicode/source-control-core/server/SourceControlRateLimit";
import {
  IssueTrackerError,
  isStandaloneIssueTracker,
  type IssueInvolvementFilter,
  type IssueTrackerApi,
  type StandaloneIssueTrackerApi,
  type TrackerIssueDetail,
  type TrackerIssueSummary,
} from "@onicode/source-control-core/server/IssueTracker";
import {
  asIssueTrackerAccount,
  IssueTrackerAccount,
} from "@onicode/source-control-core/server/IssueTrackerHost";
import { parseUrl } from "@onicode/shared/url";

import * as IssueTrackerBindings from "../persistence/IssueTrackerBindings.ts";
import * as ProjectService from "../project/ProjectService.ts";
import * as RepositoryIdentityResolver from "../project/RepositoryIdentityResolver.ts";
import { sourceControlDefinitions } from "../sourceControl/sourceControlDefinitions.ts";
import * as IssueTrackerRegistry from "./IssueTrackerRegistry.ts";

const DEFAULT_LIST_LIMIT = 50;
const SCOPE_CONCURRENCY = 4;
// Every client polls linked issues; one shared read per window keeps them off the tracker's quota.
const DETAIL_CACHE_TTL = Duration.seconds(30);
const DETAIL_CACHE_CAPACITY = 512;

/** Equal by `key` alone, so the cache can run the read it carries on a miss. */
class TrackerRead<A> implements Equal.Equal {
  readonly key: string;
  readonly lookup: Effect.Effect<A, IssueError>;
  constructor(key: string, lookup: Effect.Effect<A, IssueError>) {
    this.key = key;
    this.lookup = lookup;
  }
  [Equal.symbol](that: unknown): boolean {
    return that instanceof TrackerRead && that.key === this.key;
  }
  [Hash.symbol](): number {
    return Hash.string(this.key);
  }
}

/**
 * One issue as one tracker account sees it; the host's credential is the server's own. The
 * workspace keeps two Linear workspaces' `ENG-7` apart.
 */
function detailCacheKey(project: ProjectTrackerBinding, input: IssueRef): string {
  return [project.tracker, project.host, project.accountId ?? "", project.scope, input.key]
    .concat(input.workspace ?? "")
    .map((part) => part.toLowerCase())
    .join(" ");
}

const interactive =
  <I, A>(call: (input: I) => Effect.Effect<A, IssueError>) =>
  (input: I) =>
    call(input).pipe(Effect.provideService(SourceControlRateLimit.Interactive, true));

const traced =
  <I, A>(operation: string, call: (input: I) => Effect.Effect<A, IssueError>) =>
  (input: I) =>
    call(input).pipe(Effect.withSpan(`IssueService.${operation}`));

/** A tracker may fail with no detail; the wire errors carry a non-empty one. */
export function trackerDetail(error: IssueTrackerError): string {
  return error.detail.trim() || "The issue tracker request failed.";
}

/** One project's resolved tracker: which driver, where it is served from, and its container. */
interface ProjectTrackerBinding {
  readonly tracker: IssueTrackerKind;
  readonly host: string;
  readonly scope: string;
  /** The standalone tracker account the scope is read with; null for git hosts and first accounts. */
  readonly accountId: IssueTrackerAccountId | null;
}

/** A project the registry has a tracker for, ready to serve reads and writes. */
interface SupportedProject extends ProjectTrackerBinding {
  readonly project: OrchestrationProjectShell;
  readonly api: IssueTrackerApi;
}

/**
 * The git-host path of resolution: the tracker is the host's own, named like the remote's
 * provider kind. A fork reads its upstream and its own repository.
 */
function resolveGitHostBindings(
  project: OrchestrationProjectShell,
): ReadonlyArray<ProjectTrackerBinding> {
  return sourceControlDefinitions.issueBindings(project.repositoryIdentity).map((binding) => ({
    ...binding,
    tracker: IssueTrackerKind.make(binding.tracker),
    accountId: null,
  }));
}

function findStandaloneIssueUrl(trackers: ReadonlyArray<StandaloneIssueTrackerApi>, value: string) {
  const url = parseUrl(value);
  if (url === null) return null;
  for (const api of trackers) {
    const link = api.parseIssueUrl(url);
    if (link !== null) return { api, tracker: api.kind, link };
  }
  return null;
}

/** A failure that wrote nothing: the account does not reach the issue, or has no credential. */
const isAccountMiss = (error: IssueTrackerError) =>
  error.reason === "not-found" || error.reason === "unauthenticated";

/**
 * The tracker with each call made as the first of the accounts that reaches the issue, for a
 * reference no binding names an account for: keys repeat across workspaces, and each account
 * reaches one.
 */
function asFirstReachingAccount(
  api: IssueTrackerApi,
  accountIds: ReadonlyArray<IssueTrackerAccountId | null>,
): IssueTrackerApi {
  if (accountIds.length === 1) return asIssueTrackerAccount(api, accountIds[0] ?? null);
  // Untraced: each tracker call already opens its own span.
  const reaching = Effect.fnUntraced(function* <A>(effect: Effect.Effect<A, IssueTrackerError>) {
    let miss: IssueTrackerError | null = null;
    for (const accountId of accountIds) {
      const result = yield* Effect.result(
        Effect.provideService(effect, IssueTrackerAccount, accountId),
      );
      if (result._tag === "Success") return result.success;
      if (!isAccountMiss(result.failure)) return yield* result.failure;
      // An account that lacks the issue says more than one that is signed out.
      if (miss === null || miss.reason === "unauthenticated") miss = result.failure;
    }
    return yield* (
      miss ??
      new IssueTrackerError({
        tracker: api.kind,
        operation: "read",
        reason: "not-found",
        detail: "No connected account reaches this issue.",
      })
    );
  });
  const {
    getIssueSummary,
    create,
    comment,
    update,
    setState,
    setReaction,
    listTemplates,
    listCandidates,
  } = api;
  // Every key is required here, so a method added to IssueTrackerApi fails to compile until it
  // is wrapped instead of silently running as the first account.
  const wrapped: { readonly [K in keyof IssueTrackerApi]-?: IssueTrackerApi[K] | undefined } = {
    kind: api.kind,
    capabilities: api.capabilities,
    getViewer: (input) => reaching(api.getViewer(input)),
    listIssues: (input) => reaching(api.listIssues(input)),
    getIssue: (input) => reaching(api.getIssue(input)),
    getComments: (input) => reaching(api.getComments(input)),
    getIssueSummary: getIssueSummary && ((input) => reaching(getIssueSummary(input))),
    create: create && ((input) => reaching(create(input))),
    comment: comment && ((input) => reaching(comment(input))),
    update: update && ((input) => reaching(update(input))),
    setState: setState && ((input) => reaching(setState(input))),
    setReaction: setReaction && ((input) => reaching(setReaction(input))),
    listTemplates: listTemplates && ((input) => reaching(listTemplates(input))),
    listCandidates: listCandidates && ((input) => reaching(listCandidates(input))),
  };
  return Object.fromEntries(
    Object.entries(wrapped).filter(([, value]) => value !== undefined),
  ) as unknown as IssueTrackerApi;
}

/** Lower case, so a scope keeps its cursor whichever project's spelling is read. Clients group by the leading host. */
function listCursorKey(project: ProjectTrackerBinding): string {
  const scope = project.scope.toLowerCase();
  return project.accountId === null
    ? `${project.host} ${scope}`
    : `${project.host} ${project.accountId}/${scope}`;
}

function viewerKey(project: ProjectTrackerBinding): string {
  return project.accountId === null ? project.host : `${project.host} ${project.accountId}`;
}

function isTrackerUnusable(error: IssueTrackerError): boolean {
  return error.reason === "missing-tool" || error.reason === "unauthenticated";
}

function toIssueError(operation: string): (error: IssueTrackerError) => IssueError {
  return (error) =>
    isTrackerUnusable(error)
      ? new IssueUnavailableError({
          reason: error.reason === "missing-tool" ? "tool-missing" : "unauthenticated",
          tracker: error.tracker,
          cause: error,
        })
      : new IssueOperationError({
          operation,
          detail: trackerDetail(error),
          ...(error.reason === "not-found" || error.reason === "rate-limited"
            ? { reason: error.reason }
            : {}),
          ...(error.retryAt === undefined ? {} : { retryAt: error.retryAt }),
          cause: error,
        });
}

const isNotFound = (error: IssueError) =>
  error._tag === "IssueOperationError" && error.reason === "not-found";

const readOnly = (operation: string) =>
  new IssueOperationError({
    operation,
    reason: "read-only",
    detail:
      "This issue belongs to a repository outside the selected project and cannot be changed from here.",
  });

const unsupported = (operation: string, what: string) =>
  new IssueOperationError({ operation, detail: `This tracker does not support ${what}.` });

export class IssueService extends Context.Service<
  IssueService,
  {
    readonly list: (input: IssueListInput) => Effect.Effect<IssueListResult, IssueError>;
    readonly detail: (input: IssueDetailInput) => Effect.Effect<IssueDetail, IssueError>;
    readonly summary: (input: IssueRef) => Effect.Effect<IssueSummary, IssueError>;
    readonly comments: (
      input: IssueCommentsPageInput,
    ) => Effect.Effect<IssueCommentsPage, IssueError>;
    readonly templates: (
      input: IssueTemplatesInput,
    ) => Effect.Effect<IssueTemplateList, IssueError>;
    readonly candidates: (
      input: IssueCandidatesInput,
    ) => Effect.Effect<IssueCandidates, IssueError>;
    readonly create: (input: IssueCreateInput) => Effect.Effect<IssueCreateResult, IssueError>;
    readonly comment: (input: IssueCommentInput) => Effect.Effect<void, IssueError>;
    readonly update: (input: IssueUpdateInput) => Effect.Effect<void, IssueError>;
    readonly setState: (input: IssueSetStateInput) => Effect.Effect<void, IssueError>;
    readonly react: (input: IssueReactionInput) => Effect.Effect<void, IssueError>;
    /**
     * The tracker and container a project's own issues live in: the one `key`'s prefix names, else
     * the bound `scope` named, else the project's default.
     */
    readonly projectScope: (
      projectId: ProjectId,
      select?: { readonly key?: string | undefined; readonly scope?: string | undefined },
    ) => Effect.Effect<IssueScopeRef & { readonly host: string }, IssueError>;
    /**
     * Resolve through a matching project, preferring `preferredProjectId`; other scopes are read-only.
     * An unbound `preferredProjectId` serves a standalone link no project binds the team of, if
     * the issue's canonical URL names the link's workspace; that read comes back as `issue`.
     */
    readonly resolveUrl: (
      url: string,
      preferredProjectId?: ProjectId,
    ) => Effect.Effect<
      IssueRef & {
        readonly host: string;
        readonly writable: boolean;
        readonly issue?: IssueDetail;
      },
      IssueError
    >;
  }
>()("onicode/issue/IssueService") {}

const make = Effect.gen(function* () {
  const registry = yield* IssueTrackerRegistry.IssueTrackerRegistry;
  const projects = yield* ProjectService.ProjectService;
  const repositoryIdentities = yield* RepositoryIdentityResolver.RepositoryIdentityResolver;
  const bindings = yield* IssueTrackerBindings.IssueTrackerBindingsRepository;
  const cacheOptions = {
    capacity: DETAIL_CACHE_CAPACITY,
    timeToLive: (exit: Exit.Exit<unknown, unknown>) =>
      Exit.isSuccess(exit) ? DETAIL_CACHE_TTL : Duration.zero,
  };
  const details = yield* Cache.makeWith(
    (read: TrackerRead<TrackerIssueDetail>) => read.lookup,
    cacheOptions,
  );
  // Linked rows and the tab icon poll the summary, so it shares reads the same way.
  const summaries = yield* Cache.makeWith(
    (read: TrackerRead<TrackerIssueSummary | TrackerIssueDetail>) => read.lookup,
    cacheOptions,
  );
  /** After a write, the next read must see it. */
  const invalidateDetail = (project: ProjectTrackerBinding, input: IssueRef) => {
    const key = detailCacheKey(project, input);
    return Effect.all([
      Cache.invalidate(details, new TrackerRead(key, Effect.never)),
      Cache.invalidate(summaries, new TrackerRead(key, Effect.never)),
    ]);
  };

  const resolveStandaloneBindings = (
    binding: IssueTrackerProjectBinding | undefined,
  ): ReadonlyArray<ProjectTrackerBinding> => {
    if (binding === undefined) return [];
    const api = registry.get(binding.tracker);
    if (api === null || !isStandaloneIssueTracker(api)) return [];
    const host = api.standalone.host;
    const accountId = binding.accountId ?? null;
    return issueTrackerBindingScopes(binding).map((scope) => ({
      tracker: binding.tracker,
      host,
      scope,
      accountId,
    }));
  };

  /**
   * Where a project's issues live: its bound standalone tracker, its git host's, or both. The
   * tracker bound in Settings leads, as the one the user chose for the project.
   */
  const resolveProjectTracker = (
    project: OrchestrationProjectShell,
    binding: IssueTrackerProjectBinding | undefined,
  ) =>
    (project.repositoryIdentity != null
      ? Effect.succeed(project.repositoryIdentity)
      : repositoryIdentities.resolve(project.workspaceRoot)
    ).pipe(
      Effect.map((repositoryIdentity) => {
        const gitHost = resolveGitHostBindings({ ...project, repositoryIdentity });
        return [...resolveStandaloneBindings(binding), ...gitHost];
      }),
    );

  const listProjects = (filter: {
    readonly projectId?: IssueListInput["projectId"];
    readonly excludeProjectIds?: IssueListInput["excludeProjectIds"];
    readonly host?: string | undefined;
    /** The link's `host:port`, for a host whose binding keeps its port. */
    readonly authority?: string | undefined;
  }) =>
    (filter.projectId === undefined
      ? projects.listShells()
      : projects.getShell(filter.projectId).pipe(Effect.map(Option.toArray))
    ).pipe(
      Effect.mapError(
        (cause) =>
          new IssueOperationError({
            operation: "listProjects",
            detail: "The project list could not be read.",
            cause,
          }),
      ),
      Effect.flatMap(
        Effect.fnUntraced(function* (allShells) {
          const shells = allShells.filter(
            (project) => !filter.excludeProjectIds?.includes(project.id),
          );
          if (shells.length === 0) return [];
          // One read per listing, not per project, and never cached so Settings changes apply.
          const bound = yield* bindings.list(filter.projectId).pipe(
            // A failed read is not an unbound project: that would list the git host's issues alone.
            Effect.mapError(
              (cause) =>
                new IssueOperationError({
                  operation: "listProjects",
                  detail: "The project's issue tracker binding could not be read.",
                  cause,
                }),
            ),
          );
          return yield* Effect.forEach(
            shells,
            (project) =>
              resolveProjectTracker(project, bound.get(project.id)).pipe(
                Effect.map((candidates) =>
                  candidates.map((candidate) => ({ project, ...candidate })),
                ),
              ),
            { concurrency: SCOPE_CONCURRENCY },
          );
        }),
      ),
      Effect.map((bound) => {
        const host = filter.host?.trim().toLowerCase();
        const supported: SupportedProject[] = [];
        const unimplemented = new Map<
          string,
          { tracker: IssueTrackerKind; projectIds: Set<ProjectId> }
        >();
        for (const candidate of bound.flat()) {
          if (host !== undefined && candidate.host !== host && candidate.host !== filter.authority)
            continue;
          const api = registry.get(candidate.tracker);
          if (api === null) {
            const counted = unimplemented.get(candidate.host);
            if (counted === undefined)
              unimplemented.set(candidate.host, {
                tracker: candidate.tracker,
                projectIds: new Set([candidate.project.id]),
              });
            else counted.projectIds.add(candidate.project.id);
            continue;
          }
          supported.push({ ...candidate, api: asIssueTrackerAccount(api, candidate.accountId) });
        }
        return { supported, unimplemented };
      }),
    );

  /**
   * A project with no binding for a standalone tracker, the No project included, serves its
   * references with the environment's credentials, as the first account that reaches the issue.
   * The tracker decides what the account may change.
   */
  const requireStandaloneProject = (
    ref: IssueScopeRef,
    api: StandaloneIssueTrackerApi,
    /** Accounts already known not to reach the reference. */
    skip: ReadonlySet<IssueTrackerAccountId | null> = new Set(),
  ): Effect.Effect<SupportedProject & { readonly writable: boolean }, IssueError> => {
    const host = ref.host?.trim().toLowerCase();
    if (host !== undefined && host !== api.standalone.host) {
      return Effect.fail(new IssueUnavailableError({ reason: "tracker-unsupported" }));
    }
    return Effect.all([
      projects.getShell(ref.projectId).pipe(
        Effect.mapError(
          (cause) =>
            new IssueOperationError({
              operation: "listProjects",
              detail: "The project list could not be read.",
              cause,
            }),
        ),
      ),
      bindings.listAccounts.pipe(
        Effect.mapError(
          (cause) =>
            new IssueOperationError({
              operation: "listProjects",
              detail: "The issue tracker accounts could not be read.",
              cause,
            }),
        ),
      ),
    ]).pipe(
      Effect.flatMap(([shell, added]) =>
        Option.match(shell, {
          onNone: () => Effect.fail(new IssueUnavailableError({ reason: "tracker-unsupported" })),
          onSome: (project) =>
            Effect.succeed({
              project,
              api: asFirstReachingAccount(
                api,
                [
                  null,
                  ...added
                    .filter((account) => account.tracker === api.kind)
                    .map((account) => account.accountId),
                ].filter((accountId) => !skip.has(accountId)),
              ),
              accountId: null,
              tracker: ref.tracker,
              host: api.standalone.host,
              scope: ref.scope.trim(),
              writable: true,
            }),
        }),
      ),
    );
  };

  /**
   * The project whose checkout and credentials serve a reference, and whether the reference is
   * one of the project's own scopes. A reference naming another scope on the same tracker and
   * host is served by the project but marked read-only. A project with two trackers serves the
   * one the reference names, and one with no binding for a standalone tracker still serves it.
   */
  const requireProject = (
    ref: IssueScopeRef,
  ): Effect.Effect<SupportedProject & { readonly writable: boolean }, IssueError> =>
    listProjects({ projectId: ref.projectId }).pipe(
      Effect.flatMap(({ supported }) => {
        const host = ref.host?.trim().toLowerCase();
        const onTracker = supported.filter((candidate) => candidate.tracker === ref.tracker);
        const api = registry.get(ref.tracker);
        if (onTracker.length === 0 && api !== null && isStandaloneIssueTracker(api)) {
          return requireStandaloneProject(ref, api);
        }
        const onHost = onTracker.filter(
          (candidate) => host === undefined || candidate.host === host,
        );
        const scope = ref.scope.trim();
        const own = onHost.find(
          (candidate) => candidate.scope.toLowerCase() === scope.toLowerCase(),
        );
        const served = own ?? onHost[0];
        if (served === undefined) {
          return Effect.fail(new IssueUnavailableError({ reason: "tracker-unsupported" }));
        }
        return Effect.succeed({ ...served, scope, writable: own !== undefined });
      }),
    );

  const requireWritable = (ref: IssueScopeRef, operation: string) =>
    requireProject(ref).pipe(
      Effect.filterOrFail(
        (project) => project.writable,
        () => readOnly(operation),
      ),
    );

  const list: IssueService["Service"]["list"] = Effect.fn("IssueService.list")(function* (input) {
    // Trackers name every host so a client narrowed to one can switch to another; only the
    // narrowed host's scopes are read.
    const { supported, unimplemented } = yield* listProjects({
      projectId: input.projectId,
      excludeProjectIds: input.excludeProjectIds,
    });
    const host = input.host?.trim().toLowerCase();
    const limit = input.limit ?? DEFAULT_LIST_LIMIT;
    // Worktrees of one repository are separate projects, and every repository of an Azure
    // DevOps project shares its work items; read each scope once.
    const scopes = new Map<string, SupportedProject>();
    for (const project of supported) {
      if (host !== undefined && project.host !== host) continue;
      const key = listCursorKey(project);
      if (!scopes.has(key)) scopes.set(key, project);
    }
    const continuing = input.cursors;
    const viewers = new Map<string, string>();
    const trackers = new Map<string, IssueTrackerSummary>();
    const entries: IssueListEntry[] = [];
    const errors: IssueListProjectError[] = [];
    const nextCursors: Record<string, string> = {};

    const involvement = input.involvement ?? "all";
    const sort = input.sort ?? "updated";
    // Only a narrowed listing compares against the signed-in account; `all` never reads it.
    const viewerRequests = new Map<string, Effect.Effect<string, IssueTrackerError>>();
    for (const project of involvement === "all" ? [] : scopes.values()) {
      const key = viewerKey(project);
      if (viewerRequests.has(key)) continue;
      const request = yield* Effect.cached(
        project.api
          .getViewer({ cwd: project.project.workspaceRoot, host: project.host })
          .pipe(Effect.tap((viewer) => Effect.sync(() => viewers.set(key, viewer)))),
      );
      viewerRequests.set(key, request);
    }

    // Every project counts once, including worktrees of a scope read once below and forks.
    const counted = new Set<string>();
    for (const project of supported) {
      const countKey = `${project.host} ${project.project.id}`;
      if (counted.has(countKey)) continue;
      counted.add(countKey);
      const existing = trackers.get(project.host);
      trackers.set(
        project.host,
        existing === undefined
          ? {
              tracker: project.tracker,
              host: project.host,
              projectCount: 1,
              capabilities: project.api.capabilities,
              configured: true,
              detail: null,
            }
          : { ...existing, projectCount: existing.projectCount + 1 },
      );
    }
    // Accounts of one tracker share its host: the host is unusable only when every account read
    // failed for want of a tool or credential. Otherwise a failure stays its projects' error.
    const hostAccounts = new Map<string, Set<string>>();
    for (const project of scopes.values()) {
      const accounts = hostAccounts.get(project.host) ?? new Set();
      hostAccounts.set(project.host, accounts.add(viewerKey(project)));
    }
    const unusable = new Map<string, string>();
    const usable = new Set<string>();
    const recordFailure = (project: SupportedProject, failure: IssueTrackerError) => {
      if (isTrackerUnusable(failure)) unusable.set(viewerKey(project), trackerDetail(failure));
      else usable.add(project.host);
    };

    yield* Effect.forEach(
      scopes.values(),
      (project) =>
        Effect.gen(function* () {
          const key = listCursorKey(project);
          // A continuation names the scopes it wants; one not named has nothing to add.
          if (continuing !== undefined && continuing[key] === undefined) return;
          const cursor = continuing?.[key];
          const filter: Effect.Effect<IssueInvolvementFilter, IssueTrackerError> =
            involvement === "all"
              ? Effect.succeed({ involvement })
              : viewerRequests
                  .get(viewerKey(project))!
                  .pipe(Effect.map((viewer) => ({ involvement, viewer })));
          const page = yield* filter.pipe(
            Effect.flatMap((filter) =>
              project.api.listIssues({
                ...filter,
                cwd: project.project.workspaceRoot,
                host: project.host,
                scope: project.scope,
                state: input.state,
                limit,
                query: input.query,
                labels: input.labels,
                assignee: input.assignee,
                sort,
                cursor,
                refresh: input.refresh,
              }),
            ),
            Effect.result,
          );
          if (page._tag === "Failure") {
            if (cursor !== undefined) nextCursors[key] = cursor;
            recordFailure(project, page.failure);
            // A fork or a multi-team binding reads several scopes; the failure names its own.
            const forked = supported.some(
              (other) => other.project.id === project.project.id && other.scope !== project.scope,
            );
            errors.push({
              projectId: project.project.id,
              projectTitle: forked
                ? `${project.project.title} (${project.scope})`
                : project.project.title,
              message: trackerDetail(page.failure),
            });
            return;
          }
          usable.add(project.host);
          for (const item of page.success.items) {
            entries.push({
              ...item,
              tracker: project.tracker,
              host: project.host,
              scope: project.scope,
              projectId: project.project.id,
              projectTitle: project.project.title,
            });
          }
          if (page.success.nextCursor !== null) nextCursors[key] = page.success.nextCursor;
        }),
      { concurrency: SCOPE_CONCURRENCY, discard: true },
    );

    for (const [host, accounts] of hostAccounts) {
      const details = [...accounts].map((account) => unusable.get(account));
      const existing = trackers.get(host);
      if (existing === undefined || usable.has(host) || details.includes(undefined)) continue;
      trackers.set(host, { ...existing, configured: false, detail: details[0] ?? null });
    }
    for (const [host, { tracker, projectIds }] of unimplemented) {
      trackers.set(host, {
        tracker,
        host,
        projectCount: projectIds.size,
        capabilities: null,
        configured: false,
        detail: "Issues cannot be browsed for this host yet.",
      });
    }
    const sortField = sort === "created" ? "createdAt" : "updatedAt";
    const parseSortTime = (iso: string) => {
      const time = Date.parse(iso);
      return Number.isNaN(time) ? Number.NEGATIVE_INFINITY : time;
    };
    entries.sort((a, b) => {
      const aTime = parseSortTime(a[sortField]);
      const bTime = parseSortTime(b[sortField]);
      if (aTime !== bTime) return bTime > aTime ? 1 : -1;
      return b[sortField].localeCompare(a[sortField]);
    });
    return {
      viewers: Object.fromEntries(viewers),
      trackers: [...trackers.values()],
      entries,
      errors,
      nextCursors,
      scopes: [...scopes.values()].map((project) => ({
        projectId: project.project.id,
        tracker: project.tracker,
        host: project.host,
        scope: project.scope,
      })),
    };
  });

  /** Where the tracker finds `input`'s issue when `project` serves it. */
  const issueTarget = (project: SupportedProject, input: IssueRef) => ({
    cwd: project.project.workspaceRoot,
    host: project.host,
    scope: project.scope,
    key: input.key,
    workspace: input.workspace,
  });

  const toDetail = (
    project: SupportedProject & { readonly writable: boolean },
    issue: TrackerIssueDetail,
  ): IssueDetail => ({
    ...issue,
    tracker: project.tracker,
    host: project.host,
    scope: project.scope,
    projectId: project.project.id,
    projectTitle: project.project.title,
    writable: project.writable,
    capabilities: project.api.capabilities,
  });

  const detail: IssueService["Service"]["detail"] = (input) =>
    requireProject(input).pipe(
      Effect.tap((project) =>
        input.refresh === true ? invalidateDetail(project, input) : Effect.void,
      ),
      Effect.flatMap((project) =>
        Cache.get(
          details,
          new TrackerRead(
            detailCacheKey(project, input),
            project.api
              .getIssue(issueTarget(project, input))
              .pipe(Effect.mapError(toIssueError("detail"))),
          ),
        ).pipe(Effect.map((issue) => toDetail(project, issue))),
      ),
    );

  const summary: IssueService["Service"]["summary"] = (input) =>
    requireProject(input).pipe(
      Effect.flatMap((project) => {
        const target = issueTarget(project, input);
        return Cache.get(
          summaries,
          new TrackerRead(
            detailCacheKey(project, input),
            (project.api.getIssueSummary?.(target) ?? project.api.getIssue(target)).pipe(
              Effect.mapError(toIssueError("summary")),
            ),
          ),
        ).pipe(
          // Named fields, so a tracker's fuller read never rides along to the client.
          Effect.map((issue): IssueSummary => ({
            tracker: project.tracker,
            host: project.host,
            scope: project.scope,
            key: issue.key,
            displayKey: issue.displayKey,
            title: issue.title,
            url: issue.url,
            author: issue.author,
            state: issue.state,
            createdAt: issue.createdAt,
            updatedAt: issue.updatedAt,
            closedAt: issue.closedAt,
            assignees: issue.assignees,
            labels: issue.labels,
            commentCount: issue.commentCount,
          })),
        );
      }),
    );

  const comments: IssueService["Service"]["comments"] = (input) =>
    requireProject(input).pipe(
      Effect.flatMap((project) =>
        project.api
          .getComments({
            ...issueTarget(project, input),
            cursor: input.cursor,
          })
          .pipe(Effect.mapError(toIssueError("comments"))),
      ),
    );

  const templates: IssueService["Service"]["templates"] = (input) =>
    requireProject(input).pipe(
      Effect.flatMap((project) =>
        project.api.listTemplates === undefined || !project.api.capabilities.templates
          ? Effect.fail(unsupported("templates", "issue templates"))
          : project.api
              .listTemplates({
                cwd: project.project.workspaceRoot,
                host: project.host,
                scope: project.scope,
                workspace: input.workspace,
              })
              .pipe(Effect.mapError(toIssueError("templates"))),
      ),
    );

  const candidates: IssueService["Service"]["candidates"] = (input) =>
    requireProject(input).pipe(
      Effect.flatMap((project) =>
        project.api.listCandidates === undefined ||
        !(project.api.capabilities.labels || project.api.capabilities.assignees)
          ? Effect.fail(unsupported("candidates", "labels or assignees"))
          : project.api
              .listCandidates({
                cwd: project.project.workspaceRoot,
                host: project.host,
                scope: project.scope,
                workspace: input.workspace,
                ...(input.query?.trim() ? { query: input.query.trim() } : {}),
              })
              .pipe(Effect.mapError(toIssueError("candidates"))),
      ),
    );

  const create: IssueService["Service"]["create"] = (input) =>
    requireWritable(input, "create").pipe(
      Effect.flatMap((project) =>
        project.api.create === undefined || !project.api.capabilities.create
          ? Effect.fail(unsupported("create", "filing issues"))
          : project.api
              .create({
                cwd: project.project.workspaceRoot,
                host: project.host,
                scope: project.scope,
                title: input.title,
                body: input.body,
                labels: input.labels,
                assignees: input.assignees,
              })
              .pipe(Effect.mapError(toIssueError("create"))),
      ),
    );

  const comment: IssueService["Service"]["comment"] = (input) =>
    requireWritable(input, "comment").pipe(
      Effect.flatMap((project) => {
        if (input.body.trim().length === 0) {
          return Effect.fail(
            new IssueOperationError({ operation: "comment", detail: "A comment needs some text." }),
          );
        }
        if (project.api.comment === undefined || !project.api.capabilities.comment) {
          return Effect.fail(unsupported("comment", "comments"));
        }
        if (input.audience === "internal" && project.api.capabilities.internalComments !== true) {
          return Effect.fail(
            new IssueOperationError({
              operation: "comment",
              detail:
                "This tracker has no internal notes: everyone who can see the issue sees its comments. Leave the audience out to post one.",
            }),
          );
        }
        return project.api
          .comment({
            ...issueTarget(project, input),
            body: input.body,
            audience: input.audience,
          })
          .pipe(
            Effect.mapError(toIssueError("comment")),
            Effect.ensuring(invalidateDetail(project, input)),
          );
      }),
    );

  const update: IssueService["Service"]["update"] = (input) =>
    requireWritable(input, "update").pipe(
      Effect.flatMap((project) => {
        const { capabilities } = project.api;
        const wantsWords = input.title !== undefined || input.body !== undefined;
        if (!wantsWords && input.labels === undefined && input.assignees === undefined) {
          return Effect.fail(
            new IssueOperationError({ operation: "update", detail: "Nothing to change." }),
          );
        }
        if (project.api.update === undefined) return Effect.fail(unsupported("update", "edits"));
        if (wantsWords && !capabilities.edit)
          return Effect.fail(unsupported("update", "editing the title or body"));
        if (input.labels !== undefined && !capabilities.labels)
          return Effect.fail(unsupported("update", "labels"));
        if (input.assignees !== undefined && !capabilities.assignees)
          return Effect.fail(unsupported("update", "assignees"));
        return project.api
          .update({
            ...issueTarget(project, input),
            title: input.title,
            body: input.body,
            labels: input.labels,
            assignees: input.assignees,
          })
          .pipe(
            Effect.mapError(toIssueError("update")),
            Effect.ensuring(invalidateDetail(project, input)),
          );
      }),
    );

  const setState: IssueService["Service"]["setState"] = (input) =>
    requireWritable(input, "setState").pipe(
      Effect.flatMap((project) => {
        if (project.api.setState === undefined)
          return Effect.fail(unsupported("setState", "changing an issue's state"));
        return project.api
          .setState({
            ...issueTarget(project, input),
            state: input.state,
          })
          .pipe(
            Effect.mapError(toIssueError("setState")),
            Effect.ensuring(invalidateDetail(project, input)),
          );
      }),
    );

  const react: IssueService["Service"]["react"] = (input) =>
    requireWritable(input, "react").pipe(
      Effect.flatMap((project) =>
        project.api.setReaction === undefined || !project.api.capabilities.reactions
          ? Effect.fail(unsupported("react", "reactions"))
          : project.api
              .setReaction({
                ...issueTarget(project, input),
                subjectId: input.subjectId,
                content: input.content,
                reacted: input.reacted,
              })
              .pipe(
                Effect.mapError(toIssueError("react")),
                Effect.ensuring(invalidateDetail(project, input)),
              ),
      ),
    );

  const projectScope: IssueService["Service"]["projectScope"] = Effect.fn(
    "IssueService.projectScope",
  )(function* (projectId, select) {
    const { supported } = yield* listProjects({ projectId });
    if (supported.length === 0) {
      return yield* new IssueUnavailableError({ reason: "tracker-unsupported" });
    }
    const requested = select?.scope?.trim().toLowerCase();
    const key = select?.key?.toLowerCase();
    // Agents also write a scope as `<tracker>:<scope>`, such as linear:OPS.
    const separator = requested?.indexOf(":") ?? -1;
    const tracker = separator > 0 ? requested?.slice(0, separator) : undefined;
    const prefixed = requested?.slice(separator + 1).trim();
    const named =
      requested === undefined
        ? undefined
        : (supported.find((candidate) => candidate.scope.toLowerCase() === requested) ??
          supported.find(
            (candidate) =>
              candidate.tracker === tracker && candidate.scope.toLowerCase() === prefixed,
          ));
    // A key that names its own scope, such as OPS-12, wins over another scope of the same tracker.
    const own =
      supported.find(
        (candidate) =>
          (named === undefined || candidate.tracker === named.tracker) &&
          key?.startsWith(`${candidate.scope.toLowerCase()}-`),
      ) ??
      named ??
      (requested === undefined ? supported[0] : undefined);
    if (own === undefined) {
      return yield* new IssueOperationError({
        operation: "scope",
        reason: "not-found",
        detail: `This project does not track ${select?.scope?.trim()}. Use one of: ${[...new Set(supported.map((candidate) => candidate.scope))].join(", ")}.`,
      });
    }
    return { projectId, tracker: own.tracker, host: own.host, scope: own.scope };
  });

  /** A host whose hostname does not name it, such as GitHub Enterprise Server, is vouched for by a project on it. */
  const parseOnProjectHost = (value: string) => {
    const url = parseUrl(value);
    if (url === null) return Effect.succeed(null);
    return listProjects({ host: url.hostname, authority: url.host.toLowerCase() }).pipe(
      Effect.map(({ supported, unimplemented }) => {
        // A tracker not served here still names the link, so it can say why it cannot resolve.
        const trackers = new Set([
          ...supported.map((project) => project.tracker),
          ...[...unimplemented.values()].map((entry) => entry.tracker),
        ]);
        for (const tracker of trackers) {
          const parsed = sourceControlDefinitions.parseIssueUrl(
            value,
            SourceControlProviderKind.make(tracker),
          );
          if (parsed !== null) return parsed;
        }
        return null;
      }),
    );
  };

  const resolveUrl: IssueService["Service"]["resolveUrl"] = Effect.fn("IssueService.resolveUrl")(
    function* (url, preferredProjectId) {
      const recognized = sourceControlDefinitions.parseIssueUrl(url);
      const standalone =
        recognized === null ? findStandaloneIssueUrl(registry.standalone, url) : null;
      const gitHost = recognized ?? (standalone === null ? yield* parseOnProjectHost(url) : null);
      const parsed =
        gitHost === null
          ? standalone
          : { tracker: IssueTrackerKind.make(gitHost.definition.kind), link: gitHost.link };
      if (parsed === null) {
        return yield* new IssueOperationError({
          operation: "resolveUrl",
          reason: "not-found",
          detail: "This is not a recognised issue URL.",
        });
      }
      const { tracker, link } = parsed;
      // A link names its server's port; a binding does only when its remote or web URL did.
      const authority = new URL(url).host.toLowerCase();
      const { supported, unimplemented } = yield* listProjects({ host: link.host, authority });
      const onHost = supported.filter((project) => project.tracker === tracker);
      const onAuthority = onHost.filter((project) => project.host === authority);
      const onTracker = onAuthority.length > 0 ? onAuthority : onHost;
      const preferFirst = (projects: ReadonlyArray<SupportedProject>) =>
        projects.find((project) => project.project.id === preferredProjectId) ?? projects[0];
      const scope = link.scope.toLowerCase();
      const serve = (candidates: ReadonlyArray<SupportedProject>) => {
        const own =
          preferFirst(candidates.filter((project) => project.scope.toLowerCase() === scope)) ??
          preferFirst(
            candidates.filter(
              (project) => project.scope.toLowerCase() === gitHost?.link.scopeAlias,
            ),
          );
        const lender = own ?? preferFirst(candidates);
        if (lender === undefined) return null;
        const served = {
          ...lender,
          scope: own === undefined ? link.scope : lender.scope,
          writable: own !== undefined,
        };
        const resolved = {
          projectId: lender.project.id,
          tracker: lender.tracker,
          host: lender.host,
          scope: served.scope,
          key: link.key,
          writable: served.writable,
        };
        return { lender, served, resolved };
      };
      if (standalone === null) {
        const found = serve(onTracker);
        if (found === null) {
          // A project is on the host, but no tracker serves it here, as with its feature flag off.
          if ([authority, link.host].some((host) => unimplemented.get(host)?.tracker === tracker)) {
            return yield* new IssueUnavailableError({ reason: "tracker-unsupported", tracker });
          }
          return yield* new IssueOperationError({
            operation: "resolveUrl",
            detail: `No project in Onicode is on ${link.host}. Add any ${link.host} project to read its issues.`,
          });
        }
        return found.resolved;
      }
      // Team keys repeat across workspaces, and each account reaches one: try the accounts in
      // preference order. The tracker refuses an issue outside the workspace the URL names, and
      // its canonical URL names the workspace by the key later calls should carry.
      // The exact team before a fallback, as `serve` and the client resolver pick it.
      const exact = onTracker.filter((project) => project.scope.toLowerCase() === scope);
      const preferred = (projects: ReadonlyArray<SupportedProject>) => [
        ...projects.filter((project) => project.project.id === preferredProjectId),
        ...projects,
      ];
      // A bound project lends the tracker read-only for its other teams; a caller with no binding,
      // the No project included, serves them itself, so only an exact team's binding goes first.
      const callerUnbound =
        preferredProjectId !== undefined &&
        !onTracker.some((project) => project.project.id === preferredProjectId);
      const lenders = callerUnbound ? exact : onTracker;
      const ordered = [...preferred(exact), ...preferred(lenders)];
      const accounts = [...new Set(ordered.map((project) => project.accountId))];
      if (accounts.length === 0 && preferredProjectId === undefined) {
        const { displayName, scopeNoun } = standalone.api.standalone;
        return yield* new IssueOperationError({
          operation: "resolveUrl",
          detail: `No project in Onicode reads ${displayName} issues. Connect a ${displayName} account in Settings and bind a project to one of its ${scopeNoun}s to read this issue.`,
        });
      }
      let failure: IssueError | null = null;
      const tried = new Set<IssueTrackerAccountId | null>();
      for (const accountId of accounts) {
        const found = serve(lenders.filter((project) => project.accountId === accountId));
        if (found === null) continue;
        tried.add(accountId);
        const read = yield* found.lender.api
          .getIssue({
            cwd: found.lender.project.workspaceRoot,
            host: found.lender.host,
            scope: link.scope,
            key: link.key,
            workspace: standalone.link.workspace,
          })
          .pipe(Effect.mapError(toIssueError("resolveUrl")), Effect.result);
        if (read._tag === "Failure") {
          // Another workspace's account not having the key is a miss, not why the link failed.
          if (failure === null || isNotFound(failure)) failure = read.failure;
          continue;
        }
        const canonicalUrl = parseUrl(read.success.url);
        const canonical = canonicalUrl === null ? null : standalone.api.parseIssueUrl(canonicalUrl);
        return {
          ...found.resolved,
          workspace: (canonical?.workspace ?? standalone.link.workspace).toLowerCase(),
          issue: toDetail(found.served, read.success),
        };
      }
      // Only a miss sends the link to the caller's other accounts: a refusal such as a rate limit
      // is why it failed.
      const missed =
        failure === null || isNotFound(failure) || failure._tag === "IssueUnavailableError";
      if (callerUnbound && missed) {
        const served = yield* requireStandaloneProject(
          { projectId: preferredProjectId, tracker, scope: link.scope },
          standalone.api,
          tried,
        );
        const issue = yield* served.api
          .getIssue({
            cwd: served.project.workspaceRoot,
            host: served.host,
            scope: link.scope,
            key: link.key,
            workspace: standalone.link.workspace,
          })
          .pipe(Effect.mapError(toIssueError("resolveUrl")));
        const canonicalUrl = parseUrl(issue.url);
        const canonical = canonicalUrl === null ? null : standalone.api.parseIssueUrl(canonicalUrl);
        return {
          projectId: served.project.id,
          tracker: served.tracker,
          host: served.host,
          scope: served.scope,
          key: link.key,
          writable: served.writable,
          workspace: (canonical?.workspace ?? standalone.link.workspace).toLowerCase(),
          issue: toDetail(served, issue),
        };
      }
      return yield* (
        failure ??
        new IssueOperationError({
          operation: "resolveUrl",
          reason: "not-found",
          detail: "This issue URL does not belong to the connected workspace.",
        })
      );
    },
  );

  return IssueService.of({
    list,
    detail: traced("detail", detail),
    summary: traced("summary", summary),
    comments: traced("comments", comments),
    templates: traced("templates", templates),
    candidates: interactive(traced("candidates", candidates)),
    create: interactive(traced("create", create)),
    comment: interactive(traced("comment", comment)),
    update: interactive(traced("update", update)),
    setState: interactive(traced("setState", setState)),
    react: interactive(traced("react", react)),
    projectScope,
    resolveUrl,
  });
});

export const layer = Layer.effect(IssueService, make);
