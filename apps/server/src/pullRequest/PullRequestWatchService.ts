import {
  CommandId,
  ThreadId,
  type OrchestrationV2Command,
  type OrchestrationV2ThreadShell,
  type PullRequestChecksState,
  type PullRequestRef,
  type PullRequestUnavailableError,
  type PullRequestWatchUntil,
} from "@onicode/contracts";
import { rollupCheckStatuses } from "@onicode/source-control-core/server/pullRequestChecks";
import * as Cause from "effect/Cause";
import * as Context from "effect/Context";
import * as SecureRandom from "effect/crypto/SecureRandom";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";

import * as Orchestrator from "../orchestration-v2/Orchestrator.ts";
import * as PullRequestService from "./PullRequestService.ts";
import {
  resolveTarget,
  type PullRequestTarget,
  type PullRequestTargetError,
  type ResolvedTarget,
} from "./pullRequestTarget.ts";
import {
  threadPullRequestKeysEqual,
  threadPullRequestsOf,
} from "../sourceControl/threadPullRequests.ts";

export class PullRequestWatchFailedError extends Schema.TaggedError<PullRequestWatchFailedError>()(
  "PullRequestWatchFailedError",
  { cause: Schema.Defect() },
) {
  override get message(): string {
    return "Could not change whether the pull request is watched.";
  }
}

export class PullRequestWatchFromSubagentError extends Schema.TaggedError<PullRequestWatchFromSubagentError>()(
  "PullRequestWatchFromSubagentError",
  {},
) {
  override get message(): string {
    return "This thread is a subagent, so it cannot watch pull requests. Its parent thread owns the pull request: finish your task and report back instead.";
  }
}

export class PullRequestWatchThreadNotFoundError extends Schema.TaggedError<PullRequestWatchThreadNotFoundError>()(
  "PullRequestWatchThreadNotFoundError",
  { threadId: ThreadId },
) {
  override get message(): string {
    return `Thread ${this.threadId} was not found.`;
  }
}

// A pressed Watch waits on this read, so a slow host falls back to the link's last sync.
export const CLIENT_WATCH_READ_DEADLINE = "5 seconds";

type PullRequestWatchCommand = Extract<
  OrchestrationV2Command,
  { readonly type: "thread.pull-request.watch" }
>;

export class PullRequestWatchService extends Context.Service<
  PullRequestWatchService,
  {
    readonly setWatching: (input: {
      readonly threadId: ThreadId;
      readonly target: PullRequestTarget;
      readonly watching: boolean;
      readonly until?: PullRequestWatchUntil | undefined;
    }) => Effect.Effect<
      ResolvedTarget & {
        readonly watching: boolean;
        readonly wasWatching: boolean;
        /** Set when the PR already ended, so no watch was registered. */
        readonly state?: "merged" | "closed";
        /** CI as the start read saw it. Checks that already passed never wake the watch. */
        readonly checksState?: PullRequestChecksState;
      },
      | PullRequestWatchFailedError
      | PullRequestWatchFromSubagentError
      | PullRequestWatchThreadNotFoundError
      | PullRequestTargetError
      | PullRequestUnavailableError
    >;
    /** Seeds a client's watch start with CI as a fresh read finds it; other commands pass as is. */
    readonly seedClientWatch: (
      command: PullRequestWatchCommand,
    ) => Effect.Effect<PullRequestWatchCommand>;
  }
>()("onicode/pullRequest/PullRequestWatchService") {}

interface StartRead {
  readonly ended?: "merged" | "closed";
  readonly checksState?: PullRequestChecksState | undefined;
  readonly checksPending?: boolean | undefined;
  /** The head the verdict describes, so a push before the first watch read is news. */
  readonly headSha?: string | undefined;
}

