import * as NodeCrypto from "@effect/platform-node/NodeCrypto";
import { assert, describe, it, vi } from "@effect/vitest";
import * as NodeServices from "@effect/platform-node/NodeServices";
import {
  CommandId,
  ChatAttachmentId,
  type ChatAttachment,
  EventId,
  MessageId,
  ProjectId,
  ProviderDriverKind,
  ProviderInstanceId,
  ProviderThreadId,
  RunId,
  TextGenerationError,
  ThreadId,
} from "@onicode/contracts";
import * as DateTime from "effect/DateTime";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as FileSystem from "effect/FileSystem";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as TestClock from "effect/testing/TestClock";
import { ATTACHMENT_ONLY_BOOTSTRAP_PROMPT } from "@onicode/shared/composerContextReferences";

import * as SqlitePersistence from "../persistence/Sqlite.ts";
import * as ProjectStore from "./ProjectStore.ts";
import * as ServerSettings from "../serverSettings.ts";
import * as TextGeneration from "../textGeneration/TextGeneration.ts";
import { CodexProviderCapabilitiesV2 } from "./Adapters/CodexAdapterV2.ts";
import * as CommandReceiptStore from "./CommandReceiptStore.ts";
import * as EffectOutbox from "./EffectOutbox.ts";
import * as EventSink from "./EventSink.ts";
import * as ProjectionStore from "./ProjectionStore.ts";
import * as ProviderAdapterRegistry from "./ProviderAdapterRegistry.ts";
import * as ThreadManagement from "./ThreadManagementService.ts";
import * as ThreadTitleRegeneration from "./ThreadTitleRegenerationService.ts";
import * as ProviderReplayHarness from "./testkit/ProviderReplayHarness.ts";
import { placeholderThreadTitle } from "./placeholderThreadTitle.ts";
import type * as ProviderAdapter from "@onicode/provider-core/server/ProviderAdapter";

const projectId = ProjectId.make("project:title-regeneration");
const modelSelection = {
  instanceId: ProviderInstanceId.make("codex"),
  model: "gpt-5.1-codex",
} as const;

const adapter = {
  instanceId: modelSelection.instanceId,
  driver: ProviderDriverKind.make("codex"),
  getCapabilities: () => Effect.succeed(CodexProviderCapabilitiesV2),
  planSelectionTransition: () => Effect.succeed({ type: "apply_on_next_turn" as const }),
  openSession: () => Effect.die("provider execution is disabled in title regeneration tests"),
} as ProviderAdapter.ProviderAdapterV2["Service"];

function makeHarness(
  options: {
    readonly generateTitle?: TextGeneration.TextGeneration["Service"]["generateThreadTitle"];
  } = {},
) {
  const layerDatabase = SqlitePersistence.layerMemory;
  const layerRegistry = ProviderAdapterRegistry.layerFromAdapters([adapter]);
  const layerOrchestrator = ProviderReplayHarness.layerWithRegistry(
    { name: "thread-title-regeneration" },
    layerRegistry,
    { databaseLayer: layerDatabase, runEffectWorker: false },
  );
  const layerServerSettings = ServerSettings.layerTest({});
  const layerThreadManagement = ThreadManagement.layer.pipe(
    Layer.provide(layerOrchestrator),
    Layer.provide(layerServerSettings),
    Layer.provide(CommandReceiptStore.layer.pipe(Layer.provide(layerDatabase))),
  );
  const layerOutbox = EffectOutbox.layer.pipe(Layer.provide(layerDatabase));
  const generateThreadTitle = vi.fn(
    options.generateTitle ?? (() => Effect.succeed({ title: "Generated title" })),
  );
  const layerProjectedProjects = Layer.mock(ProjectStore.ProjectStoreV2)({
    get: (requestedProjectId) =>
      Effect.succeed(
        requestedProjectId === projectId
          ? Option.some({
              projectId,
              title: "Project",
              workspaceRoot: "/repo",
              defaultModelSelection: modelSelection,
              defaultThreadEnvMode: null,
              hasCustomTitle: false,
              autoPull: false,
              faviconPath: null,
              projectIcon: null,
              scripts: [],
              trackedFolders: [],
              pinnedAt: null,
              archivedAt: null,
              createdAt: "2026-06-20T00:00:00.000Z",
              updatedAt: "2026-06-20T00:00:00.000Z",
              deletedAt: null,
            })
          : Option.none(),
      ),
  });
  const layerTitleRegeneration = ThreadTitleRegeneration.layer.pipe(
    Layer.provide(
      Layer.mergeAll(
        layerThreadManagement,
        ProjectionStore.layer.pipe(
          Layer.provide(NodeCrypto.layerDigest),
          Layer.provide(layerDatabase),
        ),
        layerProjectedProjects,
        Layer.mock(TextGeneration.TextGeneration)({ generateThreadTitle }),
        layerServerSettings,
        NodeServices.layer,
      ),
    ),
  );
  return {
    layer: Layer.mergeAll(
      layerOrchestrator,
      layerThreadManagement,
      layerTitleRegeneration,
      layerOutbox,
      layerDatabase,
    ),
    generateThreadTitle,
  };
}

function createThread(input: {
  readonly command: string;
  readonly thread: string;
  readonly worktreePath?: string;
}) {
  return Effect.gen(function* () {
    const threads = yield* ThreadManagement.ThreadManagementService;
    const threadId = ThreadId.make(input.thread);
    yield* threads.dispatch({
      type: "thread.create",
      commandId: CommandId.make(input.command),
      threadId,
      projectId,
      title: "Seed title",
      modelSelection,
      runtimeMode: "full-access",
      interactionMode: "default",
      branch: null,
      worktreePath: input.worktreePath ?? null,
      createdBy: "user",
      creationSource: "web",
    });
    return threadId;
  });
}

function dispatchUserMessage(input: {
  readonly command: string;
  readonly threadId: ThreadId;
  readonly text: string;
  readonly attachments?: ReadonlyArray<ChatAttachment>;
}) {
  return Effect.gen(function* () {
    const threads = yield* ThreadManagement.ThreadManagementService;
    yield* threads.dispatch({
      type: "message.dispatch",
      commandId: CommandId.make(input.command),
      threadId: input.threadId,
      messageId: MessageId.make(`${input.command}:message`),
      text: input.text,
      attachments: input.attachments ?? [],
      modelSelection,
      dispatchMode: { type: "defer_start" },
      createdBy: "user",
      creationSource: "web",
    });
  });
}

