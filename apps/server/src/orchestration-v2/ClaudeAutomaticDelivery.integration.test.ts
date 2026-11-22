import type { SDKMessage, SDKUserMessage } from "@anthropic-ai/claude-agent-sdk";
import * as NodeCrypto from "@effect/platform-node/NodeCrypto";
import * as NodeServices from "@effect/platform-node/NodeServices";
import { assert, it, vi } from "@effect/vitest";
import {
  ClaudeSettings,
  CommandId,
  EventId,
  MessageId,
  NodeId,
  ProjectId,
  ProviderInstanceId,
  type RunId,
  ScheduledTaskUpsertInput,
  ThreadId,
  type OrchestrationV2DomainEvent,
  type OrchestrationV2Run,
} from "@onicode/contracts";
import * as DateTime from "effect/DateTime";
import * as Deferred from "effect/Deferred";
import * as SecureRandom from "effect/crypto/SecureRandom";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Fiber from "effect/Fiber";
import * as Layer from "effect/Layer";
import * as Path from "effect/Path";
import * as Queue from "effect/Queue";
import * as Schema from "effect/Schema";
import * as Stream from "effect/Stream";
import * as TestClock from "effect/testing/TestClock";
import * as ScheduledTaskService from "../scheduledTasks/ScheduledTaskService.ts";
import * as ManagedPolicy from "../policy/ManagedPolicy.ts";
import * as SecretRequests from "../secrets/SecretRequests.ts";
import * as ManagedProjectFolders from "../project/ManagedProjectFolders.ts";
import * as Scheduler from "../scheduling/Scheduler.ts";
import * as SqlitePersistence from "../persistence/Sqlite.ts";
import * as ClaudeAdapterV2 from "./Adapters/ClaudeAdapterV2.ts";
import * as CommandReceiptStore from "./CommandReceiptStore.ts";
import * as EffectWorker from "./EffectWorker.ts";
import * as EventSink from "./EventSink.ts";
import * as IdAllocator from "@onicode/provider-core/server/IdAllocator";
import * as McpProviderSessions from "@onicode/provider-core/server/McpProviderSessions";
import * as ProviderContinuationRequests from "@onicode/provider-core/server/ProviderContinuationRequests";
import * as LegacyV1ThreadImporter from "./legacy/LegacyV1ThreadImporter.ts";
import * as Orchestrator from "./Orchestrator.ts";
import * as ProjectionStore from "./ProjectionStore.ts";
import * as ProviderAdapterRegistry from "./ProviderAdapterRegistry.ts";
import * as ThreadLaunchService from "./ThreadLaunchService.ts";
import * as ThreadManagementService from "./ThreadManagementService.ts";
import * as ServerSettings from "../serverSettings.ts";
import * as ProviderReplayHarness from "./testkit/ProviderReplayHarness.ts";
import { checkpointWorkspace } from "@onicode/provider-testing/replayWorkspace";

const sessionId = "automatic-delivery-session";
const settings = Schema.decodeSync(ClaudeSettings)({});
const decodeScheduledTask = Schema.decodeEffect(ScheduledTaskUpsertInput);
const refusal =
  "The user doesn't want to take this action right now. STOP what you are doing and wait for the user to tell you how to proceed.";
const statusIds = ["status-1", "status-2", "status-3", "status-4"];
const modelSelection = {
  instanceId: ProviderInstanceId.make("claudeAgent"),
  model: "claude-sonnet-4-6",
};

function frame(value: unknown): SDKMessage {
  return value as SDKMessage;
}

const batch = frame({
  type: "assistant",
  uuid: "batch",
  session_id: sessionId,
  parent_tool_use_id: null,
  message: {
    id: "tool-batch",
    role: "assistant",
    type: "message",
    model: modelSelection.model,
    content: [
      { type: "tool_use", id: "build", name: "Bash", input: { command: "make" } },
      ...statusIds.map((id) => ({
        type: "tool_use",
        id,
        name: "mcp__onicode__task_status",
        input: { taskId: id },
      })),
    ],
    stop_reason: "tool_use",
    stop_sequence: null,
    usage: { input_tokens: 1, output_tokens: 1 },
  },
});

// A CLI that echoes the prompt on its first frame is one that confirms each steer it reads.
const echoingBatch = (prompt: SDKUserMessage) =>
  frame({ ...(batch as object), user_message_uuids: [prompt.uuid] });

const toolResult = (id: string, cancelled: boolean) =>
  frame({
    type: "user",
    uuid: `result-${id}`,
    session_id: sessionId,
    parent_tool_use_id: null,
    message: {
      role: "user",
      content: [
        {
          type: "tool_result",
          tool_use_id: id,
          is_error: cancelled,
          content: cancelled ? refusal : id === "build" ? "build OK" : "Task completed normally",
        },
      ],
    },
    ...(cancelled ? { tool_result_meta: [{ id, non_execution_kind: "cancelled" }] } : {}),
  });

const result = (uuid: string, aborted: boolean) =>
  frame({
    type: "result",
    subtype: "success",
    uuid,
    session_id: sessionId,
    is_error: false,
    num_turns: 1,
    result: "",
    stop_reason: "end_turn",
    terminal_reason: aborted ? "aborted_tools" : "completed",
    permission_denials: [],
    duration_ms: 1,
    duration_api_ms: 1,
    total_cost_usd: 0,
    usage: { input_tokens: 1, output_tokens: 1 },
    modelUsage: {},
  });

// A finished child whose result its parent run has claimed for `messageId`.
const claimedChildResult = (input: {
  readonly parent: OrchestrationV2Run;
  readonly rootNodeId: NodeId;
  readonly messageId: MessageId;
  readonly taskId: NodeId;
  readonly now: DateTime.Utc;
}): ReadonlyArray<OrchestrationV2DomainEvent> => {
  const { parent, rootNodeId, messageId, taskId, now } = input;
  return [
    {
      id: EventId.make("cohort"),
      type: "run.updated",
      threadId: parent.threadId,
      runId: parent.id,
      occurredAt: now,
      payload: {
        ...parent,
        delegatedCompletion: {
          disposition: "open",
          nextGeneration: 2,
          delivery: { generation: 1, messageId, taskIds: [taskId] },
        },
      },
    },
    {
      id: EventId.make("child"),
      type: "subagent.updated",
      threadId: parent.threadId,
      runId: parent.id,
      nodeId: taskId,
      occurredAt: now,
      payload: {
        id: taskId,
        threadId: parent.threadId,
        runId: parent.id,
        parentNodeId: rootNodeId,
        origin: "app_owned",
        createdBy: "agent",
        driver: ClaudeAdapterV2.CLAUDE_PROVIDER,
        providerInstanceId: modelSelection.instanceId,
        providerThreadId: null,
        childThreadId: null,
        nativeTaskRef: null,
        prompt: "Background work",
        title: "Child",
        model: null,
        completionWake: "always",
        completionDelivery: { state: "claimed", observedByRunId: null },
        status: "completed",
        result: "done",
        startedAt: now,
        completedAt: now,
        updatedAt: now,
      },
    },
  ];
};

const deliveries = [
  "child completion",
  "scheduled message",
  "user steering",
  "promoted queued message",
] as const;

