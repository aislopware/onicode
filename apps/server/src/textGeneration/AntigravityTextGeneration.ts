import {
  ANTIGRAVITY_DEFAULT_MODEL,
  PROVIDER_SEND_TURN_MAX_IMAGE_BYTES,
  ProviderSetupError,
  TextGenerationError,
} from "@onicode/contracts";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as FileSystem from "effect/FileSystem";
import * as Option from "effect/Option";
import * as Path from "effect/Path";
import * as Ref from "effect/Ref";
import * as Result from "effect/Result";
import * as Schema from "effect/Schema";
import * as Scope from "effect/Scope";
import * as Stream from "effect/Stream";
import { type AcpError, AcpRequestError } from "effect-acp/errors";
import type * as EffectAcpSchema from "effect-acp/compat";

import {
  antigravityModelOptions,
  antigravityThinkingConfig,
  applyAntigravityAcpModelSelection,
  buildAntigravityPrompt,
} from "../provider/acp/AntigravityAcpSupport.ts";
import { removeAntigravitySessionFiles } from "../provider/acp/AntigravitySessionFiles.ts";
import { watchAntigravityPromptOutcome } from "../provider/acp/AntigravityTurnOutcome.ts";
import {
  ANTIGRAVITY_SIGN_IN_REQUIRED_MESSAGE,
  isAntigravitySignInRequiredError,
  stageAntigravityIsolatedProfileDirectory,
} from "../provider/antigravityAuthSupport.ts";
import type * as AcpSessionRuntime from "@onicode/provider-acp/server/AcpSessionRuntime";
import { collectSessionConfigOptionValues } from "@onicode/provider-acp/server/runtimeModel";
import {
  boundedRedactedText,
  MAX_PROVIDER_FAILURE_MESSAGE_LENGTH,
} from "@onicode/provider-core/server/failure";
import * as ProviderHost from "@onicode/provider-core/server/ProviderHost";
import * as TextGenerationOperations from "@onicode/provider-core/server/textGenerationOperations";

const ANTIGRAVITY_TIMEOUT_MS = 180_000;
const MAX_OUTPUT_CHARS = 128_000;
const ANTIGRAVITY_TEXT_ATTACHMENT_MAX_BYTES = 28 * 1024 * 1024;
const IMAGE_BLOCK_FRAMING_BYTES = 128;
const antigravityImageBlockBytes = (size: number) =>
  4 * Math.ceil(size / 3) + IMAGE_BLOCK_FRAMING_BYTES;
// Quota, auth and other failures would fail again without the images, so only this one retries.
const IMAGE_REJECTION = /\bimages?\b/i;
// Only a rejection of images as such marks a model text-only, not a size, count or format limit.
const IMAGES_UNSUPPORTED =
  /image[^\n]*(?:not support|unsupported)|(?:not support|unsupported)[^\n]*image/i;
const IMAGE_FORMAT_REJECTION = /format|media.?type|mime/i;
const isTextGenerationError = Schema.is(TextGenerationError);
const isProviderSetupError = Schema.is(ProviderSetupError);
const isAcpRequestError = Schema.is(AcpRequestError);
const isNativeSessionId = Schema.is(Schema.String.check(Schema.isUUID(4)));
const decodeJsonValue = Schema.decodeEffect(Schema.fromJsonString(Schema.Unknown));
const PluginPreferences = Schema.Struct({
  plugins: Schema.optionalKey(
    Schema.Record(
      Schema.String,
      Schema.Struct({
        enabled: Schema.optionalKey(Schema.Boolean),
      }),
    ),
  ),
});
const decodePluginPreferences = Schema.decodeUnknownOption(
  Schema.fromJsonString(PluginPreferences),
);
const LOWEST_THINKING_LEVELS = ["none", "off", "minimal", "low", "medium"] as const;
const THINKING_SUFFIX_PATTERN = /-(?:none|off|minimal|low|medium|high|xhigh|max)$/i;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function lowestThinkingLevel(values: ReadonlyArray<string>): string | undefined {
  for (const candidate of LOWEST_THINKING_LEVELS) {
    const match = values.find((value) => value.toLowerCase() === candidate);
    if (match !== undefined) return match;
  }
  return undefined;
}

