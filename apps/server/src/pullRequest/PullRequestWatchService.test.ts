import {
  CommandId,
  ProjectId,
  ProviderInstanceId,
  ThreadId,
  PullRequestOperationError,
  PullRequestUnavailableError,
  type PullRequestCheck,
  type PullRequestDetail,
  type PullRequestSummary,
  type OrchestrationV2ServerCommand as OrchestrationCommand,
  type OrchestrationProjectShell,
  type ThreadPullRequestLink,
  SourceControlProviderKind,
} from "@onicode/contracts";
import { describe, expect, it } from "@effect/vitest";
import * as SecureRandom from "effect/crypto/SecureRandom";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Ref from "effect/Ref";
import * as TestClock from "effect/testing/TestClock";

import * as Orchestrator from "../orchestration-v2/Orchestrator.ts";
import {
  type PullRequestTestThread,
  v2PullRequestThread,
} from "../orchestration-v2/testkit/pullRequestFixtures.ts";
import * as ProjectService from "../project/ProjectService.ts";
import * as PullRequestService from "./PullRequestService.ts";
import * as PullRequestWatchService from "./PullRequestWatchService.ts";
import type { PullRequestTarget } from "./pullRequestTarget.ts";

const PROJECT_ID = ProjectId.make("project-1");
const THREAD_ID = ThreadId.make("thread-1");
const check = (name: string, status: PullRequestCheck["status"]): PullRequestCheck => ({
  name,
  status,
  description: null,
  url: null,
});
const testCrypto = SecureRandom.make((size) => new Uint8Array(size).fill(7));

function makeProject(
  repositoryIdentity: OrchestrationProjectShell["repositoryIdentity"] = {
    canonicalKey: "github.com/onicode/onicode",
    locator: {
      source: "git-remote",
      remoteName: "origin",
      remoteUrl: "git@github.com:Onicode/Onicode.git",
    },
    provider: SourceControlProviderKind.make("github"),
    displayName: "Onicode/Onicode",
    owner: "Onicode",
    name: "Onicode",
  },
): OrchestrationProjectShell {
  return {
    id: PROJECT_ID,
    title: "Project",
    workspaceRoot: "/workspace/project",
    defaultModelSelection: null,
    scripts: [],
    repositoryIdentity,
    createdAt: "2026-08-01T00:00:00.000Z",
    updatedAt: "2026-08-01T00:00:00.000Z",
  };
}

function makeThread(pullRequests: ReadonlyArray<ThreadPullRequestLink>): PullRequestTestThread {
  return {
    id: THREAD_ID,
    projectId: PROJECT_ID,
    title: "Thread",
    modelSelection: { instanceId: ProviderInstanceId.make("codex"), model: "gpt-5" },
    runtimeMode: "full-access",
    interactionMode: "default",
    branch: null,
    worktreePath: null,
    pullRequests,
    createdAt: "2026-08-01T00:00:00.000Z",
    updatedAt: "2026-08-20T00:00:00.000Z",
    archivedAt: null,
    settledOverride: null,
    settledAt: null,
    latestUserMessageAt: "2026-08-20T00:00:00.000Z",
  };
}

function makeLink(
  number: number,
  overrides: Partial<ThreadPullRequestLink> & {
    readonly headBranch?: string;
    readonly baseBranch?: string;
  } = {},
): ThreadPullRequestLink {
  const { headBranch, baseBranch, ...rest } = overrides;
  return {
    host: "github.com",
    repository: "onicode/onicode",
    number,
    url: `https://github.com/onicode/onicode/pull/${number}`,
    source: "manual",
    linkedAt: "2026-08-10T00:00:00.000Z",
    snapshot:
      headBranch === undefined
        ? null
        : {
            state: "open",
            title: `PR ${number}`,
            headBranch,
            baseBranch: baseBranch ?? "main",
            isDraft: false,
            updatedAt: null,
            syncedAt: "2026-08-27T00:00:00.000Z",
          },
    stack: null,
    ...rest,
  };
}

interface HarnessOptions {
  readonly thread?: PullRequestTestThread | null;
  readonly project?: OrchestrationProjectShell | null;
  readonly readFailure?: PullRequestService.PullRequestError;
  /** Completed when the summary read starts, which then never answers. */
  readonly readHangs?: Deferred.Deferred<void>;
  readonly readState?: PullRequestSummary["state"];
  readonly readChecksState?: PullRequestSummary["checksState"];
  readonly readHeadSha?: string;
  readonly detailChecks?: ReadonlyArray<PullRequestCheck>;
  readonly detailState?: PullRequestDetail["state"];
  readonly detailHeadSha?: string;
  readonly detailFailure?: PullRequestService.PullRequestError;
}

