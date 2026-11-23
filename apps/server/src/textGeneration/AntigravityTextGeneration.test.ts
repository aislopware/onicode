import * as NodeSqlite from "node:sqlite";

import * as NodeServices from "@effect/platform-node/NodeServices";
import { it } from "@effect/vitest";
import {
  ANTIGRAVITY_DEFAULT_MODEL,
  PROVIDER_SEND_TURN_MAX_IMAGE_BYTES,
  ProviderInstanceId,
  ProviderSetupError,
} from "@onicode/contracts";
import { symlinksSupported } from "@onicode/shared/testing/symlinks";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as Fiber from "effect/Fiber";
import * as FileSystem from "effect/FileSystem";
import * as Layer from "effect/Layer";
import * as Path from "effect/Path";
import * as Queue from "effect/Queue";
import * as Schema from "effect/Schema";
import * as Scope from "effect/Scope";
import * as Stream from "effect/Stream";
import { type AcpError, AcpRequestError, AcpTransportError } from "effect-acp/errors";
import type * as AcpSchema from "effect-acp/compat";
import { expect } from "vite-plus/test";

import type * as AcpSessionRuntime from "@onicode/provider-acp/server/AcpSessionRuntime";
import * as ProviderHost from "@onicode/provider-core/server/ProviderHost";
import * as TestProviderHost from "@onicode/provider-testing/TestProviderHost";
import { removeAntigravitySessionFiles } from "../provider/acp/AntigravitySessionFiles.ts";

import {
  type AntigravityTextGenerationOptions,
  isAntigravityTextGenerationAvailable,
  makeAntigravityTextGeneration,
} from "./AntigravityTextGeneration.ts";

const layer = TestProviderHost.layer().pipe(Layer.provideMerge(NodeServices.layer));
const USER_INPUT_STEP = 14;
const ERROR_MESSAGE_STEP = 17;
const errorStepPayload = (error: string) =>
  Buffer.concat([
    Buffer.from([0x0a, 0x24]),
    Buffer.from(`Agent execution terminated due to error.\u0012agent executor error: ${error}`),
  ]);

type TextRuntime = Effect.Success<ReturnType<AntigravityTextGenerationOptions["makeRuntime"]>>;

const requestContext = { requestId: "test-request", method: "test" };
const SESSION_ID = "047c62f6-607b-44db-bfbe-f83b67e9e8b1";
const modelSelection = {
  instanceId: ProviderInstanceId.make("antigravity-test"),
  model: "gemini-test",
};
const encodeMetadata = Schema.encodeEffect(
  Schema.fromJsonString(Schema.Struct({ cwd: Schema.String })),
);

interface PromptContext {
  readonly emit: (
    update: AcpSchema.SessionNotification["update"],
    sessionId?: string,
  ) => Effect.Effect<void, AcpError>;
  readonly appendSteps: (steps: ReadonlyArray<readonly [number, Uint8Array]>) => void;
  readonly request: Parameters<TextRuntime["prompt"]>[0];
  permission: Parameters<TextRuntime["handleRequestPermission"]>[0];
  question: Parameters<TextRuntime["handleElicitation"]>[0];
  readFile: Parameters<TextRuntime["handleReadTextFile"]>[0];
  createTerminal: Parameters<TextRuntime["handleCreateTerminal"]>[0];
  extension: Parameters<TextRuntime["handleUnknownExtRequest"]>[0];
  readonly cwd: string;
}

