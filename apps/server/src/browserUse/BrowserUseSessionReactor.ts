import * as Effect from "effect/Effect";
import * as FiberSet from "effect/FiberSet";
import * as Stream from "effect/Stream";

import * as Orchestrator from "../orchestration-v2/Orchestrator.ts";
import * as PreviewManager from "../preview/Manager.ts";
import { forkParked } from "../serverActivation.ts";
import * as BrowserUseService from "./BrowserUseService.ts";

/**
 * A thread's browser tabs outlive its turns so the next one resumes on them. They close when
 * the thread is archived or deleted, which is when nothing will resume there: its Browser
 * panel tabs, and the tabs its driver opened in the user's browser.
 */
export const make = Effect.gen(function* () {
  const orchestrator = yield* Orchestrator.OrchestratorV2;
  const browserUse = yield* BrowserUseService.BrowserUseService;
  const previews = yield* PreviewManager.PreviewManager;

  const start = Effect.fn("BrowserUseSessionReactor.start")(function* () {
    // Closing tabs unloads pages and runs obu, which must not hold back the event stream.
    const closing = yield* FiberSet.make<void>();
    yield* forkParked(
      Stream.runForEach(orchestrator.streamDomainEvents, (event) =>
        event.type === "thread.archived" || event.type === "thread.deleted"
          ? previews.close({ threadId: event.threadId }).pipe(
              Effect.catch((cause) =>
                Effect.logWarning("Could not close a thread's browser tabs.", { cause }),
              ),
              Effect.andThen(
                browserUse.endSession(event.threadId, { forget: event.type === "thread.deleted" }),
              ),
              FiberSet.run(closing),
              Effect.asVoid,
            )
          : Effect.void,
      ),
    );
  });
  return { start };
});