function thinkingSuffixRank(modelId: string): number {
  const lower = modelId.toLowerCase();
  if (lower.endsWith("-none") || lower.endsWith("-off") || lower.endsWith("-minimal")) return 0;
  if (lower.endsWith("-low")) return 1;
  if (!THINKING_SUFFIX_PATTERN.test(lower)) return 2;
  if (lower.endsWith("-medium")) return 3;
  return 4;
}

function lowestThinkingVariant(
  options: ReadonlyArray<string>,
  modelId: string | undefined,
): string | undefined {
  if (!modelId) return undefined;
  const family = modelId.replace(THINKING_SUFFIX_PATTERN, "");
  for (const suffix of ["none", "off", "minimal", "low"] as const) {
    const candidate = `${family}-${suffix}`;
    const match = options.find((value) => value.toLowerCase() === candidate.toLowerCase());
    if (match !== undefined) return match;
  }
  return undefined;
}

function resolveAntigravityTextDefaultModel(
  configOptions: ReadonlyArray<EffectAcpSchema.SessionConfigOption>,
  configuredDefaultModel: string | undefined,
): string | undefined {
  const modelConfig = configOptions.find((option) => option.id === "model");
  const current = modelConfig?.type === "select" ? modelConfig.currentValue : undefined;
  const values = antigravityModelOptions(configOptions).map((option) => option.value);
  if (values.length === 0) return configuredDefaultModel;

  const pickCheapest = (candidates: ReadonlyArray<string>) => {
    let best: string | undefined;
    let bestRank = Number.POSITIVE_INFINITY;
    for (const candidate of candidates) {
      const rank = thinkingSuffixRank(candidate);
      if (rank < bestRank) {
        best = candidate;
        bestRank = rank;
      }
    }
    return best;
  };

  // Prefer Flash (fast/cheap tier), then Hydrogen, then a lower-thinking variant of the default.
  const flashLite = pickCheapest(
    values.filter((value) => /^gemini-.*flash-lite(?:$|-)/i.test(value)),
  );
  if (flashLite !== undefined) return flashLite;
  const flash = pickCheapest(values.filter((value) => /^gemini-.*flash(?:$|-)/i.test(value)));
  if (flash !== undefined) return flash;
  const hydrogen = pickCheapest(values.filter((value) => /^gemini-.*hydrogen(?:$|-)/i.test(value)));
  if (hydrogen !== undefined) return hydrogen;

  return (
    lowestThinkingVariant(values, configuredDefaultModel) ??
    lowestThinkingVariant(values, current) ??
    configuredDefaultModel
  );
}

function acpRequestErrorDetail(cause: unknown): string | undefined {
  if (!isAcpRequestError(cause)) return undefined;
  const detail = boundedRedactedText(cause.errorMessage, MAX_PROVIDER_FAILURE_MESSAGE_LENGTH);
  return detail.length > 0 ? detail : undefined;
}

type AntigravityTextRuntime = Pick<
  AcpSessionRuntime.AcpSessionRuntime["Service"],
  | "start"
  | "setMode"
  | "getConfigOptions"
  | "getEvents"
  | "setModel"
  | "setConfigOption"
  | "prompt"
  | "cancel"
  | "handleSessionUpdate"
  | "handleRequestPermission"
  | "handleElicitation"
  | "handleReadTextFile"
  | "handleWriteTextFile"
  | "handleCreateTerminal"
  | "handleTerminalOutput"
  | "handleTerminalWaitForExit"
  | "handleTerminalKill"
  | "handleTerminalRelease"
  | "handleUnknownExtRequest"
>;

