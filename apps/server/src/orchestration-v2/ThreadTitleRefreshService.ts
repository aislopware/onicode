import { CommandId, type ThreadId } from "@onicode/contracts";
import * as DateTime from "effect/DateTime";
import * as Duration from "effect/Duration";
import * as Effect from "effect/Effect";
import * as FiberMap from "effect/FiberMap";
import * as Stream from "effect/Stream";

import { forkParked } from "../serverActivation.ts";
import * as Orchestrator from "./Orchestrator.ts";
import * as ProjectionStore from "./ProjectionStore.ts";
import {
  awaitsTitleSubject,
  canRefreshGeneratedTitle,
  generatedTitleIsStale,
} from "./threadTitleRefresh.ts";

// Long enough that a user who is still working is not shown a title change between messages.
export const TITLE_REFRESH_QUIET = Duration.minutes(15);

type Moment = "run-ended" | "quiet" | "settled";

/**
 * Refreshes generated thread titles as their threads grow. The first title is written from the
 * first message alone, so it is regenerated when a run ends while the subject is still unknown,
 * when the thread then stays quiet, and when the thread settles. Quiet timers live in memory, so
 * after a restart the settle is what catches a thread that grew.
 */
export const make = Effect.gen(function* () {
  const orchestrator = yield* Orchestrator.OrchestratorV2;
  const projections = yield* ProjectionStore.ProjectionStoreV2;

  /** Returns whether the generator owns the title, so the caller knows a later moment can matter. */
  const refresh = Effect.fn("ThreadTitleRefreshService.refresh")(function* (
    threadId: ThreadId,
    moment: Moment,
  ) {
    const thread = yield* projections.getThread(threadId);
    if (thread.generatedTitle == null) return false;
    if (!canRefreshGeneratedTitle(thread)) return true;
    const shell = yield* projections.getThreadShell(threadId);
    if (shell === null || !generatedTitleIsStale(thread, shell)) return true;
    // A known subject waits until the user has stopped, so the title does not move mid-work.
    if (moment === "run-ended" ? !awaitsTitleSubject(thread) : shell.activeRunId !== null) {
      return true;
    }
    const now = yield* DateTime.now;
    yield* orchestrator.dispatch({
      type: "thread.title.refresh",
      commandId: CommandId.make(`server:title-refresh:${threadId}:${DateTime.toEpochMillis(now)}`),
      threadId,
    });
    return true;
  });

  const afterRun = Effect.fn("ThreadTitleRefreshService.afterRun")(function* (threadId: ThreadId) {
    if (!(yield* refresh(threadId, "run-ended"))) return;
    yield* Effect.sleep(TITLE_REFRESH_QUIET);
    yield* refresh(threadId, "quiet");
  });

  const guarded = <E>(threadId: ThreadId, effect: Effect.Effect<unknown, E>) =>
    effect.pipe(
      Effect.catch((cause) =>
        Effect.logWarning("thread title refresh skipped", { threadId, cause }),
      ),
    );

  const start = Effect.fn("ThreadTitleRefreshService.start")(function* () {
    // Keyed by thread: a newer run or a settle replaces the waiting timer.
    const pending = yield* FiberMap.make<ThreadId>();
    yield* forkParked(
      Stream.runForEach(orchestrator.streamDomainEvents, (event) => {
        if (
          event.type === "run.updated" &&
          (event.payload.status === "completed" ||
            event.payload.status === "interrupted" ||
            event.payload.status === "failed")
        ) {
          return FiberMap.run(
            pending,
            event.threadId,
            guarded(event.threadId, afterRun(event.threadId)),
          );
        }
        // A short first run can end while the first title is still being written.
        if (
          event.type === "thread.metadata-updated" &&
          canRefreshGeneratedTitle(event.payload) &&
          awaitsTitleSubject(event.payload) &&
          event.payload.generatedTitle.coveredAt === null
        ) {
          return FiberMap.run(
            pending,
            event.threadId,
            guarded(event.threadId, afterRun(event.threadId)),
          );
        }
        if (event.type === "thread.settled") {
          return FiberMap.run(
            pending,
            event.threadId,
            guarded(event.threadId, refresh(event.threadId, "settled")),
          );
        }
        return Effect.void;
      }),
    );
  });
  return { start };
});