it.effect.each(deliveries)(
  "delivers %s without Claude cancelling the pending tool calls in its batch",
  (delivery) =>
    Effect.gen(function* () {
      const cwd = yield* checkpointWorkspace("claude-automatic-delivery");
      const sdkMessages = yield* Queue.unbounded<SDKMessage>();
      const offers: SDKUserMessage[] = [];
      const nativeQueue: SDKUserMessage[] = [];
      // @effect-diagnostics-next-line abortControllerInEffect:off - The fake SDK interrupts its batch independently of the test fiber.
      const batchAbort = new AbortController();
      const adapter = yield* ClaudeAdapterV2.makeClaudeAdapterV2({
        instanceId: modelSelection.instanceId,
        settings,
        environment: {},
        attachmentsDir: cwd,
        fileSystem: yield* FileSystem.FileSystem,
        path: yield* Path.Path,
        random: yield* SecureRandom.SecureRandom,
        idAllocator: yield* IdAllocator.IdAllocatorV2,
        queryRunner: {
          allocateSessionId: Effect.succeed(sessionId),
          open: () =>
            Effect.succeed({
              messages: Stream.fromQueue(sdkMessages),
              offer: (message) =>
                Effect.sync(() => {
                  offers.push(message);
                  nativeQueue.push(message);
                  // Claude Code 2.1.289 aborts on now-priority commands even while Bash blocks reads.
                  if (nativeQueue.some((command) => command.priority === "now")) {
                    batchAbort.abort({ kind: "interrupt" });
                  }
                }),
              setModel: () => Effect.void,
              applyFlagSettings: () => Effect.void,
              setPermissionMode: () => Effect.void,
              renameSession: () => Effect.void,
              backgroundTasks: () => Effect.succeed(false),
              mcpServerStatus: Effect.succeed([]),
              readMcpResource: () => Effect.die("unexpected MCP resource read"),
              callMcpTool: () => Effect.die("unexpected MCP tool call"),
              mcpAuthenticate: () => Effect.die("unexpected MCP sign-in"),
              mcpSubmitOAuthCallbackUrl: () => Effect.die("unexpected MCP sign-in"),
              reconnectMcpServer: () => Effect.die("unexpected MCP reconnect"),
              listPermissionRules: Effect.die("unexpected permission rules read"),
              getContextUsage: Effect.die("unexpected context usage read"),
              sessionDialog: (subtype) => Effect.die(`unexpected ${subtype}`),
              initializationResult: Effect.die("unexpected initialization result read"),
              reloadOutputStyles: Effect.die("unexpected output style reload"),
              toggleMcpServer: () => Effect.die("unexpected MCP toggle"),
              clearMcpServerAuth: () => Effect.die("unexpected MCP clear auth"),
              stopTask: () => Effect.void,
              interrupt: Effect.die("automatic delivery must never interrupt"),
              close: Effect.void,
            }),
          forkSession: () => Effect.die("unused"),
          subagentLaunchToolUseId: () => Effect.succeed(null),
          subagentModel: () => Effect.succeed(null),
          assertComplete: Effect.void,
        },
      });
      yield* Effect.gen(function* () {
        const orchestrator = yield* Orchestrator.OrchestratorV2;
        const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
        const threadId = ThreadId.make("thread:automatic-delivery");
        const projectId = ProjectId.make("project:automatic-delivery");
        const watch = (predicate: (event: OrchestrationV2DomainEvent) => boolean) =>
          orchestrator.streamDomainEvents.pipe(
            Stream.filter(predicate),
            Stream.take(1),
            Stream.runDrain,
            Effect.forkScoped,
          );
        yield* orchestrator.dispatch({
          type: "thread.create",
          commandId: CommandId.make("create"),
          threadId,
          projectId,
          title: "Automatic delivery",
          modelSelection,
          runtimeMode: "full-access",
          interactionMode: "default",
          branch: null,
          worktreePath: cwd,
          createdBy: "user",
          creationSource: "web",
        });
        const running = yield* watch(
          (event) => event.type === "provider-turn.updated" && event.payload.status === "running",
        );
        yield* orchestrator.dispatch({
          type: "message.dispatch",
          commandId: CommandId.make("first"),
          threadId,
          messageId: MessageId.make("first"),
          text: "Build and check all four tasks.",
          attachments: [],
          dispatchMode: { type: "start_immediately" },
          createdBy: "user",
          creationSource: "web",
        });
        yield* worker.drain();
        yield* Fiber.join(running);
        const pending = yield* watch(
          (event) =>
            event.type === "turn-item.updated" &&
            event.payload.nativeItemRef?.nativeId === "status-4",
        );
        yield* Queue.offer(
          sdkMessages,
          delivery === "child completion" ? echoingBatch(offers[0]!) : batch,
        );
        yield* Fiber.join(pending);
        const before = yield* orchestrator.getThreadProjection(threadId);
        assert.equal(before.turnItems.filter((item) => item.status === "running").length, 5);
        const parent = before.runs[0]!;
        if (parent.rootNodeId === null) return yield* Effect.die("parent has no root node");
        const messageId = MessageId.make("automatic-notice");
        if (delivery === "child completion") {
          const taskId = NodeId.make("completed-child");
          yield* (yield* EventSink.EventSinkV2).write({
            events: claimedChildResult({
              parent,
              rootNodeId: parent.rootNodeId,
              messageId,
              taskId,
              now: yield* DateTime.now,
            }),
          });
          const command = {
            type: "message.dispatch" as const,
            commandId: CommandId.make("completion"),
            threadId,
            messageId,
            text: "Child completed",
            attachments: [],
            dispatchMode: { type: "queue_after_active" as const },
            createdBy: "agent" as const,
            creationSource: "server" as const,
            delegatedCompletion: { parentRunId: parent.id, generation: 1, taskIds: [taskId] },
          };
          yield* orchestrator.dispatch(command);
          yield* orchestrator.dispatch(command);
          yield* orchestrator.recoverDelegatedTasks;
        } else if (delivery === "scheduled message") {
          yield* Effect.gen(function* () {
            const service = yield* ScheduledTaskService.ScheduledTaskService;
            const { task } = yield* service.upsert(
              yield* decodeScheduledTask({
                title: "Status check",
                prompt: "Check the four tasks.",
                enabled: true,
                schedule: { type: "interval", everyMs: 60_000 },
                projectId,
                threadId,
                workspaceStrategy: { type: "root" },
                modelSelection,
                runtimeMode: "full-access",
                interactionMode: "default",
              }),
            );
            const ran = yield* service.runNow({ id: task.id });
            assert.equal(ran.task.lastRunStatus, "succeeded");
          }).pipe(
            Effect.provide(
              ScheduledTaskService.layer.pipe(
                Layer.provide(ThreadManagementService.layer),
                Layer.provide(ServerSettings.layerTest()),
                Layer.provide(
                  Layer.mock(LegacyV1ThreadImporter.LegacyV1ThreadImporter)({
                    ensureTranscript: () =>
                      Effect.succeed({ importedThreadCount: 0, importedMessageCount: 0 }),
                  }),
                ),
                Layer.provide(Layer.mock(ThreadLaunchService.ThreadLaunchService)({})),
                Layer.provide(ManagedPolicy.layerTest()),
                Layer.provide(
                  Layer.mock(ManagedProjectFolders.ManagedProjectFolders)({
                    namedProjectsRoot: "/projects",
                    isScratchProject: () => Effect.succeed(false),
                  }),
                ),
                Layer.provide(Layer.mock(SecretRequests.SecretRequests)({})),
                Layer.provide(
                  CommandReceiptStore.layer.pipe(Layer.provide(SqlitePersistence.layerMemory)),
                ),
                Layer.provide(
                  Layer.mergeAll(NodeCrypto.layer, Scheduler.layer, SqlitePersistence.layerMemory),
                ),
              ),
            ),
          );
        } else if (delivery === "user steering") {
          yield* orchestrator.dispatch({
            type: "message.dispatch",
            commandId: CommandId.make("steer"),
            threadId,
            messageId,
            text: "Change direction now.",
            attachments: [],
            dispatchMode: { type: "steer_active", targetRunId: parent.id },
            createdBy: "user",
            creationSource: "web",
          });
        } else {
          yield* orchestrator.dispatch({
            type: "message.dispatch",
            commandId: CommandId.make("queue"),
            threadId,
            messageId,
            text: "Change direction now.",
            attachments: [],
            dispatchMode: { type: "queue_after_active" },
            createdBy: "user",
            creationSource: "web",
          });
          const queuedRun = (yield* orchestrator.getThreadProjection(threadId)).runs[1]!;
          yield* orchestrator.dispatch({
            type: "queued-message.promote-to-steer",
            commandId: CommandId.make("promote"),
            threadId,
            queuedRunId: queuedRun.id,
            targetRunId: parent.id,
          });
          const promoted = yield* orchestrator.getThreadProjection(threadId);
          assert.equal(promoted.runs.find((run) => run.id === queuedRun.id)?.status, "cancelled");
          assert.equal(
            promoted.messages.find((message) => message.id === messageId)?.runId,
            parent.id,
          );
          const steer = promoted.turnItems.find(
            (item) => item.type === "user_message" && item.messageId === messageId,
          );
          assert.ok(steer?.type === "user_message");
          assert.equal(steer.inputIntent, "promoted_queued_to_steer");
        }
        yield* worker.drain();
        const queuesBehindActiveTurn = delivery === "scheduled message";
        assert.equal(batchAbort.signal.aborted, false);
        assert.lengthOf(offers, queuesBehindActiveTurn ? 1 : 2);
        if (!queuesBehindActiveTurn) assert.equal(offers[1]?.priority, "next");
        // Claude has only queued the result; it is delivered once Claude says it read it.
        const delivered =
          delivery === "child completion"
            ? yield* watch(
                (event) =>
                  event.type === "subagent.updated" &&
                  event.payload.completionDelivery?.state === "delivered",
              )
            : null;
        if (delivered !== null) {
          const queuedInClaude = yield* orchestrator.getThreadProjection(threadId);
          assert.equal(queuedInClaude.subagents[0]?.completionDelivery?.state, "claimed");
        }
        const finished = yield* watch(
          (event) =>
            event.type === "run.updated" &&
            event.payload.id === parent.id &&
            event.payload.status === "waiting",
        );
        yield* Queue.offer(sdkMessages, toolResult("build", false));
        for (const id of statusIds)
          yield* Queue.offer(sdkMessages, toolResult(id, batchAbort.signal.aborted));
        yield* Queue.offer(
          sdkMessages,
          delivery === "child completion"
            ? frame({
                ...(result("completed", false) as object),
                user_message_uuid: offers[1]!.uuid,
              })
            : result("completed", false),
        );
        yield* Fiber.join(finished);
        if (delivered !== null) yield* Fiber.join(delivered);
        yield* worker.drain();
        yield* orchestrator.resumeQueuedRuns;
        yield* worker.drain();
        if (queuesBehindActiveTurn) {
          const queued = (yield* orchestrator.getThreadProjection(threadId)).runs[1]!;
          const noticeFinished = yield* watch(
            (event) =>
              event.type === "run.updated" &&
              event.payload.id === queued.id &&
              event.payload.status === "waiting",
          );
          yield* Queue.offer(sdkMessages, result("notice-completed", false));
          yield* Fiber.join(noticeFinished);
          yield* worker.drain();
        }
        const after = yield* orchestrator.getThreadProjection(threadId);
        const reads = after.turnItems.filter((item) =>
          statusIds.includes(item.nativeItemRef?.nativeId ?? ""),
        );
        assert.equal(reads.length, 4);
        for (const read of reads) {
          assert.equal(read.status, "completed");
          assert.equal(read.toolNonExecutionKind, undefined);
          assert.equal(read.type === "dynamic_tool" && read.output === refusal, false);
        }
        assert.lengthOf(offers, 2);
        assert.equal(offers.filter((offer) => offer.priority === "now").length, 0);
        if (delivery === "child completion") {
          assert.lengthOf(after.runs, 1);
          assert.deepEqual(
            after.messages
              .filter((message) => message.id === messageId)
              .map((message) => message.runId),
            [parent.id],
          );
          assert.equal(after.subagents[0]?.completionDelivery?.state, "delivered");
        } else if (queuesBehindActiveTurn) {
          assert.lengthOf(after.runs, 2);
          assert.equal(
            after.messages.filter((message) => message.scheduledTaskId !== undefined).length,
            1,
          );
        }
        yield* orchestrator.recoverDelegatedTasks;
        yield* orchestrator.resumeQueuedRuns;
        yield* worker.drain();
        assert.equal(offers.length, 2);
      }).pipe(
        Effect.provide(
          ProviderReplayHarness.layerWithRegistry(
            { name: "claude-automatic-delivery" },
            ProviderAdapterRegistry.layerSingle(adapter),
            { runEffectWorker: false },
          ),
        ),
      );
    }).pipe(
      Effect.provide(
        Layer.mergeAll(
          IdAllocator.layer,
          McpProviderSessions.layer,
          NodeServices.layer,
          NodeCrypto.layer,
        ),
      ),
    ),
);