const make = Effect.gen(function* () {
  const engine = yield* Orchestrator.OrchestratorV2;
  const pullRequests = yield* PullRequestService.PullRequestService;
  const random = yield* SecureRandom.SecureRandom;

  const requireThread = Effect.fn("PullRequestWatchService.requireThread")(function* (
    threadId: ThreadId,
  ) {
    const thread = yield* engine
      .getThreadShell(threadId)
      .pipe(Effect.mapError((cause) => new PullRequestWatchFailedError({ cause })));
    if (thread === null || thread.deletedAt !== null) {
      return yield* new PullRequestWatchThreadNotFoundError({ threadId });
    }
    return thread;
  });

  // Pending: nothing has failed, so any failure the first read finds is news. A read without a
  // verdict (state left out, no checks yet, all skipped, a manual GitLab job) leaves the seed to
  // the link, since an old pass would read as news.
  const readStart = Effect.fn("PullRequestWatchService.readStart")(function* (
    reference: PullRequestRef,
  ) {
    const read = yield* pullRequests
      .summary(
        { ...reference, allowStale: false },
        { recoverTransientFailure: false, bypassCache: true },
      )
      .pipe(
        Effect.catchTags({
          // Host and transport errors can recover while the watch is active.
          // A failed read leaves the seed to the link's last sync.
          PullRequestOperationError: () => Effect.undefined,
        }),
      );
    const ended = (state: "merged" | "closed"): StartRead => ({ ended: state });
    if (read?.state === "merged" || read?.state === "closed") return ended(read.state);
    // A summary can take the host's own rollup: GitHub's counts a cancelled check a newer run
    // replaced, and GitLab's covers only the parent pipeline. The watch reads every check, so a
    // verdict needs a fresh, complete read of them; an incomplete one keeps only a failure.
    const confirmed =
      read?.checksState === "failing" || read?.checksState === "passing"
        ? yield* pullRequests.invalidate({ reference }).pipe(
            Effect.andThen(pullRequests.detail({ ...reference, allowStale: false })),
            Effect.map((detail) => ({
              state: detail.state,
              headSha: detail.headSha,
              checksState:
                detail.checksTruncated === true ||
                detail.checks.some((check) => check.placeholder === true)
                  ? read.checksState === "failing"
                    ? "failing"
                    : undefined
                  : rollupCheckStatuses(detail.checks.map((check) => check.status)),
            })),
            Effect.catchTags({ PullRequestOperationError: () => Effect.undefined }),
          )
        : undefined;
    if (confirmed?.state === "merged" || confirmed?.state === "closed") {
      return ended(confirmed.state);
    }
    // A pass the detail could not confirm may be the parent pipeline only; a failure stands.
    const checksState =
      (confirmed === undefined
        ? read?.checksState === "passing"
          ? undefined
          : read?.checksState
        : confirmed.checksState) ?? undefined;
    const start: StartRead = {
      checksState,
      checksPending: checksState === undefined ? undefined : checksState === "pending",
      headSha: confirmed?.headSha ?? read?.headSha,
    };
    return start;
  });

  // Keep the registration span stable across the service extraction.
  const setWatching = Effect.fn("PullRequestsToolkit.setWatching")(function* (input: {
    readonly threadId: ThreadId;
    readonly target: PullRequestTarget;
    readonly watching: boolean;
    readonly until?: PullRequestWatchUntil | undefined;
  }) {
    const { watching } = input;
    const thread = yield* requireThread(input.threadId);
    const project = yield* pullRequests
      .addressedProject(thread.projectId)
      .pipe(Effect.mapError((cause) => new PullRequestWatchFailedError({ cause })));
    const target = yield* resolveTarget(input.target, project);
    const watchedLink = (shell: OrchestrationV2ThreadShell) =>
      threadPullRequestsOf(shell).find(
        (link) => link.source !== "stack-dismissed" && threadPullRequestKeysEqual(link, target),
      );
    if (watching && thread.lineage.relationshipToParent === "subagent") {
      return yield* new PullRequestWatchFromSubagentError();
    }
    const before = watchedLink(thread);
    const wasWatching = before?.watch !== undefined;
    // A watch on an ended PR could only report that it ended, so report it now instead.
    const alreadyEnded = (state: "merged" | "closed") => ({
      ...target,
      watching: false,
      wasWatching,
      state,
    });
    // A closed snapshot still gets a read below, since the PR may have reopened.
    if (watching && before?.snapshot?.state === "merged") return alreadyEnded("merged");
    let start: StartRead | undefined;
    if (watching && !wasWatching) {
      start = yield* readStart({
        projectId: thread.projectId,
        host: target.host,
        repository: target.repository,
        number: target.number,
      });
      if (start.ended !== undefined) return alreadyEnded(start.ended);
    }
    const uuid = yield* random.uuidV4().pipe(Effect.orDie);
    yield* engine
      .dispatch({
        type: "thread.pull-request.watch",
        commandId: CommandId.make(`server:mcp-pr-watch:${thread.id}:${uuid}`),
        threadId: thread.id,
        host: target.host,
        repository: target.repository,
        number: target.number,
        watching,
        ...(watching ? { link: { url: target.url, source: "agent" as const } } : {}),
        ...(watching && input.until !== undefined && { until: input.until }),
        ...(watching && start?.checksPending !== undefined
          ? { checksPending: start.checksPending }
          : {}),
        ...(watching && start?.headSha !== undefined ? { headSha: start.headSha } : {}),
      })
      .pipe(
        Effect.catchCause((cause) =>
          Cause.hasInterruptsOnly(cause)
            ? Effect.failCause(cause as Cause.Cause<never>)
            : Effect.fail(new PullRequestWatchFailedError({ cause })),
        ),
      );
    const after = yield* requireThread(thread.id);
    return {
      ...target,
      watching: watchedLink(after)?.watch !== undefined,
      wasWatching,
      ...(start?.checksState !== undefined && { checksState: start.checksState }),
    };
  });

  const seedClientWatch = Effect.fn("PullRequestWatchService.seedClientWatch")(function* (
    command: PullRequestWatchCommand,
  ) {
    // A caller that read CI itself keeps its answer.
    if (!command.watching || command.checksPending !== undefined) return command;
    const thread = yield* engine
      .getThreadShell(command.threadId)
      .pipe(Effect.orElseSucceed(() => null));
    if (thread === null || thread.deletedAt !== null) return command;
    const link = threadPullRequestsOf(thread).find(
      (candidate) =>
        candidate.source !== "stack-dismissed" && threadPullRequestKeysEqual(candidate, command),
    );
    if (link?.watch !== undefined || link?.snapshot?.state === "merged") return command;
    // A read the server cannot make, or not in time, leaves the seed to the link, as before.
    const start = yield* readStart({
      projectId: thread.projectId,
      host: command.host,
      repository: command.repository,
      number: command.number,
    }).pipe(
      Effect.timeoutOption(CLIENT_WATCH_READ_DEADLINE),
      Effect.map(Option.getOrUndefined),
      Effect.orElseSucceed(() => undefined),
    );
    if (start === undefined || start.ended !== undefined) return command;
    if (start.checksPending === undefined && start.headSha === undefined) return command;
    return {
      ...command,
      ...(start.checksPending !== undefined && { checksPending: start.checksPending }),
      ...(start.headSha !== undefined && { headSha: start.headSha }),
    };
  });

  return PullRequestWatchService.of({ setWatching, seedClientWatch });
});

export const layer = Layer.effect(PullRequestWatchService, make);