export interface AntigravityTextGenerationOptions {
  readonly profileDirectory: string;
  readonly defaultModel?: Effect.Effect<string | undefined>;
  /**
   * Optional isolated `--gemini-dir` launcher. When supplied, text generation stages a temporary
   * profile directory containing only auth/settings files (omitting user/plugin hooks, MCP servers,
   * and sidecars) and bypasses `isAntigravityTextGenerationAvailable`.
   */
  readonly makeIsolatedRuntime?: (input: {
    readonly cwd: string;
    readonly profileDirectory: string;
  }) => Effect.Effect<AntigravityTextRuntime, AcpError | ProviderSetupError, Scope.Scope>;
  readonly makeRuntime: (
    cwd: string,
  ) => Effect.Effect<AntigravityTextRuntime, AcpError | ProviderSetupError, Scope.Scope>;
  readonly withProcess: <A, E, R>(
    stop: Effect.Effect<void>,
    task: Effect.Effect<A, E, R>,
  ) => Effect.Effect<A, E | ProviderSetupError, R | Scope.Scope>;
}

const isEmptyConfigurationFile = Effect.fn("AntigravityTextGeneration.isEmptyConfigurationFile")(
  function* (configurationPath: string, wrapperKey: string) {
    const fs = yield* FileSystem.FileSystem;
    if (!(yield* fs.exists(configurationPath))) {
      return true;
    }
    const info = yield* fs.stat(configurationPath).pipe(Effect.option);
    if (Option.isNone(info) || info.value.type !== "File" || info.value.size > 64_000n) {
      return false;
    }
    // CLI 1.5.8 writes a 0-byte mcp_config.json before any server exists.
    return yield* fs.readFileString(configurationPath).pipe(
      Effect.flatMap((text) =>
        text.trim().length === 0
          ? Effect.succeed(true)
          : decodeJsonValue(text).pipe(
              Effect.map((decoded) => {
                if (Array.isArray(decoded)) return decoded.length === 0;
                if (!isRecord(decoded)) return false;
                const target = decoded[wrapperKey] ?? decoded;
                if (Array.isArray(target)) return target.length === 0;
                return isRecord(target) && Object.keys(target).length === 0;
              }),
            ),
      ),
      Effect.orElseSucceed(() => false),
    );
  },
);

/** Global and plugin hooks, MCP servers, and sidecars can run before a helper can deny a tool request. */
export const isAntigravityTextGenerationAvailable = Effect.fn(
  "isAntigravityTextGenerationAvailable",
)(function* (profileDirectory: string) {
  const fs = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  const configDirectory = path.join(profileDirectory, "config");
  for (const [name, wrapperKey] of [
    ["hooks.json", "hooks"],
    ["mcp_config.json", "mcpServers"],
  ] as const) {
    if (!(yield* isEmptyConfigurationFile(path.join(configDirectory, name), wrapperKey))) {
      return false;
    }
  }

  const pluginsDirectory = path.join(configDirectory, "plugins");
  if (!(yield* fs.exists(pluginsDirectory))) {
    return true;
  }
  const pluginsInfo = yield* fs.stat(pluginsDirectory).pipe(Effect.option);
  if (Option.isNone(pluginsInfo) || pluginsInfo.value.type !== "Directory") {
    return false;
  }
  const configPath = path.join(configDirectory, "config.json");
  const disabledPlugins = yield* fs.readFileString(configPath).pipe(
    Effect.map((contents) => {
      const parsed = decodePluginPreferences(contents);
      if (Option.isNone(parsed)) return new Set<string>();
      return new Set(
        Object.entries(parsed.value.plugins ?? {}).flatMap(([name, plugin]) =>
          plugin.enabled === false ? [name] : [],
        ),
      );
    }),
    Effect.orElseSucceed(() => new Set<string>()),
  );
  const plugins = yield* fs.readDirectory(pluginsDirectory).pipe(Effect.option);
  if (Option.isNone(plugins)) {
    return false;
  }
  for (const plugin of plugins.value) {
    if (disabledPlugins.has(plugin)) continue;
    const pluginDirectory = path.join(pluginsDirectory, plugin);
    const pluginInfo = yield* fs.stat(pluginDirectory).pipe(Effect.option);
    if (Option.isNone(pluginInfo) || pluginInfo.value.type !== "Directory") continue;
    for (const [name, wrapperKey] of [
      ["hooks.json", "hooks"],
      ["mcp_config.json", "mcpServers"],
      ["sidecars.json", "sidecars"],
    ] as const) {
      if (!(yield* isEmptyConfigurationFile(path.join(pluginDirectory, name), wrapperKey))) {
        return false;
      }
    }
  }
  return true;
});