/** Records a settled source run with one exchange, as a provider turn would. */
function recordRun(input: {
  readonly threadId: ThreadId;
  readonly runId: RunId;
  readonly ordinal: number;
  readonly user: string;
  readonly assistant: string;
  readonly status?: "completed" | "rolled_back" | "cancelled" | "queued";
  readonly freshContext?: boolean;
}) {
  return Effect.gen(function* () {
    const eventSink = yield* EventSink.EventSinkV2;
    const now = yield* DateTime.now;
    const providerThreadId = ProviderThreadId.make(`${input.threadId}:provider`);
    const message = (role: "user" | "assistant", text: string) => ({
      id: EventId.make(`${input.runId}:${role}`),
      type: "message.updated" as const,
      threadId: input.threadId,
      runId: input.runId,
      occurredAt: now,
      payload: {
        id: MessageId.make(`${input.runId}:${role}`),
        threadId: input.threadId,
        runId: input.runId,
        nodeId: null,
        role,
        text,
        attachments: [],
        streaming: false,
        createdBy: "user" as const,
        creationSource: "web" as const,
        createdAt: now,
        updatedAt: now,
      },
    });
    yield* eventSink.write({
      events: [
        {
          id: EventId.make(`${input.runId}:created`),
          type: "run.created",
          threadId: input.threadId,
          runId: input.runId,
          occurredAt: now,
          payload: {
            id: input.runId,
            threadId: input.threadId,
            ordinal: input.ordinal,
            providerInstanceId: modelSelection.instanceId,
            modelSelection,
            providerThreadId,
            userMessageId: MessageId.make(`${input.runId}:user`),
            rootNodeId: null,
            activeAttemptId: null,
            status: input.status ?? "completed",
            requestedAt: now,
            startedAt: input.status === "cancelled" || input.status === "queued" ? null : now,
            completedAt: input.status === "queued" ? null : now,
            checkpointId: null,
            contextHandoffId: null,
            ...(input.freshContext ? { freshContext: true } : {}),
          },
        },
        {
          id: EventId.make(`${input.runId}:provider-thread`),
          type: "provider-thread.updated",
          threadId: input.threadId,
          occurredAt: now,
          payload: {
            id: providerThreadId,
            driver: adapter.driver,
            providerInstanceId: modelSelection.instanceId,
            providerSessionId: null,
            appThreadId: input.threadId,
            ownerNodeId: null,
            nativeThreadRef: { driver: adapter.driver, nativeId: "native", strength: "strong" },
            nativeConversationHeadRef: null,
            status: "idle",
            firstRunOrdinal: 1,
            lastRunOrdinal: input.ordinal,
            handoffIds: [],
            forkedFrom: null,
            createdAt: now,
            updatedAt: now,
          },
        },
        message("user", input.user),
        message("assistant", input.assistant),
      ],
    });
  });
}

function armRegeneration(input: { readonly command: string; readonly threadId: ThreadId }) {
  return Effect.gen(function* () {
    const threads = yield* ThreadManagement.ThreadManagementService;
    const requestId = CommandId.make(input.command);
    yield* threads.dispatch({
      type: "thread.metadata.update",
      commandId: requestId,
      threadId: input.threadId,
      regenerateTitle: true,
    });
    return requestId;
  });
}

/** Forks a source thread at its middle run, between a rolled-back run and later work. */
function forkSeededSource(
  prefix: string,
  sideChat = false,
  rollbackPriorRun = true,
  unstartedStatus: "cancelled" | "queued" = "cancelled",
) {
  return Effect.gen(function* () {
    const threads = yield* ThreadManagement.ThreadManagementService;
    const sourceThreadId = yield* createThread({
      command: `${prefix}:create`,
      thread: `${prefix}:source`,
    });
    yield* recordRun({
      threadId: sourceThreadId,
      runId: RunId.make(`${prefix}:run:0`),
      ordinal: 1,
      user: "Rolled back attempt",
      assistant: "Discarded.",
      status: rollbackPriorRun ? "rolled_back" : "completed",
    });
    yield* TestClock.adjust(1);
    yield* recordRun({
      threadId: sourceThreadId,
      runId: RunId.make(`${prefix}:run:cancelled`),
      ordinal: 2,
      user: "Queued message the fork point never reached",
      assistant: "",
      status: unstartedStatus,
    });
    const forkRunId = RunId.make(`${prefix}:run:1`);
    yield* recordRun({
      threadId: sourceThreadId,
      runId: forkRunId,
      ordinal: 3,
      user: "Move session storage from Redis to Postgres",
      assistant: "Sessions now persist in the Postgres sessions table.",
    });
    yield* recordRun({
      threadId: sourceThreadId,
      runId: RunId.make(`${prefix}:run:2`),
      ordinal: 4,
      user: "Later work the fork never saw",
      assistant: "Done.",
    });
    const forkThreadId = ThreadId.make(`${prefix}:target`);
    yield* threads.dispatch({
      type: "thread.fork",
      commandId: CommandId.make(`${prefix}:fork`),
      sourceThreadId,
      targetThreadId: forkThreadId,
      sourcePoint: { type: "run", runId: forkRunId },
      sideChat,
      createdBy: "user",
      creationSource: "web",
    });
    return { sourceThreadId, forkRunId, forkThreadId };
  });
}

describe("formatThreadTitleContext", () => {
  it("pins the first user message ahead of the retained tail once content stops fitting", () => {
    const context = ThreadTitleRegeneration.formatThreadTitleContext([
      { role: "user", text: `Ancient context that anchors the topic ${"x".repeat(600)}` },
      { role: "assistant", text: "y".repeat(6_000) },
      { role: "user", text: "z".repeat(1_500) },
    ]);
    assert.isTrue(context.message.includes("USER:\nAncient context that anchors the topic"));
    assert.isTrue(context.message.includes("[Earlier content truncated]\n\n"));
    assert.isTrue(context.message.includes("y".repeat(100)));
  });
});

