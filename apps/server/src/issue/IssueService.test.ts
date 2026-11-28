import { describe, expect, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as SourceControlRateLimit from "@onicode/source-control-core/server/SourceControlRateLimit";
import {
  IssueTrackerAccountId,
  IssueTrackerKind,
  type IssueTrackerProjectBinding,
  SourceControlProviderKind,
  type OrchestrationProjectShell,
  type ProjectId,
} from "@onicode/contracts";
import {
  IssueTrackerError,
  type IssueTrackerApi,
  type StandaloneIssueTrackerApi,
  type TrackerIssueDetail,
} from "@onicode/source-control-core/server/IssueTracker";
import { IssueTrackerAccount } from "@onicode/source-control-core/server/IssueTrackerHost";

import { PersistenceSqlError } from "../persistence/Errors.ts";
import * as IssueTrackerBindings from "../persistence/IssueTrackerBindings.ts";
import { normalizeGitRemoteUrl } from "@onicode/shared/git";

import * as ProjectService from "../project/ProjectService.ts";
import * as RepositoryIdentityResolver from "../project/RepositoryIdentityResolver.ts";
import * as IssueService from "./IssueService.ts";
import * as IssueTrackerRegistry from "./IssueTrackerRegistry.ts";

const GITHUB = IssueTrackerKind.make("github");
const LINEAR = IssueTrackerKind.make("linear");

/** A standalone tracker shaped like Linear: its own host, team keys for scopes, `ENG-1` keys. */
function fakeStandaloneTracker(
  overrides: Partial<IssueTrackerApi> = {},
): StandaloneIssueTrackerApi {
  return {
    ...fakeTracker({
      getViewer: () => Effect.succeed("usr_julius"),
      // As Linear does: the connected workspace, or a key it was renamed from.
      getIssue: (input) =>
        input.workspace === undefined ||
        ["acme", "acme-old"].includes(input.workspace.toLowerCase())
          ? Effect.succeed({
              ...issue(input.key),
              url: `https://linear.app/acme/issue/${input.key}/canonical-title`,
              displayKey: input.key,
              viewer: "usr_julius",
            })
          : Effect.fail(
              new IssueTrackerError({
                tracker: LINEAR,
                operation: "getIssue",
                reason: "not-found",
                detail: "This issue link names another Linear workspace than the connected one.",
              }),
            ),
      ...overrides,
    }),
    kind: LINEAR,
    standalone: {
      tracker: LINEAR,
      displayName: "Linear",
      host: "linear.app",
      scopeNoun: "team",
      credential: { label: "Personal API key", helpUrl: null },
    },
    parseIssueUrl: (url) => {
      const links = new Map([
        ["https://linear.app/acme/issue/ENG-1/old-title", "acme"],
        ["https://linear.app/acme/issue/ENG-1/issue-title", "acme"],
        ["https://linear.app/acme/issue/ENG-1/canonical-title", "acme"],
        ["https://linear.app/other-workspace/issue/ENG-1/old-title", "other-workspace"],
        ["https://linear.app/Acme/issue/ENG-1/old-title", "Acme"],
        ["https://linear.app/acme-old/issue/ENG-1/old-title", "acme-old"],
        ["https://linear.app/other-workspace/issue/ENG-1/canonical-title", "other-workspace"],
      ]);
      const workspace = links.get(url.href);
      return workspace === undefined
        ? null
        : { host: "linear.app", workspace, scope: "ENG", key: "ENG-1" };
    },
    probeCredential: () => Effect.succeed({ account: "julius", scopes: [] }),
  };
}

function project(input: {
  readonly id: string;
  readonly repository?: string;
  readonly provider?: string;
  readonly host?: string;
}): OrchestrationProjectShell {
  const host = input.host ?? "github.com";
  return {
    id: input.id as ProjectId,
    title: input.id,
    workspaceRoot: `/w/${input.id}`,
    ...(input.repository
      ? {
          repositoryIdentity: {
            canonicalKey: normalizeGitRemoteUrl(`https://${host}/${input.repository}.git`),
            locator: {
              source: "git-remote" as const,
              remoteName: "origin",
              remoteUrl: `https://${host}/${input.repository}.git`,
            },
            provider: SourceControlProviderKind.make(input.provider ?? "github"),
            displayName: input.repository,
          },
        }
      : {}),
    defaultModelSelection: null,
    scripts: [],
    createdAt: "2026-07-01T00:00:00Z",
    updatedAt: "2026-07-01T00:00:00Z",
  };
}

function issue(key: string, updatedAt = "2026-07-02T00:00:00Z"): TrackerIssueDetail {
  return {
    key,
    displayKey: `#${key}`,
    title: `Issue ${key}`,
    url: `https://github.com/acme/web/issues/${key}`,
    author: null,
    state: { category: "open", name: "open" },
    createdAt: "2026-07-01T00:00:00Z",
    updatedAt,
    closedAt: null,
    assignees: [],
    labels: [],
    commentCount: 0,
    body: "",
    viewer: "julius",
    viewerPermissions: { setState: true, comment: true, edit: true, labels: true, assignees: true },
    states: [],
    comments: [],
    commentsCursor: null,
    events: [],
    linkedPullRequests: [],
  };
}

function fakeTracker(overrides: Partial<IssueTrackerApi> = {}): IssueTrackerApi {
  return {
    kind: GITHUB,
    capabilities: {
      create: true,
      edit: true,
      comment: true,
      labels: true,
      assignees: true,
      reactions: false,
      templates: false,
      linkedPullRequests: true,
      events: true,
      search: true,
    },
    getViewer: () => Effect.succeed("julius"),
    listIssues: () => Effect.succeed({ items: [], nextCursor: null }),
    getIssue: (input) => Effect.succeed(issue(input.key)),
    getComments: () => Effect.succeed({ comments: [], nextCursor: null }),
    comment: () => Effect.void,
    update: () => Effect.void,
    setState: () => Effect.void,
    ...overrides,
  };
}

function makeService(input: {
  readonly projects: ReadonlyArray<OrchestrationProjectShell>;
  readonly trackers: ReadonlyArray<IssueTrackerApi>;
  readonly bindings?: Record<string, IssueTrackerProjectBinding>;
  readonly readBindings?: ReturnType<
    IssueTrackerBindings.IssueTrackerBindingsRepository["Service"]["list"]
  >;
  /** Accounts added beside each tracker's first. */
  readonly accounts?: ReadonlyArray<{
    readonly tracker: IssueTrackerKind;
    readonly accountId: IssueTrackerAccountId;
  }>;
}) {
  return IssueService.IssueService.pipe(
    Effect.provide(
      IssueService.layer.pipe(
        Layer.provide(
          Layer.mergeAll(
            Layer.mock(IssueTrackerBindings.IssueTrackerBindingsRepository)({
              list: () =>
                input.readBindings ??
                Effect.succeed(
                  new Map(
                    Object.entries(input.bindings ?? {}).map(([id, binding]) => [
                      id as ProjectId,
                      binding,
                    ]),
                  ),
                ),
              listAccounts: Effect.succeed(input.accounts ?? []),
            }),
            Layer.succeed(
              IssueTrackerRegistry.IssueTrackerRegistry,
              IssueTrackerRegistry.fromTrackers(input.trackers),
            ),
            Layer.mock(ProjectService.ProjectService)({
              listShells: () => Effect.succeed(input.projects),
              getShell: (projectId) =>
                Effect.succeed(
                  Option.fromNullishOr(input.projects.find((p) => p.id === projectId)),
                ),
            }),
            Layer.mock(RepositoryIdentityResolver.RepositoryIdentityResolver)({
              resolve: () => Effect.succeed(null),
            }),
          ),
        ),
      ),
    ),
  );
}

describe("IssueService", () => {
  it.effect("places a project where its host keeps its issues", () =>
    Effect.gen(function* () {
      const azure = IssueTrackerKind.make("azure-devops");
      const service = yield* makeService({
        projects: [
          project({
            id: "p",
            provider: "azure-devops",
            host: "dev.azure.com",
            repository: "acme/Web App/_git/web",
          }),
        ],
        trackers: [fakeTracker({ kind: azure })],
      });
      expect(yield* service.projectScope("p" as ProjectId)).toEqual({
        projectId: "p",
        tracker: "azure-devops",
        host: "dev.azure.com",
        scope: "acme/Web App",
      });
    }),
  );

  it.effect("reads another repository on the same host but refuses to write to it", () =>
    Effect.gen(function* () {
      const writes: string[] = [];
      const service = yield* makeService({
        projects: [project({ id: "web", repository: "acme/web" })],
        trackers: [
          fakeTracker({
            comment: (input) =>
              Effect.sync(() => {
                writes.push(`${input.scope}#${input.key}`);
              }),
          }),
        ],
      });
      const own = { projectId: "web" as ProjectId, tracker: GITHUB, scope: "acme/web", key: "1" };
      const external = { ...own, scope: "other/backend", key: "7" };

      expect((yield* service.detail(own)).writable).toBe(true);
      // The external issue is served by the project's checkout and credentials, read-only.
      const detail = yield* service.detail(external);
      expect(detail).toMatchObject({ writable: false, scope: "other/backend", key: "7" });

      yield* service.comment({ ...own, body: "ok" });
      const refused = yield* service.comment({ ...external, body: "no" }).pipe(Effect.result);
      expect(refused).toMatchObject({
        _tag: "Failure",
        failure: { _tag: "IssueOperationError", reason: "read-only" },
      });
      expect(writes).toEqual(["acme/web#1"]);

      // A link's port picks the server: the project bound with it, not a default-port namesake.
      const ported = yield* makeService({
        projects: [
          project({ id: "plain", repository: "acme/web", provider: "forgejo", host: "gitea.test" }),
          project({
            id: "ported",
            repository: "acme/web",
            provider: "forgejo",
            host: "gitea.test:8443",
          }),
        ],
        trackers: [fakeTracker({ kind: IssueTrackerKind.make("forgejo") })],
      });
      expect(yield* ported.resolveUrl("https://gitea.test:8443/acme/web/issues/3")).toMatchObject({
        projectId: "ported",
        host: "gitea.test:8443",
        key: "3",
        writable: true,
      });
      expect(yield* ported.resolveUrl("https://gitea.test/acme/web/issues/3")).toMatchObject({
        projectId: "plain",
        host: "gitea.test",
      });

      // A GitHub Enterprise Server's hostname need not say "github"; its project vouches for it.
      const enterprise = yield* makeService({
        projects: [project({ id: "ghes", repository: "acme/web", host: "code.acme.test" })],
        trackers: [fakeTracker()],
      });
      expect(
        yield* enterprise.resolveUrl("https://code.acme.test/acme/web/issues/7"),
      ).toMatchObject({ projectId: "ghes", host: "code.acme.test", key: "7", writable: true });

      // The same holds for a self-hosted Forgejo.
      const forgejo = yield* makeService({
        projects: [
          project({
            id: "forge",
            repository: "acme/web",
            provider: "forgejo",
            host: "code.acme.test",
          }),
        ],
        trackers: [fakeTracker({ kind: IssueTrackerKind.make("forgejo") })],
      });
      expect(yield* forgejo.resolveUrl("https://code.acme.test/acme/web/issues/7")).toMatchObject({
        projectId: "forge",
        host: "code.acme.test",
        key: "7",
      });

      // A project's host whose tracker is not offered here, as TanStack Source with its flag off,
      // whether the host is public or vouched for by the project alone.
      const unoffered = yield* makeService({
        projects: [
          project({ id: "lab", repository: "acme/web", provider: "gitlab", host: "gitlab.com" }),
          project({ id: "own", repository: "acme/web", provider: "gitlab", host: "git.acme.test" }),
        ],
        trackers: [fakeTracker()],
      });
      for (const url of [
        "https://gitlab.com/acme/web/-/issues/3",
        "https://git.acme.test/acme/web/-/issues/3",
      ]) {
        expect(yield* Effect.flip(unoffered.resolveUrl(url))).toMatchObject({
          _tag: "IssueUnavailableError",
          reason: "tracker-unsupported",
          tracker: "gitlab",
        });
      }

      // A fork lists its upstream's issues and its own, counted once, and may write to both.
      const fork = project({ id: "fork", repository: "acme/web" });
      const forked = yield* makeService({
        projects: [
          {
            ...fork,
            repositoryIdentity: {
              ...fork.repositoryIdentity!,
              origin: { canonicalKey: "github.com/me/web" },
            },
          },
        ],
        trackers: [
          fakeTracker({
            listIssues: (input) =>
              Effect.succeed({ items: [issue(input.scope)], nextCursor: null }),
            comment: (input) =>
              Effect.sync(() => {
                writes.push(`${input.scope}#${input.key}`);
              }),
          }),
        ],
      });
      const listed = yield* forked.list({ state: "open" });
      expect(listed.entries.map((entry) => entry.scope).sort()).toEqual(["acme/web", "me/web"]);
      expect(listed.trackers).toEqual([expect.objectContaining({ projectCount: 1 })]);
      yield* forked.comment({
        ...own,
        projectId: "fork" as ProjectId,
        scope: "me/web",
        body: "ok",
      });
      expect(writes).toEqual(["acme/web#1", "me/web#1"]);

      // Another host is not reachable through this project's credentials at all.
      const elsewhere = yield* Effect.flip(service.detail({ ...own, host: "ghe.example.test" }));
      expect(elsewhere).toMatchObject({
        _tag: "IssueUnavailableError",
        reason: "tracker-unsupported",
      });
    }),
  );

  it.effect(
    "reads a summary without the conversation, trimming a tracker that has no summary",
    () =>
      Effect.gen(function* () {
        const reads: string[] = [];
        const conversation = {
          ...issue("4"),
          body: "A long body",
          comments: [
            { id: "c1", author: null, body: "hi", createdAt: issue("4").createdAt, url: null },
          ],
        };
        const ref = { projectId: "web" as ProjectId, tracker: GITHUB, scope: "acme/web", key: "4" };
        const expected = {
          tracker: "github",
          host: "github.com",
          scope: "acme/web",
          key: "4",
          displayKey: "#4",
          title: "Issue 4",
          commentCount: 0,
        };
        const projects = [project({ id: "web", repository: "acme/web" })];

        const withSummary = yield* makeService({
          projects,
          trackers: [
            fakeTracker({
              getIssue: () => Effect.sync(() => (reads.push("detail"), conversation)),
              getIssueSummary: () => Effect.sync(() => (reads.push("summary"), conversation)),
            }),
          ],
        });
        const summary = yield* withSummary.summary(ref);
        expect(summary).toMatchObject(expected);
        expect(reads).toEqual(["summary"]);

        const withoutSummary = yield* makeService({
          projects,
          trackers: [fakeTracker({ getIssue: () => Effect.succeed(conversation) })],
        });
        const trimmed = yield* withoutSummary.summary(ref);
        expect(trimmed).toMatchObject(expected);
        expect(Object.keys(summary).sort()).toEqual(Object.keys(trimmed).sort());
        expect(trimmed).not.toHaveProperty("body");
        expect(trimmed).not.toHaveProperty("comments");
      }),
  );

  it.effect("names the host when no project is on a GitHub issue URL's host", () =>
    Effect.gen(function* () {
      const service = yield* makeService({
        projects: [project({ id: "folder" })],
        trackers: [fakeTracker()],
      });
      const failure = yield* Effect.flip(
        service.resolveUrl("https://github.com/acme/web/issues/1", "folder" as ProjectId),
      );
      expect(failure).toMatchObject({
        _tag: "IssueOperationError",
        operation: "resolveUrl",
        detail: expect.stringContaining("github.com"),
      });
    }),
  );

  it.effect("resolves both legacy Azure work item URLs through the bound project", () =>
    Effect.gen(function* () {
      const tracker = IssueTrackerKind.make("azure-devops");
      const host = "acme.visualstudio.com";
      const service = yield* makeService({
        projects: [
          project({ id: "api", repository: "Api/_git/api", provider: tracker, host }),
          project({
            id: "web",
            repository: "DefaultCollection/Web/_git/web",
            provider: tracker,
            host,
          }),
        ],
        trackers: [fakeTracker({ kind: tracker })],
      });
      for (const path of ["Web", "DefaultCollection/Web"]) {
        const ref = yield* service.resolveUrl(
          `https://${host}/${path}/_workitems/edit/9`,
          "api" as ProjectId,
        );
        expect(ref).toEqual({
          projectId: "web",
          tracker,
          host,
          scope: "DefaultCollection/Web",
          key: "9",
          writable: true,
        });
      }
    }),
  );

  it.effect("sends writes as interactive requests and reads as background ones", () =>
    Effect.gen(function* () {
      const interactive: Array<readonly [string, boolean]> = [];
      const record = (operation: string) =>
        Effect.gen(function* () {
          interactive.push([operation, yield* SourceControlRateLimit.Interactive]);
        });
      const service = yield* makeService({
        projects: [project({ id: "web", repository: "acme/web" })],
        trackers: [
          fakeTracker({
            getComments: () =>
              record("comments").pipe(Effect.as({ comments: [], nextCursor: null })),
            comment: () => record("comment"),
          }),
        ],
      });
      const ref = {
        projectId: "web" as ProjectId,
        tracker: GITHUB,
        scope: "acme/web",
        key: "42",
      };
      yield* service.comments({ ...ref, cursor: "next" });
      yield* service.comment({ ...ref, body: "Repro attached." });
      expect(interactive).toEqual([
        ["comments", false],
        ["comment", true],
      ]);
    }),
  );

  it.effect("shares one tracker read of an issue until a write or an explicit refresh", () =>
    Effect.gen(function* () {
      let reads = 0;
      let summaryReads = 0;
      const service = yield* makeService({
        projects: [
          project({ id: "web", repository: "acme/web" }),
          project({ id: "worktree", repository: "acme/web" }),
        ],
        trackers: [
          fakeTracker({
            getIssue: (input) => Effect.sync(() => (reads++, issue(input.key))),
            getIssueSummary: (input) => Effect.sync(() => (summaryReads++, issue(input.key))),
          }),
        ],
      });
      const ref = { projectId: "web" as ProjectId, tracker: GITHUB, scope: "acme/web", key: "42" };
      // Every client and project polling one issue costs the tracker one read.
      yield* Effect.all(
        [
          service.detail(ref),
          service.detail(ref),
          service.detail({ ...ref, projectId: "worktree" as ProjectId }),
        ],
        { concurrency: "unbounded" },
      );
      expect((yield* service.detail(ref)).projectId).toBe("web");
      expect(reads).toBe(1);
      yield* service.detail({ ...ref, key: "43" });
      expect(reads).toBe(2);
      // Another workspace's issue with the same key is another issue.
      yield* service.detail({ ...ref, workspace: "other" });
      expect(reads).toBe(3);
      // Linked rows poll the summary, which shares its reads the same way.
      yield* Effect.all([service.summary(ref), service.summary(ref)], {
        concurrency: "unbounded",
      });
      expect(summaryReads).toBe(1);

      yield* service.comment({ ...ref, body: "Repro attached." });
      yield* service.detail(ref);
      yield* service.summary(ref);
      expect(reads).toBe(4);
      expect(summaryReads).toBe(2);

      // An explicit refresh sees what changed on the tracker itself.
      yield* service.detail({ ...ref, refresh: true });
      yield* service.detail(ref);
      yield* service.summary(ref);
      expect(reads).toBe(5);
      expect(summaryReads).toBe(3);
    }),
  );

  it.effect("refuses what the tracker did not declare before calling it", () =>
    Effect.gen(function* () {
      let calls = 0;
      const service = yield* makeService({
        projects: [project({ id: "web", repository: "acme/web" })],
        trackers: [
          fakeTracker({
            setState: () =>
              Effect.sync(() => {
                calls += 1;
              }),
            setReaction: () =>
              Effect.sync(() => {
                calls += 1;
              }),
          }),
        ],
      });
      const ref = { projectId: "web" as ProjectId, tracker: GITHUB, scope: "acme/web", key: "1" };

      // The state is the tracker's own name, passed through: the tracker is what proves it.
      yield* service.setState({ ...ref, state: "In Review" });
      const reaction = yield* Effect.flip(
        service.react({ ...ref, content: "heart", reacted: true }),
      );
      expect(reaction._tag).toBe("IssueOperationError");
      expect(calls).toBe(1);

      // An internal note on a tracker without them says so, unless it has no comments at all.
      const internal = { ...ref, body: "Note", audience: "internal" as const };
      expect(yield* Effect.flip(service.comment(internal))).toMatchObject({
        detail: expect.stringContaining("This tracker has no internal notes"),
      });
      const commentless = yield* makeService({
        projects: [project({ id: "web", repository: "acme/web" })],
        trackers: [
          fakeTracker({ capabilities: { ...fakeTracker().capabilities, comment: false } }),
        ],
      });
      expect(yield* Effect.flip(commentless.comment(internal))).toMatchObject({
        detail: "This tracker does not support comments.",
      });
    }),
  );

  it.effect("deduplicates and pages repositories despite partial failures", () =>
    Effect.gen(function* () {
      const listed: string[] = [];
      let failContinuation = true;
      const service = yield* makeService({
        projects: [
          project({ id: "web", repository: "Acme/Web" }),
          project({ id: "web-worktree", repository: "acme/web" }),
          project({ id: "api", repository: "acme/api" }),
          project({ id: "docs", repository: "acme/docs" }),
          project({
            id: "gitlab",
            repository: "acme/gl",
            provider: "gitlab",
            host: "gitlab.com",
          }),
          project({ id: "plain" }),
        ],
        trackers: [
          fakeTracker({
            listIssues: (input) =>
              Effect.gen(function* () {
                listed.push(input.scope);
                if (input.scope === "acme/api")
                  return yield* new IssueTrackerError({
                    tracker: GITHUB,
                    operation: "listIssues",
                    reason: "failed",
                    // A tracker may fail without a detail; the wire error still needs one.
                    detail: " ",
                  });
                if (input.scope === "acme/docs")
                  return { items: [issue("3", "2026-07-04T00:00:00Z")], nextCursor: null };
                if (input.cursor === "next" && failContinuation) {
                  failContinuation = false;
                  return yield* new IssueTrackerError({
                    tracker: GITHUB,
                    operation: "listIssues",
                    reason: "failed",
                    detail: "try again",
                  });
                }
                if (input.cursor === "next")
                  return { items: [issue("4", "2026-07-01T00:00:00Z")], nextCursor: null };
                return {
                  items: [issue("1"), issue("2", "2026-07-05T00:00:00Z")],
                  nextCursor: "next",
                };
              }),
          }),
        ],
      });
      const result = yield* service.list({ state: "open" });
      expect(listed.sort()).toEqual(["Acme/Web", "acme/api", "acme/docs"]);
      expect(result.entries.map((entry) => [entry.projectId, entry.key])).toEqual([
        ["web", "2"],
        ["docs", "3"],
        ["web", "1"],
      ]);
      expect(
        result.entries.filter((entry) => entry.projectId === "web").map((entry) => entry.scope),
      ).toEqual(["Acme/Web", "Acme/Web"]);
      expect(result.nextCursors).toEqual({ "github.com acme/web": "next" });
      expect(result.errors).toEqual([
        { projectId: "api", projectTitle: "api", message: "The issue tracker request failed." },
      ]);
      expect(result.viewers).toEqual({});
      expect(result.trackers).toEqual([
        expect.objectContaining({ host: "github.com", projectCount: 4, configured: true }),
        expect.objectContaining({
          host: "gitlab.com",
          tracker: "gitlab",
          projectCount: 1,
          configured: false,
          capabilities: null,
        }),
      ]);

      // Narrowing to one host still names every tracker, so the client can switch between them.
      // Narrowing by involvement reads the account it compares against.
      const narrowed = yield* service.list({
        state: "open",
        host: "github.com",
        involvement: "assigned",
      });
      expect(narrowed.viewers).toEqual({ "github.com": "julius" });
      expect(narrowed.entries.every((entry) => entry.host === "github.com")).toBe(true);
      expect(narrowed.trackers.map((tracker) => tracker.host)).toEqual([
        "github.com",
        "gitlab.com",
      ]);

      // A continuation only reads the scopes it names.
      listed.length = 0;
      const failed = yield* service.list({
        state: "open",
        cursors: result.nextCursors,
      });
      expect(listed).toEqual(["Acme/Web"]);
      expect(failed.nextCursors).toEqual(result.nextCursors);
      expect(failed.errors).toEqual([
        { projectId: "web", projectTitle: "web", message: "try again" },
      ]);
      const next = yield* service.list({ state: "open", cursors: failed.nextCursors });
      expect(next.entries.map((entry) => [entry.projectId, entry.key])).toEqual([["web", "4"]]);
      expect(next.nextCursors).toEqual({});
      expect(next.errors).toEqual([]);

      // The cursor belongs to the scope, not to the project whose spelling first read it.
      listed.length = 0;
      const through = yield* service.list({
        state: "open",
        projectId: "web-worktree" as ProjectId,
        cursors: result.nextCursors,
      });
      expect(listed).toEqual(["acme/web"]);
      expect(through.entries.map((entry) => entry.key)).toEqual(["4"]);
    }),
  );
});

describe("IssueService with a standalone tracker", () => {
  it.effect("reads bindings once per listing and sees changes on the next listing", () =>
    Effect.gen(function* () {
      const web = project({ id: "web", repository: "acme/web" });
      const docs = project({ id: "docs", repository: "acme/docs" });
      let reads = 0;
      let scope = "ENG";
      const service = yield* makeService({
        projects: [web, docs],
        trackers: [fakeTracker(), fakeStandaloneTracker()],
        readBindings: Effect.sync(() => {
          reads += 1;
          return new Map([[web.id, { tracker: LINEAR, scope }]]);
        }),
      });

      const first = yield* service.list({ state: "open" });
      expect(first.scopes?.map((entry) => entry.scope)).toEqual(["ENG", "acme/web", "acme/docs"]);
      expect(reads).toBe(1);

      scope = "OPS";
      const next = yield* service.list({ state: "open", refresh: true });
      expect(next.scopes?.map((entry) => entry.scope)).toEqual(["OPS", "acme/web", "acme/docs"]);
      expect(reads).toBe(2);

      const missing = yield* service.list({ state: "open", projectId: "missing" as ProjectId });
      expect(missing.scopes).toEqual([]);
      expect(reads).toBe(2);
    }),
  );

  it.effect("reads each project's team as the account it is bound with", () =>
    Effect.gen(function* () {
      const asWhom = Effect.map(IssueTrackerAccount, (account) => account ?? "first");
      const fail = (reason: IssueTrackerError["reason"], retryAt?: number) =>
        new IssueTrackerError({
          tracker: LINEAR,
          operation: "read",
          reason,
          detail: reason,
          ...(retryAt === undefined ? {} : { retryAt }),
        });
      const twoAccounts = (overrides: Partial<IssueTrackerApi>) =>
        makeService({
          projects: [project({ id: "work" }), project({ id: "home" })],
          trackers: [fakeStandaloneTracker(overrides)],
          bindings: {
            work: { tracker: LINEAR, scope: "ENG" },
            home: {
              tracker: LINEAR,
              accountId: IssueTrackerAccountId.make("second"),
              scope: "ENG",
            },
          },
        });
      // As Linear does, each account refuses an issue outside its own workspace.
      const inOwnWorkspace = (workspace: string | undefined) =>
        Effect.flatMap(asWhom, (account) => {
          const own = account === "first" ? "acme" : "other-workspace";
          return workspace !== undefined && workspace.toLowerCase() !== own
            ? Effect.fail(fail("not-found"))
            : Effect.succeed({ account, workspace: own });
        });
      const readInOwnWorkspace: IssueTrackerApi["getIssue"] = (input) =>
        Effect.map(inOwnWorkspace(input.workspace), ({ account, workspace }) => ({
          ...issue(input.key),
          title: account,
          url: `https://linear.app/${workspace}/issue/${input.key}/canonical-title`,
        }));
      const calls: string[] = [];
      const service = yield* twoAccounts({
        getViewer: () => Effect.map(asWhom, (account) => `viewer-${account}`),
        listIssues: (input) =>
          Effect.map(asWhom, (account) => {
            calls.push(`${account} ${input.cursor ?? ""}`);
            return { items: [issue("ENG-1")], nextCursor: `${account}-next` };
          }),
        getIssue: readInOwnWorkspace,
      });

      // The same team key on two accounts is two scopes, each with its own cursor and viewer.
      const listed = yield* service.list({ state: "open", involvement: "assigned" });
      expect(calls.sort()).toEqual(["first ", "second "]);
      expect(listed).toMatchObject({
        viewers: { "linear.app": "viewer-first", "linear.app second": "viewer-second" },
        nextCursors: { "linear.app eng": "first-next", "linear.app second/eng": "second-next" },
      });
      calls.length = 0;
      yield* service.list({ state: "open", cursors: { "linear.app second/eng": "second-next" } });
      expect(calls).toEqual(["second second-next"]);
      const ref = { tracker: LINEAR, scope: "ENG", key: "ENG-1", projectId: "home" as ProjectId };
      expect(yield* service.detail(ref)).toMatchObject({ title: "second" });

      // A link resolves through the account whose workspace it names, whichever is preferred.
      expect(
        yield* service.resolveUrl(
          "https://linear.app/other-workspace/issue/ENG-1/old-title",
          "work" as ProjectId,
        ),
      ).toMatchObject({ projectId: "home", issue: { title: "second" } });

      // Unbound, a link reads and writes as whichever connected account reaches its workspace.
      const commentedAs: string[] = [];
      const unbound = yield* makeService({
        projects: [project({ id: "notes" })],
        trackers: [
          fakeStandaloneTracker({
            getIssue: readInOwnWorkspace,
            comment: (input) =>
              Effect.map(inOwnWorkspace(input.workspace), ({ account }) => {
                commentedAs.push(account);
              }),
            listCandidates: (input) =>
              Effect.map(inOwnWorkspace(input.workspace), ({ account }) => ({
                labels: [{ name: account, color: null, description: null }],
                assignees: [],
                truncated: false,
              })),
          }),
        ],
        accounts: [{ tracker: LINEAR, accountId: IssueTrackerAccountId.make("second") }],
      });
      const unboundLink = yield* unbound.resolveUrl(
        "https://linear.app/other-workspace/issue/ENG-1/old-title",
        "notes" as ProjectId,
      );
      expect(unboundLink).toMatchObject({
        projectId: "notes",
        writable: true,
        workspace: "other-workspace",
        issue: { title: "second" },
      });
      yield* unbound.comment({ ...unboundLink, body: "Follow-up" });
      expect(commentedAs).toEqual(["second"]);
      // Its pickers read the same account's team, not the first account's team of the same key.
      expect(yield* unbound.candidates(unboundLink)).toMatchObject({
        labels: [{ name: "second" }],
      });

      // A refusal on the exact team's account is why the link failed; a miss tries only the rest.
      const reads: string[] = [];
      const exactTeam = (failure: (account: string) => IssueTrackerError | null) =>
        makeService({
          projects: [project({ id: "notes" }), project({ id: "home" })],
          trackers: [
            fakeStandaloneTracker({
              getIssue: (input) =>
                Effect.flatMap(asWhom, (account) => {
                  reads.push(account);
                  const refused = failure(account);
                  return refused === null ? readInOwnWorkspace(input) : Effect.fail(refused);
                }),
            }),
          ],
          bindings: {
            home: {
              tracker: LINEAR,
              accountId: IssueTrackerAccountId.make("second"),
              scope: "ENG",
            },
          },
          accounts: [{ tracker: LINEAR, accountId: IssueTrackerAccountId.make("second") }],
        });
      const otherLink = "https://linear.app/other-workspace/issue/ENG-1/old-title";
      const rateLimited = yield* exactTeam((account) =>
        account === "second" ? fail("rate-limited", 60) : null,
      );
      expect(
        yield* Effect.flip(rateLimited.resolveUrl(otherLink, "notes" as ProjectId)),
      ).toMatchObject({ reason: "rate-limited", retryAt: 60 });
      expect(reads).toEqual(["second"]);
      reads.length = 0;
      const missing = yield* exactTeam(() => fail("not-found"));
      const missed = yield* Effect.flip(missing.resolveUrl(otherLink, "notes" as ProjectId));
      expect(missed).toMatchObject({ reason: "not-found" });
      expect(reads).toEqual(["second", "first"]);
      // A signed-out account does not hide that a connected one lacks the issue.
      const signedOut = yield* exactTeam((account) =>
        fail(account === "first" ? "unauthenticated" : "not-found"),
      );
      expect(
        yield* Effect.flip(signedOut.resolveUrl(otherLink, "notes" as ProjectId)),
      ).toMatchObject({ _tag: "IssueOperationError", reason: "not-found" });
      // With every account tried, why they failed stands: a reconnect prompt, or a rate limit.
      const bothTeams = (failure: (account: string) => IssueTrackerError) =>
        makeService({
          projects: [project({ id: "notes" }), project({ id: "work" }), project({ id: "home" })],
          trackers: [
            fakeStandaloneTracker({
              getIssue: () => Effect.flatMap(asWhom, (account) => Effect.fail(failure(account))),
            }),
          ],
          bindings: {
            work: { tracker: LINEAR, scope: "ENG" },
            home: {
              tracker: LINEAR,
              accountId: IssueTrackerAccountId.make("second"),
              scope: "ENG",
            },
          },
          accounts: [{ tracker: LINEAR, accountId: IssueTrackerAccountId.make("second") }],
        });
      const allSignedOut = yield* bothTeams(() => fail("unauthenticated"));
      expect(
        yield* Effect.flip(allSignedOut.resolveUrl(otherLink, "notes" as ProjectId)),
      ).toMatchObject({ _tag: "IssueUnavailableError", reason: "unauthenticated" });
      const limitedAfterSignOut = yield* bothTeams((account) =>
        account === "first" ? fail("unauthenticated") : fail("rate-limited", 60),
      );
      expect(
        yield* Effect.flip(limitedAfterSignOut.resolveUrl(otherLink, "notes" as ProjectId)),
      ).toMatchObject({ reason: "rate-limited", retryAt: 60 });

      // One refused key leaves the group usable, even on a continuation that skips the other.
      const refusing = (refused: ReadonlyArray<string>) =>
        twoAccounts({
          listIssues: () =>
            Effect.flatMap(asWhom, (account) =>
              refused.includes(account)
                ? Effect.fail(fail("unauthenticated"))
                : Effect.succeed({ items: [], nextCursor: null }),
            ),
        });
      const partly = yield* (yield* refusing(["second"])).list({ state: "open" });
      expect(partly).toMatchObject({
        trackers: [{ configured: true }],
        errors: [{ projectId: "home" }],
      });
      const continued = yield* (yield* refusing(["second"])).list({
        state: "open",
        cursors: { "linear.app second/eng": "second-next" },
      });
      expect(continued.trackers).toMatchObject([{ configured: true }]);
      const none = yield* (yield* refusing(["first", "second"])).list({ state: "open" });
      expect(none.trackers).toMatchObject([{ configured: false }]);

      // The preferred account missing the key does not hide why the link's own account failed.
      const limited = yield* twoAccounts({
        getIssue: () =>
          Effect.flatMap(asWhom, (account) =>
            Effect.fail(account === "first" ? fail("not-found") : fail("rate-limited", 60)),
          ),
      });
      expect(
        yield* Effect.flip(
          limited.resolveUrl(
            "https://linear.app/other-workspace/issue/ENG-1/old-title",
            "work" as ProjectId,
          ),
        ),
      ).toMatchObject({ reason: "rate-limited", retryAt: 60 });

      // Two users of one workspace: the account bound to the link's exact team serves it.
      const sameWorkspace = yield* makeService({
        projects: [project({ id: "ops" }), project({ id: "eng" })],
        trackers: [fakeStandaloneTracker()],
        bindings: {
          ops: { tracker: LINEAR, scope: "OPS" },
          eng: { tracker: LINEAR, accountId: IssueTrackerAccountId.make("second"), scope: "ENG" },
        },
      });
      expect(
        yield* sameWorkspace.resolveUrl(
          "https://linear.app/acme/issue/ENG-1/old-title",
          "ops" as ProjectId,
        ),
      ).toMatchObject({ projectId: "eng", writable: true });
    }),
  );

  it.effect("rejects a URL from another workspace with the same team and issue key", () =>
    Effect.gen(function* () {
      const service = yield* makeService({
        projects: [project({ id: "web" })],
        trackers: [fakeStandaloneTracker()],
        bindings: { web: { tracker: LINEAR, scope: "ENG" } },
      });
      expect(
        yield* service.resolveUrl("https://linear.app/acme/issue/ENG-1/old-title"),
      ).toMatchObject({ key: "ENG-1", writable: true, workspace: "acme" });
      // Workspace keys are compared as the client compares them, whatever their case.
      expect(
        yield* service.resolveUrl("https://linear.app/Acme/issue/ENG-1/old-title"),
      ).toMatchObject({ key: "ENG-1" });
      // A link from before the workspace was renamed reads on, under the key it has now.
      expect(
        yield* service.resolveUrl("https://linear.app/acme-old/issue/ENG-1/old-title"),
      ).toMatchObject({ key: "ENG-1", workspace: "acme" });
      // The workspace a reference names reaches the tracker on reads and writes alike.
      const asked: Array<string | undefined> = [];
      const recording = yield* makeService({
        projects: [project({ id: "web" })],
        trackers: [
          fakeStandaloneTracker({
            getComments: (input) =>
              Effect.sync(() => {
                asked.push(input.workspace);
                return { comments: [], nextCursor: null };
              }),
            setState: (input) => Effect.sync(() => void asked.push(input.workspace)),
          }),
        ],
        bindings: { web: { tracker: LINEAR, scope: "ENG" } },
      });
      const ref = {
        projectId: "web" as ProjectId,
        tracker: LINEAR,
        scope: "ENG",
        key: "ENG-1",
        workspace: "acme",
      };
      yield* recording.comments({ ...ref, cursor: "c" });
      yield* recording.setState({ ...ref, state: "Done" });
      expect(asked).toEqual(["acme", "acme"]);
      const otherWorkspace = "https://linear.app/other-workspace/issue/ENG-1/old-title";
      // Bound or not, the link must name the workspace the environment's credential reaches.
      const unbound = yield* makeService({
        projects: [project({ id: "notes" })],
        trackers: [fakeStandaloneTracker()],
      });
      const refusal = {
        _tag: "Failure",
        failure: {
          _tag: "IssueOperationError",
          operation: "resolveUrl",
          reason: "not-found",
          detail: "This issue link names another Linear workspace than the connected one.",
        },
      };
      expect(yield* service.resolveUrl(otherWorkspace).pipe(Effect.result)).toMatchObject(refusal);
      expect(
        yield* unbound.resolveUrl(otherWorkspace, "notes" as ProjectId).pipe(Effect.result),
      ).toMatchObject(refusal);
      // Unbound, a link from before the workspace was renamed reads on too.
      expect(
        yield* unbound.resolveUrl(
          "https://linear.app/acme-old/issue/ENG-1/old-title",
          "notes" as ProjectId,
        ),
      ).toMatchObject({ projectId: "notes", workspace: "acme" });
    }),
  );

  it.effect("lists a bound project's standalone scopes beside its git host and serves them", () =>
    Effect.gen(function* () {
      const listed: string[] = [];
      const record = (tracker: IssueTrackerKind): Partial<IssueTrackerApi> => ({
        listIssues: (input) =>
          Effect.sync(() => {
            listed.push(`${tracker} ${input.host} ${input.scope}`);
            return {
              items: [issue(tracker === LINEAR ? `${input.scope}-1` : "1")],
              nextCursor: null,
            };
          }),
      });
      const projects = [project({ id: "web", repository: "acme/web" }), project({ id: "notes" })];
      const trackers = [fakeTracker(record(GITHUB)), fakeStandaloneTracker(record(LINEAR))];
      const bound = yield* makeService({
        projects: [...projects, project({ id: "api", repository: "acme/api" })],
        trackers,
        bindings: {
          ["web" as ProjectId]: { tracker: LINEAR, scope: "ENG", additionalScopes: ["OPS"] },
        },
      });

      const result = yield* bound.list({ state: "open" });
      expect(listed.sort()).toEqual([
        "github github.com acme/api",
        "github github.com acme/web",
        "linear linear.app ENG",
        "linear linear.app OPS",
      ]);
      expect(result.entries.map((entry) => [entry.tracker, entry.scope, entry.key]).sort()).toEqual(
        [
          ["github", "acme/api", "1"],
          ["github", "acme/web", "1"],
          ["linear", "ENG", "ENG-1"],
          ["linear", "OPS", "OPS-1"],
        ],
      );
      expect(
        result.trackers
          .map((tracker) => [tracker.tracker, tracker.host, tracker.projectCount])
          .sort(),
      ).toEqual([
        ["github", "github.com", 2],
        ["linear", "linear.app", 1],
      ]);
      expect(result.scopes).toContainEqual({
        projectId: "web",
        tracker: "linear",
        host: "linear.app",
        scope: "ENG",
      });
      // Narrowed to one project, both of its trackers are still read.
      listed.length = 0;
      const selected = yield* bound.list({ state: "open", projectId: "web" as ProjectId });
      expect(listed.sort()).toEqual([
        "github github.com acme/web",
        "linear linear.app ENG",
        "linear linear.app OPS",
      ]);
      expect(selected.entries.map((entry) => [entry.projectId, entry.scope]).sort()).toEqual([
        ["web", "ENG"],
        ["web", "OPS"],
        ["web", "acme/web"],
      ]);
      listed.length = 0;
      yield* bound.list({ state: "open", excludeProjectIds: ["web" as ProjectId] });
      expect(listed).toEqual(["github github.com acme/api"]);
      // A reference picks the project's tracker by kind; both are the project's own, so writable.
      const linearRef = {
        projectId: "web" as ProjectId,
        tracker: LINEAR,
        scope: "ENG",
        key: "ENG-1",
      };
      expect(yield* bound.detail(linearRef)).toMatchObject({ host: "linear.app", writable: true });
      expect(yield* bound.detail({ ...linearRef, scope: "OPS", key: "OPS-1" })).toMatchObject({
        writable: true,
      });
      expect(yield* bound.detail({ ...linearRef, scope: "QA", key: "QA-1" })).toMatchObject({
        writable: false,
      });
      expect((yield* bound.projectScope("web" as ProjectId)).scope).toBe("ENG");
      expect((yield* bound.projectScope("web" as ProjectId, { key: "ops-12" })).scope).toBe("OPS");
      expect(
        (yield* bound.projectScope("web" as ProjectId, { key: "OPS-12", scope: "ENG" })).scope,
      ).toBe("OPS");
      expect(
        (yield* bound.projectScope("web" as ProjectId, { key: "OPS-12", scope: "acme/web" })).scope,
      ).toBe("acme/web");
      expect((yield* bound.projectScope("web" as ProjectId, { scope: "linear:ops" })).scope).toBe(
        "OPS",
      );
      // The tracker prefix narrows the match, so another tracker's scope is not found.
      expect(
        yield* bound.projectScope("web" as ProjectId, { scope: "github:OPS" }).pipe(Effect.flip),
      ).toMatchObject({ reason: "not-found" });
      expect(yield* bound.projectScope("web" as ProjectId, { scope: "ACME/Web" })).toMatchObject({
        tracker: GITHUB,
        host: "github.com",
        scope: "acme/web",
      });
      expect(
        yield* bound.detail({
          projectId: "web" as ProjectId,
          tracker: GITHUB,
          scope: "acme/web",
          key: "1",
        }),
      ).toMatchObject({ host: "github.com", writable: true });

      const resolved = yield* bound
        .resolveUrl("https://linear.app/acme/issue/ENG-1/issue-title", "web" as ProjectId)
        .pipe(Effect.result);
      expect(resolved).toMatchObject({
        _tag: "Success",
        success: {
          projectId: "web",
          tracker: LINEAR,
          host: "linear.app",
          scope: "ENG",
          key: "ENG-1",
          writable: true,
        },
      });

      // Unbound, the standalone scope leaves the listing, but the environment's credential still
      // serves references, from a project with no repository (No project) too.
      listed.length = 0;
      const unbound = yield* makeService({ projects, trackers });
      const after = yield* unbound.list({ state: "open" });
      expect(listed).toEqual(["github github.com acme/web"]);
      expect(after.trackers.map((tracker) => tracker.tracker)).toEqual(["github"]);
      const noProjectRef = { ...linearRef, projectId: "notes" as ProjectId };
      expect(yield* unbound.detail(noProjectRef)).toMatchObject({
        projectId: "notes",
        host: "linear.app",
        writable: true,
      });
      expect(
        yield* Effect.flip(unbound.resolveUrl("https://linear.app/acme/issue/ENG-1/issue-title")),
      ).toMatchObject({
        _tag: "IssueOperationError",
        detail: expect.stringContaining("Connect a Linear account in Settings and bind a project"),
      });
      yield* unbound.comment({ ...noProjectRef, body: "Follow-up" });
      yield* unbound.setState({ ...noProjectRef, state: "Done" });
      expect(
        yield* unbound.resolveUrl(
          "https://linear.app/acme/issue/ENG-1/issue-title",
          "notes" as ProjectId,
        ),
      ).toMatchObject({ projectId: "notes", host: "linear.app", key: "ENG-1", writable: true });
      expect(
        yield* Effect.flip(unbound.detail({ ...noProjectRef, host: "linear.example" })),
      ).toMatchObject({ _tag: "IssueUnavailableError", reason: "tracker-unsupported" });
      // Another project's team does not make a No project link read-only through that project.
      const boundElsewhere = yield* makeService({
        projects,
        trackers,
        bindings: { ["web" as ProjectId]: { tracker: LINEAR, scope: "OPS" } },
      });
      const elsewhereLink = yield* boundElsewhere.resolveUrl(
        "https://linear.app/acme/issue/ENG-1/issue-title",
        "notes" as ProjectId,
      );
      expect(elsewhereLink).toMatchObject({ projectId: "notes", scope: "ENG", writable: true });
      yield* boundElsewhere.comment({ ...elsewhereLink, body: "Follow-up" });

      // A project with no remote at all can still be bound, and then has only the standalone one.
      const standaloneOnly = yield* makeService({
        projects,
        trackers,
        bindings: { ["notes" as ProjectId]: { tracker: LINEAR, scope: "OPS" } },
      });
      const notes = yield* standaloneOnly.list({ state: "open", projectId: "notes" as ProjectId });
      expect(notes.entries.map((entry) => [entry.projectId, entry.scope])).toEqual([
        ["notes", "OPS"],
      ]);

      // Bindings that cannot be read fail the listing; they are not taken for "none bound".
      const unreadable = yield* makeService({
        projects,
        trackers,
        readBindings: Effect.fail(
          new PersistenceSqlError({
            operation: "IssueTrackerBindingsRepository.list",
            detail: "disk I/O error",
          }),
        ),
      });
      expect(yield* Effect.flip(unreadable.list({ state: "open" }))).toMatchObject({
        _tag: "IssueOperationError",
        operation: "listProjects",
      });
    }),
  );

  it.effect("orders merged entries by createdAt when sort is created and updatedAt otherwise", () =>
    Effect.gen(function* () {
      const seenSorts: string[] = [];
      const service = yield* makeService({
        projects: [
          project({ id: "web", repository: "acme/web" }),
          project({ id: "api", repository: "acme/api" }),
        ],
        trackers: [
          fakeTracker({
            listIssues: (input) => {
              seenSorts.push(input.sort);
              if (input.scope === "acme/web") {
                return Effect.succeed({
                  items: [
                    {
                      ...issue("old-created-recent-update"),
                      createdAt: "2026-07-06T01:00:00+02:00",
                      updatedAt: "2026-07-10T00:00:00Z",
                    },
                  ],
                  nextCursor: null,
                });
              }
              return Effect.succeed({
                items: [
                  {
                    ...issue("new-created-older-update"),
                    createdAt: "2026-07-05T23:30:00Z",
                    updatedAt: "2026-07-06T00:00:00Z",
                  },
                ],
                nextCursor: null,
              });
            },
          }),
        ],
      });

      const byCreated = yield* service.list({ state: "open", sort: "created" });
      expect(seenSorts).toEqual(["created", "created"]);
      expect(byCreated.entries.map((entry) => entry.key)).toEqual([
        "new-created-older-update",
        "old-created-recent-update",
      ]);

      seenSorts.length = 0;
      const byUpdated = yield* service.list({ state: "open" });
      expect(seenSorts).toEqual(["updated", "updated"]);
      expect(byUpdated.entries.map((entry) => entry.key)).toEqual([
        "old-created-recent-update",
        "new-created-older-update",
      ]);
    }),
  );
});