it.effect("applies an effort change to the running Claude query without waiting for a turn", () =>
  Effect.gen(function* () {
    const cwd = yield* checkpointWorkspace("claude-live-effort");
    const sdkMessages = yield* Queue.unbounded<SDKMessage>();
    const flagSettings: unknown[] = [];
    const adapter = yield* ClaudeAdapterV2.makeClaudeAdapterV2({
      instanceId: modelSelection.instanceId,
      settings,
      environment: {},
      attachmentsDir: cwd,
      fileSystem: yield* FileSystem.FileSystem,
      path: yield* Path.Path,
      random: yield* SecureRandom.SecureRandom,
      idAllocator: yield* IdAllocator.IdAllocatorV2,
      queryRunner: {
        allocateSessionId: Effect.succeed(sessionId),
        open: () =>
          Effect.succeed({
            messages: Stream.fromQueue(sdkMessages),
            offer: () => Effect.void,
            setModel: () => Effect.void,
            applyFlagSettings: (flags) =>
              Effect.sync(() => {
                flagSettings.push(flags);
              }),
            setPermissionMode: () => Effect.void,
            renameSession: () => Effect.void,
            backgroundTasks: () => Effect.succeed(false),
            mcpServerStatus: Effect.succeed([]),
            readMcpResource: () => Effect.die("unexpected MCP resource read"),
            callMcpTool: () => Effect.die("unexpected MCP tool call"),
            mcpAuthenticate: () => Effect.die("unexpected MCP sign-in"),
            mcpSubmitOAuthCallbackUrl: () => Effect.die("unexpected MCP sign-in"),
            reconnectMcpServer: () => Effect.die("unexpected MCP reconnect"),
            listPermissionRules: Effect.die("unexpected permission rules read"),
            getContextUsage: Effect.die("unexpected context usage read"),
            sessionDialog: (subtype) => Effect.die(`unexpected ${subtype}`),
            initializationResult: Effect.die("unexpected initialization result read"),
            reloadOutputStyles: Effect.die("unexpected output style reload"),
            toggleMcpServer: () => Effect.die("unexpected MCP toggle"),
            clearMcpServerAuth: () => Effect.die("unexpected MCP clear auth"),
            stopTask: () => Effect.void,
            interrupt: Effect.die("an effort change must not interrupt the turn"),
            close: Effect.void,
          }),
        forkSession: () => Effect.die("unused"),
        subagentLaunchToolUseId: () => Effect.succeed(null),
        subagentModel: () => Effect.succeed(null),
        assertComplete: Effect.void,
      },
    });
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const threadId = ThreadId.make("thread:live-effort");
      const high = { ...modelSelection, options: [{ id: "effort", value: "high" }] };
      const running = yield* orchestrator.streamDomainEvents.pipe(
        Stream.filter(
          (event) => event.type === "provider-turn.updated" && event.payload.status === "running",
        ),
        Stream.take(1),
        Stream.runDrain,
        Effect.forkScoped,
      );
      yield* orchestrator.dispatch({
        type: "thread.create",
        commandId: CommandId.make("create"),
        threadId,
        projectId: ProjectId.make("project:live-effort"),
        title: "Live effort",
        modelSelection: high,
        runtimeMode: "full-access",
        interactionMode: "default",
        branch: null,
        worktreePath: cwd,
        createdBy: "user",
        creationSource: "web",
      });
      yield* orchestrator.dispatch({
        type: "message.dispatch",
        commandId: CommandId.make("first"),
        threadId,
        messageId: MessageId.make("first"),
        text: "Keep working.",
        attachments: [],
        dispatchMode: { type: "start_immediately" },
        createdBy: "user",
        creationSource: "web",
      });
      yield* worker.drain();
      yield* Fiber.join(running);
      assert.deepEqual(flagSettings, []);

      yield* orchestrator.dispatch({
        type: "thread.model-selection.set",
        commandId: CommandId.make("medium"),
        threadId,
        modelSelection: { ...modelSelection, options: [{ id: "effort", value: "medium" }] },
      });
      yield* worker.drain();

      assert.deepEqual(flagSettings, [{ effortLevel: "medium", fastMode: null, ultracode: null }]);
    }).pipe(
      Effect.provide(
        ProviderReplayHarness.layerWithRegistry(
          { name: "claude-live-effort" },
          ProviderAdapterRegistry.layerSingle(adapter),
          { runEffectWorker: false },
        ),
      ),
    );
  }).pipe(
    Effect.provide(
      Layer.mergeAll(
        IdAllocator.layer,
        McpProviderSessions.layer,
        NodeServices.layer,
        NodeCrypto.layer,
      ),
    ),
  ),
);

const renamedClaudeSessionTitles = (options: { readonly renameDuringStartup: boolean }) =>
  Effect.gen(function* () {
    const cwd = yield* checkpointWorkspace("claude-live-rename");
    const sdkMessages = yield* Queue.unbounded<SDKMessage>();
    const renames: string[] = [];
    const startupEntered = yield* Deferred.make<void>();
    const startupReleased = yield* Deferred.make<void>();
    const adapter = yield* ClaudeAdapterV2.makeClaudeAdapterV2({
      instanceId: modelSelection.instanceId,
      settings,
      environment: {},
      attachmentsDir: cwd,
      fileSystem: yield* FileSystem.FileSystem,
      path: yield* Path.Path,
      random: yield* SecureRandom.SecureRandom,
      idAllocator: yield* IdAllocator.IdAllocatorV2,
      queryRunner: {
        allocateSessionId: Effect.succeed(sessionId),
        open: () =>
          Effect.succeed({
            messages: Stream.fromQueue(sdkMessages),
            offer: () => Effect.void,
            setModel: () => Effect.void,
            applyFlagSettings: () => Effect.void,
            setPermissionMode: () => Effect.void,
            renameSession: (title) =>
              Effect.sync(() => {
                renames.push(title);
              }),
            backgroundTasks: () => Effect.succeed(false),
            mcpServerStatus: Effect.succeed([]),
            readMcpResource: () => Effect.die("unexpected MCP resource read"),
            callMcpTool: () => Effect.die("unexpected MCP tool call"),
            mcpAuthenticate: () => Effect.die("unexpected MCP sign-in"),
            mcpSubmitOAuthCallbackUrl: () => Effect.die("unexpected MCP sign-in"),
            reconnectMcpServer: () => Effect.die("unexpected MCP reconnect"),
            listPermissionRules: Effect.die("unexpected permission rules read"),
            getContextUsage: Effect.die("unexpected context usage read"),
            sessionDialog: (subtype) => Effect.die(`unexpected ${subtype}`),
            initializationResult: Effect.die("unexpected initialization result read"),
            reloadOutputStyles: Effect.die("unexpected output style reload"),
            toggleMcpServer: () => Effect.die("unexpected MCP toggle"),
            clearMcpServerAuth: () => Effect.die("unexpected MCP clear auth"),
            stopTask: () => Effect.void,
            interrupt: Effect.die("a rename must not interrupt the turn"),
            close: Effect.void,
          }),
        forkSession: () => Effect.die("unused"),
        subagentLaunchToolUseId: () => Effect.succeed(null),
        subagentModel: () => Effect.succeed(null),
        assertComplete: Effect.void,
      },
    });
    return yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const threadId = ThreadId.make("thread:live-rename");
      const running = yield* orchestrator.streamDomainEvents.pipe(
        Stream.filter(
          (event) => event.type === "provider-turn.updated" && event.payload.status === "running",
        ),
        Stream.take(1),
        Stream.runDrain,
        Effect.forkScoped,
      );
      yield* orchestrator.dispatch({
        type: "thread.create",
        commandId: CommandId.make("create"),
        threadId,
        projectId: ProjectId.make("project:live-rename"),
        title: "Seed title",
        modelSelection,
        runtimeMode: "full-access",
        interactionMode: "default",
        branch: null,
        worktreePath: cwd,
        createdBy: "user",
        creationSource: "web",
      });
      yield* orchestrator.dispatch({
        type: "message.dispatch",
        commandId: CommandId.make("first"),
        threadId,
        messageId: MessageId.make("first"),
        text: "Keep working.",
        attachments: [],
        dispatchMode: { type: "start_immediately" },
        createdBy: "user",
        creationSource: "web",
      });
      const rename = orchestrator.dispatch({
        type: "thread.metadata.update",
        commandId: CommandId.make("rename"),
        threadId,
        title: "Renamed title",
      });
      if (options.renameDuringStartup) {
        const starting = yield* worker.drain().pipe(Effect.forkScoped);
        yield* Deferred.await(startupEntered);
        yield* rename;
        yield* Deferred.succeed(startupReleased, undefined);
        yield* Fiber.join(starting);
        yield* Fiber.join(running);
      } else {
        yield* worker.drain();
        yield* Fiber.join(running);
        assert.deepEqual(renames, []);
        yield* rename;
      }
      yield* worker.drain();
      return renames;
    }).pipe(
      Effect.provide(
        ProviderReplayHarness.layerWithRegistry(
          { name: "claude-live-rename" },
          ProviderAdapterRegistry.layerSingle({
            ...adapter,
            openSession: (input) =>
              options.renameDuringStartup
                ? Deferred.succeed(startupEntered, undefined).pipe(
                    Effect.andThen(Deferred.await(startupReleased)),
                    Effect.andThen(adapter.openSession(input)),
                  )
                : adapter.openSession(input),
          }),
          { runEffectWorker: false },
        ),
      ),
    );
  }).pipe(
    Effect.provide(
      Layer.mergeAll(
        IdAllocator.layer,
        McpProviderSessions.layer,
        NodeServices.layer,
        NodeCrypto.layer,
      ),
    ),
  );

