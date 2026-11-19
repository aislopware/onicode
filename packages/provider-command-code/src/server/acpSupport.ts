import {
  ProviderDriverKind,
  type ProviderOptionDescriptor,
  type RuntimeMode,
} from "@onicode/contracts";
import type * as SecureRandom from "effect/crypto/SecureRandom";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import type * as Scope from "effect/Scope";
import type * as ChildProcessSpawner from "effect/process/ChildProcessSpawner";
import type * as EffectAcpSchema from "effect-acp/compat";
import type * as EffectAcpErrors from "effect-acp/errors";

import * as AcpSessionRuntime from "@onicode/provider-acp/server/AcpSessionRuntime";
import {
  acpProviderOptionDescriptors,
  flattenSelectChoices,
  isAcpThoughtLevelOption,
} from "@onicode/provider-acp/server/sessionConfig";
import type * as ProviderAdapter from "@onicode/provider-core/server/ProviderAdapter";

import type { CommandCodeSettings } from "../settings.ts";

export const COMMAND_CODE_DRIVER_KIND = ProviderDriverKind.make("commandCode");
export const COMMAND_CODE_DEFAULT_BINARY = "command-code";
export const COMMAND_CODE_MODEL_CONFIG_ID = "model";
/** Checks the stored login locally; it opens no session and starts no browser. */
export const COMMAND_CODE_CLI_AUTH_METHOD_ID = "command-code-cli";

/** Command Code has no Onicode "Auto": its `dont-ask` mode denies whatever needs approval. */
export const COMMAND_CODE_SUPPORTED_RUNTIME_MODES = [
  "approval-required",
  "auto-accept-edits",
  "full-access",
] as const satisfies ReadonlyArray<RuntimeMode>;

/**
 * Command Code's session mode for a thread. An explicit approval or sandbox
 * policy runs `default`, which asks before every edit and command, so a
 * permissive mode never runs writes the policy meant to confine.
 */
export function commandCodeSessionModeForPolicy(
  policy: Pick<
    ProviderAdapter.ProviderAdapterV2RuntimePolicy,
    "runtimeMode" | "approvalPolicy" | "sandboxPolicy"
  >,
): "default" | "auto-accept" | "bypass" {
  if (policy.approvalPolicy !== undefined || policy.sandboxPolicy !== undefined) return "default";
  switch (policy.runtimeMode) {
    case "auto-accept-edits":
      return "auto-accept";
    case "full-access":
      return "bypass";
    default:
      return "default";
  }
}

type CommandCodeAcpRuntimeSettings = Pick<CommandCodeSettings, "binaryPath">;

export function buildCommandCodeAcpSpawnInput(
  settings: CommandCodeAcpRuntimeSettings | null | undefined,
  cwd: string,
  environment?: NodeJS.ProcessEnv,
): AcpSessionRuntime.AcpSpawnInput {
  return {
    command: settings?.binaryPath || COMMAND_CODE_DEFAULT_BINARY,
    args: ["acp"],
    cwd,
    ...(environment === undefined ? {} : { env: environment }),
  };
}

export interface CommandCodeAcpRuntimeInput extends Omit<
  AcpSessionRuntime.AcpSessionRuntimeOptions,
  "authMethodId" | "spawn"
> {
  readonly settings: CommandCodeAcpRuntimeSettings | null | undefined;
  readonly environment?: NodeJS.ProcessEnv;
}

export const makeCommandCodeAcpRuntime = Effect.fn(
  "CommandCodeAcpSupport.makeCommandCodeAcpRuntime",
)(function* (
  input: CommandCodeAcpRuntimeInput,
): Effect.fn.Return<
  AcpSessionRuntime.AcpSessionRuntime["Service"],
  EffectAcpErrors.AcpError,
  ChildProcessSpawner.ChildProcessSpawner | SecureRandom.SecureRandom | Scope.Scope
> {
  const { settings, environment, ...runtimeOptions } = input;
  const context = yield* Layer.build(
    AcpSessionRuntime.layer({
      ...runtimeOptions,
      spawn: buildCommandCodeAcpSpawnInput(settings, input.cwd, environment),
    }),
  );
  return yield* Effect.service(AcpSessionRuntime.AcpSessionRuntime).pipe(Effect.provide(context));
});

export interface CommandCodeModelEntry {
  readonly id: string;
  readonly name: string;
  readonly description?: string;
}

/**
 * What Command Code's sessions have shown about its models. Listing them needs
 * `session/new`, which starts the user's MCP servers, so the status probe never
 * asks and the catalog fills in as threads run.
 */
export interface CommandCodeCatalog {
  readonly models: ReadonlyArray<CommandCodeModelEntry>;
  /** A model's effort levels appear only while that model runs, so they gather per model. */
  readonly effortByModel: ReadonlyMap<string, ProviderOptionDescriptor>;
  /** The model a new session starts on, which "Command Code default" selects. */
  readonly defaultModelId: string | undefined;
}

export const EMPTY_COMMAND_CODE_CATALOG: CommandCodeCatalog = {
  models: [],
  effortByModel: new Map(),
  defaultModelId: undefined,
};

function findModelOption(
  configOptions: ReadonlyArray<EffectAcpSchema.SessionConfigOption>,
): Extract<EffectAcpSchema.SessionConfigOption, { readonly type: "select" }> | undefined {
  const option = configOptions.find(
    (candidate) =>
      candidate.type === "select" &&
      (candidate.id === COMMAND_CODE_MODEL_CONFIG_ID || candidate.category === "model"),
  );
  return option?.type === "select" ? option : undefined;
}

export function commandCodeCurrentModelId(
  configOptions: ReadonlyArray<EffectAcpSchema.SessionConfigOption> | null | undefined,
): string | undefined {
  const value = findModelOption(configOptions ?? [])?.currentValue.trim();
  return value ? value : undefined;
}

/** Folds a session's advertised configuration into the catalog; unchanged input returns `catalog` itself. */
export function learnCommandCodeCatalog(
  catalog: CommandCodeCatalog,
  configOptions: ReadonlyArray<EffectAcpSchema.SessionConfigOption>,
): CommandCodeCatalog {
  const modelOption = findModelOption(configOptions);
  if (modelOption === undefined) return catalog;
  const models = flattenSelectChoices(modelOption.options).flatMap((choice) => {
    const id = choice.value.trim();
    if (id.length === 0) return [];
    const name = choice.name.trim() || id;
    const description = choice.description?.trim();
    return [{ id, name, ...(description ? { description } : {}) }];
  });
  const currentModel = modelOption.currentValue.trim();
  const effortOption = configOptions.filter(isAcpThoughtLevelOption);
  const effort =
    effortOption.length === 0
      ? undefined
      : acpProviderOptionDescriptors({ configOptions: effortOption, modeState: undefined })[0];

  const modelsChanged =
    models.length > 0 &&
    (models.length !== catalog.models.length ||
      models.some((model, index) => {
        const known = catalog.models[index];
        return (
          known === undefined ||
          known.id !== model.id ||
          known.name !== model.name ||
          known.description !== model.description
        );
      }));
  const effortChanged =
    effort !== undefined &&
    currentModel.length > 0 &&
    JSON.stringify(catalog.effortByModel.get(currentModel)) !== JSON.stringify(effort);
  if (!modelsChanged && !effortChanged) return catalog;
  return {
    ...catalog,
    models: modelsChanged ? models : catalog.models,
    effortByModel: effortChanged
      ? new Map([...catalog.effortByModel, [currentModel, effort]])
      : catalog.effortByModel,
  };
}
