import { EnvironmentId, OrchestratorMcpFailure, type RuntimeMode } from "@onicode/contracts";
import { assert, it } from "@effect/vitest";
import * as Duration from "effect/Duration";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Schema from "effect/Schema";
import * as TestClock from "effect/testing/TestClock";

import * as PeerLinks from "../peer/PeerLinks.ts";
import * as PeerMcpClient from "../peer/PeerMcpClient.ts";
import * as ServerSettings from "../serverSettings.ts";
import * as WorkflowAgentLinks from "./WorkflowAgentLinks.ts";

const isFailure = Schema.is(OrchestratorMcpFailure);

interface Call {
  readonly environmentId: string;
  readonly tool: string;
  readonly arguments: Record<string, unknown>;
  readonly runtimeMode: string;
}

/** The other environment, played by the test: each tool answers from a list, in order. */
const peer = (
  answers: Record<string, ReadonlyArray<unknown>>,
  enabled = true,
  maxRuntimeMode?: RuntimeMode,
) => {
  const calls: Array<Call> = [];
  const left = new Map(Object.entries(answers).map(([tool, list]) => [tool, [...list]]));
  const layer = Layer.mergeAll(
    Layer.succeed(
      PeerMcpClient.PeerMcpClient,
      PeerMcpClient.PeerMcpClient.of({
        call: (input) =>
          Effect.suspend(() => {
            calls.push({
              environmentId: input.environmentId,
              tool: input.tool,
              arguments: input.arguments,
              runtimeMode: input.limits.runtimeMode,
            });
            const answer = left.get(input.tool)?.shift();
            return isFailure(answer) ? Effect.fail(answer) : Effect.succeed(answer as never);
          }),
      }),
    ),
    Layer.mock(PeerLinks.PeerLinks)({ enabled: Effect.succeed(enabled) }),
    Layer.mock(ServerSettings.ServerSettingsService)({
      managedPolicy:
        maxRuntimeMode === undefined
          ? undefined
          : { organization: "Example Corp", lockedSettings: [], limits: { maxRuntimeMode } },
    }),
  );
  return { calls, layer };
};

const input = {
  key: "run-1/write/0",
  origin: { runId: "run-1", workflow: "fix", step: "write" },
  title: "fix: write",
  prompt: "Write the fix.",
  outputSchema: { type: "object" },
  model: undefined,
  project: "/work/app",
  environment: "env-b",
  thread: undefined,
};
const run = (status: string) => ({ recentRuns: [{ runId: "turn-1", status }], items: [] });
const waited = { threadId: "thread-1", runId: "turn-1", status: "idle" };

it.effect("starts a thread there by the project's folder, with the step's key as the request", () =>
  Effect.gen(function* () {
    const there = peer({
      oni_project_list: [
        { projects: [{ id: "p-other", workspaceRoot: "/work/other" }], nextCursor: 1 },
        { projects: [{ id: "p-app", workspaceRoot: "/work/app" }], nextCursor: null },
      ],
      oni_thread_launch: [{ threadId: "thread-1", runId: "turn-1" }],
    });
    const options = {
      workspace: { type: "worktree", baseRef: "origin/main", startFromOrigin: true },
      runtimeMode: "auto-accept-edits",
    } as const;
    const [turn, continued] = yield* Effect.provide(
      Effect.flatMap(WorkflowAgentLinks.make, (links) =>
        Effect.all([
          links.start(
            { ...input, launch: options },
            "env-b",
            "workflow:k",
            EnvironmentId.make("env-a"),
          ),
          // A thread keeps how it started.
          links
            .start(
              { ...input, thread: "thread-1", launch: options },
              "env-b",
              "workflow:c",
              EnvironmentId.make("env-a"),
            )
            .pipe(Effect.flip),
        ]),
      ),
      there.layer,
    );
    assert.deepStrictEqual(turn, { threadId: "thread-1", turnId: "turn-1", environment: "env-b" });
    assert.include(continued.reason, "A continued `thread` keeps how it started");
    const launch = there.calls.at(-1);
    assert.deepStrictEqual(launch?.arguments, {
      projectId: "p-app",
      title: "fix: write",
      runtimeMode: "auto-accept-edits",
      workspaceStrategy: { type: "worktree", baseRef: "origin/main", startFromOrigin: true },
      message: "Write the fix.",
      outputSchema: { type: "object" },
      clientRequestId: "workflow:k",
      workflowRun: { environmentId: "env-a", runId: "run-1", workflow: "fix", step: "write" },
    });
    // The link's own access is the ceiling, not a mode narrower than it.
    assert.equal(launch?.runtimeMode, "full-access");

    // An administrator's ceiling here holds over a full-access link too.
    const capped = peer(
      {
        oni_project_list: [
          { projects: [{ id: "p-app", workspaceRoot: "/work/app" }], nextCursor: null },
        ],
        oni_thread_launch: [{ threadId: "thread-1", runId: "turn-1" }],
      },
      true,
      "auto",
    );
    yield* Effect.provide(
      Effect.flatMap(WorkflowAgentLinks.make, (links) =>
        links.start(input, "env-b", "workflow:k", EnvironmentId.make("env-a")),
      ),
      capped.layer,
    );
    assert.equal(capped.calls.at(-1)?.runtimeMode, "auto");
  }),
);