describe("ThreadTitleRegenerationService", () => {
  it.effect("keeps a title set before the first message unless it is a placeholder", () =>
    Effect.gen(function* () {
      const seed = "work on this and pr to dev";
      const harness = makeHarness();
      yield* Effect.gen(function* () {
        const threads = yield* ThreadManagement.ThreadManagementService;
        const outbox = yield* EffectOutbox.EffectOutboxV2;
        // An MCP launch names the thread, or leaves a placeholder, and sends no message.
        for (const [key, title, expectedTitle, generates, compactsFirst, titleRenamed] of [
          ["preset", "[API] #4821 — retry webhook deliveries after a timeout", undefined, false],
          ["placeholder", placeholderThreadTitle("Parent", 0), seed, true],
          ["named like a placeholder", "Release thread 2", undefined, false, false, true],
          ["preset, then compacted", "Release checklist", undefined, false, true],
          ["named, then compacted", "Release notes", undefined, false, true, true],
          // An agent that gives only a prompt gets the prompt as the title.
          ["prompt-titled, then compacted", "/compact", seed, true, true],
        ] as const) {
          const threadId = ThreadId.make(`thread:title:${key}`);
          yield* threads.dispatch({
            type: "thread.create",
            commandId: CommandId.make(`command:title:${key}:create`),
            threadId,
            projectId,
            title,
            ...(titleRenamed ? { titleRenamed } : {}),
            modelSelection,
            runtimeMode: "full-access",
            interactionMode: "default",
            branch: null,
            worktreePath: null,
            createdBy: "agent",
            creationSource: "mcp",
          });
          if (compactsFirst) {
            yield* dispatchUserMessage({
              command: `command:title:${key}:compact`,
              threadId,
              text: "/compact",
            });
          }
          const requestId = CommandId.make(`command:title:${key}:message`);
          yield* threads.dispatch({
            type: "message.dispatch",
            commandId: requestId,
            threadId,
            messageId: MessageId.make(`command:title:${key}:message:message`),
            text: seed,
            titleSeed: seed,
            attachments: [],
            modelSelection,
            dispatchMode: { type: "defer_start" },
            createdBy: "user",
            creationSource: "web",
          });

          const settled = yield* threads.getThreadProjection(threadId);
          assert.equal(settled.thread.title, expectedTitle ?? title, key);
          assert.equal(
            (yield* outbox.listByCommandId(requestId)).some(
              (effect) => effect.request.type === "thread-title.generate",
            ),
            generates,
            key,
          );
        }
      }).pipe(Effect.provide(harness.layer));
    }),
  );

  it.effect("keeps a fork's own rename when its first message arrives", () =>
    Effect.gen(function* () {
      const seed = "Try SQLite instead";
      const harness = makeHarness();
      yield* Effect.gen(function* () {
        const threads = yield* ThreadManagement.ThreadManagementService;
        const titleRegeneration = yield* ThreadTitleRegeneration.ThreadTitleRegenerationService;
        const outbox = yield* EffectOutbox.EffectOutboxV2;
        const prefix = "thread:title:renamed-fork";
        const { sourceThreadId, forkRunId, forkThreadId } = yield* forkSeededSource(prefix);
        const rename = (key: string, threadId: ThreadId, title: string) =>
          threads.dispatch({
            type: "thread.metadata.update",
            commandId: CommandId.make(`${prefix}:${key}:rename`),
            threadId,
            title,
          });
        const fork = (key: string, title?: string, createdBy: "user" | "agent" = "agent") =>
          Effect.gen(function* () {
            const targetThreadId = ThreadId.make(`${prefix}:${key}`);
            yield* threads.dispatch({
              type: "thread.fork",
              commandId: CommandId.make(`${prefix}:${key}:fork`),
              sourceThreadId,
              targetThreadId,
              sourcePoint: { type: "run", runId: forkRunId },
              ...(title === undefined || createdBy === "user"
                ? { title, createdBy: "user", creationSource: "web" }
                : { title, createdBy: "agent", creationSource: "mcp" }),
            });
            return targetThreadId;
          });
        yield* rename("renamed", forkThreadId, "Storage migration notes");
        yield* rename("source", sourceThreadId, "Renamed source");
        const forkOfRenamedSource = yield* fork("fork-of-renamed-source");
        const regenerated = yield* fork("regenerated");
        yield* rename("regenerated", regenerated, "Scratch name");
        yield* titleRegeneration.execute({
          threadId: regenerated,
          requestId: yield* armRegeneration({
            command: `${prefix}:regenerated:regenerate`,
            threadId: regenerated,
          }),
          kind: { type: "regenerate" },
        });
        // The generator returns the name the user typed, so the title stays but is generated now.
        const regeneratedUnchanged = yield* fork("regenerated-unchanged");
        yield* rename("regenerated-unchanged", regeneratedUnchanged, "Generated title");
        yield* titleRegeneration.execute({
          threadId: regeneratedUnchanged,
          requestId: yield* armRegeneration({
            command: `${prefix}:regenerated-unchanged:regenerate`,
            threadId: regeneratedUnchanged,
          }),
          kind: { type: "regenerate" },
        });
        const titled = yield* fork("titled", "Release checklist");
        // A client names a fork after the thread on screen, which can lag behind the source.
        const clientTitled = yield* fork("client-titled", "Seed title fork", "user");
        // A title generated from only a `/compact` is the generator's, not a name to keep.
        const generatedAfterCompact = yield* createThread({
          command: `${prefix}:generated-after-compact:create`,
          thread: `${prefix}:generated-after-compact`,
        });
        yield* dispatchUserMessage({
          command: `${prefix}:generated-after-compact:compact`,
          threadId: generatedAfterCompact,
          text: "/compact",
        });
        yield* titleRegeneration.execute({
          threadId: generatedAfterCompact,
          requestId: yield* armRegeneration({
            command: `${prefix}:generated-after-compact:regenerate`,
            threadId: generatedAfterCompact,
          }),
          kind: { type: "regenerate" },
        });
        const compacted = yield* fork("compacted");
        yield* rename("compacted", compacted, "Storage migration notes");
        yield* dispatchUserMessage({
          command: `${prefix}:compacted:compact`,
          threadId: compacted,
          text: "/compact",
        });

        for (const [key, threadId, expectedTitle, generates] of [
          ["renamed", forkThreadId, "Storage migration notes", false],
          ["fork of a renamed source", forkOfRenamedSource, seed, true],
          ["renamed, then regenerated", regenerated, seed, true],
          ["renamed, then regenerated to the same name", regeneratedUnchanged, seed, true],
          ["titled by the agent that forked it", titled, "Release checklist", false],
          ["titled by a client", clientTitled, seed, true],
          ["renamed, then compacted", compacted, "Storage migration notes", false],
          ["generated after a /compact", generatedAfterCompact, seed, true],
        ] as const) {
          const requestId = CommandId.make(`${prefix}:${key}:message`);
          yield* threads.dispatch({
            type: "message.dispatch",
            commandId: requestId,
            threadId,
            messageId: MessageId.make(`${prefix}:${key}:message:message`),
            text: seed,
            titleSeed: seed,
            attachments: [],
            modelSelection,
            dispatchMode: { type: "defer_start" },
            createdBy: "user",
            creationSource: "web",
          });

          const settled = yield* threads.getThreadProjection(threadId);
          assert.equal(settled.thread.title, expectedTitle, key);
          assert.equal(
            (yield* outbox.listByCommandId(requestId)).some(
              (effect) => effect.request.type === "thread-title.generate",
            ),
            generates,
            key,
          );
        }
      }).pipe(Effect.provide(harness.layer));
    }),
  );

  it.effect(
    "titles an image-only first message without sending bootstrap text to the title model",
    () =>
      Effect.gen(function* () {
        for (const prefix of ["", "Ultrathink:\n"]) {
          const harness = makeHarness();
          yield* Effect.gen(function* () {
            const threads = yield* ThreadManagement.ThreadManagementService;
            const service = yield* ThreadTitleRegeneration.ThreadTitleRegenerationService;
            const threadId = yield* createThread({
              command: "image:create",
              thread: "image:thread",
            });
            const text = prefix + ATTACHMENT_ONLY_BOOTSTRAP_PROMPT;
            const attachments = [
              {
                type: "image" as const,
                id: ChatAttachmentId.make("diagram"),
                name: "diagram.png",
                mimeType: "image/png",
                sizeBytes: 10,
              },
            ];
            yield* dispatchUserMessage({ command: "image:message", threadId, text, attachments });
            const requestId = yield* armRegeneration({ command: "image:title", threadId });
            const messageId = MessageId.make("image:message:message");
            yield* service.execute({ threadId, requestId, kind: { type: "initial", messageId } });
            const call = harness.generateThreadTitle.mock.calls[0]?.[0];
            assert.equal(call?.message, "");
            assert.deepEqual(call?.attachments, attachments);
            const projection = yield* threads.getThreadProjection(threadId);
            assert.equal(projection.messages[0]?.text, text);
            assert.equal(projection.thread.title, "Generated title");
          }).pipe(Effect.provide(harness.layer));
        }
      }),
  );

  it.effect("arms and clears the regeneration marker through metadata commands", () =>
    Effect.gen(function* () {
      const harness = makeHarness();
      yield* Effect.gen(function* () {
        const threads = yield* ThreadManagement.ThreadManagementService;
        const outbox = yield* EffectOutbox.EffectOutboxV2;
        const threadId = yield* createThread({
          command: "command:title:arm:create",
          thread: "thread:title:arm",
        });

        const firstRequest = yield* armRegeneration({ command: "command:title:arm:1", threadId });
        const armed = yield* threads.getThreadProjection(threadId);
        assert.equal(armed.thread.titleRegeneration?.requestId, firstRequest);
        assert.deepEqual(
          (yield* outbox.listByCommandId(firstRequest)).map((effect) => effect.request),
          [{ type: "thread-title.generate", kind: { type: "regenerate" } }],
        );

        yield* threads.dispatch({
          type: "thread.metadata.update",
          commandId: CommandId.make("command:title:arm:abandon"),
          threadId,
          regenerateTitle: false,
        });
        const abandoned = yield* threads.getThreadProjection(threadId);
        assert.isNotOk(abandoned.thread.titleRegeneration);

        yield* armRegeneration({ command: "command:title:arm:2", threadId });
        yield* threads.dispatch({
          type: "thread.metadata.update",
          commandId: CommandId.make("command:title:arm:rename"),
          threadId,
          title: "Manual title",
        });
        const renamed = yield* threads.getThreadProjection(threadId);
        assert.equal(renamed.thread.title, "Manual title");
        assert.isNotOk(renamed.thread.titleRegeneration);
      }).pipe(Effect.provide(harness.layer));
    }),
  );

  it.effect("skips execution when the marker was superseded by a newer request", () =>
    Effect.gen(function* () {
      const harness = makeHarness();
      yield* Effect.gen(function* () {
        const threads = yield* ThreadManagement.ThreadManagementService;
        const titleRegeneration = yield* ThreadTitleRegeneration.ThreadTitleRegenerationService;
        const threadId = yield* createThread({
          command: "command:title:stale:create",
          thread: "thread:title:stale",
        });
        const staleRequest = yield* armRegeneration({ command: "command:title:stale:1", threadId });
        const currentRequest = yield* armRegeneration({
          command: "command:title:stale:2",
          threadId,
        });

        yield* titleRegeneration.execute({
          threadId,
          requestId: staleRequest,
          kind: { type: "regenerate" },
        });

        assert.equal(harness.generateThreadTitle.mock.calls.length, 0);
        const projection = yield* threads.getThreadProjection(threadId);
        assert.equal(projection.thread.title, "Seed title");
        assert.equal(projection.thread.titleRegeneration?.requestId, currentRequest);
      }).pipe(Effect.provide(harness.layer));
    }),
  );

  it.effect("lands the regenerated title from the conversation digest", () =>
    Effect.gen(function* () {
      const harness = makeHarness({
        generateTitle: () => Effect.succeed({ title: "Fresh title" }),
      });
      const fs = yield* FileSystem.FileSystem;
      const existingWorktree = yield* fs.makeTempDirectoryScoped({ prefix: "title-worktree-" });
      yield* Effect.gen(function* () {
        const threads = yield* ThreadManagement.ThreadManagementService;
        const titleRegeneration = yield* ThreadTitleRegeneration.ThreadTitleRegenerationService;
        const cases = [
          { name: "removed", worktreePath: "/repo-worktrees/removed-by-cleanup", cwd: "/repo" },
          { name: "existing", worktreePath: existingWorktree, cwd: existingWorktree },
        ];
        for (const [index, testCase] of cases.entries()) {
          const threadId = yield* createThread({
            command: `command:title:landing:${testCase.name}:create`,
            thread: `thread:title:landing:${testCase.name}`,
            worktreePath: testCase.worktreePath,
          });
          yield* dispatchUserMessage({
            command: `command:title:landing:${testCase.name}:message`,
            threadId,
            text: "Investigate the flaky login test",
          });
          const requestId = yield* armRegeneration({
            command: `command:title:landing:${testCase.name}:1`,
            threadId,
          });

          yield* titleRegeneration.execute({ threadId, requestId, kind: { type: "regenerate" } });

          const call = harness.generateThreadTitle.mock.calls[index]?.[0];
          assert.equal(call?.previousTitle, "Seed title");
          assert.equal(call?.cwd, testCase.cwd);
          assert.include(call?.message, "USER:\nInvestigate the flaky login test");
          const projection = yield* threads.getThreadProjection(threadId);
          assert.equal(projection.thread.title, "Fresh title");
          assert.isNotOk(projection.thread.titleRegeneration);
          assert.deepInclude(projection.thread.lastTitleRegeneration, {
            requestId,
            status: "renamed",
          });
        }
      }).pipe(Effect.provide(harness.layer));
    }).pipe(Effect.provide(NodeServices.layer)),
  );

  it.effect.each([
    { name: "its first message", followUp: true },
    { name: "no message of its own yet", followUp: false },
    { name: "a message still queued behind the fork point", followUp: false, held: true },
  ])(
    "titles a fork with $name from the source history up to the fork point",
    ({ followUp, held }) =>
      Effect.gen(function* () {
        const harness = makeHarness();
        yield* Effect.gen(function* () {
          const threads = yield* ThreadManagement.ThreadManagementService;
          const titleRegeneration = yield* ThreadTitleRegeneration.ThreadTitleRegenerationService;
          const { forkThreadId } = yield* forkSeededSource(
            "thread:title:fork",
            false,
            true,
            held ? "queued" : "cancelled",
          );
          const requestId = CommandId.make("command:title:fork:request");
          const messageId = MessageId.make("command:title:fork:request:message");
          if (followUp) {
            yield* threads.dispatch({
              type: "message.dispatch",
              commandId: requestId,
              threadId: forkThreadId,
              messageId,
              text: "Try SQLite instead",
              titleSeed: "Try SQLite instead",
              attachments: [],
              modelSelection,
              dispatchMode: { type: "defer_start" },
              createdBy: "user",
              creationSource: "web",
            });
          } else {
            yield* armRegeneration({ command: requestId, threadId: forkThreadId });
          }

          yield* titleRegeneration.execute({
            threadId: forkThreadId,
            requestId,
            kind: followUp ? { type: "initial", messageId } : { type: "regenerate" },
          });

          const call = harness.generateThreadTitle.mock.calls[0]?.[0];
          assert.equal(call?.message, followUp ? "Try SQLite instead" : "");
          assert.equal(call?.forkSource?.title, "Seed title");
          assert.include(call?.forkSource?.history, "Move session storage from Redis to Postgres");
          assert.include(call?.forkSource?.history, "Sessions now persist in the Postgres");
          assert.notInclude(call?.forkSource?.history, "Later work the fork never saw");
          assert.notInclude(call?.forkSource?.history, "Rolled back attempt");
          assert.notInclude(
            call?.forkSource?.history,
            "Queued message the fork point never reached",
          );
          const fork = yield* threads.getThreadRecords(forkThreadId, []);
          assert.equal(fork.thread.title, "Generated title");
        }).pipe(Effect.provide(harness.layer));
      }),
  );

  it.effect.each(["plain", "deleted source"] as const)(
    "refuses to title an empty thread with %s history",
    (history) =>
      Effect.gen(function* () {
        const harness = makeHarness();
        yield* Effect.gen(function* () {
          const threads = yield* ThreadManagement.ThreadManagementService;
          const titleRegeneration = yield* ThreadTitleRegeneration.ThreadTitleRegenerationService;
          const { forkThreadId } = yield* forkSeededSource("thread:title:empty");
          const { thread } = yield* threads.getThreadProjection(forkThreadId);
          const eventSink = yield* EventSink.EventSinkV2;
          yield* eventSink.write({
            events: [
              {
                id: EventId.make("empty:lineage"),
                type: "thread.metadata-updated",
                threadId: forkThreadId,
                occurredAt: yield* DateTime.now,
                payload: {
                  ...thread,
                  forkedFrom:
                    history === "plain"
                      ? null
                      : {
                          type: "run",
                          threadId: ThreadId.make("absent"),
                          runId: RunId.make("absent"),
                        },
                },
              },
            ],
          });
          const requestId = yield* armRegeneration({
            command: "empty:request",
            threadId: forkThreadId,
          });
          yield* titleRegeneration.execute({
            threadId: forkThreadId,
            requestId,
            kind: { type: "regenerate" },
          });
          assert.equal(harness.generateThreadTitle.mock.calls.length, 0);
          const fork = yield* threads.getThreadRecords(forkThreadId, []);
          assert.deepInclude(fork.thread.lastTitleRegeneration, {
            requestId,
            status: "failed",
            message: "This thread has no messages to title yet.",
          });
        }).pipe(Effect.provide(harness.layer));
      }),
  );

  it.effect.each([
    { label: "after fork", beforeFork: false },
    { label: "with completedAt before fork", beforeFork: true },
  ] as const)("keeps the fork point when the source rolls it back $label", ({ beforeFork }) =>
    Effect.gen(function* () {
      const harness = makeHarness();
      yield* Effect.gen(function* () {
        const threads = yield* ThreadManagement.ThreadManagementService;
        const titleRegeneration = yield* ThreadTitleRegeneration.ThreadTitleRegenerationService;
        const { sourceThreadId, forkRunId, forkThreadId } = yield* forkSeededSource(
          "thread:title:rollback",
          false,
          false,
        );
        yield* TestClock.adjust(1);
        const source = yield* threads.getThreadRecords(sourceThreadId, ["runs"]);
        const run = source.runs.find((run) => run.id === forkRunId);
        assert.isDefined(run);
        // The rollback also rewinds the queued run cancelled before it started.
        const cancelled = source.runs.find((candidate) => candidate.status === "cancelled");
        assert.isDefined(cancelled);
        const now = yield* DateTime.now;
        const completedAt = beforeFork ? DateTime.subtract(now, { milliseconds: 2 }) : now;
        const eventSink = yield* EventSink.EventSinkV2;
        yield* eventSink.write({
          events: [run, cancelled].map((rewound) => ({
            id: EventId.make(`source:rollback:${rewound.id}`),
            type: "run.updated" as const,
            threadId: sourceThreadId,
            runId: rewound.id,
            occurredAt: now,
            payload: { ...rewound, status: "rolled_back" as const, completedAt },
          })),
        });
        const requestId = yield* armRegeneration({
          command: "rollback:request",
          threadId: forkThreadId,
        });
        yield* titleRegeneration.execute({
          threadId: forkThreadId,
          requestId,
          kind: { type: "regenerate" },
        });
        const call = harness.generateThreadTitle.mock.calls[0]?.[0];
        assert.include(call?.forkSource?.history, "Move session storage from Redis to Postgres");
        assert.include(call?.forkSource?.history, "Rolled back attempt");
        assert.notInclude(call?.forkSource?.history, "Later work the fork never saw");
        assert.notInclude(call?.forkSource?.history, "Queued message the fork point never reached");
        const fork = yield* threads.getThreadRecords(forkThreadId, []);
        assert.equal(fork.thread.title, "Generated title");
      }).pipe(Effect.provide(harness.layer));
    }),
  );

  it.effect("does not inherit imported history when the fork point is missing", () =>
    Effect.gen(function* () {
      const harness = makeHarness();
      yield* Effect.gen(function* () {
        const threads = yield* ThreadManagement.ThreadManagementService;
        const titleRegeneration = yield* ThreadTitleRegeneration.ThreadTitleRegenerationService;
        const { sourceThreadId, forkThreadId } = yield* forkSeededSource("thread:title:invalid");
        yield* dispatchUserMessage({
          command: "imported:message",
          threadId: sourceThreadId,
          text: "Imported history",
        });
        const source = yield* threads.getThreadRecords(sourceThreadId, []);
        const fork = yield* threads.getThreadRecords(forkThreadId, []);
        const eventSink = yield* EventSink.EventSinkV2;
        yield* eventSink.write({
          events: [
            {
              id: EventId.make("source:imported"),
              type: "thread.metadata-updated",
              threadId: sourceThreadId,
              occurredAt: yield* DateTime.now,
              payload: { ...source.thread, historyOrigin: "v1_import" },
            },
            {
              id: EventId.make("fork:missing-run"),
              type: "thread.metadata-updated",
              threadId: forkThreadId,
              occurredAt: yield* DateTime.now,
              payload: {
                ...fork.thread,
                forkedFrom: {
                  type: "run",
                  threadId: sourceThreadId,
                  runId: RunId.make("absent:run"),
                },
              },
            },
          ],
        });
        const requestId = yield* armRegeneration({
          command: "invalid:request",
          threadId: forkThreadId,
        });
        yield* titleRegeneration.execute({
          threadId: forkThreadId,
          requestId,
          kind: { type: "regenerate" },
        });
        assert.equal(harness.generateThreadTitle.mock.calls.length, 0);
        const updated = yield* threads.getThreadRecords(forkThreadId, []);
        assert.deepInclude(updated.thread.lastTitleRegeneration, {
          requestId,
          status: "failed",
          message: "This thread has no messages to title yet.",
        });
      }).pipe(Effect.provide(harness.layer));
    }),
  );

  it.effect("inherits a side chat's own exchange without its parent's history", () =>
    Effect.gen(function* () {
      const harness = makeHarness();
      yield* Effect.gen(function* () {
        const threads = yield* ThreadManagement.ThreadManagementService;
        const titleRegeneration = yield* ThreadTitleRegeneration.ThreadTitleRegenerationService;
        const { forkThreadId: sideChatId } = yield* forkSeededSource(
          "thread:title:side-ancestor",
          true,
        );
        const runId = RunId.make("side:run");
        yield* recordRun({
          threadId: sideChatId,
          runId,
          ordinal: 1,
          user: "Side topic",
          assistant: "Side answer",
        });
        const threadId = ThreadId.make("side:fork");
        yield* threads.dispatch({
          type: "thread.fork",
          commandId: CommandId.make("side:fork"),
          sourceThreadId: sideChatId,
          targetThreadId: threadId,
          sourcePoint: { type: "run", runId },
          title: "Side fork",
          createdBy: "user",
          creationSource: "web",
        });
        const requestId = yield* armRegeneration({ command: "side:request", threadId });
        yield* titleRegeneration.execute({ threadId, requestId, kind: { type: "regenerate" } });
        const call = harness.generateThreadTitle.mock.calls[0]?.[0];
        assert.equal(call?.forkSource?.title, "Seed title fork");
        assert.include(call?.forkSource?.history, "Side topic");
        assert.include(call?.forkSource?.history, "Side answer");
        assert.notInclude(call?.forkSource?.history, "Move session storage");
      }).pipe(Effect.provide(harness.layer));
    }),
  );

  it.effect("inherits no history a fresh session discarded", () =>
    Effect.gen(function* () {
      const harness = makeHarness();
      yield* Effect.gen(function* () {
        const threads = yield* ThreadManagement.ThreadManagementService;
        const titleRegeneration = yield* ThreadTitleRegeneration.ThreadTitleRegenerationService;
        const { forkThreadId: freshSourceId } = yield* forkSeededSource("thread:title:fresh");
        const freshRunId = RunId.make("fresh:run");
        yield* recordRun({
          threadId: freshSourceId,
          runId: freshRunId,
          ordinal: 1,
          user: "Write the release notes",
          assistant: "Release notes drafted.",
          freshContext: true,
        });
        const fork = (name: string) =>
          Effect.gen(function* () {
            const threadId = ThreadId.make(`fresh:${name}`);
            yield* threads.dispatch({
              type: "thread.fork",
              commandId: CommandId.make(`fresh:${name}:fork`),
              sourceThreadId: freshSourceId,
              targetThreadId: threadId,
              sourcePoint: { type: "run", runId: freshRunId },
              title: "Fresh fork",
              createdBy: "user",
              creationSource: "web",
            });
            return threadId;
          });
        const regenerate = (threadId: ThreadId) =>
          Effect.gen(function* () {
            const requestId = yield* armRegeneration({
              command: `${threadId}:request`,
              threadId,
            });
            yield* titleRegeneration.execute({ threadId, requestId, kind: { type: "regenerate" } });
            return harness.generateThreadTitle.mock.calls.at(-1)?.[0];
          });

        const inherited = yield* regenerate(yield* fork("inherited"));
        assert.include(inherited?.forkSource?.history, "Write the release notes");
        assert.notInclude(inherited?.forkSource?.history, "Move session storage");

        const startedOver = yield* fork("started-over");
        yield* threads.dispatch({
          type: "message.dispatch",
          commandId: CommandId.make("fresh:started-over:message"),
          threadId: startedOver,
          messageId: MessageId.make("fresh:started-over:message"),
          text: "Plan the migration",
          attachments: [],
          modelSelection,
          freshContext: true,
          dispatchMode: { type: "defer_start" },
          createdBy: "user",
          creationSource: "web",
        });
        // Stopped before it started, the fresh send sets no boundary but still superseded the fork.
        const [freshRun] = (yield* threads.getThreadRecords(startedOver, ["runs"])).runs;
        assert.isDefined(freshRun);
        const eventSink = yield* EventSink.EventSinkV2;
        const now = yield* DateTime.now;
        yield* eventSink.write({
          events: [
            {
              id: EventId.make("fresh:started-over:stopped"),
              type: "run.updated",
              threadId: startedOver,
              runId: freshRun.id,
              occurredAt: now,
              payload: { ...freshRun, status: "cancelled", startedAt: null, completedAt: now },
            },
          ],
        });
        const own = yield* regenerate(startedOver);
        assert.include(own?.message, "Plan the migration");
        assert.isUndefined(own?.forkSource);

        const ranFresh = yield* fork("ran-fresh");
        yield* recordRun({
          threadId: ranFresh,
          runId: RunId.make("fresh:ran-fresh:run"),
          ordinal: 1,
          user: "Audit the logs",
          assistant: "Logs audited.",
          freshContext: true,
        });
        assert.isUndefined((yield* regenerate(ranFresh))?.forkSource);
      }).pipe(Effect.provide(harness.layer));
    }),
  );

  it.effect("clears the marker and keeps the title when generation fails", () =>
    Effect.gen(function* () {
      const harness = makeHarness({
        generateTitle: () =>
          Effect.fail(
            new TextGenerationError({
              operation: "generateThreadTitle",
              detail:
                "Codex CLI command failed: OpenAI Codex\nuser\nConversation text\nmodel unavailable: token=abc123\n",
            }),
          ),
      });
      yield* Effect.gen(function* () {
        const threads = yield* ThreadManagement.ThreadManagementService;
        const titleRegeneration = yield* ThreadTitleRegeneration.ThreadTitleRegenerationService;
        const threadId = yield* createThread({
          command: "command:title:failure:create",
          thread: "thread:title:failure",
        });
        yield* dispatchUserMessage({
          command: "command:title:failure:message",
          threadId,
          text: "Some conversation",
        });
        const requestId = yield* armRegeneration({ command: "command:title:failure:1", threadId });

        yield* titleRegeneration.execute({ threadId, requestId, kind: { type: "regenerate" } });

        assert.equal(harness.generateThreadTitle.mock.calls.length, 1);
        const projection = yield* threads.getThreadProjection(threadId);
        assert.equal(projection.thread.title, "Seed title");
        assert.isNotOk(projection.thread.titleRegeneration);
        assert.deepInclude(projection.thread.lastTitleRegeneration, {
          requestId,
          status: "failed",
          message: "model unavailable: token=[REDACTED]",
        });
      }).pipe(Effect.provide(harness.layer));
    }),
  );

  it.effect("does not retry an initial title after the provider refuses with 403", () =>
    Effect.gen(function* () {
      const harness = makeHarness({
        generateTitle: () =>
          Effect.fail(
            new TextGenerationError({
              operation: "generateThreadTitle",
              detail:
                "Codex CLI command failed: OpenAI Codex\nERROR: Reconnecting... 5/5\nERROR: unexpected status 403 Forbidden: model: gpt-6-luna\n",
            }),
          ),
      });
      yield* Effect.gen(function* () {
        const service = yield* ThreadTitleRegeneration.ThreadTitleRegenerationService;
        const threadId = yield* createThread({
          command: "command:title:forbidden:create",
          thread: "thread:title:forbidden",
        });
        yield* dispatchUserMessage({
          command: "command:title:forbidden:message",
          threadId,
          text: "Fix the flaky login test",
        });
        const requestId = yield* armRegeneration({ command: "command:title:forbidden", threadId });
        const messageId = MessageId.make("command:title:forbidden:message:message");

        const fiber = yield* Effect.forkChild(
          service.execute({ threadId, requestId, kind: { type: "initial", messageId } }),
        );
        yield* TestClock.adjust("1 minute");
        yield* Fiber.join(fiber);

        assert.equal(harness.generateThreadTitle.mock.calls.length, 1);
      }).pipe(Effect.provide(harness.layer));
    }),
  );

  it.effect("retries an initial title after a transient failure", () =>
    Effect.gen(function* () {
      const harness = makeHarness({
        generateTitle: () =>
          Effect.fail(
            new TextGenerationError({
              operation: "generateThreadTitle",
              detail: "Codex CLI request timed out.",
            }),
          ),
      });
      yield* Effect.gen(function* () {
        const service = yield* ThreadTitleRegeneration.ThreadTitleRegenerationService;
        const threadId = yield* createThread({
          command: "command:title:timeout:create",
          thread: "thread:title:timeout",
        });
        yield* dispatchUserMessage({
          command: "command:title:timeout:message",
          threadId,
          text: "Fix the flaky login test",
        });
        const requestId = yield* armRegeneration({ command: "command:title:timeout", threadId });
        const messageId = MessageId.make("command:title:timeout:message:message");

        const fiber = yield* Effect.forkChild(
          service.execute({ threadId, requestId, kind: { type: "initial", messageId } }),
        );
        yield* TestClock.adjust("1 minute");
        yield* Fiber.join(fiber);

        assert.equal(harness.generateThreadTitle.mock.calls.length, 3);
      }).pipe(Effect.provide(harness.layer));
    }),
  );

  it.effect("cuts a long failure message without splitting an emoji", () =>
    Effect.gen(function* () {
      const harness = makeHarness({
        generateTitle: () =>
          Effect.fail(
            new TextGenerationError({
              operation: "generateThreadTitle",
              detail: `${"a".repeat(298)}🚀 and the rest of the error`,
            }),
          ),
      });
      yield* Effect.gen(function* () {
        const threads = yield* ThreadManagement.ThreadManagementService;
        const titleRegeneration = yield* ThreadTitleRegeneration.ThreadTitleRegenerationService;
        const threadId = yield* createThread({
          command: "command:title:long-failure:create",
          thread: "thread:title:long-failure",
        });
        yield* dispatchUserMessage({
          command: "command:title:long-failure:message",
          threadId,
          text: "Some conversation",
        });
        const requestId = yield* armRegeneration({
          command: "command:title:long-failure:1",
          threadId,
        });

        yield* titleRegeneration.execute({ threadId, requestId, kind: { type: "regenerate" } });

        const projection = yield* threads.getThreadProjection(threadId);
        assert.equal(projection.thread.lastTitleRegeneration?.message, `${"a".repeat(298)}…`);
      }).pipe(Effect.provide(harness.layer));
    }),
  );

  // "New thread" is the model saying it has nothing better to name the thread by.
  it.effect.each(["Seed title", "New thread"] as const)(
    "records an unchanged outcome when the model answers %s",
    (answer) =>
      Effect.gen(function* () {
        let duringGeneration: Effect.Effect<void> = Effect.void;
        const harness = makeHarness({
          generateTitle: () =>
            Effect.suspend(() => duringGeneration).pipe(Effect.as({ title: answer })),
        });
        yield* Effect.gen(function* () {
          const threads = yield* ThreadManagement.ThreadManagementService;
          const eventSink = yield* EventSink.EventSinkV2;
          const titleRegeneration = yield* ThreadTitleRegeneration.ThreadTitleRegenerationService;
          const threadId = yield* createThread({
            command: "command:title:unchanged:create",
            thread: "thread:title:unchanged",
          });
          yield* dispatchUserMessage({
            command: "command:title:unchanged:message",
            threadId,
            text: "Some conversation",
          });
          const requestId = yield* armRegeneration({
            command: "command:title:unchanged:1",
            threadId,
          });
          // A provider relabels the thread while the generator reads the old title.
          const { thread } = yield* threads.getThreadProjection(threadId);
          const now = yield* DateTime.now;
          duringGeneration = eventSink
            .write({
              events: [
                {
                  id: EventId.make("event:title:unchanged:relabel"),
                  type: "thread.metadata-updated",
                  threadId,
                  occurredAt: now,
                  payload: { ...thread, title: "Provider label" },
                },
              ],
            })
            .pipe(Effect.asVoid, Effect.orDie);

          yield* titleRegeneration.execute({ threadId, requestId, kind: { type: "regenerate" } });

          const projection = yield* threads.getThreadProjection(threadId);
          assert.deepInclude(projection.thread.lastTitleRegeneration, {
            requestId,
            status: "unchanged",
          });
          // The generator kept the title it read, so that title is the one it now owns.
          assert.equal(projection.thread.title, "Seed title");
          assert.isNotNull(projection.thread.generatedTitle);
        }).pipe(Effect.provide(harness.layer));
      }),
  );

  it.effect("refreshes a generated title from the thread until it is renamed by hand", () =>
    Effect.gen(function* () {
      const harness = makeHarness();
      harness.generateThreadTitle
        .mockReturnValueOnce(Effect.succeed({ title: "Slack Thread Check", needsRefinement: true }))
        .mockReturnValueOnce(Effect.succeed({ title: "Keycloak Upgrade Plan" }));
      yield* Effect.gen(function* () {
        const threads = yield* ThreadManagement.ThreadManagementService;
        const outbox = yield* EffectOutbox.EffectOutboxV2;
        const titleRegeneration = yield* ThreadTitleRegeneration.ThreadTitleRegenerationService;
        const threadId = yield* createThread({
          command: "command:title:refresh:create",
          thread: "thread:title:refresh",
        });
        const refresh = (command: string) =>
          threads
            .dispatch({
              type: "thread.title.refresh",
              commandId: CommandId.make(command),
              threadId,
            })
            .pipe(Effect.as(CommandId.make(command)));
        const generatedTitle = threads
          .getThreadProjection(threadId)
          .pipe(Effect.map((projection) => projection.thread.generatedTitle));

        yield* refresh("command:title:refresh:untitled");
        assert.isNotOk((yield* threads.getThreadProjection(threadId)).thread.titleRegeneration);

        const initialRequest = CommandId.make("command:title:refresh:message");
        yield* threads.dispatch({
          type: "message.dispatch",
          commandId: initialRequest,
          threadId,
          messageId: MessageId.make("message:title:refresh"),
          text: "check https://example.slack.com/archives/C1/p2",
          titleSeed: "Seed title",
          attachments: [],
          modelSelection,
          dispatchMode: { type: "defer_start" },
          createdBy: "user",
          creationSource: "web",
        });
        // A generation that never answers leaves the subject unknown, so a refresh retries it.
        assert.deepEqual(yield* generatedTitle, {
          needsRefinement: true,
          refinements: 0,
          coveredAt: null,
        });
        yield* titleRegeneration.execute({
          threadId,
          requestId: initialRequest,
          kind: { type: "initial", messageId: MessageId.make("message:title:refresh") },
        });
        assert.deepEqual(yield* generatedTitle, {
          needsRefinement: true,
          refinements: 0,
          coveredAt: null,
        });

        const refreshRequest = yield* refresh("command:title:refresh:1");
        assert.deepEqual(
          (yield* outbox.listByCommandId(refreshRequest)).map((effect) => effect.request),
          [{ type: "thread-title.generate", kind: { type: "refresh" } }],
        );
        const started = (yield* threads.getThreadProjection(threadId)).thread;
        assert.equal(started.titleRegeneration?.requestId, refreshRequest);
        assert.deepEqual(started.generatedTitle, {
          needsRefinement: true,
          refinements: 1,
          coveredAt: started.titleRegeneration?.startedAt ?? null,
        });
        yield* titleRegeneration.execute({
          threadId,
          requestId: refreshRequest,
          kind: { type: "refresh" },
        });
        assert.deepInclude(harness.generateThreadTitle.mock.calls[1]?.[0], {
          previousTitle: "Slack Thread Check",
          automatic: true,
        });
        const refreshed = (yield* threads.getThreadProjection(threadId)).thread;
        assert.equal(refreshed.title, "Keycloak Upgrade Plan");
        assert.isNotOk(refreshed.titleRegeneration);
        // Only a regeneration someone asked for reports its outcome to a client.
        assert.isNotOk(refreshed.lastTitleRegeneration);
        assert.deepEqual(refreshed.generatedTitle, {
          needsRefinement: false,
          refinements: 1,
          coveredAt: started.titleRegeneration?.startedAt ?? null,
        });

        yield* threads.dispatch({
          type: "thread.metadata.update",
          commandId: CommandId.make("command:title:refresh:rename"),
          threadId,
          title: "My own name",
        });
        yield* refresh("command:title:refresh:2");
        const renamed = (yield* threads.getThreadProjection(threadId)).thread;
        assert.equal(renamed.title, "My own name");
        assert.isNull(renamed.generatedTitle);
        assert.isNotOk(renamed.titleRegeneration);
        assert.equal(harness.generateThreadTitle.mock.calls.length, 2);
      }).pipe(Effect.provide(harness.layer));
    }),
  );
});