it.effect("renames the running Claude session when the thread is renamed mid-turn", () =>
  Effect.gen(function* () {
    assert.deepEqual(yield* renamedClaudeSessionTitles({ renameDuringStartup: false }), [
      "Renamed title",
    ]);
  }),
);

it.effect("renames the Claude session when the thread is renamed while it starts", () =>
  Effect.gen(function* () {
    assert.deepEqual(yield* renamedClaudeSessionTitles({ renameDuringStartup: true }), [
      "Renamed title",
    ]);
  }),
);

const backgroundTaskStarted = (taskId: string) =>
  frame({
    type: "system",
    subtype: "task_started",
    task_id: taskId,
    tool_use_id: `tool-${taskId}`,
    description: "Background build",
    is_backgrounded: true,
    task_type: "local_bash",
    uuid: `started-${taskId}`,
    session_id: sessionId,
  });

const backgroundTaskNotification = (taskId: string) =>
  frame({
    type: "system",
    subtype: "task_notification",
    task_id: taskId,
    tool_use_id: `tool-${taskId}`,
    status: "completed",
    output_file: `/tmp/claude-replay/tasks/${taskId}.output`,
    summary: `Background build ${taskId} completed`,
    uuid: `notification-${taskId}`,
    session_id: sessionId,
  });

const wakeAssistant = (uuid: string) =>
  frame({
    type: "assistant",
    uuid,
    session_id: sessionId,
    parent_tool_use_id: null,
    message: {
      id: `msg-${uuid}`,
      role: "assistant",
      type: "message",
      model: modelSelection.model,
      content: [{ type: "text", text: `Background build finished (${uuid}).` }],
      stop_reason: null,
      stop_sequence: null,
      usage: { input_tokens: 1, output_tokens: 1 },
    },
  });

it.effect("offers a continuation for a later wake after a queued continuation is cancelled", () =>
  Effect.gen(function* () {
    const cwd = yield* checkpointWorkspace("claude-cancelled-continuation");
    const sdkMessages = yield* Queue.unbounded<SDKMessage>();
    const processed = new WeakMap<SDKMessage, Deferred.Deferred<void>>();
    const offerAndWait = Effect.fnUntraced(function* (message: SDKMessage) {
      const done = yield* Deferred.make<void>();
      processed.set(message, done);
      yield* Queue.offer(sdkMessages, message);
      yield* Deferred.await(done);
    });
    const fileSystem = yield* FileSystem.FileSystem;
    const path = yield* Path.Path;
    const random = yield* SecureRandom.SecureRandom;
    const idAllocator = yield* IdAllocator.IdAllocatorV2;
    const continuations: ProviderContinuationRequests.ProviderContinuationRequest[] = [];
    const services =
      yield* Effect.context<
        Effect.Services<ReturnType<typeof ClaudeAdapterV2.makeClaudeAdapterV2>>
      >();
    const registry = ProviderAdapterRegistry.layerFromAdaptersEffect(
      Effect.gen(function* () {
        const requests = yield* ProviderContinuationRequests.ProviderContinuationRequests;
        const adapter = yield* ClaudeAdapterV2.makeClaudeAdapterV2({
          instanceId: modelSelection.instanceId,
          settings,
          environment: {},
          attachmentsDir: cwd,
          fileSystem,
          path,
          random,
          idAllocator,
          continuationRequests: {
            offer: (request) =>
              Effect.sync(() => continuations.push(request)).pipe(
                Effect.andThen(requests.offer(request)),
              ),
          },
          queryRunner: {
            allocateSessionId: Effect.succeed(sessionId),
            open: () =>
              Effect.succeed({
                // Resolve each frame's Deferred once the adapter pulls the next one.
                messages: Stream.fromQueue(sdkMessages).pipe(
                  Stream.flatMap((message) =>
                    Stream.make(message).pipe(
                      Stream.concat(
                        Stream.fromEffect(
                          Effect.suspend(() => {
                            const done = processed.get(message);
                            return done === undefined
                              ? Effect.void
                              : Deferred.succeed(done, undefined);
                          }),
                        ).pipe(Stream.drain),
                      ),
                    ),
                  ),
                ),
                offer: () => Effect.void,
                setModel: () => Effect.void,
                applyFlagSettings: () => Effect.void,
                setPermissionMode: () => Effect.void,
                renameSession: () => Effect.void,
                backgroundTasks: () => Effect.succeed(false),
                mcpServerStatus: Effect.succeed([]),
                readMcpResource: () => Effect.die("unexpected MCP resource read"),
                callMcpTool: () => Effect.die("unexpected MCP tool call"),
                mcpAuthenticate: () => Effect.die("unexpected MCP sign-in"),
                mcpSubmitOAuthCallbackUrl: () => Effect.die("unexpected MCP sign-in"),
                reconnectMcpServer: () => Effect.die("unexpected MCP reconnect"),
                listPermissionRules: Effect.die("unexpected permission rules read"),
                getContextUsage: Effect.die("unexpected context usage read"),
                sessionDialog: (subtype) => Effect.die(`unexpected ${subtype}`),
                initializationResult: Effect.die("unexpected initialization result read"),
                reloadOutputStyles: Effect.die("unexpected output style reload"),
                toggleMcpServer: () => Effect.die("unexpected MCP toggle"),
                clearMcpServerAuth: () => Effect.die("unexpected MCP clear auth"),
                stopTask: () => Effect.void,
                interrupt: Effect.void,
                close: Effect.void,
              }),
            forkSession: () => Effect.die("unused"),
            subagentLaunchToolUseId: () => Effect.succeed(null),
            subagentModel: () => Effect.succeed(null),
            assertComplete: Effect.void,
          },
        }).pipe(Effect.provideContext(services));
        return [adapter];
      }),
    );
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const threadId = ThreadId.make("thread:cancelled-continuation");
      const watch = (predicate: (event: OrchestrationV2DomainEvent) => boolean) =>
        orchestrator.streamDomainEvents.pipe(
          Stream.filter(predicate),
          Stream.take(1),
          Stream.runDrain,
          Effect.forkScoped,
        );
      const runUpdated = (messageId: string, status: string) =>
        watch(
          (event) =>
            event.type === "run.updated" &&
            event.payload.userMessageId === messageId &&
            event.payload.status === status,
        );
      // A user turn that leaves a background task running when it ends.
      const userTurnWithBackgroundTask = Effect.fnUntraced(function* (id: string, taskId: string) {
        const running = yield* runUpdated(id, "running");
        yield* orchestrator.dispatch({
          type: "message.dispatch",
          commandId: CommandId.make(id),
          threadId,
          messageId: MessageId.make(id),
          text: "Run the build in the background.",
          attachments: [],
          dispatchMode: { type: "start_immediately" },
          createdBy: "user",
          creationSource: "web",
        });
        yield* worker.drain();
        yield* Fiber.join(running);
        const waiting = yield* runUpdated(id, "waiting");
        yield* Queue.offer(sdkMessages, backgroundTaskStarted(taskId));
        yield* Queue.offer(sdkMessages, result(`result-${id}`, false));
        yield* Fiber.join(waiting);
      });
      yield* orchestrator.dispatch({
        type: "thread.create",
        commandId: CommandId.make("create"),
        threadId,
        projectId: ProjectId.make("project:cancelled-continuation"),
        title: "Cancelled continuation",
        modelSelection,
        runtimeMode: "full-access",
        interactionMode: "default",
        branch: null,
        worktreePath: cwd,
        createdBy: "user",
        creationSource: "web",
      });

      // The first run is still finalizing when its background task wakes Claude.
      yield* userTurnWithBackgroundTask("first", "build-1");
      const queued = yield* watch(
        (event) => event.type === "run.created" && event.payload.status === "queued",
      );
      yield* offerAndWait(backgroundTaskNotification("build-1"));
      yield* offerAndWait(wakeAssistant("wake-1"));
      yield* Fiber.join(queued);
      assert.lengthOf(continuations, 1);
      const continuationRun = (yield* orchestrator.getThreadProjection(threadId)).runs.find(
        (run) => run.status === "queued",
      );
      assert.ok(continuationRun !== undefined);
      yield* orchestrator.dispatch({
        type: "queued-run.cancel",
        commandId: CommandId.make("cancel"),
        threadId,
        runId: continuationRun.id,
      });
      yield* offerAndWait(result("wake-1-result", false));
      yield* worker.drain();
      yield* orchestrator.resumeQueuedRuns;
      yield* worker.drain();
      const idle = yield* orchestrator.getThreadProjection(threadId);
      assert.equal(idle.runs.find((run) => run.id === continuationRun.id)?.status, "cancelled");
      assert.isFalse(idle.runs.some((run) => run.status === "queued"));

      yield* userTurnWithBackgroundTask("second", "build-2");
      yield* worker.drain();
      // The second user run has completed, so this continuation starts without queueing.
      const isContinuationRun = (event: OrchestrationV2DomainEvent, type: string, status: string) =>
        (event.type === "run.created" || event.type === "run.updated") &&
        event.type === type &&
        event.payload.status === status &&
        event.payload.userMessageId !== MessageId.make("first") &&
        event.payload.userMessageId !== MessageId.make("second");
      const secondCreated = yield* watch((event) =>
        isContinuationRun(event, "run.created", "starting"),
      );
      const secondRunning = yield* watch((event) =>
        isContinuationRun(event, "run.updated", "running"),
      );
      yield* offerAndWait(backgroundTaskNotification("build-2"));
      yield* offerAndWait(wakeAssistant("wake-2"));
      assert.lengthOf(
        continuations,
        2,
        "the wake after a cancelled queued continuation must offer a new continuation",
      );
      yield* Fiber.join(secondCreated);
      yield* worker.drain();
      yield* Fiber.join(secondRunning);
      const secondContinuation = (yield* orchestrator.getThreadProjection(threadId)).runs.find(
        (run) => run.status === "running",
      );
      assert.ok(secondContinuation !== undefined);
      const secondWaiting = yield* watch(
        (event) =>
          event.type === "run.updated" &&
          event.payload.id === secondContinuation.id &&
          event.payload.status === "waiting",
      );
      yield* offerAndWait(result("wake-2-result", false));
      yield* Fiber.join(secondWaiting);
      yield* worker.drain();
      const settled = yield* orchestrator.getThreadProjection(threadId);
      assert.equal(
        settled.runs.find((run) => run.id === secondContinuation.id)?.status,
        "completed",
      );
      assert.deepEqual(
        settled.turnItems.flatMap((item) =>
          item.runId === secondContinuation.id && item.type === "assistant_message"
            ? [item.text]
            : [],
        ),
        ["Background build finished (wake-2)."],
        "the continuation must carry only its own wake's output",
      );
      assert.lengthOf(continuations, 2, "two wakes must offer exactly two continuations");
    }).pipe(
      Effect.provide(
        ProviderReplayHarness.layerWithRegistry(
          { name: "claude-cancelled-continuation" },
          registry,
          { runEffectWorker: false, runContinuationWorker: true },
        ),
      ),
    );
  }).pipe(
    Effect.provide(
      Layer.mergeAll(
        IdAllocator.layer,
        McpProviderSessions.layer,
        NodeServices.layer,
        NodeCrypto.layer,
      ),
    ),
  ),
);

