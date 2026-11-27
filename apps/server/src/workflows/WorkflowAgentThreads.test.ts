import * as NodeCrypto from "@effect/platform-node/NodeCrypto";
import {
  EnvironmentId,
  type OrchestrationV2ThreadShell,
  type OrchestrationV2ThreadProjection,
  type ModelSelection,
  type Project,
  ProjectId,
  ProviderDriverKind,
  ProviderInstanceId,
  RunId,
  type RuntimeMode,
  type ServerProvider,
  ThreadId,
} from "@onicode/contracts";
import { assert, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";

import * as ServerEnvironment from "../environment/ServerEnvironment.ts";
import * as ThreadIssueService from "../orchestration-v2/ThreadIssueService.ts";
import * as ThreadLaunch from "../orchestration-v2/ThreadLaunchService.ts";
import * as ThreadManagement from "../orchestration-v2/ThreadManagementService.ts";
import * as PeerLinks from "../peer/PeerLinks.ts";
import * as PeerMcpClient from "../peer/PeerMcpClient.ts";
import * as ProjectService from "../project/ProjectService.ts";
import * as ProviderRegistry from "../provider/ProviderRegistry.ts";
import * as ServerSettings from "../serverSettings.ts";
import { WorkflowAgents } from "./WorkflowAgents.ts";
import { interruptFailure, layer } from "./WorkflowAgentThreads.ts";
import { WorkflowForge } from "./WorkflowForge.ts";

const threadId = ThreadId.make("thread-1");
const runId = RunId.make("turn-1");

it.each([
  [new ThreadManagement.ThreadManagementRunNotFoundError({ threadId, runId }), true],
  [new ThreadManagement.ThreadManagementThreadNotInterruptibleError({ threadId, runId }), true],
  [new Error("database is locked"), undefined],
])("counts a stop refused with %s as final: %s", (cause, final) => {
  assert.strictEqual(interruptFailure(cause).final, final);
});

it.effect("continues a thread as an agent's send, the way a linked environment does", () => {
  const sent: Array<ThreadManagement.ThreadManagementSendInput> = [];
  const dependencies = Layer.mergeAll(
    NodeCrypto.layer,
    Layer.mock(ThreadManagement.ThreadManagementService)({
      getThreadShell: () =>
        Effect.succeed({
          projectId: ProjectId.make("project-1"),
          deletedAt: null,
        } as OrchestrationV2ThreadShell),
      sendToThread: (input) =>
        Effect.sync(() => {
          sent.push(input);
          return { run: { id: runId } } as ThreadManagement.ThreadManagementSendResult;
        }),
    }),
    Layer.mock(ThreadLaunch.ThreadLaunchService)({}),
    Layer.mock(ProjectService.ProjectService)({}),
    Layer.mock(ProviderRegistry.ProviderRegistry)({}),
    Layer.mock(ServerSettings.ServerSettingsService)({}),
    Layer.mock(ThreadIssueService.ThreadIssueService)({}),
    Layer.mock(PeerMcpClient.PeerMcpClient)({}),
    Layer.mock(PeerLinks.PeerLinks)({}),
    Layer.mock(ServerEnvironment.ServerEnvironment)({
      getEnvironmentId: Effect.succeed(EnvironmentId.make("environment-1")),
    }),
    // Only a step with an issue reads the forge.
    Layer.succeed(WorkflowForge, {} as WorkflowForge["Service"]),
  );
  return Effect.gen(function* () {
    const agents = yield* WorkflowAgents;
    const turn = yield* agents.start({
      key: "run-1/write/0",
      origin: { runId: "run-1", workflow: "fix", step: "write" },
      title: "fix: write",
      prompt: "Write the fix.",
      outputSchema: undefined,
      model: undefined,
      project: undefined,
      environment: undefined,
      thread: threadId,
    });
    assert.deepEqual(turn, { threadId, turnId: runId });
    assert.deepEqual(
      sent.map((input) => [input.createdBy, input.creationSource]),
      [["agent", "mcp"]],
    );
  }).pipe(Effect.provide(layer.pipe(Layer.provide(dependencies))));
});

it.effect("under an administrator's ceiling, a step does not take an agent's own mode", () => {
  const projectId = ProjectId.make("project-1");
  const agent = ProviderInstanceId.make("acp-agent");
  const yolo = { id: "_oni/session-mode", value: "yolo" };
  const provider = {
    instanceId: agent,
    driver: ProviderDriverKind.make("acpRegistry"),
    enabled: true,
    installed: true,
    version: "test",
    status: "ready",
    auth: { status: "authenticated" },
    checkedAt: "2026-11-27T00:00:00.000Z",
    slashCommands: [],
    skills: [],
    models: [{ slug: "default", name: "Default", isCustom: false, capabilities: null }],
  } as unknown as ServerProvider;
  const startUnder = (ceiling: RuntimeMode, model?: { options: Array<typeof yolo> }) => {
    const launched: Array<ModelSelection> = [];
    const dependencies = Layer.mergeAll(
      NodeCrypto.layer,
      Layer.mock(ThreadManagement.ThreadManagementService)({}),
      Layer.mock(ThreadLaunch.ThreadLaunchService)({
        launch: (input) =>
          Effect.sync(() => {
            launched.push(input.modelSelection);
            return {
              threadId: input.threadId,
              projection: {
                runs: [{ id: runId, userMessageId: input.initialMessage?.messageId }],
              } as unknown as OrchestrationV2ThreadProjection,
            } as ThreadLaunch.ThreadLaunchResult;
          }),
      }),
      Layer.mock(ProjectService.ProjectService)({
        getById: () => Effect.succeedSome({ id: projectId } as Project),
      }),
      Layer.mock(ProviderRegistry.ProviderRegistry)({ getProviders: Effect.succeed([provider]) }),
      ServerSettings.ServerSettingsService.layerTest(
        {
          projectSettingsFolded: true,
          projectSettingsOverrides: {
            [projectId]: {
              defaultModelSelection: { instanceId: agent, model: "default", options: [yolo] },
              providerInstances: { [agent]: { enabled: true } },
            },
          },
        },
        { organization: "Acme", lockedSettings: [], limits: { maxRuntimeMode: ceiling } },
      ),
      Layer.mock(ThreadIssueService.ThreadIssueService)({}),
      Layer.mock(PeerMcpClient.PeerMcpClient)({}),
      Layer.mock(PeerLinks.PeerLinks)({}),
      Layer.mock(ServerEnvironment.ServerEnvironment)({
        getEnvironmentId: Effect.succeed(EnvironmentId.make("environment-1")),
      }),
      Layer.succeed(WorkflowForge, {} as WorkflowForge["Service"]),
    );
    return Effect.gen(function* () {
      const agents = yield* WorkflowAgents;
      const started = yield* agents
        .start({
          key: "run-1/write/0",
          origin: { runId: "run-1", workflow: "fix", step: "write" },
          title: "fix: write",
          prompt: "Write the fix.",
          outputSchema: undefined,
          model:
            model === undefined ? undefined : { instanceId: agent, model: "default", ...model },
          project: projectId,
          environment: undefined,
          thread: undefined,
        })
        .pipe(Effect.result);
      return { failed: started._tag === "Failure", options: launched.map((s) => s.options) };
    }).pipe(Effect.provide(layer.pipe(Layer.provide(dependencies))));
  };
  return Effect.gen(function* () {
    assert.deepEqual(yield* startUnder("full-access"), { failed: false, options: [[yolo]] });
    assert.deepEqual(yield* startUnder("auto"), { failed: false, options: [[]] });
    assert.deepEqual(yield* startUnder("auto", { options: [yolo] }), {
      failed: true,
      options: [],
    });
  });
});
