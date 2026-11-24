import * as NodeCrypto from "@effect/platform-node/NodeCrypto";
import { expect, it } from "@effect/vitest";
import { EnvironmentId, type ManagedPolicySummary } from "@onicode/contracts";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Stream from "effect/Stream";

import * as ServerSettings from "../../../serverSettings.ts";
import * as Workflows from "../../../workflows/Workflows.ts";
import * as McpInvocationContext from "../../McpInvocationContext.ts";
import * as McpToolAccess from "../../McpToolAccess.ts";
import * as McpToolAccessTestkit from "../../McpToolAccess.testkit.ts";
import * as WorkflowHandlers from "./handlers.ts";
import { WorkflowToolkit } from "./tools.ts";

const handlersWith = (
  workflows: Partial<Workflows.Workflows["Service"]>,
  options: {
    readonly client?: Partial<McpInvocationContext.McpClientCaller>;
    readonly managedPolicy?: ManagedPolicySummary;
  } = {},
) =>
  McpToolAccess.HandlersLayer.layer(WorkflowHandlers.layer).pipe(
    Layer.provideMerge(
      Layer.mergeAll(
        NodeCrypto.layer,
        McpToolAccessTestkit.liveThreadsLayer,
        ServerSettings.layerTest({}, options.managedPolicy),
        Layer.succeed(McpInvocationContext.McpInvocationContext, {
          environmentId: EnvironmentId.make("environment:workflow"),
          requestNamespace: "client:workflow",
          thread: undefined,
          client: {
            sessionId: "session-1",
            label: "onicode CLI",
            access: "full-access",
            ...options.client,
          },
          capabilities: new Set(["orchestration" as const]),
          issuedAt: 0,
        }),
        Layer.mock(Workflows.Workflows)(workflows),
      ),
    ),
  );

const lastResult = (input: Record<string, unknown>) =>
  Effect.gen(function* () {
    const toolkit = yield* WorkflowToolkit;
    const parts = yield* toolkit
      .handle("oni_workflow", input as never)
      .pipe(Stream.unwrap, Stream.runCollect);
    return parts.at(-1)?.result;
  });

it.effect("start passes an explicit null payload through and defaults an omitted one to {}", () =>
  Effect.gen(function* () {
    const payloads: Array<unknown> = [];
    yield* Effect.gen(function* () {
      const toolkit = yield* WorkflowToolkit;
      for (const input of [{ input: null }, {}]) {
        yield* toolkit
          .handle("oni_workflow", {
            action: "start",
            workflow: "echo",
            idempotencyKey: "trial",
            ...input,
          })
          .pipe(Stream.unwrap, Stream.runCollect);
      }
    }).pipe(
      Effect.provide(
        handlersWith({
          start: (_workflow, input) =>
            Effect.sync(() => {
              payloads.push(input);
              return { runId: "run-1" };
            }),
          get: () => Effect.succeed({ run: undefined, steps: [], references: [] } as never),
        }),
      ),
    );
    expect(payloads).toEqual([null, {}]);
  }),
);

it.effect("signal refuses a run destination mixed with a channel destination", () =>
  Effect.gen(function* () {
    const delivered: Array<unknown> = [];
    const results = yield* Effect.gen(function* () {
      const toolkit = yield* WorkflowToolkit;
      return yield* Effect.forEach(
        [
          { runId: "run-1", wait: "review", channel: "other" },
          { workflow: "other", channel: "other", runId: "run-1" },
          { runId: "run-1", wait: "review" },
        ],
        (address) =>
          toolkit
            .handle("oni_workflow", { action: "signal", value: { choice: "yes" }, ...address })
            .pipe(
              Stream.unwrap,
              Stream.runCollect,
              Effect.map((parts) => parts.at(-1)?.result),
            ),
      );
    }).pipe(
      Effect.provide(
        handlersWith({
          signal: (address) =>
            Effect.sync(() => {
              delivered.push(address);
              return "delivered" as const;
            }),
        }),
      ),
    );
    for (const result of results.slice(0, -1)) {
      expect(result).toMatchObject({ code: "invalid_request" });
    }
    expect(results.at(-1)).toEqual({ delivery: "delivered" });
    expect(delivered).toEqual([{ runId: "run-1", wait: "review" }]);
  }),
);

// Agent steps launch at modes the workflow picks, so a narrower client must not drive them.
it.effect.each([
  { name: "a Supervised client", client: { access: "approval-required" as const } },
  {
    name: "a client narrowed to Auto",
    client: {
      access: "full-access" as const,
      narrowedTo: { runtimeMode: "auto" as const, interactionMode: "default" as const },
    },
  },
  {
    name: "a client narrowed to plan",
    client: {
      access: "full-access" as const,
      narrowedTo: { runtimeMode: "full-access" as const, interactionMode: "plan" as const },
    },
  },
])(
  "refuses $name the actions that drive runs, change their store or hand out hook URLs",
  ({ client }) =>
    Effect.gen(function* () {
      const called: Array<string> = [];
      const record = (name: string) => () =>
        Effect.sync(() => {
          called.push(name);
          return name as never;
        });
      const results = yield* Effect.forEach(
        [
          { action: "start", workflow: "echo", idempotencyKey: "trial" },
          { action: "signal", runId: "run-1", wait: "review", value: true },
          { action: "resume", runId: "run-1" },
          { action: "cancel", runId: "run-1" },
          { action: "store_set", key: "seen", value: 1 },
          { action: "store_delete", key: "seen" },
          { action: "hooks" },
        ],
        lastResult,
      ).pipe(
        Effect.provide(
          handlersWith(
            {
              start: record("start"),
              signal: record("signal"),
              resume: record("resume"),
              cancel: record("cancel"),
              storeSet: record("storeSet"),
              storeDelete: record("storeDelete"),
              hooks: Effect.sync(() => {
                called.push("hooks");
                return [];
              }),
            },
            { client },
          ),
        ),
      );
      for (const result of results) expect(result).toMatchObject({ code: "capability_denied" });
      expect(called).toEqual([]);
    }),
);

it.effect("withholds hook URLs when the policy turns webhook tasks off", () =>
  Effect.gen(function* () {
    const result = yield* lastResult({ action: "hooks" }).pipe(
      Effect.provide(
        handlersWith(
          { hooks: Effect.die("hooks must not be read") },
          {
            managedPolicy: {
              organization: "Example Corp",
              lockedSettings: [],
              limits: { webhookTasks: false },
            },
          },
        ),
      ),
    );
    expect(result).toMatchObject({
      code: "capability_denied",
      message: "Example Corp's policy does not allow webhook tasks.",
    });
  }),
);