const makeHarness = Effect.fn("makePullRequestWatchHarness")(function* (
  options: HarnessOptions = {},
) {
  const commands = yield* Ref.make<ReadonlyArray<OrchestrationCommand>>([]);
  const reads = yield* Ref.make<ReadonlyArray<string>>([]);
  const thread = options.thread === undefined ? makeThread([]) : options.thread;
  const project = options.project === undefined ? makeProject() : options.project;
  const dependencies = Layer.mergeAll(
    Layer.mock(ProjectService.ProjectService)({
      getShell: () => Effect.succeed(Option.fromNullishOr(project)),
    }),
    Layer.mock(Orchestrator.OrchestratorV2)({
      getThreadShell: (id) =>
        Effect.succeed(id === thread?.id ? v2PullRequestThread(thread) : null),
      dispatch: (command) =>
        Ref.update(commands, (recorded) => [...recorded, command]).pipe(
          Effect.as({ sequence: 1, storedEvents: [] }),
        ),
    }),
    Layer.mock(PullRequestService.PullRequestService)({
      summary: (input) =>
        options.readHangs !== undefined
          ? Deferred.succeed(options.readHangs, undefined).pipe(Effect.andThen(Effect.never))
          : options.readFailure !== undefined
            ? Effect.fail(options.readFailure)
            : Effect.succeed({
                ...input,
                provider: SourceControlProviderKind.make("github"),
                title: "Pull request",
                url: `https://github.com/${input.repository}/pull/${input.number}`,
                state: options.readState ?? "open",
                ...(options.readChecksState !== undefined && {
                  checksState: options.readChecksState,
                }),
                ...(options.readHeadSha !== undefined && { headSha: options.readHeadSha }),
                headBranch: "feature",
                baseBranch: "main",
                updatedAt: "2026-08-20T00:00:00.000Z",
              }),
      invalidate: () => Ref.update(reads, (calls) => [...calls, "invalidate"]),
      detail: () =>
        Ref.update(reads, (calls) => [...calls, "detail"]).pipe(
          Effect.andThen(
            options.detailFailure === undefined ? Effect.void : Effect.fail(options.detailFailure),
          ),
          Effect.as({
            state: options.detailState ?? "open",
            checks: options.detailChecks ?? [check("build", "failure")],
            ...(options.detailHeadSha !== undefined && { headSha: options.detailHeadSha }),
          } as unknown as PullRequestDetail),
        ),
    }),
    Layer.succeed(SecureRandom.SecureRandom, testCrypto),
  );
  const service = yield* PullRequestWatchService.PullRequestWatchService.pipe(
    Effect.provide(PullRequestWatchService.layer.pipe(Layer.provide(dependencies))),
  );
  return {
    commands,
    reads,
    setWatching: (watching: boolean, target: PullRequestTarget) =>
      service.setWatching({ threadId: THREAD_ID, target, watching }),
    seedClientWatch: service.seedClientWatch,
  };
});