it.effect("registers a folder that is not a project there yet", () =>
  Effect.gen(function* () {
    for (const [project, title] of [
      [input.project, "app"],
      ["/work/team\\api", "team\\api"],
      ["C:\\work\\app", "app"],
    ]) {
      const there = peer({
        oni_project_list: [{ projects: [], nextCursor: null }],
        oni_project_create: [{ id: "p-new", workspaceRoot: project }],
        oni_thread_launch: [{ threadId: "thread-1", runId: "turn-1" }],
      });
      yield* Effect.provide(
        Effect.flatMap(WorkflowAgentLinks.make, (links) =>
          links.start({ ...input, project }, "env-b", "workflow:k", EnvironmentId.make("env-a")),
        ),
        there.layer,
      );
      assert.deepStrictEqual(
        there.calls.map((call) => [call.tool, call.arguments.projectId ?? call.arguments.title]),
        [
          ["oni_project_list", undefined],
          ["oni_project_create", title],
          ["oni_thread_launch", "p-new"],
        ],
      );
    }
  }),
);

it.effect("finds a Windows folder there however its separators are written", () =>
  Effect.gen(function* () {
    const there = peer({
      oni_project_list: [
        { projects: [{ id: "p-app", workspaceRoot: "C:\\work\\app" }], nextCursor: null },
      ],
      oni_thread_launch: [{ threadId: "thread-1", runId: "turn-1" }],
    });
    yield* Effect.provide(
      Effect.flatMap(WorkflowAgentLinks.make, (links) =>
        links.start(
          { ...input, project: "C:/work/app" },
          "env-b",
          "workflow:k",
          EnvironmentId.make("env-a"),
        ),
      ),
      there.layer,
    );
    assert.equal(there.calls.at(-1)?.arguments.projectId, "p-app");
  }),
);

it.effect("does not start anything while linked environments is off", () =>
  Effect.gen(function* () {
    const there = peer({}, false);
    const error = yield* Effect.provide(
      Effect.flatMap(WorkflowAgentLinks.make, (links) =>
        links.start(input, "env-b", "workflow:k", EnvironmentId.make("env-a")),
      ),
      there.layer,
    ).pipe(Effect.flip);
    assert.match(error.reason, /Linked environments is off/);
    assert.deepStrictEqual(there.calls, []);
  }),
);