const makeFixture = Effect.fn("makeAntigravityTextGenerationFixture")(function* (
  options: {
    readonly outputs?: ReadonlyArray<string>;
    readonly prompt?: (context: PromptContext) => Effect.Effect<AcpSchema.PromptResponse, AcpError>;
    readonly startError?: AcpError;
    readonly rejectAdmission?: boolean;
    readonly sessionId?: string;
    readonly startupSkills?: boolean;
    readonly initialModel?: string;
    readonly defaultModel?: string;
    readonly models?: ReadonlyArray<{ readonly value: string; readonly name: string }>;
    readonly thinkingOptions?: ReadonlyArray<{ readonly value: string; readonly name: string }>;
    readonly isolated?: boolean;
    readonly rejectThinkingUpdate?: boolean;
    readonly promptCapabilities?: AcpSchema.AgentCapabilities["promptCapabilities"] | undefined;
    readonly sqliteSessionLog?: boolean;
  } = {},
) {
  const fs = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  const host = yield* ProviderHost.ProviderHost;
  const root = yield* fs.makeTempDirectoryScoped({ prefix: "oni-antigravity-text-test-" });
  const profileDirectory = path.join(root, "profile");
  const projectDirectory = path.join(root, "project");
  const conversations = path.join(profileDirectory, "antigravity-acp", "conversations");
  const nativeSessionId = options.sessionId ?? SESSION_ID;
  const sessionBase = path.join(conversations, SESSION_ID);
  const brainDirectory = path.join(profileDirectory, "antigravity-acp", "brain", SESSION_ID);
  yield* fs.makeDirectory(projectDirectory, { recursive: true });
  yield* fs.makeDirectory(conversations, { recursive: true });
  yield* fs.writeFileString(path.join(projectDirectory, "keep.txt"), "untouched");
  yield* fs.writeFileString(path.join(conversations, "user-session.db"), "keep native history");
  const enteredPrompt = yield* Deferred.make<void>();
  const state = {
    workspaces: [] as Array<string>,
    isolatedProfileDirectories: [] as Array<string>,
    closed: [] as Array<string>,
    prompts: [] as Array<Parameters<TextRuntime["prompt"]>[0]>,
    selectedModels: [] as Array<string>,
    selectedModes: [] as Array<string>,
    promptThinkingLevels: [] as Array<string>,
    thinkingUpdates: [] as Array<string>,
    nativeFilesAtClose: [] as Array<boolean>,
    cancellations: 0,
    stop: undefined as Effect.Effect<void> | undefined,
  };
  const outputs = [...(options.outputs ?? ['{"title":"Repair login"}'])];
  const incoming: Omit<PromptContext, "cwd" | "emit" | "appendSteps" | "request"> = {
    permission: () => Effect.die("No permission handler."),
    question: () => Effect.die("No question handler."),
    readFile: () => Effect.die("No file handler."),
    createTerminal: () => Effect.die("No terminal handler."),
    extension: () => Effect.die("No extension handler."),
  };
  const appendStepsTo =
    (targetSessionBase: string): PromptContext["appendSteps"] =>
    (steps) => {
      const database = new NodeSqlite.DatabaseSync(`${targetSessionBase}.db`);
      try {
        database.exec(
          "CREATE TABLE IF NOT EXISTS steps (idx INTEGER PRIMARY KEY, step_type INTEGER, step_payload BLOB)",
        );
        const insert = database.prepare(
          "INSERT INTO steps (step_type, step_payload) VALUES (?, ?)",
        );
        for (const [type, payload] of steps) insert.run(type, payload);
      } finally {
        database.close();
      }
    };

  const withProcess: AntigravityTextGenerationOptions["withProcess"] = (stop, task) =>
    Effect.gen(function* () {
      if (options.rejectAdmission) {
        return yield* new ProviderSetupError({
          instanceId: modelSelection.instanceId,
          operation: "launch",
          detail: "Sign in before starting Antigravity.",
        });
      }
      const scope = yield* Scope.Scope;
      const child = yield* Effect.forkIn(task, scope);
      state.stop = Fiber.interrupt(child).pipe(Effect.andThen(stop));
      yield* Effect.addFinalizer(() =>
        Effect.sync(() => {
          state.stop = undefined;
        }),
      );
      return yield* Fiber.await(child).pipe(
        Effect.flatten,
        Effect.ensuring(Fiber.interrupt(child)),
      );
    });

  const makeRuntimeForProfile = (
    cwd: string,
    activeProfileDirectory: string,
  ): ReturnType<AntigravityTextGenerationOptions["makeRuntime"]> =>
    Effect.gen(function* () {
      const events = yield* Queue.unbounded<AcpSessionRuntime.AcpSessionRuntimeEvent>();
      const activeSessionBase = path.join(
        activeProfileDirectory,
        "antigravity-acp",
        "conversations",
        SESSION_ID,
      );
      const activeBrainDirectory = path.join(
        activeProfileDirectory,
        "antigravity-acp",
        "brain",
        SESSION_ID,
      );
      state.workspaces.push(cwd);
      expect(yield* fs.readDirectory(cwd).pipe(Effect.orDie)).toEqual([]);
      yield* Effect.addFinalizer(() =>
        Effect.gen(function* () {
          state.nativeFilesAtClose.push(
            yield* fs.exists(`${activeSessionBase}.db`).pipe(Effect.orDie),
          );
          state.closed.push(cwd);
        }),
      );
      let sessionUpdate: Parameters<TextRuntime["handleSessionUpdate"]>[0] = () => Effect.void;
      let currentModel = options.initialModel ?? modelSelection.model;
      let thinkingLevel = "high";
      const advertisedModels = options.models ?? [
        { value: modelSelection.model, name: "Gemini test" },
        { value: "gemini-native-default", name: "Gemini native default" },
      ];
      const advertisedThinking = options.thinkingOptions ?? [
        { value: "low", name: "Low" },
        { value: "high", name: "High" },
      ];

      return {
        start: () =>
          Effect.gen(function* () {
            if (options.startError) return yield* options.startError;
            if (options.startupSkills) {
              for (const directory of [".agents", ".gemini"]) {
                yield* fs
                  .makeDirectory(path.join(cwd, directory, "skills"), { recursive: true })
                  .pipe(Effect.orDie);
              }
            }
            yield* fs
              .makeDirectory(path.dirname(activeSessionBase), { recursive: true })
              .pipe(Effect.orDie);
            yield* fs.makeDirectory(activeBrainDirectory, { recursive: true }).pipe(Effect.orDie);
            if (!options.sqliteSessionLog) {
              yield* fs
                .writeFileString(`${activeSessionBase}.db`, "helper session")
                .pipe(Effect.orDie);
              yield* fs
                .writeFileString(`${activeSessionBase}.db-wal`, "helper journal")
                .pipe(Effect.orDie);
            }
            const metadata = yield* encodeMetadata({ cwd }).pipe(Effect.orDie);
            yield* fs.writeFileString(`${activeSessionBase}.meta`, metadata).pipe(Effect.orDie);
            yield* fs
              .writeFileString(path.join(activeBrainDirectory, "output.txt"), "helper artifact")
              .pipe(Effect.orDie);
            return {
              sessionId: nativeSessionId,
              initializeResult: {
                protocolVersion: 1,
                ...(options.promptCapabilities === undefined
                  ? {}
                  : { agentCapabilities: { promptCapabilities: options.promptCapabilities } }),
              },
              sessionSetupResult: { sessionId: nativeSessionId },
              modelConfigId: "model",
            };
          }),
        setMode: (mode) =>
          Effect.sync(() => {
            state.selectedModes.push(mode);
            return {};
          }),
        getConfigOptions: Effect.sync(() => {
          const config: Array<AcpSchema.SessionConfigOption> = [
            {
              id: "model",
              name: "Model",
              category: "model",
              type: "select",
              currentValue: currentModel,
              options: advertisedModels,
            },
          ];
          if (currentModel === modelSelection.model || options.thinkingOptions !== undefined) {
            config.push({
              id: "thinking_level",
              name: "Thinking level",
              category: "thought_level",
              type: "select",
              currentValue: thinkingLevel,
              options: advertisedThinking,
            });
          }
          return config;
        }),
        getEvents: () => Stream.fromQueue(events),
        setModel: (model) =>
          Effect.sync(() => {
            state.selectedModels.push(model);
            currentModel = model;
          }),
        setConfigOption: (id, value) =>
          Effect.gen(function* () {
            if (id === "thinking_level" && typeof value === "string") {
              state.thinkingUpdates.push(value);
              if (options.rejectThinkingUpdate) {
                return yield* AcpRequestError.invalidParams(
                  "Thinking level is no longer available.",
                );
              }
              thinkingLevel = value;
            }
            return { configOptions: [] };
          }),
        prompt: (request) =>
          Effect.gen(function* () {
            state.prompts.push(request);
            state.promptThinkingLevels.push(thinkingLevel);
            yield* Deferred.succeed(enteredPrompt, undefined);
            const emit: PromptContext["emit"] = (update, sessionId = nativeSessionId) =>
              sessionUpdate({ sessionId, update });
            if (options.prompt) {
              return yield* options.prompt({
                emit,
                appendSteps: appendStepsTo(activeSessionBase),
                request,
                ...incoming,
                cwd,
              });
            }
            const output = outputs.shift() ?? '{"title":"Repair login"}';
            yield* emit({
              sessionUpdate: "agent_thought_chunk",
              content: { type: "text", text: "Choose concise text." },
            });
            yield* emit(
              {
                sessionUpdate: "agent_message_chunk",
                content: { type: "text", text: "unrelated session" },
              },
              "another-session",
            );
            for (const text of [output.slice(0, 9), output.slice(9)]) {
              yield* emit({
                sessionUpdate: "agent_message_chunk",
                content: { type: "text", text },
              });
            }
            return { stopReason: "end_turn" };
          }),
        cancel: Effect.gen(function* () {
          state.cancellations += 1;
          const acknowledge = yield* Deferred.make<void>();
          yield* Queue.offer(events, { _tag: "EventStreamBarrier", acknowledge });
          yield* Deferred.await(acknowledge);
        }),
        handleSessionUpdate: (handler) =>
          Effect.sync(() => {
            sessionUpdate = handler;
          }),
        handleRequestPermission: (handler) =>
          Effect.sync(() => {
            incoming.permission = handler;
          }),
        handleElicitation: (handler) =>
          Effect.sync(() => {
            incoming.question = handler;
          }),
        handleReadTextFile: (handler) =>
          Effect.sync(() => {
            incoming.readFile = handler;
          }),
        handleWriteTextFile: () => Effect.void,
        handleCreateTerminal: (handler) =>
          Effect.sync(() => {
            incoming.createTerminal = handler;
          }),
        handleTerminalOutput: () => Effect.void,
        handleTerminalWaitForExit: () => Effect.void,
        handleTerminalKill: () => Effect.void,
        handleTerminalRelease: () => Effect.void,
        handleUnknownExtRequest: (handler) =>
          Effect.sync(() => {
            incoming.extension = handler;
          }),
      } satisfies TextRuntime;
    });

  const makeRuntime: AntigravityTextGenerationOptions["makeRuntime"] = (cwd) =>
    makeRuntimeForProfile(cwd, profileDirectory);

  const textGeneration = yield* makeAntigravityTextGeneration({
    profileDirectory,
    ...(options.defaultModel === undefined
      ? {}
      : { defaultModel: Effect.succeed(options.defaultModel) }),
    ...(options.isolated
      ? {
          makeIsolatedRuntime: ({ cwd, profileDirectory: isolatedDir }) =>
            Effect.sync(() => {
              state.isolatedProfileDirectories.push(isolatedDir);
            }).pipe(Effect.andThen(makeRuntimeForProfile(cwd, isolatedDir))),
        }
      : {}),
    makeRuntime,
    withProcess,
  });
  const titleInput = { cwd: projectDirectory, message: "Repair Google login", modelSelection };
  const assertCleaned = Effect.gen(function* () {
    expect(state.closed).toEqual(state.workspaces);
    for (const workspace of state.workspaces) {
      expect(yield* fs.exists(workspace)).toBe(false);
    }
    expect(yield* fs.exists(`${sessionBase}.db`)).toBe(false);
    expect(yield* fs.exists(`${sessionBase}.db-wal`)).toBe(false);
    expect(yield* fs.exists(`${sessionBase}.meta`)).toBe(false);
    expect(yield* fs.exists(brainDirectory)).toBe(false);
    expect(yield* fs.readFileString(path.join(projectDirectory, "keep.txt"))).toBe("untouched");
    expect(yield* fs.readFileString(path.join(conversations, "user-session.db"))).toBe(
      "keep native history",
    );
  });
  return {
    fs,
    path,
    host,
    profileDirectory,
    projectDirectory,
    sessionBase,
    brainDirectory,
    state,
    incoming,
    enteredPrompt,
    textGeneration,
    titleInput,
    assertCleaned,
  };
});