// A steer Claude never read runs as the next turn, between the messages sent before and after
// it, even when they share a millisecond and the later one picks another model.
it.effect.each([
  "Claude cancels it twice",
  "the Claude process exits",
  "the server restarts",
  "the user sends it now",
  // Claude's read commits outside the thread lock, between Send now's plan and its commit.
  "Claude reads it while the user sends it now",
  "Claude cancels it and the user stops",
  "a later steer lands before the run-end reaction",
  "a later client steer lands before the run-end reaction",
  // Checkpoint capture ends the run between the steer's two reads of the active run.
  "a later client steer lands as its run ends",
  // A wire-valid steer that names its run as well as the intent.
  "a later client steer names its stopped run",
  "a later restart lands before the run-end reaction",
  "a later client restart lands before the run-end reaction",
  // Notices the app steers in itself. Claude queues them like any steer, so one it never read
  // is queued again, and nobody is told to resend text they did not write.
  "Claude cancels a child result",
  "the Claude process exits with a child result unread",
  "the user stops with a child result unread",
  "Claude cancels a task question",
  "the user stops with a task question unread",
  // A result that echoes no prompt cannot prove Claude will start the notice, so Claude is
  // asked to give it back. One it took runs as its next turn; only one it gives back, or one
  // it never answers for, is queued again.
  "the turn ends with no echo and Claude gives a child result back",
  "the turn ends with no echo after Claude took a child result",
  "the turn ends with no echo and Claude refuses the child result it took",
  "the turn ends with no echo and Claude completes the child result it took unstarted",
  "the turn ends with no echo and Claude never answers for a child result",
  "the turn ends with no echo and Claude never starts the child result it kept",
  // Until Claude echoes a prompt uuid it could not say it read a notice, so it is handed none.
  "a child result arrives before Claude echoes a prompt",
  // Stop lands first, so the question Claude then hands back must not start a turn.
  "the user stops before Claude echoes, with a task question planned",
  // Claude's read and the accept that read starts commit apart, so the accept can be lost.
  "Claude read a child result whose accept was lost before the run ended",
  "Claude read a child result whose accept was lost before a restart",
  // Anything that offers results finds the read one first, as startup's task pass does.
  "Claude read a child result whose accept was lost before a wake policy change",
  // Planned for Claude and not yet handed over when the server stops.
  "the server restarts before a child result is offered",
  "the server restarts before a task question is offered",
  "the server restarts before a task question is offered, and the first requeue fails",
  // A notice steered in after the user's steer is queued again behind it.
  "the server restarts with a steer and a later task question unread",
  "the server restarts with two task questions unread",
] as const)("handles an unread steer when %s", (outcome) =>
  Effect.gen(function* () {
    const cwd = yield* checkpointWorkspace("claude-unread-steer");
    const sdkMessages = yield* Queue.unbounded<SDKMessage>();
    const activeRunReadEntered = yield* Deferred.make<void>();
    const activeRunReadRelease = yield* Deferred.make<void>();
    let activeRunReads: number | undefined;
    let failNoticeRead = false;
    const offers: SDKUserMessage[] = [];
    const takenBack: Array<string> = [];
    const cancelAsked = yield* Deferred.make<void>();
    const adapter = yield* ClaudeAdapterV2.makeClaudeAdapterV2({
      instanceId: modelSelection.instanceId,
      settings,
      environment: {},
      attachmentsDir: cwd,
      fileSystem: yield* FileSystem.FileSystem,
      path: yield* Path.Path,
      random: yield* SecureRandom.SecureRandom,
      idAllocator: yield* IdAllocator.IdAllocatorV2,
      queryRunner: {
        allocateSessionId: Effect.succeed(sessionId),
        open: () =>
          Effect.succeed({
            messages: Stream.fromQueue(sdkMessages),
            offer: (message) => Effect.sync(() => void offers.push(message)),
            setModel: () => Effect.void,
            applyFlagSettings: () => Effect.void,
            setPermissionMode: () => Effect.void,
            renameSession: () => Effect.void,
            backgroundTasks: () => Effect.succeed(false),
            cancelQueuedMessage: (uuid) =>
              Effect.suspend(() => {
                takenBack.push(uuid);
                // Claude never answers: the cancel's deadline settles it.
                return outcome.endsWith("never answers for a child result")
                  ? Deferred.succeed(cancelAsked, undefined).pipe(Effect.andThen(Effect.never))
                  : outcome.endsWith("never starts the child result it kept")
                    ? Deferred.succeed(cancelAsked, undefined).pipe(Effect.as(false))
                    : Effect.succeed(
                        !outcome.endsWith("after Claude took a child result") &&
                          !outcome.endsWith("refuses the child result it took") &&
                          !outcome.endsWith("child result it took unstarted"),
                      );
              }),
            mcpServerStatus: Effect.succeed([]),
            readMcpResource: () => Effect.die("unexpected MCP resource read"),
            callMcpTool: () => Effect.die("unexpected MCP tool call"),
            mcpAuthenticate: () => Effect.die("unexpected MCP sign-in"),
            mcpSubmitOAuthCallbackUrl: () => Effect.die("unexpected MCP sign-in"),
            reconnectMcpServer: () => Effect.die("unexpected MCP reconnect"),
            listPermissionRules: Effect.die("unexpected permission rules read"),
            getContextUsage: Effect.die("unexpected context usage read"),
            sessionDialog: (subtype) => Effect.die(`unexpected ${subtype}`),
            initializationResult: Effect.die("unexpected initialization result read"),
            reloadOutputStyles: Effect.die("unexpected output style reload"),
            toggleMcpServer: () => Effect.die("unexpected MCP toggle"),
            clearMcpServerAuth: () => Effect.die("unexpected MCP clear auth"),
            stopTask: () => Effect.void,
            interrupt: Queue.shutdown(sdkMessages),
            close: Effect.void,
          }),
        forkSession: () => Effect.die("unused"),
        subagentLaunchToolUseId: () => Effect.succeed(null),
        subagentModel: () => Effect.succeed(null),
        assertComplete: Effect.void,
      },
    });
    yield* Effect.gen(function* () {
      const orchestrator = yield* Orchestrator.OrchestratorV2;
      const worker = yield* EffectWorker.OrchestrationEffectWorkerV2;
      const threadId = ThreadId.make("thread:unread-steer");
      const steerId = MessageId.make("steer");
      const watch = (predicate: (event: OrchestrationV2DomainEvent) => boolean) =>
        orchestrator.streamDomainEvents.pipe(
          Stream.filter(predicate),
          Stream.take(1),
          Stream.runDrain,
          Effect.forkScoped,
        );
      const settled = (runId: RunId, status: "waiting") =>
        watch(
          (event) =>
            event.type === "run.updated" &&
            event.payload.id === runId &&
            event.payload.status === status,
        );
      const pending = () =>
        watch(
          (event) =>
            event.type === "turn-item.updated" &&
            event.payload.type === "user_message" &&
            event.payload.messageId === steerId &&
            event.payload.status === "pending",
        );
      const answer = (offer: SDKUserMessage) =>
        Queue.offer(
          sdkMessages,
          frame({
            ...(result(`result-${offer.uuid}`, false) as object),
            user_message_uuid: offer.uuid,
          }),
        );
      const cancel = (offer: SDKUserMessage) =>
        Queue.offer(
          sdkMessages,
          frame({
            type: "command_lifecycle",
            command_uuid: offer.uuid,
            state: "cancelled",
            uuid: `cancelled-${offer.uuid}`,
            session_id: sessionId,
          }),
        );
      const offeredText = (offer: SDKUserMessage | undefined) =>
        JSON.stringify(offer?.message.content ?? null);
      const queued = Effect.map(orchestrator.getThreadProjection(threadId), (projection) =>
        projection.runs
          .filter((run) => run.status === "queued")
          .toSorted(
            (left, right) =>
              (left.queuePosition ?? left.ordinal) - (right.queuePosition ?? right.ordinal),
          ),
      );
      const queuedOrder = Effect.map(queued, (runs) =>
        runs.map((run) => [run.userMessageId, run.queueHeld === true]),
      );
      yield* orchestrator.dispatch({
        type: "thread.create",
        commandId: CommandId.make("create"),
        threadId,
        projectId: ProjectId.make("project:unread-steer"),
        title: "Unread steer",
        modelSelection,
        runtimeMode: "full-access",
        interactionMode: "default",
        branch: null,
        worktreePath: cwd,
        createdBy: "user",
        creationSource: "web",
      });
      const running = yield* watch(
        (event) => event.type === "provider-turn.updated" && event.payload.status === "running",
      );
      const send = (
        id: string,
        dispatchMode:
          | { readonly type: "start_immediately" | "queue_after_active" }
          | { readonly type: "steer_active" | "restart_active"; readonly targetRunId: RunId },
        model?: string,
        deliveryIntent?: "steer" | "restart",
      ) =>
        orchestrator.dispatch({
          type: "message.dispatch",
          commandId: CommandId.make(id),
          threadId,
          messageId: MessageId.make(id),
          text: `Message ${id}`,
          attachments: [],
          ...(model === undefined ? {} : { modelSelection: { ...modelSelection, model } }),
          ...(deliveryIntent === undefined ? {} : { deliveryIntent }),
          dispatchMode,
          createdBy: "user",
          creationSource: "web",
        });
      yield* send("first", { type: "start_immediately" });
      yield* worker.drain();
      yield* Fiber.join(running);
      const parent = (yield* orchestrator.getThreadProjection(threadId)).runs[0]!;
      if (outcome.includes("child result") || outcome.includes("task question")) {
        const question = outcome.includes("task question");
        const noticeId = MessageId.make("notice");
        const sink = yield* EventSink.EventSinkV2;
        const echoes = !outcome.includes("before Claude echoes");
        if (echoes) {
          const echoed = yield* watch(
            (event) =>
              event.type === "turn-item.updated" &&
              event.payload.nativeItemRef?.nativeId === "status-4",
          );
          yield* Queue.offer(sdkMessages, echoingBatch(offers[0]!));
          yield* Fiber.join(echoed);
        }
        if (!question) {
          if (parent.rootNodeId === null) return yield* Effect.die("parent has no root node");
          yield* sink.write({
            events: claimedChildResult({
              parent,
              rootNodeId: parent.rootNodeId,
              messageId: noticeId,
              taskId: NodeId.make("completed-child"),
              now: yield* DateTime.now,
            }),
          });
        }
        const noticeItem = (status: "pending" | "cancelled") =>
          watch(
            (event) =>
              event.type === "turn-item.updated" &&
              event.payload.type === "notification" &&
              event.payload.status === status,
          );
        const noticeRun = (
          type: "run.created" | "run.updated",
          matches: (run: OrchestrationV2Run) => boolean,
        ) =>
          watch(
            (event) =>
              (event.type === "run.created" || event.type === "run.updated") &&
              event.type === type &&
              event.payload.userMessageId === noticeId &&
              matches(event.payload),
          );
        const task = Effect.map(
          orchestrator.getThreadProjection(threadId),
          (projection) => projection.subagents[0]?.completionDelivery?.state,
        );
        const notice = Effect.map(orchestrator.getThreadProjection(threadId), (projection) =>
          projection.turnItems.find((item) => item.type === "notification"),
        );
        const noticeRuns = Effect.map(orchestrator.getThreadProjection(threadId), (projection) =>
          projection.runs.filter((run) => run.userMessageId === noticeId),
        );
        // A restart that finds the turn over: recovery cancels the run it cut short, then
        // looks for what that run left unread or unaccepted.
        const cut = Effect.gen(function* () {
          const live = yield* orchestrator.getThreadProjection(threadId);
          const now = yield* DateTime.now;
          yield* sink.write({
            commandId: CommandId.make("command:runtime-reconcile:startup:unread-notice"),
            events: [
              ...live.providerTurns.map((turn, index) => ({
                id: EventId.make(`recovered-turn-${index}`),
                type: "provider-turn.updated" as const,
                threadId,
                occurredAt: now,
                payload: { ...turn, status: "completed" as const, completedAt: now },
              })),
              ...live.runs.map((run, index) => ({
                id: EventId.make(`recovered-run-${index}`),
                type: "run.updated" as const,
                threadId,
                runId: run.id,
                occurredAt: now,
                payload: { ...run, status: "cancelled" as const, completedAt: now },
              })),
            ],
          });
        });
        const restart = Effect.andThen(cut, orchestrator.recoverDelegatedTasks);
        const afterSteer = outcome.includes("a steer and a later");
        if (afterSteer) {
          const unreadSteer = yield* pending();
          yield* send("steer", { type: "steer_active", targetRunId: parent.id });
          yield* worker.drain();
          yield* Fiber.join(unreadSteer);
        }
        const unread = yield* noticeItem("pending");
        const sendNotice = (messageId: MessageId, askingTask: string) =>
          orchestrator.dispatch({
            type: "message.dispatch",
            commandId: CommandId.make(messageId),
            threadId,
            messageId,
            text: "Child notice.",
            attachments: [],
            dispatchMode: { type: "queue_after_active" },
            createdBy: "agent",
            creationSource: "server",
            ...(question
              ? {
                  notification: {
                    source: {
                      kind: "delegated_task" as const,
                      taskIds: [NodeId.make(askingTask)],
                    },
                    outcome: "updated" as const,
                    summary: "Delegated task has a question",
                  },
                }
              : {
                  delegatedCompletion: {
                    parentRunId: parent.id,
                    generation: 1,
                    taskIds: [NodeId.make("completed-child")],
                  },
                }),
          });
        yield* sendNotice(noticeId, "asking-child");
        if (outcome.includes("two task questions")) {
          const laterId = MessageId.make("later-notice");
          yield* sendNotice(laterId, "later-asking-child");
          // Neither was handed over. Queued again, they keep the order they were steered in.
          yield* restart;
          assert.deepEqual(yield* queuedOrder, [
            [noticeId, true],
            [laterId, true],
          ]);
          return;
        }
        if (outcome.startsWith("the server restarts before")) {
          // Nobody has read a notice the worker has not handed over yet.
          assert.equal((yield* notice)?.status, "pending");
          failNoticeRead = outcome.endsWith("the first requeue fails");
          yield* restart;
          if (outcome.endsWith("the first requeue fails")) {
            // A requeue that fails is not remembered as refused: the next pass sends it again.
            assert.isFalse(failNoticeRead);
            assert.deepEqual(yield* queuedOrder, []);
            yield* orchestrator.recoverDelegatedTasks;
          }
          assert.deepEqual(yield* queuedOrder, [[noticeId, true]]);
          // The planned steer can still run, late, and finds its turn over. The notice
          // already has its run, so it is not queued a second time.
          yield* worker.drain();
          assert.lengthOf(yield* noticeRuns, 1);
          if (!question) assert.equal(yield* task, "claimed");
          assert.lengthOf(offers, 1);
          return;
        }
        const stopsFirst = outcome.startsWith("the user stops before");
        if (stopsFirst) {
          yield* orchestrator.dispatch({
            type: "run.interrupt",
            commandId: CommandId.make("stop"),
            threadId,
            runId: parent.id,
            holdQueue: true,
          });
        }
        yield* worker.drain();
        if (!echoes) {
          // Claude was never handed the notice, which waits for the next turn instead: held
          // when Stop had already landed, as Stop holds everything queued before it.
          assert.lengthOf(offers, 1);
          assert.deepEqual(yield* queuedOrder, [[noticeId, stopsFirst]]);
          if (!question) assert.equal(yield* task, "claimed");
          return;
        }
        if (afterSteer) {
          // Claude was handed both and read neither. Both are queued again from the same
          // place, and the notice, steered in second, waits behind the steer.
          assert.deepEqual(
            offers.map((offer) => offer.priority),
            [undefined, "next", "next"],
          );
          yield* restart;
          assert.deepEqual(yield* queuedOrder, [
            [steerId, true],
            [noticeId, true],
          ]);
          return;
        }
        assert.deepEqual(
          offers.map((offer) => offer.priority),
          [undefined, "next"],
        );
        // Claude has only queued the result, so nothing counts it as delivered yet.
        if (!question) assert.equal(yield* task, "claimed");
        yield* Fiber.join(unread);
        const reported = Effect.map(orchestrator.getThreadProjection(threadId), (projection) =>
          projection.turnItems.flatMap((item) =>
            item.type === "system_notice" ? [item.title] : [],
          ),
        );

        if (outcome.startsWith("Claude cancels")) {
          const ended = yield* settled(parent.id, "waiting");
          const restarted = yield* noticeRun("run.updated", (run) => run.status === "starting");
          const rerunning = yield* watch(
            (event) =>
              event.type === "provider-turn.updated" &&
              event.payload.status === "running" &&
              event.payload.runAttemptId !== parent.activeAttemptId,
          );
          yield* Queue.offer(sdkMessages, toolResult("build", false));
          for (const id of statusIds) yield* Queue.offer(sdkMessages, toolResult(id, false));
          yield* answer(offers[0]!);
          yield* cancel(offers[1]!);
          yield* Fiber.join(ended);
          yield* worker.drain();
          yield* Fiber.join(restarted);
          yield* worker.drain();
          yield* Fiber.join(rerunning);
          // One commit closes the unread notice and queues it again, so no crash parts them.
          const requeue = yield* sink
            .readByCommandId({
              commandId: CommandId.make(`command:unread-steer:${parent.id}:${noticeId}`),
            })
            .pipe(Stream.runCollect);
          assert.isTrue(
            Array.from(requeue).some(
              ({ event }) =>
                event.type === "turn-item.updated" &&
                event.payload.type === "notification" &&
                event.payload.status === "cancelled",
            ),
          );
          // It reaches Claude again as the next turn's prompt, still undelivered until then.
          assert.deepEqual(
            offers.map((offer) => offer.priority),
            [undefined, "next", undefined],
          );
          assert.notInclude(yield* reported, "Steer delivery not confirmed");
          if (question) {
            assert.include(offeredText(offers[2]), "Child notice.");
            return;
          }
          assert.equal(yield* task, "claimed");
          const delivered = yield* watch(
            (event) =>
              event.type === "subagent.updated" &&
              event.payload.completionDelivery?.state === "delivered",
          );
          const reran = yield* noticeRun("run.updated", (run) => run.status === "waiting");
          yield* answer(offers[2]!);
          yield* Fiber.join(reran);
          yield* worker.drain();
          yield* Fiber.join(delivered);
          return;
        }

        if (outcome.startsWith("the turn ends with no echo")) {
          const read = outcome.endsWith("after Claude took a child result");
          // The unread notice is closed when it is queued again, and completed when Claude read it.
          const settledNotice = yield* watch(
            (event) =>
              event.type === "turn-item.updated" &&
              event.payload.type === "notification" &&
              (event.payload.status === "cancelled" || event.payload.status === "completed"),
          );
          const requeued = yield* noticeRun("run.created", () => true);
          const ended = yield* settled(parent.id, "waiting");
          yield* Queue.offer(sdkMessages, toolResult("build", false));
          for (const id of statusIds) yield* Queue.offer(sdkMessages, toolResult(id, false));
          yield* Queue.offer(sdkMessages, result("result-without-echo", false));
          const dropped = outcome.endsWith("refuses the child result it took")
            ? "refused"
            : outcome.endsWith("child result it took unstarted")
              ? "completed"
              : undefined;
          if (read || dropped !== undefined) {
            const lifecycle = (state: "started" | "refused" | "completed") =>
              frame({
                type: "command_lifecycle",
                command_uuid: offers[1]?.uuid,
                state,
                uuid: `${state}-notice`,
                session_id: sessionId,
              });
            yield* Queue.offer(sdkMessages, lifecycle(dropped ?? "started"));
            if (read) yield* answer(offers[1]!);
          }
          if (outcome.endsWith("never answers for a child result")) {
            yield* Deferred.await(cancelAsked);
            yield* TestClock.adjust("2 seconds");
          }
          if (outcome.endsWith("never starts the child result it kept")) {
            yield* Deferred.await(cancelAsked);
            yield* TestClock.adjust("10 seconds");
          }
          yield* Fiber.join(ended);
          yield* worker.drain();
          yield* Fiber.join(settledNotice);
          if (!read) yield* Fiber.join(requeued);
          yield* worker.drain();
          assert.equal((yield* notice)?.status, read ? "completed" : "cancelled");
          assert.lengthOf(yield* noticeRuns, read ? 0 : 1);
          assert.equal(yield* task, read ? "delivered" : "claimed");
          assert.deepEqual(
            takenBack.map((uuid) => offers.findIndex((offer) => offer.uuid === uuid)),
            [1],
          );
          return;
        }

        if (outcome.includes("accept was lost")) {
          // The adapter's events arrive in order, so its own pending mark is in before this one.
          const built = yield* watch(
            (event) =>
              event.type === "turn-item.updated" &&
              event.payload.nativeItemRef?.nativeId === "build" &&
              event.payload.status === "completed",
          );
          yield* Queue.offer(sdkMessages, toolResult("build", false));
          yield* Fiber.join(built);
          const read = yield* notice;
          if (read === undefined) return yield* Effect.die("the notice has no item");
          const now = yield* DateTime.now;
          // Claude's read is committed without the accept it starts: only the provider's own
          // event starts one, and this write names no driver.
          yield* sink.write({
            events: [
              {
                id: EventId.make("read"),
                type: "turn-item.updated",
                threadId,
                occurredAt: now,
                payload: { ...read, status: "completed", completedAt: now, updatedAt: now },
              },
            ],
          });
          if (outcome.endsWith("a restart")) {
            yield* restart;
          } else if (outcome.endsWith("a wake policy change")) {
            // The run is over and nothing has accepted the read yet when a command offers the
            // thread's results, as startup does for each finished task before its own accept.
            yield* cut;
            yield* orchestrator.dispatch({
              type: "delegated_task.wake-policy",
              commandId: CommandId.make("wake-policy"),
              parentThreadId: threadId,
              taskId: NodeId.make("completed-child"),
              completionWake: "settled_only",
            });
          } else {
            // A run's end starts the queue last, so the next run starts after the accept.
            const ended = yield* settled(parent.id, "waiting");
            const next = yield* watch(
              (event) =>
                event.type === "run.updated" &&
                event.payload.userMessageId === MessageId.make("before") &&
                event.payload.status === "starting",
            );
            yield* send("before", { type: "queue_after_active" });
            for (const id of statusIds) yield* Queue.offer(sdkMessages, toolResult(id, false));
            yield* answer(offers[1]!);
            yield* Fiber.join(ended);
            yield* worker.drain();
            yield* Fiber.join(next);
          }
          // The result Claude read is delivered, and nothing offers it again, as a turn of
          // its own or as a second steer. A result's text is written from its tasks.
          assert.equal(yield* task, "delivered");
          assert.lengthOf(yield* noticeRuns, 0);
          assert.lengthOf(
            offers.filter((offer) => offer.priority === "next"),
            1,
          );
          return;
        }

        if (outcome.startsWith("the Claude process exits")) {
          const held = yield* noticeRun("run.updated", (run) => run.queueHeld === true);
          yield* Queue.shutdown(sdkMessages);
          yield* Fiber.join(held);
          yield* worker.drain();
          // The provider failure holds the queue, as it does for a steer the user sent.
          assert.deepEqual(yield* queuedOrder, [[noticeId, true]]);
          assert.equal(yield* task, "claimed");
          assert.lengthOf(offers, 2);
          return;
        }

        // Stop drops the stopped run's child results, read or not. A question stays the child's
        // to have answered, so it waits in the held queue.
        const dropped = yield* question
          ? noticeRun("run.created", (run) => run.status === "queued")
          : noticeItem("cancelled");
        yield* orchestrator.dispatch({
          type: "run.interrupt",
          commandId: CommandId.make("stop"),
          threadId,
          runId: parent.id,
          holdQueue: true,
        });
        yield* worker.drain();
        yield* Fiber.join(dropped);
        assert.deepEqual(yield* queuedOrder, question ? [[noticeId, true]] : []);
        // The question named no model, so released later it runs on the thread's pick.
        if (question) {
          const held = (yield* orchestrator.getThreadProjection(threadId)).messages.find(
            (message) => message.id === noticeId,
          );
          assert.isTrue(held?.followsThreadSelection);
        }
        if (!question) assert.equal(yield* task, "disposed");
        assert.notInclude(yield* reported, "Steer delivery not confirmed");
        assert.lengthOf(offers, 2);
        return;
      }
      yield* send("before", { type: "queue_after_active" });
      const unread = yield* pending();
      yield* send("steer", { type: "steer_active", targetRunId: parent.id });
      yield* worker.drain();
      yield* Fiber.join(unread);
      yield* send("after", { type: "queue_after_active" }, "claude-opus-4-1");
      assert.deepEqual(
        offers.map((offer) => offer.priority),
        [undefined, "next"],
      );

      if (outcome === "Claude cancels it twice") {
        // The run-end reaction starts the next run outside the worker, so drain after it commits.
        const starting = (messageId: string) =>
          watch(
            (event) =>
              event.type === "run.updated" &&
              event.payload.userMessageId === MessageId.make(messageId) &&
              event.payload.status === "starting",
          );
        const ended = yield* settled(parent.id, "waiting");
        const beforeStarting = yield* starting("before");
        const nextRunning = yield* watch(
          (event) =>
            event.type === "provider-turn.updated" &&
            event.payload.status === "running" &&
            event.payload.runAttemptId !== parent.activeAttemptId,
        );
        yield* answer(offers[0]!);
        yield* cancel(offers[1]!);
        yield* Fiber.join(ended);
        yield* worker.drain();
        yield* Fiber.join(beforeStarting);
        yield* worker.drain();
        yield* Fiber.join(nextRunning);
        // The older message runs first; the steer waits right behind it on its own model.
        const behind = yield* queued;
        assert.deepEqual(
          behind.map((run) => run.userMessageId),
          [steerId, MessageId.make("after")],
        );
        assert.deepEqual(behind[0]?.modelSelection, modelSelection);
        assert.include(offeredText(offers[2]), "Message before");
        // Promoted into that run, Claude drops it again; it is queued again, then reaches Claude.
        const before = (yield* orchestrator.getThreadProjection(threadId)).runs.find(
          (run) => run.userMessageId === MessageId.make("before"),
        )!;
        yield* orchestrator.dispatch({
          type: "queued-message.promote-to-steer",
          commandId: CommandId.make("promote"),
          threadId,
          queuedRunId: behind[0]!.id,
          targetRunId: before.id,
        });
        yield* worker.drain();
        assert.equal(offers[3]?.priority, "next");
        const beforeEnded = yield* settled(before.id, "waiting");
        const steerStarting = yield* starting("steer");
        yield* answer(offers[2]!);
        yield* cancel(offers[3]!);
        yield* Fiber.join(beforeEnded);
        yield* worker.drain();
        yield* Fiber.join(steerStarting);
        yield* worker.drain();
        assert.deepEqual(yield* queuedOrder, [[MessageId.make("after"), false]]);
        assert.equal(offers.length, 5);
        assert.equal(offers[4]?.priority, undefined);
        assert.include(offeredText(offers[4]), "Message steer");
        return;
      }

      if (outcome === "the Claude process exits") {
        const requeued = yield* watch(
          (event) =>
            event.type === "run.updated" &&
            event.payload.userMessageId === steerId &&
            event.payload.status === "queued",
        );
        yield* Queue.shutdown(sdkMessages);
        yield* Fiber.join(requeued);
        yield* worker.drain();
        // The provider failure holds the queue, the follow-up included, until someone resumes it.
        assert.deepEqual(yield* queuedOrder, [
          [MessageId.make("before"), true],
          [steerId, true],
          [MessageId.make("after"), true],
        ]);
        assert.equal(offers.length, 2);
        return;
      }

      if (outcome.startsWith("a later")) {
        const lands = outcome === "a later client steer lands as its run ends";
        if (lands) activeRunReads = 0;
        const late = lands
          ? yield* send("late", { type: "start_immediately" }, undefined, "steer").pipe(
              Effect.forkScoped,
            )
          : undefined;
        if (lands) yield* Deferred.await(activeRunReadEntered);
        // The reaction skips reconcile writes, so the turn ends unseen by it.
        const now = yield* DateTime.now;
        yield* (yield* EventSink.EventSinkV2).write({
          commandId: CommandId.make("command:runtime-reconcile:late-steer"),
          events: [
            {
              id: EventId.make("late-steer-parent-completed"),
              type: "run.updated" as const,
              threadId,
              runId: parent.id,
              occurredAt: now,
              payload: {
                ...parent,
                status:
                  outcome === "a later client steer names its stopped run"
                    ? ("interrupted" as const)
                    : ("completed" as const),
                completedAt: now,
              },
            },
          ],
        });
        // Clients send a steer or restart as a start, with the run picked under the lock.
        const client = outcome.includes("client");
        if (outcome.includes("restart")) {
          yield* client
            ? send("late", { type: "start_immediately" }, undefined, "restart")
            : send("late", { type: "restart_active", targetRunId: parent.id });
          // A restart starts ahead of the queue, as it would have while the turn ran.
          assert.deepEqual(yield* queuedOrder, [
            [MessageId.make("before"), false],
            [MessageId.make("after"), false],
          ]);
          // The run-end requeue, which recovery repeats, then places the unread steer after the
          // restart and behind only the message queued before it.
          yield* orchestrator.recoverDelegatedTasks;
          assert.deepEqual(yield* queuedOrder, [
            [MessageId.make("before"), false],
            [steerId, false],
            [MessageId.make("after"), false],
          ]);
          return;
        }
        if (late !== undefined) {
          yield* Deferred.succeed(activeRunReadRelease, undefined);
          yield* Fiber.join(late);
        } else {
          yield* outcome === "a later client steer names its stopped run"
            ? send("late", { type: "steer_active", targetRunId: parent.id }, undefined, "steer")
            : client
              ? send("late", { type: "start_immediately" }, undefined, "steer")
              : send("late", { type: "steer_active", targetRunId: parent.id });
        }
        // The steer Claude never read keeps its place ahead of the later one, which is not
        // steered into the run the queue started.
        const behind = yield* queued;
        assert.deepEqual(
          behind.map((run) => [run.userMessageId, run.queueHeld === true, run.steerWhenReadyRunId]),
          [
            [steerId, false, undefined],
            [MessageId.make("after"), false, undefined],
            [MessageId.make("late"), false, undefined],
          ],
        );
        return;
      }

      if (outcome === "the user sends it now") {
        yield* orchestrator.dispatch({
          type: "steer.send-now",
          commandId: CommandId.make("send-now"),
          threadId,
          messageId: steerId,
        });
        yield* worker.drain();
        // Claude gets the same prompt again at "now"; the steer stays unread until Claude says so.
        assert.deepEqual(takenBack, [offers[1]?.uuid ?? ""]);
        assert.equal(offers.length, 3);
        assert.equal(offers[2]?.priority, "now");
        assert.deepEqual(offers[2]?.message, offers[1]?.message);
        const item = (yield* orchestrator.getThreadProjection(threadId)).turnItems.find(
          (candidate) => candidate.type === "user_message" && candidate.messageId === steerId,
        );
        assert.equal(item?.status, "pending");
        assert.isTrue(item?.type === "user_message" && item.sentNow);
        // Claude reads it before a second tap, or the first tap's resend, lands.
        const read = yield* watch(
          (event) =>
            event.type === "turn-item.updated" &&
            event.payload.type === "user_message" &&
            event.payload.messageId === steerId &&
            event.payload.status === "completed",
        );
        yield* Queue.offer(
          sdkMessages,
          frame({
            type: "command_lifecycle",
            command_uuid: offers[2]?.uuid,
            state: "started",
            uuid: "started-steer",
            session_id: sessionId,
          }),
        );
        yield* Fiber.join(read);
        const late = yield* orchestrator.dispatch({
          type: "steer.send-now",
          commandId: CommandId.make("send-now-after-read"),
          threadId,
          messageId: steerId,
        });
        yield* worker.drain();
        assert.deepEqual(late.storedEvents, []);
        assert.equal(offers.length, 3);
        return;
      }

      if (outcome === "Claude reads it while the user sends it now") {
        const sink = yield* EventSink.EventSinkV2;
        const planned = yield* Deferred.make<void>();
        const commit = yield* Deferred.make<void>();
        const commitCommand = sink.commitCommand;
        using _commit = vi
          .spyOn(sink, "commitCommand")
          .mockImplementation((input) =>
            input.commandId === CommandId.make("send-now")
              ? Deferred.succeed(planned, undefined).pipe(
                  Effect.andThen(Deferred.await(commit)),
                  Effect.andThen(commitCommand(input)),
                )
              : commitCommand(input),
          );
        const read = yield* watch(
          (event) =>
            event.type === "turn-item.updated" &&
            event.payload.type === "user_message" &&
            event.payload.messageId === steerId &&
            event.payload.status === "completed",
        );
        const sending = yield* orchestrator
          .dispatch({
            type: "steer.send-now",
            commandId: CommandId.make("send-now"),
            threadId,
            messageId: steerId,
          })
          .pipe(Effect.forkChild({ startImmediately: true }));
        yield* Deferred.await(planned);
        yield* Queue.offer(
          sdkMessages,
          frame({
            type: "command_lifecycle",
            command_uuid: offers[1]?.uuid,
            state: "started",
            uuid: "started-steer",
            session_id: sessionId,
          }),
        );
        yield* Fiber.join(read);
        yield* Deferred.succeed(commit, undefined);
        yield* Fiber.join(sending);
        yield* worker.drain();
        const item = (yield* orchestrator.getThreadProjection(threadId)).turnItems.find(
          (candidate) => candidate.type === "user_message" && candidate.messageId === steerId,
        );
        assert.equal(item?.status, "completed");
        assert.lengthOf(offers, 2);
        return;
      }

      if (outcome === "Claude cancels it and the user stops") {
        yield* cancel(offers[1]!);
        const steerStarting = yield* watch(
          (event) =>
            event.type === "run.updated" &&
            event.payload.userMessageId === steerId &&
            event.payload.status === "starting",
        );
        yield* orchestrator.dispatch({
          type: "run.interrupt",
          commandId: CommandId.make("stop"),
          threadId,
          runId: parent.id,
          holdQueue: true,
        });
        yield* worker.drain();
        yield* Fiber.join(steerStarting);
        yield* worker.drain();
        // Stop sends the steer its turn never read as the next turn. The queue stays held.
        const after = yield* orchestrator.getThreadProjection(threadId);
        assert.isFalse(after.turnItems.some((item) => item.type === "system_notice"));
        assert.deepEqual(yield* queuedOrder, [
          [MessageId.make("before"), true],
          [MessageId.make("after"), true],
        ]);
        return;
      }

      // Recovery cancels the run, holds the queue and leaves the unread steer pending.
      const projection = yield* orchestrator.getThreadProjection(threadId);
      const now = yield* DateTime.now;
      yield* (yield* EventSink.EventSinkV2).write({
        commandId: CommandId.make("command:runtime-reconcile:startup:unread-steer"),
        events: projection.runs.map((run, index) => ({
          id: EventId.make(`recovered-run-${index}`),
          type: "run.updated" as const,
          threadId,
          runId: run.id,
          occurredAt: now,
          payload:
            run.id === parent.id
              ? { ...run, status: "cancelled" as const, completedAt: now }
              : { ...run, queueHeld: true },
        })),
      });
      yield* orchestrator.recoverDelegatedTasks;
      yield* worker.drain();
      assert.deepEqual(yield* queuedOrder, [
        [MessageId.make("before"), true],
        [steerId, true],
        [MessageId.make("after"), true],
      ]);
      assert.equal(offers.length, 2);
    }).pipe(
      Effect.provide(
        ProviderReplayHarness.layerWithRegistry(
          { name: "claude-unread-steer" },
          ProviderAdapterRegistry.layerSingle(adapter),
          {
            runEffectWorker: false,
            // Holds the steer's second read of the active run, the one after the queue starts.
            wrapProjectionStore: (store) => ({
              ...store,
              getThreadRecords: ((threadId, fields, options) => {
                const read = store.getThreadRecords(threadId, fields, options);
                // The read a requeued notice's dispatch makes of the run it was steered into.
                const noticeRead =
                  fields.length === 1 &&
                  fields[0] === "turnItems" &&
                  options?.turnItemTypes?.join() === "notification" &&
                  options.turnItemStatuses?.includes("cancelled") === true;
                if (noticeRead) {
                  return Effect.suspend(() => {
                    if (!failNoticeRead) return read;
                    failNoticeRead = false;
                    return Effect.fail(
                      new ProjectionStore.ProjectionStoreReadError({ threadId, cause: "injected" }),
                    );
                  });
                }
                const activeRunRead =
                  fields.length === 1 && fields[0] === "runs" && options?.runStatuses?.length === 4;
                if (!activeRunRead) return read;
                // Counted per run: dispatch reuses one read effect for both reads.
                return Effect.suspend(() =>
                  activeRunReads === undefined || ++activeRunReads !== 2
                    ? read
                    : Deferred.succeed(activeRunReadEntered, undefined).pipe(
                        Effect.andThen(Deferred.await(activeRunReadRelease)),
                        Effect.andThen(read),
                      ),
                );
              }) as typeof store.getThreadRecords,
            }),
          },
        ),
      ),
    );
  }).pipe(
    Effect.provide(
      Layer.mergeAll(
        IdAllocator.layer,
        McpProviderSessions.layer,
        NodeServices.layer,
        NodeCrypto.layer,
      ),
    ),
  ),
);