it.effect("waits in pieces, rides out a dropped route, and returns the output and reply", () =>
  Effect.gen(function* () {
    const there = peer({
      oni_thread_wait: [
        { ...waited, timedOut: true },
        new OrchestratorMcpFailure({ code: "orchestration_error", message: "unreachable" }),
        { ...waited, timedOut: false, output: { branch: "fix/1" } },
      ],
      oni_thread_read: [
        run("completed"),
        {
          recentRuns: [],
          hasMore: false,
          nextPosition: null,
          items: [
            { type: "assistant_message", runId: "turn-0", text: "earlier", textTruncated: false },
            {
              type: "assistant_message",
              runId: "turn-1",
              itemId: "item-9",
              text: "Done, ",
              textTruncated: true,
              nextTextOffset: 6,
            },
          ],
        },
        { recentRuns: [], items: [{ text: "pushed.", textTruncated: false }] },
      ],
    });
    const result = yield* Effect.provide(
      Effect.flatMap(WorkflowAgentLinks.make, (links) =>
        links.result({ threadId: "thread-1", turnId: "turn-1" }, "env-b"),
      ),
      there.layer,
    ).pipe(Effect.raceFirst(Effect.forever(TestClock.adjust("1 second"))));
    assert.deepStrictEqual(result, {
      ending: "completed",
      detail: "",
      reply: "Done, pushed.",
      output: { branch: "fix/1" },
    });
    const waits = there.calls.filter((call) => call.tool === "oni_thread_wait");
    assert.equal(waits.length, 3);
    // A peer waits 45 s at most, so a longer piece would come back early.
    assert.isTrue(waits.every((call) => Number(call.arguments.timeoutMs) <= 45_000));
    // The reply is read from the newest page, not by how a peer pages a read without a cursor.
    const reads = there.calls.filter((call) => call.tool === "oni_thread_read");
    assert.equal(reads[1]?.arguments.beforePosition, Number.MAX_SAFE_INTEGER);
  }),
);

it.effect("a turn a restart continued there is followed to its continuation's reply", () =>
  Effect.gen(function* () {
    const there = peer({
      oni_thread_wait: [{ ...waited, runId: "turn-2", timedOut: false, output: { head: "abc" } }],
      oni_thread_read: [
        {
          recentRuns: [
            { runId: "turn-2", status: "completed" },
            { runId: "turn-1", status: "interrupted" },
          ],
          items: [],
        },
        // An older peer refuses a read back from the end, and reads from the start without one.
        new OrchestratorMcpFailure({
          code: "invalid_request",
          message:
            "Invalid parameters for tool 'oni_thread_read': Unknown argument \"beforePosition\". Accepted arguments: threadId, afterPosition, limit.",
        }),
        {
          recentRuns: [],
          hasMore: false,
          nextPosition: null,
          items: [
            { type: "assistant_message", runId: "turn-1", text: "cut off", textTruncated: false },
            { type: "assistant_message", runId: "turn-2", text: "finished", textTruncated: false },
          ],
        },
      ],
    });
    const result = yield* Effect.provide(
      Effect.flatMap(WorkflowAgentLinks.make, (links) =>
        links.result({ threadId: "thread-1", turnId: "turn-1" }, "env-b"),
      ),
      there.layer,
    );
    assert.deepStrictEqual(result, {
      ending: "completed",
      detail: "",
      reply: "finished",
      output: { head: "abc" },
    });
    const reads = there.calls.filter((call) => call.tool === "oni_thread_read");
    assert.deepStrictEqual(
      reads.slice(1).map((call) => [call.arguments.beforePosition, call.arguments.afterPosition]),
      [
        [Number.MAX_SAFE_INTEGER, undefined],
        [undefined, undefined],
      ],
    );
  }),
);

const assistant = (runId: string, text: string) => ({
  type: "assistant_message",
  runId,
  text,
  textTruncated: false,
});

