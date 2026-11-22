import { describe, expect, it } from "vite-plus/test";
import type { OrchestrationV2ThreadProjection } from "@onicode/contracts";

import {
  canForkProjectedAssistantItem,
  canPromoteQueuedRun,
  deriveThreadQueueWorkflowState,
  getUserQueuedThreadRuns,
  resolveLatestMergeBackRun,
  resolveQueuedRunSendNow,
  threadCanSteer,
  threadSupportsProviderHandoff,
} from "./threadWorkflows.ts";

const capabilities = (input?: {
  readonly queued?: boolean;
  readonly steer?: boolean;
  readonly restartSteer?: boolean;
  readonly interrupt?: boolean;
  readonly nativeFork?: boolean;
  readonly portableFork?: boolean;
  readonly goals?: boolean;
}) =>
  ({
    turns: {
      supportsQueuedMessages: input?.queued ?? false,
      supportsActiveSteering: input?.steer ?? false,
      supportsSteeringByInterruptRestart: input?.restartSteer ?? false,
      supportsInterrupt: input?.interrupt ?? false,
      ...(input?.goals === undefined ? {} : { supportsNativeGoals: input.goals }),
    },
    threads: {
      canForkThread: input?.nativeFork ?? false,
      canForkFromTurn: input?.nativeFork ?? false,
    },
    identity: { nativeThreadIds: input?.nativeFork ? "strong" : "none" },
    context: { supportsFullThreadHandoff: input?.portableFork ?? false },
  }) as never;