it.layer(layer)("AntigravityTextGeneration", (it) => {
  it.effect.each([false, true])(
    "generates all helper types and cleans up with startupSkills=%s",
    (startupSkills) =>
      Effect.gen(function* () {
        const fixture = yield* makeFixture({
          startupSkills,
          outputs: [
            '{"subject":"  Repair Google login.\\nExtra line","body":"  Keep the remote callback.  ","branch":"Repair Login"}',
            '```json\n{"title":" Repair Google login\\nExtra line","body":"  ## Summary\\nSupport remote callbacks.  "}\n```',
            '{"branch":" Repair Google Login "}',
            '{"title":"  \\"Repair Google login\\"  "}',
          ],
        });
        const common = { cwd: fixture.projectDirectory, modelSelection };
        expect(
          yield* fixture.textGeneration.generateCommitMessage({
            ...common,
            branch: "feature/login",
            stagedSummary: "M login.ts",
            stagedPatch: "+handleRemoteCallback()",
            includeBranch: true,
          }),
        ).toEqual({
          subject: "Repair Google login",
          body: "Keep the remote callback.",
          branch: "feature/repair-login",
        });
        expect(
          yield* fixture.textGeneration.generatePrContent({
            ...common,
            baseBranch: "main",
            headBranch: "feature/login",
            commitSummary: "Repair login",
            diffSummary: "M login.ts",
            diffPatch: "+handleRemoteCallback()",
          }),
        ).toEqual({ title: "Repair Google login", body: "## Summary\nSupport remote callbacks." });
        expect(
          yield* fixture.textGeneration.generateBranchName({
            ...common,
            message: "Repair Google login",
          }),
        ).toEqual({ branch: "repair-google-login" });
        expect(yield* fixture.textGeneration.generateThreadTitle(fixture.titleInput)).toEqual({
          title: "Repair Google login",
        });
        expect(new Set(fixture.state.workspaces).size).toBe(4);
        expect(fixture.state.workspaces).not.toContain(fixture.projectDirectory);
        expect(fixture.state.nativeFilesAtClose).toEqual([true, true, true, true]);
        expect(fixture.state.selectedModes).toEqual(["default", "default", "default", "default"]);
        expect(fixture.state.selectedModels).toEqual(Array(4).fill(modelSelection.model));
        expect(fixture.state.prompts[0]?.prompt).toEqual([
          { type: "text", text: expect.stringContaining("+handleRemoteCallback()") },
        ]);
        yield* fixture.assertCleaned;
      }),
  );

  it.effect.each(["tool_call", "tool_call_update"] as const)(
    "aborts on %s even without a permission request",
    (sessionUpdate) =>
      Effect.gen(function* () {
        const fixture = yield* makeFixture({
          prompt: ({ emit }) =>
            emit({ sessionUpdate, toolCallId: "tool-1", title: "Read files" }).pipe(
              Effect.andThen(Effect.never),
            ),
        });
        const error = yield* fixture.textGeneration
          .generateThreadTitle(fixture.titleInput)
          .pipe(Effect.flip);
        expect(error.detail).toContain("tool work");
        expect(fixture.state.cancellations).toBe(1);
        yield* fixture.assertCleaned;
      }),
  );

  it.effect("rejects native permission and choice requests instead of answering them", () =>
    Effect.gen(function* () {
      const fixture = yield* makeFixture({ prompt: () => Effect.never });
      const child = yield* fixture.textGeneration
        .generateThreadTitle(fixture.titleInput)
        .pipe(Effect.forkChild);
      yield* Deferred.await(fixture.enteredPrompt);
      const reply = yield* fixture.incoming.permission(
        {
          sessionId: SESSION_ID,
          toolCall: { toolCallId: "question-1", title: "Choose a title", kind: "other" },
          options: [
            { optionId: "first-native-choice", name: "Use this title", kind: "allow_once" },
            { optionId: "second-native-choice", name: "Use this title", kind: "allow_once" },
          ],
        },
        requestContext,
      );
      const error = yield* Fiber.join(child).pipe(Effect.flip);
      expect(reply).toEqual({ outcome: { outcome: "cancelled" } });
      expect(error.detail).toContain("permission or user input");
      yield* fixture.assertCleaned;
    }),
  );

  it.effect.each(["file", "terminal", "extension"] as const)(
    "denies unexpected %s requests",
    (kind) =>
      Effect.gen(function* () {
        const fixture = yield* makeFixture({ prompt: () => Effect.never });
        const child = yield* fixture.textGeneration
          .generateThreadTitle(fixture.titleInput)
          .pipe(Effect.forkChild);
        yield* Deferred.await(fixture.enteredPrompt);
        const request =
          kind === "file"
            ? fixture.incoming.readFile(
                { sessionId: SESSION_ID, path: "/not-allowed" },
                requestContext,
              )
            : kind === "terminal"
              ? fixture.incoming.createTerminal(
                  { sessionId: SESSION_ID, command: "not-allowed" },
                  requestContext,
                )
              : fixture.incoming.extension("_ask_user", {}, requestContext);
        const denied = yield* request.pipe(Effect.exit);
        expect(Exit.isFailure(denied)).toBe(true);
        const error = yield* Fiber.join(child).pipe(Effect.flip);
        expect(error.detail).toContain("tool or user input");
        yield* fixture.assertCleaned;
      }),
  );

  it.effect.each([
    { output: "   ", detail: "empty" },
    { output: "No JSON here", detail: "invalid structured output" },
    { output: '{"title":42}', detail: "invalid structured output" },
    { output: "x".repeat(128_001), detail: "output limit" },
  ])("rejects $detail output and closes the runtime", ({ output, detail }) =>
    Effect.gen(function* () {
      const fixture = yield* makeFixture({ outputs: [output] });
      const error = yield* fixture.textGeneration
        .generateThreadTitle(fixture.titleInput)
        .pipe(Effect.flip);
      expect(error.detail).toContain(detail);
      yield* fixture.assertCleaned;
    }),
  );

  it.effect("does not return partial JSON after native cancellation", () =>
    Effect.gen(function* () {
      const fixture = yield* makeFixture({
        prompt: ({ emit }) =>
          emit({
            sessionUpdate: "agent_message_chunk",
            content: { type: "text", text: '{"title":"Partial title"}' },
          }).pipe(Effect.as({ stopReason: "cancelled" })),
      });
      const error = yield* fixture.textGeneration
        .generateThreadTitle(fixture.titleInput)
        .pipe(Effect.flip);
      expect(error.detail).toContain("cancelled");
      yield* fixture.assertCleaned;
    }),
  );

  it.effect.each([
    {
      name: "auth required",
      cause: new AcpRequestError({ code: -32000, errorMessage: "Raw provider failure text." }),
      detail: "Sign in to Antigravity in Settings before you continue.",
    },
    {
      name: "no browser sign-in",
      cause: new AcpRequestError({
        code: -32603,
        errorMessage: "No browser is available here to sign in.",
      }),
      detail: "Sign in to Antigravity in Settings before you continue.",
    },
    {
      name: "request error message",
      cause: new AcpRequestError({ code: -32603, errorMessage: "Upstream model overloaded." }),
      detail: "Upstream model overloaded.",
    },
    {
      name: "redacted request error message",
      cause: new AcpRequestError({
        code: -32603,
        errorMessage: "Request rejected: api_key=abc123",
      }),
      detail: "Request rejected: api_key=[REDACTED]",
    },
    {
      name: "blank request error message",
      cause: new AcpRequestError({ code: -32603, errorMessage: "   " }),
      detail: "Antigravity text generation failed.",
    },
    {
      name: "transport error",
      cause: new AcpTransportError({
        detail: "Raw transport failure.",
        cause: new Error("closed"),
      }),
      detail: "Antigravity text generation failed.",
    },
  ])("maps ACP $name from the prompt request", ({ cause, detail }) =>
    Effect.gen(function* () {
      const fixture = yield* makeFixture({
        prompt: () => Effect.fail(cause),
      });
      const error = yield* fixture.textGeneration
        .generateThreadTitle(fixture.titleInput)
        .pipe(Effect.flip);
      expect(error.detail).toBe(detail);
      yield* fixture.assertCleaned;
    }),
  );

  it.effect.each([
    {
      name: "quota error step with chat notice",
      steps: [
        [USER_INPUT_STEP, Buffer.from("title")],
        [
          ERROR_MESSAGE_STEP,
          errorStepPayload(
            "Error 429, Message: Quota exhausted for test model., Status: RESOURCE_EXHAUSTED, Details: [map[@type:type.googleapis.com/google.rpc.ErrorInfo domain:googleapis.com metadata:map[quotaResetDelay:3600s] reason:QUOTA_EXHAUSTED]]",
          ),
        ],
      ] as ReadonlyArray<readonly [number, Uint8Array]>,
      message: "Usage Limit Reached",
      stopReason: "end_turn" as const,
      detail: "You have reached your Antigravity quota for this period.",
    },
    {
      name: "rate limit 429 error step with empty output",
      steps: [
        [USER_INPUT_STEP, Buffer.from("title")],
        [
          ERROR_MESSAGE_STEP,
          errorStepPayload(
            "Error 429, Message: Resource has been exhausted (e.g. check quota)., Status: RESOURCE_EXHAUSTED, Details: []",
          ),
        ],
      ] as ReadonlyArray<readonly [number, Uint8Array]>,
      message: undefined,
      stopReason: "end_turn" as const,
      detail: "Resource has been exhausted (e.g. check quota).",
    },
    {
      name: "502 bad gateway error step",
      steps: [
        [USER_INPUT_STEP, Buffer.from("title")],
        [
          ERROR_MESSAGE_STEP,
          errorStepPayload(
            "Error 502, Message: Bad Gateway: Failed to connect to backend API, Status: 502 Bad Gateway, Details: []",
          ),
        ],
      ] as ReadonlyArray<readonly [number, Uint8Array]>,
      message: undefined,
      stopReason: "end_turn" as const,
      detail: "Bad Gateway: Failed to connect to backend API",
    },
    {
      name: "refusal stop reason",
      steps: [] as ReadonlyArray<readonly [number, Uint8Array]>,
      message: undefined,
      stopReason: "refusal" as const,
      detail: "You have reached your Antigravity quota for this period.",
    },
  ])(
    "reports Antigravity's upstream error for $name before removing session files",
    ({ steps, message, stopReason, detail }) =>
      Effect.gen(function* () {
        const fixture = yield* makeFixture({
          sqliteSessionLog: true,
          prompt: ({ emit, appendSteps }) =>
            Effect.gen(function* () {
              if (steps.length > 0) appendSteps(steps);
              if (message !== undefined) {
                yield* emit({
                  sessionUpdate: "agent_message_chunk",
                  content: { type: "text", text: message },
                });
              }
              return { stopReason };
            }),
        });
        const error = yield* fixture.textGeneration
          .generateThreadTitle(fixture.titleInput)
          .pipe(Effect.flip);
        expect(error.detail).toBe(detail);
        yield* fixture.assertCleaned;
      }),
  );

  it.effect(
    "sends supported image attachments within the request budget when image prompts are advertised",
    () =>
      Effect.gen(function* () {
        const fixture = yield* makeFixture({
          promptCapabilities: { image: true },
          outputs: ['{"branch":"fix/login-button"}'],
        });
        const imageSize = PROVIDER_SEND_TURN_MAX_IMAGE_BYTES;
        const images = [
          {
            id: "ag-text-svg",
            name: "vector.svg",
            mimeType: "image/svg+xml",
            head: "<svg/>",
            size: 6,
          },
          {
            id: "ag-text-one",
            name: "one.png",
            mimeType: "image/png",
            head: "one",
            size: imageSize,
          },
          {
            id: "ag-text-two",
            name: "two.png",
            mimeType: "image/png",
            head: "two",
            size: imageSize,
          },
          {
            id: "ag-text-six",
            name: "six.png",
            mimeType: "image/png",
            head: "six",
            size: imageSize,
          },
        ];
        const missing = {
          type: "image" as const,
          id: "ag-text-missing",
          name: "missing.png",
          mimeType: "image/png",
          sizeBytes: 5,
        };
        const document = {
          type: "file" as const,
          id: "ag-text-doc",
          name: "notes.txt",
          mimeType: "text/plain",
          sizeBytes: 5,
        };
        for (const image of images) {
          const target = fixture.host.resolveAttachmentPath({
            type: "image",
            id: image.id,
            name: image.name,
            mimeType: image.mimeType,
            sizeBytes: image.size,
          })!;
          yield* fixture.fs.writeFileString(target, image.head);
          yield* fixture.fs.truncate(target, image.size);
        }
        yield* fixture.fs.writeFileString(fixture.host.resolveAttachmentPath(document)!, "hello");

        const result = yield* fixture.textGeneration.generateBranchName({
          cwd: fixture.projectDirectory,
          message: "fix this",
          attachments: [
            document,
            missing,
            ...images.map((image) => ({
              type: "image" as const,
              id: image.id,
              name: image.name,
              mimeType: image.mimeType,
              sizeBytes: image.size,
            })),
          ],
          modelSelection,
        });
        expect(result).toEqual({ branch: "fix/login-button" });
        const promptBlocks = fixture.state.prompts[0]?.prompt ?? [];
        const sentImages = promptBlocks.filter((block) => block.type === "image");
        expect(sentImages).toHaveLength(2);
        // base64 of each image's first three bytes: "one" ("b25l"), "two" ("dHdv").
        expect(sentImages[0]).toMatchObject({ type: "image", mimeType: "image/png" });
        expect(sentImages[0]?.data.startsWith("b25l")).toBe(true);
        expect(sentImages[1]).toMatchObject({ type: "image", mimeType: "image/png" });
        expect(sentImages[1]?.data.startsWith("dHdv")).toBe(true);
        yield* fixture.assertCleaned;
      }),
  );

  it.effect.each([undefined, {}, { image: false }] as const)(
    "skips image blocks when the session does not advertise image support: %s",
    (promptCapabilities) =>
      Effect.gen(function* () {
        const fixture = yield* makeFixture({
          promptCapabilities,
          outputs: ['{"title":"Fix login button"}'],
        });
        const attachment = {
          type: "image" as const,
          id: "ag-text-no-cap",
          name: "screen.png",
          mimeType: "image/png",
          sizeBytes: 5,
        };
        yield* fixture.fs.writeFile(
          fixture.host.resolveAttachmentPath(attachment)!,
          Buffer.from("hello"),
        );
        const result = yield* fixture.textGeneration.generateThreadTitle({
          ...fixture.titleInput,
          attachments: [attachment],
        });
        expect(result).toEqual({ title: "Fix login button" });
        expect(fixture.state.prompts[0]?.prompt).toHaveLength(1);
        expect(fixture.state.prompts[0]?.prompt[0]?.type).toBe("text");
        yield* fixture.assertCleaned;
      }),
  );

  it.effect.each([
    {
      name: "unsupported via ACP request error",
      rejection: "Image inputs are not supported by this model",
      viaSessionLog: false,
      imageAttempts: 1,
    },
    {
      name: "unsupported via session log error",
      rejection: "Image inputs are not supported by this model",
      viaSessionLog: true,
      imageAttempts: 1,
    },
    {
      name: "image dimensions limit",
      rejection: "At least one of the image dimensions exceed max allowed size: 2000 pixels",
      viaSessionLog: false,
      imageAttempts: 2,
    },
    {
      name: "unsupported image format",
      rejection: "Unsupported image format: image/heic",
      viaSessionLog: false,
      imageAttempts: 2,
    },
  ])(
    "retries without images when rejected ($name) and remembers only text-only models",
    ({ rejection, viaSessionLog, imageAttempts }) =>
      Effect.gen(function* () {
        let observedImageAttempts = 0;
        const fixture = yield* makeFixture({
          promptCapabilities: { image: true },
          sqliteSessionLog: viaSessionLog,
          prompt: ({ emit, appendSteps, request }) =>
            Effect.gen(function* () {
              if (request.prompt.some((block) => block.type === "image")) {
                observedImageAttempts += 1;
                if (viaSessionLog) {
                  appendSteps([
                    [USER_INPUT_STEP, Buffer.from("title")],
                    [
                      ERROR_MESSAGE_STEP,
                      errorStepPayload(
                        `Error 400, Message: ${rejection}, Status: INVALID_ARGUMENT, Details: []`,
                      ),
                    ],
                  ]);
                  return { stopReason: "end_turn" };
                }
                return yield* AcpRequestError.invalidParams(rejection);
              }
              yield* emit({
                sessionUpdate: "agent_message_chunk",
                content: { type: "text", text: '{"title":"Fix login button"}' },
              });
              return { stopReason: "end_turn" };
            }),
        });
        const attachment = {
          type: "image" as const,
          id: "ag-text-retry-image",
          name: "screen.png",
          mimeType: "image/png",
          sizeBytes: 5,
        };
        yield* fixture.fs.writeFile(
          fixture.host.resolveAttachmentPath(attachment)!,
          Buffer.from("hello"),
        );
        const input = { ...fixture.titleInput, attachments: [attachment] };
        expect(yield* fixture.textGeneration.generateThreadTitle(input)).toEqual({
          title: "Fix login button",
        });
        expect(yield* fixture.textGeneration.generateThreadTitle(input)).toEqual({
          title: "Fix login button",
        });
        expect(observedImageAttempts).toBe(imageAttempts);
        yield* fixture.assertCleaned;
      }),
  );

  it.effect(
    "does not retry non-image errors and reports the retry failure when the text-only fallback fails",
    () =>
      Effect.gen(function* () {
        let mode: "quota" | "retry-fails" = "quota";
        let promptCount = 0;
        const fixture = yield* makeFixture({
          promptCapabilities: { image: true },
          prompt: ({ request }) => {
            promptCount += 1;
            if (mode === "quota") {
              return Effect.fail(
                new AcpRequestError({ code: -32603, errorMessage: "Quota exceeded" }),
              );
            }
            return request.prompt.some((block) => block.type === "image")
              ? Effect.fail(
                  AcpRequestError.invalidParams("Image inputs are not supported by this model"),
                )
              : Effect.fail(new AcpRequestError({ code: -32603, errorMessage: "Quota exceeded" }));
          },
        });
        const attachment = {
          type: "image" as const,
          id: "ag-text-no-retry",
          name: "screen.png",
          mimeType: "image/png",
          sizeBytes: 5,
        };
        yield* fixture.fs.writeFile(
          fixture.host.resolveAttachmentPath(attachment)!,
          Buffer.from("hello"),
        );
        const input = { ...fixture.titleInput, attachments: [attachment] };

        const quotaError = yield* fixture.textGeneration
          .generateThreadTitle(input)
          .pipe(Effect.flip);
        expect(quotaError.detail).toBe("Quota exceeded");
        expect(promptCount).toBe(1);

        mode = "retry-fails";
        promptCount = 0;
        const retryError = yield* fixture.textGeneration
          .generateThreadTitle(input)
          .pipe(Effect.flip);
        expect(retryError.detail).toBe("Quota exceeded");
        expect(promptCount).toBe(2);
        yield* fixture.assertCleaned;
      }),
  );

  it.effect.each([
    { relativePath: "unexpected.txt", kind: "file" },
    { relativePath: ".agents/skills/unexpected.txt", kind: "file" },
    { relativePath: ".gemini/unexpected", kind: "directory" },
    { relativePath: ".agents/skills", kind: "file" },
    ...(symlinksSupported ? [{ relativePath: ".gemini/skills", kind: "symlink" }] : []),
  ])(
    "rejects an unexpected $kind at $relativePath in its temporary workspace",
    ({ relativePath, kind }) =>
      Effect.gen(function* () {
        const fs = yield* FileSystem.FileSystem;
        const path = yield* Path.Path;
        const target =
          kind === "symlink"
            ? yield* fs.makeTempDirectoryScoped({ prefix: "oni-antigravity-link-test-" })
            : undefined;
        const fixture = yield* makeFixture({
          startupSkills: true,
          prompt: ({ cwd, emit }) =>
            Effect.gen(function* () {
              const entry = path.join(cwd, relativePath);
              if (yield* fs.exists(entry).pipe(Effect.orDie)) {
                yield* fs.remove(entry, { recursive: true }).pipe(Effect.orDie);
              }
              yield* (
                target !== undefined
                  ? fs.symlink(target, entry)
                  : kind === "directory"
                    ? fs.makeDirectory(entry)
                    : fs.writeFileString(entry, "unexpected")
              ).pipe(Effect.orDie);
              yield* emit({
                sessionUpdate: "agent_message_chunk",
                content: { type: "text", text: '{"title":"Ignored output"}' },
              });
              return { stopReason: "end_turn" };
            }),
        });
        const error = yield* fixture.textGeneration
          .generateThreadTitle(fixture.titleInput)
          .pipe(Effect.flip);
        expect(error.detail).toContain("wrote files");
        if (target !== undefined) expect(yield* fs.exists(target)).toBe(true);
        yield* fixture.assertCleaned;
      }),
  );

  it.effect.each([
    { name: "lowest default when unset", selected: undefined, expected: "low", updates: ["low"] },
    { name: "explicit saved high", selected: "high", expected: "high", updates: [] },
    {
      name: "after switching models",
      selected: "low",
      expected: "low",
      updates: ["low"],
      initialModel: "gemini-native-default",
    },
    { name: "when not advertised", selected: "medium", expected: "high", updates: [] },
    {
      name: "when rejected",
      selected: "low",
      expected: "high",
      updates: ["low"],
      rejectThinkingUpdate: true,
    },
  ])(
    "generates a title with the thinking level $name",
    ({ selected, expected, updates, initialModel, rejectThinkingUpdate }) =>
      Effect.gen(function* () {
        const fixture = yield* makeFixture({
          initialModel: initialModel ?? modelSelection.model,
          rejectThinkingUpdate: rejectThinkingUpdate === true,
        });
        const result = yield* fixture.textGeneration.generateThreadTitle({
          ...fixture.titleInput,
          modelSelection: {
            ...modelSelection,
            model: initialModel === undefined ? ANTIGRAVITY_DEFAULT_MODEL : modelSelection.model,
            ...(selected === undefined
              ? {}
              : { options: [{ id: "thinking_level", value: selected }] }),
          },
        });
        expect(result).toEqual({ title: "Repair login" });
        expect(fixture.state.selectedModels).toEqual(
          initialModel === undefined ? [] : [modelSelection.model],
        );
        expect(fixture.state.thinkingUpdates).toEqual(updates);
        expect(fixture.state.promptThinkingLevels).toEqual([expected]);
        yield* fixture.assertCleaned;
      }),
  );

  it.effect.each([
    {
      name: "prefers flash-low over flagship and high-thinking chat default",
      models: [
        { value: "gemini-4-neon-high", name: "Gemini 4 Neon (High)" },
        { value: "gemini-4-neon-low", name: "Gemini 4 Neon (Low)" },
        { value: "gemini-4-hydrogen-low", name: "Gemini 4 Hydrogen (Low)" },
        { value: "gemini-3.8-flash-high", name: "Gemini 3.8 Flash (High)" },
        { value: "gemini-3.8-flash-medium", name: "Gemini 3.8 Flash (Medium)" },
        { value: "gemini-3.8-flash-low", name: "Gemini 3.8 Flash (Low)" },
      ],
      initialModel: "gemini-4-neon-high",
      defaultModel: "gemini-3.8-flash-high",
      requestedModel: ANTIGRAVITY_DEFAULT_MODEL,
      expectedModel: "gemini-3.8-flash-low",
      expectedThinking: undefined as string | undefined,
    },
    {
      name: "picks unsuffixed flash and lowers thought_level to minimal",
      models: [
        { value: "gemini-4-carbon", name: "Gemini 4 Carbon" },
        { value: "gemini-3.8-flash", name: "Gemini 3.8 Flash" },
      ],
      thinkingOptions: [
        { value: "minimal", name: "Minimal" },
        { value: "low", name: "Low" },
        { value: "high", name: "High" },
      ],
      initialModel: "gemini-4-carbon",
      requestedModel: ANTIGRAVITY_DEFAULT_MODEL,
      expectedModel: "gemini-3.8-flash",
      expectedThinking: "minimal",
    },
    {
      name: "prefers hydrogen-low when no flash model is offered",
      models: [
        { value: "gemini-4-neon-high", name: "Gemini 4 Neon (High)" },
        { value: "gemini-4-neon-low", name: "Gemini 4 Neon (Low)" },
        { value: "gemini-4-carbon-high", name: "Gemini 4 Carbon (High)" },
        { value: "gemini-4-hydrogen-high", name: "Gemini 4 Hydrogen (High)" },
        { value: "gemini-4-hydrogen-low", name: "Gemini 4 Hydrogen (Low)" },
      ],
      initialModel: "gemini-4-neon-high",
      requestedModel: ANTIGRAVITY_DEFAULT_MODEL,
      expectedModel: "gemini-4-hydrogen-low",
      expectedThinking: undefined,
    },
    {
      name: "lowers to the -low sibling of the current model when only neon/carbon exist",
      models: [
        { value: "gemini-4-neon-high", name: "Gemini 4 Neon (High)" },
        { value: "gemini-4-neon-low", name: "Gemini 4 Neon (Low)" },
        { value: "gemini-4-carbon-high", name: "Gemini 4 Carbon (High)" },
      ],
      initialModel: "gemini-4-neon-high",
      requestedModel: ANTIGRAVITY_DEFAULT_MODEL,
      expectedModel: "gemini-4-neon-low",
      expectedThinking: undefined,
    },
    {
      name: "falls back to the cheapest fast model when an explicit model is unavailable",
      models: [
        { value: "gemini-4-neon-high", name: "Gemini 4 Neon (High)" },
        { value: "gemini-3.8-flash-low", name: "Gemini 3.8 Flash (Low)" },
      ],
      thinkingOptions: [
        { value: "low", name: "Low" },
        { value: "high", name: "High" },
      ],
      initialModel: "gemini-4-neon-high",
      requestedModel: "gemini-retired-model",
      savedThinking: "high",
      expectedModel: "gemini-3.8-flash-low",
      expectedThinking: "low",
    },
  ])(
    "defaults text generation to the cheapest fast model: $name",
    ({
      models,
      thinkingOptions,
      initialModel,
      defaultModel,
      requestedModel,
      savedThinking,
      expectedModel,
      expectedThinking,
    }) =>
      Effect.gen(function* () {
        const fixture = yield* makeFixture({
          models,
          ...(thinkingOptions === undefined ? {} : { thinkingOptions }),
          initialModel,
          ...(defaultModel === undefined ? {} : { defaultModel }),
        });
        expect(
          yield* fixture.textGeneration.generateThreadTitle({
            ...fixture.titleInput,
            modelSelection: {
              ...modelSelection,
              model: requestedModel,
              ...(savedThinking === undefined
                ? {}
                : { options: [{ id: "thinking_level", value: savedThinking }] }),
            },
          }),
        ).toEqual({ title: "Repair login" });
        expect(fixture.state.selectedModels).toEqual([expectedModel]);
        if (expectedThinking !== undefined) {
          expect(fixture.state.promptThinkingLevels).toEqual([expectedThinking]);
        }
        yield* fixture.assertCleaned;
      }),
  );

  it.effect.each([
    {
      name: "hooks.json",
      relativePath: "hooks.json",
      value: '{"default":{"PreInvocation":[{"command":"do-not-run"}]}}',
      available: false,
    },
    {
      name: "mcp_config.json",
      relativePath: "mcp_config.json",
      value: '{"mcpServers":{"server":{"command":"do-not-run"}}}',
      available: false,
    },
    {
      name: "invalid hooks.json",
      relativePath: "hooks.json",
      value: "invalid JSON",
      available: false,
    },
    {
      name: "oversized mcp_config.json",
      relativePath: "mcp_config.json",
      value: "x".repeat(64_001),
      available: false,
    },
    { name: "empty hooks.json", relativePath: "hooks.json", value: "{}", available: true },
    {
      name: "empty hooks wrapper",
      relativePath: "hooks.json",
      value: '{"hooks":{}}',
      available: true,
    },
    {
      name: "empty mcpServers wrapper",
      relativePath: "mcp_config.json",
      value: '{"mcpServers":{}}',
      available: true,
    },
    { name: "0-byte mcp_config.json", relativePath: "mcp_config.json", value: "", available: true },
    {
      name: "plugin hooks.json",
      relativePath: "plugins/helper/hooks.json",
      value: '{"PreToolUse":[{"command":"echo hi"}]}',
      available: false,
    },
    {
      name: "plugin mcp_config.json",
      relativePath: "plugins/helper/mcp_config.json",
      value: '{"mcpServers":{"helper":{"command":"node"}}}',
      available: false,
    },
    {
      name: "plugin sidecars.json",
      relativePath: "plugins/helper/sidecars.json",
      value: '{"sidecars":[{"name":"board","command":"node"}]}',
      available: false,
    },
    {
      name: "empty plugin sidecars.json",
      relativePath: "plugins/helper/sidecars.json",
      value: '{"sidecars":[]}',
      available: true,
    },
    {
      name: "disabled plugin hooks.json",
      relativePath: "plugins/disabled-plugin/hooks.json",
      value: '{"PreToolUse":[{"command":"echo hi"}]}',
      configJson: '{"plugins":{"disabled-plugin":{"enabled":false}}}',
      available: true,
    },
    {
      name: "plugin hooks.json with invalid config.json",
      relativePath: "plugins/disabled-plugin/hooks.json",
      value: '{"PreToolUse":[{"command":"echo hi"}]}',
      configJson: "not json",
      available: false,
    },
  ])(
    "checks $name before starting a helper, available=$available",
    ({ relativePath, value, configJson, available }) =>
      Effect.gen(function* () {
        const fixture = yield* makeFixture();
        const configDirectory = fixture.path.join(fixture.profileDirectory, "config");
        const target = fixture.path.join(configDirectory, relativePath);
        yield* fixture.fs.makeDirectory(fixture.path.dirname(target), { recursive: true });
        yield* fixture.fs.writeFileString(target, value);
        if (configJson !== undefined) {
          yield* fixture.fs.writeFileString(
            fixture.path.join(configDirectory, "config.json"),
            configJson,
          );
        }
        expect(yield* isAntigravityTextGenerationAvailable(fixture.profileDirectory)).toBe(
          available,
        );
        const result = yield* fixture.textGeneration
          .generateThreadTitle(fixture.titleInput)
          .pipe(Effect.exit);
        expect(Exit.isSuccess(result)).toBe(available);
        expect(fixture.state.workspaces.length).toBe(available ? 1 : 0);
        yield* fixture.assertCleaned;
      }),
  );

  it.effect(
    "stages an isolated gemini profile without hooks, MCP servers, or plugins when makeIsolatedRuntime is supplied",
    () =>
      Effect.gen(function* () {
        let observedIsolatedDir: string | undefined;
        let hasAuthToken = false;
        let hasSettings = false;
        let hasAuthCache = false;
        let hasDefaultProject = false;
        let hasHooks = true;
        let hasMcp = true;
        let hasPlugins = true;
        let hasUserSession = true;
        let current: Effect.Success<ReturnType<typeof makeFixture>> | undefined;
        const fixture = yield* makeFixture({
          isolated: true,
          sqliteSessionLog: true,
          prompt: ({ emit, appendSteps }) =>
            Effect.gen(function* () {
              const isolatedDir = current!.state.isolatedProfileDirectories.at(-1)!;
              observedIsolatedDir = isolatedDir;
              hasAuthToken = yield* current!.fs
                .exists(
                  current!.path.join(isolatedDir, "antigravity-acp", "antigravity-oauth-token"),
                )
                .pipe(Effect.orDie);
              hasSettings = yield* current!.fs
                .exists(current!.path.join(isolatedDir, "antigravity-acp", "settings.json"))
                .pipe(Effect.orDie);
              hasAuthCache = yield* current!.fs
                .exists(
                  current!.path.join(
                    isolatedDir,
                    "antigravity-acp",
                    "cache",
                    "acp-auth-state.json",
                  ),
                )
                .pipe(Effect.orDie);
              hasDefaultProject = yield* current!.fs
                .exists(
                  current!.path.join(isolatedDir, "config", "projects", "default-cli-project.json"),
                )
                .pipe(Effect.orDie);
              hasHooks = yield* current!.fs
                .exists(current!.path.join(isolatedDir, "config", "hooks.json"))
                .pipe(Effect.orDie);
              hasMcp = yield* current!.fs
                .exists(current!.path.join(isolatedDir, "config", "mcp_config.json"))
                .pipe(Effect.orDie);
              hasPlugins = yield* current!.fs
                .exists(current!.path.join(isolatedDir, "config", "plugins"))
                .pipe(Effect.orDie);
              hasUserSession = yield* current!.fs
                .exists(
                  current!.path.join(
                    isolatedDir,
                    "antigravity-acp",
                    "conversations",
                    "user-session.db",
                  ),
                )
                .pipe(Effect.orDie);
              if (current!.state.prompts.length === 1) {
                yield* emit({
                  sessionUpdate: "agent_message_chunk",
                  content: { type: "text", text: '{"title":"Isolated title"}' },
                });
                return { stopReason: "end_turn" };
              }
              appendSteps([
                [USER_INPUT_STEP, Buffer.from("title")],
                [
                  ERROR_MESSAGE_STEP,
                  errorStepPayload(
                    "Error 502, Message: Isolated backend error, Status: 502 Bad Gateway, Details: []",
                  ),
                ],
              ]);
              return { stopReason: "end_turn" };
            }),
        });
        current = fixture;

        const acpDir = fixture.path.join(fixture.profileDirectory, "antigravity-acp");
        const configDir = fixture.path.join(fixture.profileDirectory, "config");
        yield* fixture.fs.makeDirectory(fixture.path.join(acpDir, "cache"), { recursive: true });
        yield* fixture.fs.makeDirectory(fixture.path.join(configDir, "projects"), {
          recursive: true,
        });
        yield* fixture.fs.makeDirectory(fixture.path.join(configDir, "plugins", "evil"), {
          recursive: true,
        });
        yield* fixture.fs.writeFileString(
          fixture.path.join(acpDir, "settings.json"),
          '{"auth":{"type":"oauth-personal"}}',
        );
        yield* fixture.fs.writeFileString(
          fixture.path.join(acpDir, "antigravity-oauth-token"),
          '{"token":{"access_token":"secret"}}',
        );
        yield* fixture.fs.writeFileString(
          fixture.path.join(acpDir, "cache", "acp-auth-state.json"),
          '{"methodId":"oauth-personal"}',
        );
        yield* fixture.fs.writeFileString(
          fixture.path.join(configDir, "projects", "default-cli-project.json"),
          '{"id":"default-cli-project"}',
        );
        yield* fixture.fs.writeFileString(
          fixture.path.join(configDir, "hooks.json"),
          '{"PreToolUse":[{"command":"do-not-run"}]}',
        );
        yield* fixture.fs.writeFileString(
          fixture.path.join(configDir, "mcp_config.json"),
          '{"mcpServers":{"evil":{"command":"do-not-run"}}}',
        );
        yield* fixture.fs.writeFileString(
          fixture.path.join(configDir, "plugins", "evil", "hooks.json"),
          '{"PreToolUse":[{"command":"do-not-run"}]}',
        );

        expect(yield* fixture.textGeneration.generateThreadTitle(fixture.titleInput)).toEqual({
          title: "Isolated title",
        });
        expect(observedIsolatedDir).toBeDefined();
        expect(observedIsolatedDir).not.toBe(fixture.profileDirectory);
        expect(hasAuthToken).toBe(true);
        expect(hasSettings).toBe(true);
        expect(hasAuthCache).toBe(true);
        expect(hasDefaultProject).toBe(true);
        expect(hasHooks).toBe(false);
        expect(hasMcp).toBe(false);
        expect(hasPlugins).toBe(false);
        expect(hasUserSession).toBe(false);
        expect(yield* fixture.fs.exists(observedIsolatedDir!)).toBe(false);

        const upstreamError = yield* fixture.textGeneration
          .generateThreadTitle(fixture.titleInput)
          .pipe(Effect.flip);
        expect(upstreamError.detail).toBe("Isolated backend error");
        for (const isolatedDir of fixture.state.isolatedProfileDirectories) {
          expect(yield* fixture.fs.exists(isolatedDir)).toBe(false);
        }
        yield* fixture.assertCleaned;
      }),
  );

  it.effect("does not launch during sign-out or before sign-in", () =>
    Effect.gen(function* () {
      const fixture = yield* makeFixture({ rejectAdmission: true });
      const error = yield* fixture.textGeneration
        .generateThreadTitle(fixture.titleInput)
        .pipe(Effect.flip);
      expect(error._tag).toBe("TextGenerationError");
      expect(error.detail).toBe("Sign in before starting Antigravity.");
      expect(fixture.state.workspaces).toEqual([]);
      yield* fixture.assertCleaned;
    }),
  );

  it.effect.each(["caller", "sign-out"] as const)(
    "closes all helper resources when stopped by %s",
    (source) =>
      Effect.gen(function* () {
        const fixture = yield* makeFixture({ prompt: () => Effect.never });
        const child = yield* fixture.textGeneration
          .generateThreadTitle(fixture.titleInput)
          .pipe(Effect.forkChild);
        yield* Deferred.await(fixture.enteredPrompt);
        if (source === "caller") {
          yield* Fiber.interrupt(child);
        } else {
          if (!fixture.state.stop) return yield* Effect.die("No tracked helper to stop.");
          yield* fixture.state.stop;
          yield* Fiber.await(child);
        }
        expect(fixture.state.cancellations).toBe(1);
        yield* fixture.assertCleaned;
      }),
  );

  it.effect("refuses unsafe native session IDs without deleting other files", () =>
    Effect.gen(function* () {
      const fixture = yield* makeFixture({ sessionId: "../../user-session" });
      const error = yield* fixture.textGeneration
        .generateThreadTitle(fixture.titleInput)
        .pipe(Effect.flip);
      expect(error.detail).toContain("invalid text helper session ID");
      expect(fixture.state.prompts).toEqual([]);
      expect(yield* fixture.fs.readFileString(`${fixture.sessionBase}.db`)).toBe("helper session");
      expect(fixture.state.closed).toEqual(fixture.state.workspaces);
    }),
  );

  it.effect("keeps native sessions whose metadata belongs to a different workspace", () =>
    Effect.gen(function* () {
      const fixture = yield* makeFixture();
      yield* fixture.fs.writeFileString(`${fixture.sessionBase}.db`, "keep this session");
      const metadata = yield* encodeMetadata({ cwd: fixture.projectDirectory });
      yield* fixture.fs.writeFileString(`${fixture.sessionBase}.meta`, metadata);
      yield* removeAntigravitySessionFiles({
        profileDirectory: fixture.profileDirectory,
        sessionId: SESSION_ID,
        cwd: "different-temporary-workspace",
      });
      expect(yield* fixture.fs.readFileString(`${fixture.sessionBase}.db`)).toBe(
        "keep this session",
      );
      expect(yield* fixture.fs.readFileString(`${fixture.sessionBase}.meta`)).toBe(metadata);
    }),
  );
});

it.effect("retains filesystem services supplied only during construction", () =>
  Effect.gen(function* () {
    const fixture = yield* makeFixture({ startupSkills: true }).pipe(Effect.provide(layer));
    expect(yield* fixture.textGeneration.generateThreadTitle(fixture.titleInput)).toEqual({
      title: "Repair login",
    });
    yield* fixture.assertCleaned;
  }),
);