it.effect("the reply is read back from the newest page, and from the start on an older peer", () =>
  Effect.gen(function* () {
    const there = peer({
      oni_thread_wait: [{ ...waited, timedOut: false, output: { head: "abc" } }],
      oni_thread_read: [
        run("completed"),
        // Later turns fill the newest page; the step's reply is on the page before it.
        { recentRuns: [], earlierPosition: 200, items: [assistant("turn-2", "later")] },
        {
          recentRuns: [],
          earlierPosition: 100,
          items: [assistant("turn-1", "draft"), assistant("turn-1", "final")],
        },
      ],
    });
    const result = yield* Effect.provide(
      Effect.flatMap(WorkflowAgentLinks.make, (links) =>
        links.result({ threadId: "thread-1", turnId: "turn-1" }, "env-b"),
      ),
      there.layer,
    );
    assert.equal(result.reply, "final");
    // Pages older than the reply are never asked for.
    assert.deepStrictEqual(
      there.calls
        .filter((call) => call.tool === "oni_thread_read")
        .slice(1)
        .map((call) => call.arguments.beforePosition),
      [Number.MAX_SAFE_INTEGER, 200],
    );

    // A peer that predates the cursor ignores it and pages forward from the start.
    const older = peer({
      oni_thread_wait: [{ ...waited, timedOut: false, output: { head: "abc" } }],
      oni_thread_read: [
        run("completed"),
        { recentRuns: [], hasMore: true, nextPosition: 99, items: [assistant("turn-1", "a")] },
        { recentRuns: [], hasMore: false, nextPosition: 150, items: [assistant("turn-1", "b")] },
      ],
    });
    const fromStart = yield* Effect.provide(
      Effect.flatMap(WorkflowAgentLinks.make, (links) =>
        links.result({ threadId: "thread-1", turnId: "turn-1" }, "env-b"),
      ),
      older.layer,
    );
    assert.equal(fromStart.reply, "b");
    assert.deepStrictEqual(
      older.calls
        .filter((call) => call.tool === "oni_thread_read")
        .slice(2)
        .map((call) => call.arguments.afterPosition),
      [99],
    );
  }),
);

it.effect("a peer that answers the same page again fails the reply instead of asking forever", () =>
  Effect.gen(function* () {
    const stuck = (page: Record<string, unknown>) => ({
      recentRuns: [],
      ...page,
      items: [assistant("turn-0", "other")],
    });
    // Read back from the end, and forward on a peer that predates reading back.
    for (const page of [{ earlierPosition: 100 }, { hasMore: true, nextPosition: 100 }]) {
      const there = peer({
        oni_thread_wait: [{ ...waited, timedOut: false, output: { head: "abc" } }],
        oni_thread_read: [run("completed"), ...Array.from({ length: 4 }, () => stuck(page))],
      });
      const error = yield* Effect.provide(
        Effect.flatMap(WorkflowAgentLinks.make, (links) =>
          links.result({ threadId: "thread-1", turnId: "turn-1" }, "env-b"),
        ),
        there.layer,
      ).pipe(Effect.flip);
      assert.match(error.reason, /answered the same page of thread thread-1 again/);
      assert.isTrue(there.calls.filter((call) => call.tool === "oni_thread_read").length <= 3);
    }

    // A long reply whose next piece starts where the last one did.
    const piece = { text: "again", textTruncated: true, nextTextOffset: 6 };
    const long = peer({
      oni_thread_wait: [{ ...waited, timedOut: false, output: { head: "abc" } }],
      oni_thread_read: [
        run("completed"),
        {
          recentRuns: [],
          items: [{ ...assistant("turn-1", "Done, "), ...piece, text: "Done, ", itemId: "i" }],
        },
        ...Array.from({ length: 4 }, () => ({ recentRuns: [], items: [piece] })),
      ],
    });
    const stalled = yield* Effect.provide(
      Effect.flatMap(WorkflowAgentLinks.make, (links) =>
        links.result({ threadId: "thread-1", turnId: "turn-1" }, "env-b"),
      ),
      long.layer,
    ).pipe(Effect.flip);
    assert.match(stalled.reason, /answered the same page of thread thread-1 again/);
    assert.deepStrictEqual(
      long.calls.flatMap((call) => call.arguments.textOffset ?? []),
      [6],
    );
  }),
);

