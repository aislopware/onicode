import * as NodeCrypto from "@effect/platform-node/NodeCrypto";
import {
  EnvironmentId,
  type OrchestrationV2ThreadShell,
  ProjectId,
  RunId,
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