describe("PullRequestWatchService", () => {
  it.effect("links an unlinked watch even while the host has a transient read failure", () =>
    Effect.gen(function* () {
      const cases: ReadonlyArray<readonly [HarnessOptions, boolean | undefined]> = [
        [{ readChecksState: "pending" }, true],
        [{ readChecksState: "failing" }, false],
        // GitHub's summary rollup counts a cancelled check a newer run replaced.
        [{ readChecksState: "failing", detailChecks: [check("test", "pending")] }, true],
        [{ readChecksState: "failing", detailChecks: [check("test", "skipped")] }, undefined],
        // GitLab's summary reads only the parent pipeline; the detail adds its child pipelines.
        [{ readChecksState: "passing", detailChecks: [check("child", "pending")] }, true],
        [
          {
            readChecksState: "passing",
            detailChecks: [
              check("test", "success"),
              { ...check("Unlisted", "neutral"), placeholder: true },
            ],
          },
          undefined,
        ],
        [
          {
            readChecksState: "failing",
            detailChecks: [
              check("test", "success"),
              { ...check("Unlisted", "neutral"), placeholder: true },
            ],
          },
          false,
        ],
        // Only the detail sees GitLab's child pipelines, so an unconfirmed pass is no verdict.
        [
          {
            readChecksState: "passing",
            detailFailure: new PullRequestOperationError({
              operation: "detail",
              detail: "The host is temporarily unavailable.",
            }),
          },
          undefined,
        ],
        [
          {
            readChecksState: "failing",
            detailFailure: new PullRequestOperationError({
              operation: "detail",
              detail: "The host is temporarily unavailable.",
            }),
          },
          false,
        ],
        [{}, undefined],
        [{ readChecksState: null }, undefined],
        [
          {
            readFailure: new PullRequestOperationError({
              operation: "summary",
              detail: "The host is temporarily unavailable.",
            }),
          },
          undefined,
        ],
      ];
      for (const [options, checksPending] of cases) {
        const harness = yield* makeHarness(options);
        const result = yield* harness.setWatching(true, {
          url: "https://github.com/onicode/onicode/pull/9",
        });
        expect(result).toMatchObject({ number: 9, wasWatching: false });
        const commands = yield* Ref.get(harness.commands);
        expect(commands).toMatchObject([
          {
            type: "thread.pull-request.watch",
            number: 9,
            watching: true,
            link: { url: "https://github.com/onicode/onicode/pull/9", source: "agent" },
          },
        ]);
        const [command] = commands;
        expect(command?.type === "thread.pull-request.watch" ? command.checksPending : null).toBe(
          checksPending,
        );
      }
    }),
  );

  it.effect("tells the caller when the checks already passed, since no wake will report it", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness({
        readChecksState: "passing",
        readHeadSha: "aaaa111",
        detailChecks: [check("build", "success")],
        detailHeadSha: "bbbb222",
      });
      const result = yield* harness.setWatching(true, {
        url: "https://github.com/onicode/onicode/pull/9",
      });
      expect(result).toMatchObject({ wasWatching: false, checksState: "passing" });
      // The verdict is the confirming read's head, so a push before the first watch read is news.
      expect(yield* Ref.get(harness.commands)).toMatchObject([
        { checksPending: false, headSha: "bbbb222" },
      ]);

      const childFailing = yield* makeHarness({
        readChecksState: "passing",
        detailChecks: [check("parent", "success"), check("child", "failure")],
      });
      expect(
        (yield* childFailing.setWatching(true, {
          url: "https://github.com/onicode/onicode/pull/9",
        })).checksState,
      ).toBe("failing");

      const replaced = yield* makeHarness({
        readChecksState: "failing",
        detailChecks: [check("test", "success")],
      });
      expect(
        yield* replaced.setWatching(true, { url: "https://github.com/onicode/onicode/pull/9" }),
      ).toMatchObject({ wasWatching: false, checksState: "passing" });
      // A detail read cached from before a push would answer for the old head.
      expect(yield* Ref.get(replaced.reads)).toEqual(["invalidate", "detail"]);

      // The confirming read can find the pull request ended since the summary.
      const ended = yield* makeHarness({ readChecksState: "failing", detailState: "merged" });
      expect(
        yield* ended.setWatching(true, { url: "https://github.com/onicode/onicode/pull/9" }),
      ).toMatchObject({ watching: false, wasWatching: false, state: "merged" });
      expect(yield* Ref.get(ended.commands)).toEqual([]);
    }),
  );

  it.effect("seeds a client's watch start from a confirmed read, as an agent's start is", () =>
    Effect.gen(function* () {
      const start = {
        type: "thread.pull-request.watch" as const,
        commandId: CommandId.make("client-watch"),
        threadId: THREAD_ID,
        host: "github.com",
        repository: "onicode/onicode",
        number: 1,
        watching: true,
      };
      // The link snapshot keeps GitHub's rollup, which counts a cancelled check a rerun replaced.
      const replaced = yield* makeHarness({
        readChecksState: "failing",
        readHeadSha: "aaaa111",
        detailChecks: [check("test", "pending")],
      });
      expect(yield* replaced.seedClientWatch(start)).toEqual({
        ...start,
        checksPending: true,
        headSha: "aaaa111",
      });
      expect(yield* Ref.get(replaced.reads)).toEqual(["invalidate", "detail"]);

      const watch = {
        startedAt: "2026-08-20T00:00:00.000Z",
        headSha: null,
        failedChecks: [],
        passed: false,
        passedChecks: [],
        remarksThrough: "2026-08-20T00:00:00.000Z",
        remarkIds: [],
        conflicting: false,
        wakes: 0,
      };
      const watched = yield* makeHarness({
        readChecksState: "failing",
        thread: makeThread([makeLink(1, { headBranch: "feature", watch })]),
      });
      const offline = yield* makeHarness({
        readFailure: new PullRequestUnavailableError({ reason: "cli-missing" }),
      });
      const unchanged = [
        [watched, start],
        [replaced, { ...start, checksPending: false }],
        [replaced, { ...start, watching: false }],
        // A host the server cannot read leaves the seed to the link, as before.
        [offline, start],
      ] as const;
      for (const [harness, command] of unchanged) {
        expect(yield* harness.seedClientWatch(command)).toBe(command);
      }
      expect(yield* Ref.get(watched.reads)).toEqual([]);

      // The press waits on this read, so a host that never answers falls back in time.
      const reading = yield* Deferred.make<void>();
      const slow = yield* makeHarness({ readHangs: reading });
      const seeding = yield* slow.seedClientWatch(start).pipe(Effect.forkChild);
      yield* Deferred.await(reading);
      yield* TestClock.adjust(PullRequestWatchService.CLIENT_WATCH_READ_DEADLINE);
      expect(yield* Fiber.join(seeding)).toBe(start);
    }),
  );

  it.effect("reports a missing hosting CLI before saving a watch and still allows unwatch", () =>
    Effect.gen(function* () {
      const missing = new PullRequestUnavailableError({
        reason: "cli-missing",
        provider: SourceControlProviderKind.make("gitlab"),
        requirement: "Install glab on the Onicode server's PATH.",
      });
      const harness = yield* makeHarness({ readFailure: missing });
      const target = { url: "https://gitlab.example.com/team/project/-/merge_requests/42" };
      const error = yield* harness.setWatching(true, target).pipe(Effect.flip);
      expect(error).toEqual(missing);
      expect(yield* Ref.get(harness.commands)).toEqual([]);

      yield* harness.setWatching(false, target);
      expect(yield* Ref.get(harness.commands)).toMatchObject([
        { type: "thread.pull-request.watch", watching: false },
      ]);
    }),
  );

  it.effect("reports merged PRs without watching and keeps an active watch usable offline", () =>
    Effect.gen(function* () {
      const freshMerged = yield* makeHarness({ readState: "merged" });
      expect(
        yield* freshMerged.setWatching(true, { repository: "onicode/onicode", number: 1 }),
      ).toMatchObject({ number: 1, watching: false, wasWatching: false, state: "merged" });
      expect(yield* Ref.get(freshMerged.commands)).toEqual([]);

      const watch = {
        startedAt: "2026-08-20T00:00:00.000Z",
        headSha: null,
        failedChecks: [],
        passed: false,
        passedChecks: [],
        remarksThrough: "2026-08-20T00:00:00.000Z",
        remarkIds: [],
        conflicting: false,
        wakes: 0,
      };
      const merged = makeLink(1, { headBranch: "done" });
      const harness = yield* makeHarness({
        readFailure: new PullRequestUnavailableError({ reason: "cli-missing" }),
        thread: makeThread([
          { ...merged, snapshot: merged.snapshot && { ...merged.snapshot, state: "merged" } },
          makeLink(2, { headBranch: "idle" }),
          makeLink(3, { headBranch: "watched", watch }),
        ]),
      });
      expect(
        yield* harness.setWatching(true, { repository: "onicode/onicode", number: 1 }),
      ).toMatchObject({ number: 1, watching: false, state: "merged" });
      expect(
        yield* harness.setWatching(true, { repository: "onicode/onicode", number: 3 }),
      ).toMatchObject({ watching: true, wasWatching: true });
      expect(
        yield* harness.setWatching(false, { repository: "onicode/onicode", number: 3 }),
      ).toMatchObject({ wasWatching: true });
      expect(yield* Ref.get(harness.commands)).toMatchObject([
        { type: "thread.pull-request.watch", number: 3, watching: true },
        { type: "thread.pull-request.watch", number: 3, watching: false },
      ]);
    }),
  );

  it.effect("re-reads a pull request saved as closed, and watches it only if it reopened", () =>
    Effect.gen(function* () {
      const closed = makeLink(1, { headBranch: "closed" });
      const closedThread = makeThread([
        { ...closed, snapshot: closed.snapshot && { ...closed.snapshot, state: "closed" } },
      ]);
      const stillClosed = yield* makeHarness({ readState: "closed", thread: closedThread });
      expect(
        yield* stillClosed.setWatching(true, { repository: "onicode/onicode", number: 1 }),
      ).toMatchObject({ watching: false, state: "closed" });
      expect(yield* Ref.get(stillClosed.commands)).toEqual([]);

      const harness = yield* makeHarness({ readState: "open", thread: closedThread });
      yield* harness.setWatching(true, { repository: "onicode/onicode", number: 1 });
      expect(yield* Ref.get(harness.commands)).toMatchObject([
        { type: "thread.pull-request.watch", number: 1, watching: true },
      ]);
    }),
  );

  it.effect("refuses a watch from a subagent thread, whose parent owns the pull request", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness({
        thread: {
          ...makeThread([makeLink(1, { headBranch: "feature" })]),
          lineage: {
            rootThreadId: ThreadId.make("parent"),
            parentThreadId: ThreadId.make("parent"),
            relationshipToParent: "subagent",
          },
        },
      });
      const error = yield* harness
        .setWatching(true, { repository: "onicode/onicode", number: 1 })
        .pipe(Effect.flip);
      expect(error).toMatchObject({ _tag: "PullRequestWatchFromSubagentError" });
      expect(yield* Ref.get(harness.commands)).toEqual([]);
    }),
  );
});