export const makeAntigravityTextGeneration = Effect.fn("makeAntigravityTextGeneration")(function* (
  options: AntigravityTextGenerationOptions,
) {
  const fs = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  const host = yield* ProviderHost.ProviderHost;
  // Models that rejected images as unsupported and then succeeded without them.
  const textOnlyModels = new Set<string>();
  const hasOnlyRuntimeDirectories = Effect.fn(
    "AntigravityTextGeneration.hasOnlyRuntimeDirectories",
  )(function* (cwd: string) {
    const root = yield* fs.realPath(cwd);
    // ACP creates these empty skill directories before the first prompt.
    const allowed = new Set([
      path.join(root, ".agents"),
      path.join(root, ".agents", "skills"),
      path.join(root, ".gemini"),
      path.join(root, ".gemini", "skills"),
    ]);
    const directories = [root];
    for (const directory of directories) {
      for (const name of yield* fs.readDirectory(directory)) {
        const entry = path.join(directory, name);
        if (
          !allowed.has(entry) ||
          (yield* fs.realPath(entry)) !== entry ||
          (yield* fs.stat(entry)).type !== "Directory"
        ) {
          return false;
        }
        directories.push(entry);
      }
    }
    return true;
  });

  // Images Antigravity cannot take or that overflow the request budget stay as metadata lines.
  const readImageBlocks = Effect.fn("AntigravityTextGeneration.readImageBlocks")(function* (
    attachments: TextGenerationOperations.Request<Schema.Top>["attachments"],
    promptText: string,
  ) {
    const blocks: Array<EffectAcpSchema.ContentBlock> = [];
    let remainingBytes = ANTIGRAVITY_TEXT_ATTACHMENT_MAX_BYTES - Buffer.byteLength(promptText);
    for (const attachment of attachments ?? []) {
      if (attachment.type !== "image") continue;
      const attachmentPath = host.resolveAttachmentPath(attachment);
      if (!attachmentPath) continue;
      // Stat first so an image that cannot fit is never read.
      const info = yield* fs.stat(attachmentPath).pipe(Effect.option);
      if (Option.isNone(info) || info.value.type !== "File") continue;
      const size = Number(info.value.size);
      if (
        size > PROVIDER_SEND_TURN_MAX_IMAGE_BYTES ||
        antigravityImageBlockBytes(size) > remainingBytes
      ) {
        continue;
      }
      const built = yield* buildAntigravityPrompt({
        input: undefined,
        attachments: [attachment],
        resolveAttachmentPath: () => attachmentPath,
        promptCapabilities: { image: true },
      }).pipe(
        Effect.provideService(FileSystem.FileSystem, fs),
        Effect.provideService(Path.Path, path),
        Effect.orElseSucceed((): ReadonlyArray<EffectAcpSchema.ContentBlock> => []),
      );
      const imageBlock = built.find((block) => block.type === "image");
      if (imageBlock?.type !== "image") continue;
      const blockBytes = Buffer.byteLength(imageBlock.data) + IMAGE_BLOCK_FRAMING_BYTES;
      if (blockBytes > remainingBytes) continue;
      remainingBytes -= blockBytes;
      blocks.push(imageBlock);
    }
    return blocks;
  });

  const available = isAntigravityTextGenerationAvailable(options.profileDirectory).pipe(
    Effect.provideService(FileSystem.FileSystem, fs),
    Effect.provideService(Path.Path, path),
  );

  const runAntigravityJson = Effect.fn("AntigravityTextGeneration.runJson")(
    function* <S extends Schema.Top>(input: TextGenerationOperations.Request<S>) {
      const { operation } = input;
      const scope = yield* Scope.make();
      yield* Effect.addFinalizer((exit) => Scope.close(scope, exit));
      const runAttempt = Effect.fn("AntigravityTextGeneration.runAttempt")(function* (
        withImages: boolean,
      ) {
        const cwd = yield* fs.makeTempDirectoryScoped({ prefix: "oni-antigravity-text-" });
        const isolatedProfileDirectory = options.makeIsolatedRuntime
          ? yield* stageAntigravityIsolatedProfileDirectory({
              profileDirectory: options.profileDirectory,
              prefix: "oni-antigravity-text-profile-",
            }).pipe(
              Effect.provideService(FileSystem.FileSystem, fs),
              Effect.provideService(Path.Path, path),
            )
          : undefined;
        const runtimeProfileDirectory = isolatedProfileDirectory ?? options.profileDirectory;
        let sessionId: string | undefined;
        if (isolatedProfileDirectory === undefined) {
          yield* Effect.addFinalizer(() =>
            removeAntigravitySessionFiles({
              profileDirectory: options.profileDirectory,
              sessionId,
              cwd,
            }).pipe(
              Effect.provideService(FileSystem.FileSystem, fs),
              Effect.provideService(Path.Path, path),
            ),
          );
        }

        const attempt = yield* Effect.gen(function* () {
          const runtime = options.makeIsolatedRuntime
            ? yield* options.makeIsolatedRuntime({
                cwd,
                profileDirectory: runtimeProfileDirectory,
              })
            : yield* options.makeRuntime(cwd);
          yield* runtime.getEvents().pipe(
            Stream.runForEach((event) =>
              event._tag === "EventStreamBarrier"
                ? Deferred.succeed(event.acknowledge, undefined).pipe(Effect.asVoid)
                : Effect.void,
            ),
            Effect.forkScoped,
          );
          const output = yield* Ref.make("");
          const rejected = yield* Deferred.make<never, TextGenerationError>();
          const reject = (detail: string) =>
            Deferred.fail(rejected, new TextGenerationError({ operation, detail })).pipe(
              Effect.asVoid,
            );
          const rejectToolRequest = () =>
            reject("Antigravity text generation requested a tool or user input.").pipe(
              Effect.andThen(
                Effect.fail(
                  new AcpRequestError({
                    code: -32601,
                    errorMessage: "Tools and user input are disabled for text generation.",
                  }),
                ),
              ),
            );

          yield* runtime.handleRequestPermission(() =>
            reject("Antigravity text generation requested a tool permission or user input.").pipe(
              Effect.as({ outcome: { outcome: "cancelled" as const } }),
            ),
          );
          yield* runtime.handleElicitation(() =>
            reject("Antigravity text generation requested user input.").pipe(
              Effect.as({ action: "decline" as const }),
            ),
          );
          yield* runtime.handleReadTextFile(rejectToolRequest);
          yield* runtime.handleWriteTextFile(rejectToolRequest);
          yield* runtime.handleCreateTerminal(rejectToolRequest);
          yield* runtime.handleTerminalOutput(rejectToolRequest);
          yield* runtime.handleTerminalWaitForExit(rejectToolRequest);
          yield* runtime.handleTerminalKill(rejectToolRequest);
          yield* runtime.handleTerminalRelease(rejectToolRequest);
          yield* runtime.handleUnknownExtRequest(rejectToolRequest);
          yield* runtime.handleSessionUpdate((notification) =>
            Effect.gen(function* () {
              const update = notification.update;
              if (
                update.sessionUpdate === "tool_call" ||
                update.sessionUpdate === "tool_call_update"
              ) {
                return yield* reject("Antigravity attempted tool work during text generation.");
              }
              if (
                notification.sessionId !== sessionId ||
                update.sessionUpdate !== "agent_message_chunk" ||
                update.content.type !== "text"
              ) {
                return;
              }
              const text = update.content.text;
              const exceeded = yield* Ref.modify(output, (current) =>
                current.length + text.length > MAX_OUTPUT_CHARS
                  ? [true, current]
                  : [false, current + text],
              );
              if (exceeded) {
                return yield* reject("Antigravity text generation exceeded the output limit.");
              }
            }),
          );

          return yield* Effect.gen(function* () {
            const started = yield* runtime.start();
            sessionId = started.sessionId;
            if (!isNativeSessionId(sessionId)) {
              return yield* new TextGenerationError({
                operation,
                detail: "Antigravity returned an invalid text helper session ID.",
              });
            }
            yield* runtime.setMode("default");
            const defaultModel = resolveAntigravityTextDefaultModel(
              yield* runtime.getConfigOptions,
              yield* options.defaultModel ?? Effect.undefined,
            );
            const selectedModel = yield* applyAntigravityAcpModelSelection({
              runtime,
              model: input.modelSelection.model,
              defaultModel,
              fallbackToDefault: true,
              mapError: (cause) =>
                new TextGenerationError({
                  operation,
                  detail: "Could not select the Antigravity model for text generation.",
                  cause,
                }),
            });
            const thinking = antigravityThinkingConfig(yield* runtime.getConfigOptions);
            if (thinking !== undefined) {
              const availableThinking = collectSessionConfigOptionValues(thinking);
              const savedThinking = input.modelSelection.options?.find(
                (option) => option.id === thinking.id,
              )?.value;
              const requestedExplicitModel =
                Boolean(input.modelSelection.model) &&
                input.modelSelection.model !== ANTIGRAVITY_DEFAULT_MODEL;
              const modelFellBack =
                requestedExplicitModel &&
                selectedModel !== undefined &&
                selectedModel !== input.modelSelection.model &&
                !input.modelSelection.model.startsWith(`${selectedModel}-`);
              const targetThinking =
                typeof savedThinking === "string" && !modelFellBack
                  ? availableThinking.includes(savedThinking)
                    ? savedThinking
                    : undefined
                  : lowestThinkingLevel(availableThinking);
              if (targetThinking !== undefined && targetThinking !== thinking.currentValue) {
                yield* runtime.setConfigOption(thinking.id, targetThinking).pipe(
                  Effect.catchTags({
                    AcpRequestError: (error) =>
                      error.code === -32602
                        ? Effect.logDebug(
                            "Antigravity rejected the text generation thinking level",
                            {
                              optionId: thinking.id,
                              value: targetThinking,
                            },
                          )
                        : Effect.fail(error),
                  }),
                );
              }
            }

            const modelKey = selectedModel ?? input.modelSelection.model;
            const promptText = [
              "Use only the input below. Do not use tools, read or write files, run commands, or ask questions.",
              "Return only the requested JSON object.",
              "",
              input.prompt,
            ].join("\n");
            const imageBlocks =
              withImages &&
              started.initializeResult.agentCapabilities?.promptCapabilities?.image === true &&
              !textOnlyModels.has(modelKey)
                ? yield* readImageBlocks(input.attachments, promptText)
                : [];
            const checkPromptOutcome = yield* watchAntigravityPromptOutcome(
              runtimeProfileDirectory,
              sessionId,
            );
            const promptResult = yield* runtime
              .prompt({
                prompt: [{ type: "text", text: promptText }, ...imageBlocks],
              })
              .pipe(Effect.result);
            if (yield* Deferred.isDone(rejected)) {
              return yield* Deferred.await(rejected);
            }
            if (Result.isFailure(promptResult)) {
              const cause = promptResult.failure;
              const detail = acpRequestErrorDetail(cause);
              if (
                imageBlocks.length > 0 &&
                !isAntigravitySignInRequiredError(cause) &&
                detail !== undefined &&
                IMAGE_REJECTION.test(detail)
              ) {
                return { _tag: "imageRejected" as const, detail, modelKey, cause };
              }
              return yield* cause;
            }
            const result = promptResult.success;
            if (result.stopReason === "cancelled") {
              return yield* new TextGenerationError({
                operation,
                detail: "Antigravity text generation was cancelled.",
              });
            }
            const outcome = yield* checkPromptOutcome(result);
            if (outcome !== undefined) {
              const detail = outcome.failure.message;
              if (imageBlocks.length > 0 && IMAGE_REJECTION.test(detail)) {
                return { _tag: "imageRejected" as const, detail, modelKey };
              }
              return yield* new TextGenerationError({ operation, detail });
            }
            return { _tag: "ok" as const, text: (yield* Ref.get(output)).trim() };
          }).pipe(
            Effect.onInterrupt(() =>
              runtime.cancel.pipe(Effect.timeoutOption(2_000), Effect.ignore),
            ),
            Effect.raceFirst(Deferred.await(rejected)),
          );
        }).pipe(Effect.scoped);

        if (!(yield* hasOnlyRuntimeDirectories(cwd))) {
          return yield* new TextGenerationError({
            operation,
            detail: "Antigravity wrote files during text generation.",
          });
        }
        if (attempt._tag === "imageRejected") {
          return attempt;
        }
        if (!attempt.text) {
          return yield* new TextGenerationError({
            operation,
            detail: "Antigravity returned empty text generation output.",
          });
        }
        return {
          _tag: "ok" as const,
          value: yield* TextGenerationOperations.decodeJsonReply(
            input,
            "Antigravity",
            attempt.text,
          ),
        };
      }, Effect.scoped);

      const helper = Effect.gen(function* () {
        if (options.makeIsolatedRuntime === undefined && !(yield* available)) {
          return yield* new TextGenerationError({
            operation,
            detail:
              "Antigravity text generation is unavailable for profiles with global hooks or MCP configuration. Select another system model.",
          });
        }
        const first = yield* runAttempt(true);
        if (first._tag === "ok") {
          return first.value;
        }
        const retry = yield* runAttempt(false);
        if (retry._tag !== "ok") {
          return yield* new TextGenerationError({
            operation,
            detail: retry.detail,
            ...("cause" in retry ? { cause: retry.cause } : {}),
          });
        }
        if (IMAGES_UNSUPPORTED.test(first.detail) && !IMAGE_FORMAT_REJECTION.test(first.detail)) {
          textOnlyModels.add(first.modelKey);
        }
        return retry.value;
      }).pipe(
        Effect.timeoutOption(ANTIGRAVITY_TIMEOUT_MS),
        Effect.flatMap(
          Option.match({
            onNone: () =>
              Effect.fail(
                new TextGenerationError({
                  operation,
                  detail: "Antigravity text generation timed out.",
                }),
              ),
            onSome: Effect.succeed,
          }),
        ),
      );

      return yield* options
        .withProcess(Scope.close(scope, Exit.void), helper)
        .pipe(Effect.provideService(Scope.Scope, scope));
    },
    (effect, input) =>
      effect.pipe(
        Effect.mapError((cause) =>
          isTextGenerationError(cause)
            ? cause
            : new TextGenerationError({
                operation: input.operation,
                detail: isProviderSetupError(cause)
                  ? cause.detail
                  : isAntigravitySignInRequiredError(cause)
                    ? ANTIGRAVITY_SIGN_IN_REQUIRED_MESSAGE
                    : (acpRequestErrorDetail(cause) ?? "Antigravity text generation failed."),
                cause,
              }),
        ),
        Effect.scoped,
      ),
  );

  return TextGenerationOperations.fromRunner(
    "AntigravityTextGeneration",
    TextGenerationOperations.withoutInvestigation("Antigravity", runAntigravityJson),
  );
});