it.effect("a turn someone stopped there is stopped, and a refused link fails at once", () =>
  Effect.gen(function* () {
    const stopped = peer({
      oni_thread_wait: [{ ...waited, timedOut: false }],
      oni_thread_read: [run("interrupted")],
      oni_thread_interrupt: [{ threadId: "thread-1", runId: "turn-1", status: "interrupted" }],
    });
    const result = yield* Effect.provide(
      Effect.flatMap(WorkflowAgentLinks.make, (links) =>
        links.result({ threadId: "thread-1", turnId: "turn-1" }, "env-b"),
      ),
      stopped.layer,
    );
    assert.deepStrictEqual(result, { ending: "stopped", detail: "interrupted", reply: "" });
    // Stopping names the step's turn, so a person's turn on that thread keeps going.
    yield* Effect.provide(
      Effect.flatMap(WorkflowAgentLinks.make, (links) =>
        links.interrupt("thread-1", "env-b", "turn-1"),
      ),
      stopped.layer,
    );
    assert.deepInclude(stopped.calls.at(-1)?.arguments, { threadId: "thread-1", runId: "turn-1" });

    const revoked = peer({
      oni_thread_wait: [
        new OrchestratorMcpFailure({ code: "capability_denied", message: "link revoked" }),
      ],
    });
    const error = yield* Effect.provide(
      Effect.flatMap(WorkflowAgentLinks.make, (links) =>
        links.result({ threadId: "thread-1", turnId: "turn-1" }, "env-b"),
      ),
      revoked.layer,
    ).pipe(Effect.flip);
    assert.match(error.reason, /link revoked/);
    // A cancelled run's sweep counts it as stopped instead of asking forever.
    assert.isTrue(error.final);
    assert.equal(revoked.calls.length, 1);

    // A turn that is no longer the thread's running one has nothing left to stop.
    const notRunning = peer({
      oni_thread_interrupt: [
        new OrchestratorMcpFailure({ code: "thread_not_interruptible", message: "not running" }),
      ],
    });
    const refusal = yield* Effect.provide(
      Effect.flatMap(WorkflowAgentLinks.make, (links) =>
        links.interrupt("thread-1", "env-b", "turn-1"),
      ),
      notRunning.layer,
    ).pipe(Effect.flip);
    assert.isTrue(refusal.final);
  }),
);

it.effect("a turn that ended without its result is waited past until a later turn gives it", () =>
  Effect.gen(function* () {
    const there = peer({
      oni_thread_wait: [
        // The turn ended. The agent is waiting to be woken, and has submitted nothing.
        { ...waited, timedOut: false },
        // Quiet for five minutes, in pieces. A turn that woke in between is under way...
        ...Array.from({ length: 7 }, () => ({ ...waited, timedOut: true })),
        // ...so the wait starts over, and that turn submits.
        { ...waited, timedOut: false, output: { head: "abc" } },
      ],
      oni_thread_read: [
        run("completed"),
        { recentRuns: [{ runId: "turn-2", status: "running" }], items: [] },
        { recentRuns: [], hasMore: false, nextPosition: null, items: [] },
      ],
    });
    const result = yield* Effect.provide(
      Effect.flatMap(WorkflowAgentLinks.make, (links) =>
        links.result({ threadId: "thread-1", turnId: "turn-1" }, "env-b", {
          quiet: Duration.minutes(5),
        }),
      ),
      there.layer,
    ).pipe(Effect.raceFirst(Effect.forever(TestClock.adjust("1 second"))));
    assert.deepStrictEqual(result.output, { head: "abc" });
    const waits = there.calls.filter((call) => call.tool === "oni_thread_wait");
    assert.isTrue(waits.slice(1).every((call) => call.arguments.untilOutput === true));
  }),
);

it.effect("a thread that stays quiet gives the turn back without a result", () =>
  Effect.gen(function* () {
    const there = peer({
      oni_thread_wait: [
        { ...waited, timedOut: false },
        ...Array.from({ length: 7 }, () => ({ ...waited, timedOut: true })),
      ],
      oni_thread_read: [
        run("completed"),
        // Nothing has run since the turn the step knows.
        run("completed"),
        { recentRuns: [], hasMore: false, nextPosition: null, items: [] },
      ],
    });
    const result = yield* Effect.provide(
      Effect.flatMap(WorkflowAgentLinks.make, (links) =>
        links.result({ threadId: "thread-1", turnId: "turn-1" }, "env-b", {
          quiet: Duration.minutes(5),
        }),
      ),
      there.layer,
    ).pipe(Effect.raceFirst(Effect.forever(TestClock.adjust("1 second"))));
    assert.deepStrictEqual(result, { ending: "completed", detail: "", reply: "" });
  }),
);