describe("thread workflows", () => {
  it.each([
    ["queue only", { queued: true }, {}, false],
    ["session still attaching", undefined, {}, true],
    ["native steering", { steer: true }, {}, true],
    ["interrupt and restart", { interrupt: true, restartSteer: true }, {}, true],
    ["restart without interrupt", { restartSteer: true }, {}, false],
    ["explicit compaction", { steer: true }, { command: true }, false],
    ["automatic compaction", { steer: true }, { running: true }, false],
    [
      "automatic compaction on a server that holds steers",
      { steer: true },
      { running: true, held: true },
      true,
    ],
    [
      "held steer on a session that cannot steer",
      { queued: true },
      { running: true, held: true },
      false,
    ],
    // The provider turn ended; the server would start the steer as a turn of its own.
    ["run waiting for its checkpoint", { steer: true }, { waiting: true }, false],
  ] as const)("gates composer steering for %s", (_, turns, compaction, expected) => {
    const projection = {
      thread: { id: "thread", activeProviderThreadId: "provider-thread" },
      runs: [
        {
          id: "active",
          status: "waiting" in compaction ? "waiting" : "running",
          providerThreadId: "provider-thread",
        },
      ],
      providerThreads: [{ id: "provider-thread", providerSessionId: "session" }],
      providerSessions: [
        ...(turns === undefined
          ? []
          : [{ id: "session", status: "running", capabilities: capabilities(turns) }]),
        { id: "other", status: "ready", capabilities: capabilities({ steer: true }) },
      ],
      messages: [],
    } as unknown as OrchestrationV2ThreadProjection;
    expect(
      threadCanSteer(projection, {
        compactCommandIsActive: "command" in compaction,
        compactionRunning: "running" in compaction,
        activeRunId: projection.runs[0]!.id,
        steersAfterCompaction: "held" in compaction,
      }),
    ).toBe(expected);
  });

  it("skips message history classification when no runs are queued", () => {
    let metadataReads = 0;
    const message = {
      id: "message",
      get delegatedCompletion() {
        metadataReads++;
        return undefined;
      },
    };
    const projection = {
      runs: [{ id: "active", status: "running", userMessageId: "message", ordinal: 1 }],
      messages: [message],
    } as never;

    expect(getUserQueuedThreadRuns(projection)).toEqual([]);
    expect(metadataReads).toBe(0);
  });

  it("allows a completed thread to switch providers after its session detaches", () => {
    const projection = {
      thread: {
        id: "thread",
        activeProviderThreadId: "provider-thread",
        modelSelection: { instanceId: "cursor", model: "grok-4.7" },
      },
      runs: [{ id: "run", status: "completed" }],
      providerSessions: [],
      providerThreads: [
        {
          id: "provider-thread",
          appThreadId: "thread",
          providerInstanceId: "cursor",
          providerSessionId: "detached-session",
          nativeThreadRef: { driver: "cursor", nativeId: "agent-123", strength: "strong" },
        },
      ],
    } as unknown as OrchestrationV2ThreadProjection;

    expect(threadSupportsProviderHandoff(projection)).toBe(true);
    expect(
      threadSupportsProviderHandoff({
        ...projection,
        runs: [{ ...projection.runs[0]!, status: "running" }],
      }),
    ).toBe(false);
    expect(
      threadSupportsProviderHandoff({
        ...projection,
        providerThreads: [{ ...projection.providerThreads[0]!, nativeThreadRef: null }],
      }),
    ).toBe(true);
    expect(threadSupportsProviderHandoff({ ...projection, providerThreads: [] })).toBe(false);
  });

  it("sorts queued messages and gates promotion from capabilities", () => {
    const state = deriveThreadQueueWorkflowState({
      thread: { id: "thread", activeProviderThreadId: "provider-thread" },
      runs: [
        {
          id: "active",
          status: "running",
          providerThreadId: "provider-thread",
          activeAttemptId: "attempt-active",
          ordinal: 1,
        },
        { id: "later", status: "queued", userMessageId: "message-later", ordinal: 3 },
        {
          id: "first",
          status: "queued",
          userMessageId: "message-first",
          ordinal: 2,
          queuePosition: 1,
        },
      ],
      messages: [
        {
          id: "message-first",
          text: "First",
          attachments: [
            {
              type: "image",
              id: "attachment-first",
              name: "first.png",
              mimeType: "image/png",
              sizeBytes: 64,
            },
          ],
        },
        { id: "message-later", text: "Later" },
      ],
      providerTurns: [
        {
          id: "provider-turn-active",
          runAttemptId: "attempt-active",
          status: "running",
        },
      ],
      providerThreads: [
        {
          id: "provider-thread",
          appThreadId: "thread",
          providerSessionId: "provider-session",
        },
      ],
      providerSessions: [
        {
          id: "provider-session",
          status: "running",
          capabilities: capabilities({ queued: true, interrupt: true, restartSteer: true }),
        },
      ],
    } as never);

    expect(state.queuedRuns.map(({ run, text }) => [run.id, text])).toEqual([
      ["first", "First"],
      ["later", "Later"],
    ]);
    expect(state.queuedRuns.map(({ attachments }) => attachments.map(({ id }) => id))).toEqual([
      ["attachment-first"],
      [],
    ]);
    expect(state.activeRun?.id).toBe("active");
    expect(state.canPromoteToSteer).toBe(true);
    expect(state.canSendNow).toBe(false);
  });

  it.each(["completed", "interrupted", "cancelled", "failed"])(
    "allows sending a held message after a %s turn without a live session",
    (status) => {
      const state = deriveThreadQueueWorkflowState({
        thread: { id: "thread", activeProviderThreadId: null },
        runs: [
          { id: "previous", status, ordinal: 1 },
          { id: "queued", status: "queued", userMessageId: "message", ordinal: 2, queueHeld: true },
        ],
        messages: [{ id: "message", text: "Send this" }],
        providerThreads: [],
        providerTurns: [],
        providerSessions: [],
      } as never);

      expect(state.canSendNow).toBe(true);
      expect(state.canPromoteToSteer).toBe(false);
      expect(state.isHeld).toBe(true);
    },
  );

  it.each(["preparing", "starting", "running", "waiting"])(
    "does not offer send now while a turn is %s",
    (status) => {
      const state = deriveThreadQueueWorkflowState({
        thread: { id: "thread", activeProviderThreadId: null },
        runs: [{ id: "active", status, ordinal: 1 }],
        messages: [],
        providerThreads: [],
        providerTurns: [],
        providerSessions: [],
      } as never);

      expect(state.canSendNow).toBe(false);
    },
  );

  it("offers to skip a running compaction even when the provider cannot steer", () => {
    const state = deriveThreadQueueWorkflowState({
      thread: { id: "thread", activeProviderThreadId: null },
      runs: [
        { id: "compact", status: "running", userMessageId: "message-compact", ordinal: 1 },
        { id: "queued", status: "queued", userMessageId: "message-queued", ordinal: 2 },
        { id: "command", status: "queued", userMessageId: "message-command", ordinal: 3 },
      ],
      messages: [
        { id: "message-compact", role: "user", text: "/compact", attachments: [] },
        { id: "message-queued", role: "user", text: "Next", attachments: [] },
        { id: "message-command", role: "user", text: "/review", attachments: [] },
      ],
      providerThreads: [],
      providerTurns: [],
      providerSessions: [],
    } as never);

    expect(state.canPromoteToSteer).toBe(true);
    expect(state.promoteSkipsCompaction).toBe(true);
    // Skipping compaction starts a command as its own turn, so it is not just moved first.
    expect(resolveQueuedRunSendNow(state, "command" as never)).toEqual({
      type: "steer",
      targetRunId: "compact",
    });
  });

  it.each([
    { run: {}, skipsCompaction: false, expected: true },
    { run: { freshContext: true }, skipsCompaction: false, expected: false },
    { run: { outputSchema: { type: "object" } }, skipsCompaction: false, expected: false },
    { run: { freshContext: true }, skipsCompaction: true, expected: true },
    { run: { outputSchema: { type: "object" } }, skipsCompaction: true, expected: true },
  ])(
    "steers with $run into a turn only when it needs no turn of its own, skipsCompaction=$skipsCompaction",
    ({ run, skipsCompaction, expected }) => {
      const workflow = { canPromoteToSteer: true, promoteSkipsCompaction: skipsCompaction };
      expect(canPromoteQueuedRun(workflow, run)).toBe(expected);
    },
  );

  it.each([
    { steer: false, plain: null, review: { type: "move-first", beforeRunId: "plain" }, goal: null },
    {
      steer: true,
      plain: { type: "steer", targetRunId: "active" },
      review: { type: "move-first", beforeRunId: "plain" },
      goal: { type: "steer", targetRunId: "active" },
    },
    // The server moves /goal first for a provider that runs it, stopping the turn if it must.
    {
      steer: false,
      goals: true,
      plain: null,
      review: { type: "move-first", beforeRunId: "plain" },
      goal: { type: "steer", targetRunId: "active" },
    },
    // Without a provider that runs it, /goal is a command like any other.
    {
      steer: true,
      goals: false,
      plain: { type: "steer", targetRunId: "active" },
      review: { type: "move-first", beforeRunId: "plain" },
      goal: { type: "move-first", beforeRunId: "plain" },
    },
    // A /goal queued for another provider is left to the server, which knows that provider.
    {
      steer: true,
      goals: false,
      goalInstance: "other",
      plain: { type: "steer", targetRunId: "active" },
      review: { type: "move-first", beforeRunId: "plain" },
      goal: { type: "steer", targetRunId: "active" },
    },
    // One that named no model runs on the thread's pick, this session's provider, as the server has it.
    {
      steer: true,
      goals: false,
      goalInstance: "other",
      goalFollowsThread: true,
      plain: { type: "steer", targetRunId: "active" },
      review: { type: "move-first", beforeRunId: "plain" },
      goal: { type: "move-first", beforeRunId: "plain" },
    },
  ])("moves a queued command first instead of steering it, steer=$steer goals=$goals", (row) => {
    const queued = ["plain", "review", "goal"];
    const text = { plain: "Next", review: "/review", goal: "/goal pause" } as const;
    const projection = (activeStatus: string) =>
      ({
        thread: {
          id: "thread",
          activeProviderThreadId: "provider-thread",
          modelSelection: { instanceId: "codex" },
        },
        runs: [
          {
            id: "active",
            status: activeStatus,
            providerThreadId: "provider-thread",
            activeAttemptId: "attempt-active",
            ordinal: 1,
          },
          ...queued.map((id, index) => ({
            id,
            status: "queued",
            userMessageId: `message-${id}`,
            ordinal: index + 2,
            providerInstanceId: id === "goal" && "goalInstance" in row ? row.goalInstance : "codex",
          })),
        ],
        messages: queued.map((id) => ({
          id: `message-${id}`,
          text: text[id as keyof typeof text],
          attachments: [],
          ...(id === "goal" && "goalFollowsThread" in row ? { followsThreadSelection: true } : {}),
        })),
        providerTurns: [{ id: "turn", runAttemptId: "attempt-active", status: "running" }],
        providerThreads: [
          { id: "provider-thread", appThreadId: "thread", providerSessionId: "session" },
        ],
        providerSessions: [
          {
            id: "session",
            status: "running",
            providerInstanceId: "codex",
            capabilities: capabilities({
              queued: true,
              steer: row.steer,
              ...(row.goals === undefined ? {} : { goals: row.goals }),
            }),
          },
        ],
      }) as never;
    const running = deriveThreadQueueWorkflowState(projection("running"));
    expect(queued.map((id) => resolveQueuedRunSendNow(running, id as never))).toEqual([
      row.plain,
      row.review,
      row.goal,
    ]);
    // Moving first needs no turn of its own, so a fresh-session command still moves.
    const fresh = deriveThreadQueueWorkflowState({
      ...(projection("running") as object),
      runs: (projection("running") as { runs: ReadonlyArray<object> }).runs.map((run) =>
        (run as { id: string }).id === "review" ? { ...run, freshContext: true } : run,
      ),
    } as never);
    expect(resolveQueuedRunSendNow(fresh, "review" as never)).toEqual(row.review);
    // A command already at the front has nowhere to move.
    const reviewFirst = deriveThreadQueueWorkflowState({
      ...(projection("running") as object),
      runs: (projection("running") as { runs: ReadonlyArray<object> }).runs.map((run) =>
        (run as { id: string }).id === "review" ? { ...run, queuePosition: 1 } : run,
      ),
    } as never);
    expect(resolveQueuedRunSendNow(reviewFirst, "review" as never)).toBeNull();
    const idle = deriveThreadQueueWorkflowState(projection("completed"));
    expect(resolveQueuedRunSendNow(idle, "review" as never)).toEqual({ type: "start" });
  });

  it("marks a held steer as waiting only while its turn is compacting", () => {
    const waiting = (
      activeStatus: "running" | "waiting",
      compaction: "running" | "completed" | "failed" | null,
    ) =>
      deriveThreadQueueWorkflowState({
        thread: { id: "thread", activeProviderThreadId: null },
        runs: [
          { id: "earlier", status: "completed", ordinal: 1 },
          { id: "active", status: activeStatus, ordinal: 2 },
          { id: "steer", status: "queued", ordinal: 3, steerWhenReadyRunId: "active" },
          // The turn this one waited for is over; the server keeps the field.
          { id: "stale", status: "queued", ordinal: 4, steerWhenReadyRunId: "earlier" },
          { id: "follow-up", status: "queued", ordinal: 5 },
        ],
        messages: [],
        turnItems: [
          { type: "compaction", runId: "earlier", status: "running" },
          ...(compaction === null
            ? []
            : [{ type: "compaction", runId: "active", status: compaction }]),
        ],
        providerThreads: [],
        providerTurns: [],
        providerSessions: [],
      } as never)
        .queuedRuns.filter((queued) => queued.awaitsCompaction)
        .map(({ run }) => run.id);

    expect(waiting("running", "running")).toEqual(["steer"]);
    expect(waiting("running", "completed")).toEqual([]);
    expect(waiting("running", "failed")).toEqual([]);
    expect(waiting("running", null)).toEqual([]);
    expect(waiting("waiting", "running")).toEqual([]);
  });

  it("keeps held messages visible and clears the hold when they leave the queue", () => {
    for (const status of ["queued", "cancelled", "starting"] as const) {
      const state = deriveThreadQueueWorkflowState({
        thread: { id: "thread", activeProviderThreadId: null },
        runs: [{ id: "held", status, userMessageId: "message", ordinal: 1, queueHeld: true }],
        messages: [{ id: "message", text: "Saved message" }],
        providerThreads: [],
        providerTurns: [],
        providerSessions: [],
      } as never);
      expect(state.isHeld).toBe(status === "queued");
      expect(state.queuedRuns.map(({ text }) => text)).toEqual(
        status === "queued" ? ["Saved message"] : [],
      );
    }
  });

  it("hides automatic completion delivery from the visible queue", () => {
    const state = deriveThreadQueueWorkflowState({
      thread: { id: "thread", activeProviderThreadId: null },
      runs: [
        {
          id: "provider-wake",
          status: "queued",
          userMessageId: "provider-wake",
          ordinal: 4,
          queuePosition: 3,
        },
        {
          id: "automatic",
          status: "queued",
          userMessageId: "message-automatic",
          ordinal: 2,
          queuePosition: 1,
        },
        {
          id: "visible",
          status: "queued",
          userMessageId: "message-visible",
          ordinal: 3,
          queuePosition: 2,
        },
      ],
      messages: [
        {
          id: "provider-wake",
          text: "Model-facing text",
          notification: {
            source: { kind: "monitor" },
            outcome: "updated",
            summary: "Monitor updated",
          },
        },
        {
          id: "message-automatic",
          text: "A delegated task reached a terminal state.",
          delegatedCompletion: {
            generation: 1,
            parentRunId: "run:parent",
            taskIds: ["task:child"],
          },
        },
        { id: "message-visible", text: "Visible queued message" },
      ],
      providerTurns: [],
      providerThreads: [],
      providerSessions: [],
    } as never);

    expect(state.queuedRuns.map(({ run, text }) => [run.id, text])).toEqual([
      ["visible", "Visible queued message"],
    ]);
  });

  it("removes only the promoted head from the visible queue", () => {
    const state = deriveThreadQueueWorkflowState({
      thread: { id: "thread", activeProviderThreadId: "provider-thread" },
      runs: [
        {
          id: "promoted",
          status: "starting",
          userMessageId: "message-promoted",
          providerThreadId: "provider-thread",
          ordinal: 2,
          queuePosition: null,
        },
        {
          id: "still-queued",
          status: "queued",
          userMessageId: "message-still-queued",
          providerThreadId: "provider-thread",
          ordinal: 3,
          queuePosition: 2,
        },
      ],
      messages: [
        { id: "message-promoted", text: "Run now" },
        { id: "message-still-queued", text: "Wait longer" },
      ],
      providerTurns: [],
      providerThreads: [],
      providerSessions: [],
    } as never);

    expect(state.activeRun?.id).toBe("promoted");
    expect(state.queuedRuns.map(({ run, text }) => [run.id, text])).toEqual([
      ["still-queued", "Wait longer"],
    ]);
  });

  it.each(["preparing", "starting", "waiting"] as const)(
    "defers steering during setup and blocks it while waiting (%s)",
    (status) => {
      const state = deriveThreadQueueWorkflowState({
        thread: { id: "thread", activeProviderThreadId: "provider-thread" },
        runs: [
          {
            id: "active",
            status,
            providerThreadId: "provider-thread",
            activeAttemptId: "attempt-active",
            ordinal: 1,
          },
          { id: "queued", status: "queued", userMessageId: "message", ordinal: 2 },
        ],
        messages: [{ id: "message", text: "Queued" }],
        providerTurns: [
          {
            id: "provider-turn-active",
            runAttemptId: "attempt-active",
            status: status === "waiting" ? "completed" : "starting",
          },
        ],
        providerThreads: [
          {
            id: "provider-thread",
            appThreadId: "thread",
            providerSessionId: "provider-session",
          },
        ],
        providerSessions: [
          {
            id: "provider-session",
            status: "running",
            capabilities: capabilities({ queued: true, steer: true }),
          },
        ],
      } as never);

      expect(state.canPromoteToSteer).toBe(status !== "waiting");
    },
  );

  it("allows native, portable, and capability-unknown exact-run forks", () => {
    const projectedItem = {
      item: { type: "assistant_message", runId: "run", status: "completed" },
    } as never;

    expect(
      canForkProjectedAssistantItem({
        projectedItem,
        capabilities: capabilities({ nativeFork: true }),
      }),
    ).toBe(true);
    expect(
      canForkProjectedAssistantItem({
        projectedItem,
        capabilities: capabilities({ portableFork: true }),
      }),
    ).toBe(true);
    expect(canForkProjectedAssistantItem({ projectedItem })).toBe(true);
    expect(
      canForkProjectedAssistantItem({
        projectedItem: {
          item: { type: "assistant_message", runId: "run", status: "running" },
        } as never,
      }),
    ).toBe(false);
  });

  it("merges the newest provider-finished run while checkpoint capture is pending", () => {
    const projection = {
      runs: [
        { id: "newest-queued", status: "queued", ordinal: 3 },
        { id: "older-completed", status: "completed", ordinal: 1 },
        { id: "newest-finished", status: "waiting", ordinal: 2 },
      ],
    } as never;

    expect(resolveLatestMergeBackRun(projection)?.id).toBe("newest-finished");
  });

  it.each(["preparing", "starting", "running"] as const)(
    "does not merge older history while a newer run is %s",
    (status) => {
      const projection = {
        runs: [
          { id: "older-completed", status: "completed", ordinal: 1 },
          { id: "newer-active", status, ordinal: 2 },
        ],
      } as never;

      expect(resolveLatestMergeBackRun(projection)).toBeNull();
    },
  );
});
