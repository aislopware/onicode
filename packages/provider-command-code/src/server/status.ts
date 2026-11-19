import {
  type ModelCapabilities,
  type ServerProviderAuth,
  type ServerProviderModel,
} from "@onicode/contracts";
import * as HostProcess from "@onicode/shared/HostProcess";
import { createModelCapabilities } from "@onicode/shared/model";
import { resolveSpawnCommand } from "@onicode/shared/shell";
import type * as SecureRandom from "effect/crypto/SecureRandom";
import * as DateTime from "effect/DateTime";
import * as Effect from "effect/Effect";
import * as Option from "effect/Option";
import * as Result from "effect/Result";
import * as Schema from "effect/Schema";
import { ChildProcess, type ChildProcessSpawner } from "effect/process";
import * as EffectAcpErrors from "effect-acp/errors";

import {
  AUTH_PROBE_TIMEOUT_MS,
  buildServerProvider,
  COMPACT_SLASH_COMMAND,
  isCommandMissingCause,
  parseCliVersionResult,
  type ProviderProbeResult,
  providerModelsFromSettings,
  providerProbeEnvironment,
  resolveProviderProbeTimeoutMs,
  type ServerProviderDraft,
  spawnAndCollect,
} from "@onicode/provider-core/server/snapshotProbe";

import type { CommandCodeSettings } from "../settings.ts";
import {
  COMMAND_CODE_CLI_AUTH_METHOD_ID,
  COMMAND_CODE_DEFAULT_BINARY,
  COMMAND_CODE_DRIVER_KIND,
  COMMAND_CODE_SUPPORTED_RUNTIME_MODES,
  type CommandCodeCatalog,
  makeCommandCodeAcpRuntime,
} from "./acpSupport.ts";

const COMMAND_CODE_PRESENTATION = {
  displayName: "Command Code",
  showInteractionModeToggle: true,
  supportedRuntimeModes: COMMAND_CODE_SUPPORTED_RUNTIME_MODES,
} as const;
const EMPTY_CAPABILITIES: ModelCapabilities = createModelCapabilities({ optionDescriptors: [] });
export const COMMAND_CODE_DEFAULT_MODEL_SLUG = "default";
const COMMAND_CODE_API_KEY_ENV = "COMMAND_CODE_API_KEY";
const VERSION_PROBE_TIMEOUT_MS = 10_000;

function modelCapabilities(catalog: CommandCodeCatalog, modelId: string | undefined) {
  const effort = modelId === undefined ? undefined : catalog.effortByModel.get(modelId);
  return effort === undefined
    ? EMPTY_CAPABILITIES
    : createModelCapabilities({ optionDescriptors: [effort] });
}

/** "Command Code default" first, then every model a session has listed, then custom models. */
export function commandCodeModels(
  settings: Pick<CommandCodeSettings, "customModels">,
  catalog: CommandCodeCatalog,
): ReadonlyArray<ServerProviderModel> {
  const listed: ReadonlyArray<ServerProviderModel> = [
    {
      slug: COMMAND_CODE_DEFAULT_MODEL_SLUG,
      name: "Command Code default",
      isCustom: false,
      isDefault: true,
      capabilities: modelCapabilities(catalog, catalog.defaultModelId),
    },
    ...catalog.models.flatMap((model) =>
      model.id === COMMAND_CODE_DEFAULT_MODEL_SLUG
        ? []
        : [
            {
              slug: model.id,
              name: model.name,
              isCustom: false,
              capabilities: modelCapabilities(catalog, model.id),
            },
          ],
    ),
  ];
  return providerModelsFromSettings(listed, settings.customModels, EMPTY_CAPABILITIES);
}

const snapshot = (
  settings: CommandCodeSettings,
  catalog: CommandCodeCatalog,
  checkedAt: string,
  probe: ProviderProbeResult,
) =>
  buildServerProvider({
    driver: COMMAND_CODE_DRIVER_KIND,
    presentation: COMMAND_CODE_PRESENTATION,
    enabled: settings.enabled,
    checkedAt,
    models: commandCodeModels(settings, catalog),
    // Sent as a prompt; see `compactViaAdvertisedCommand`.
    slashCommands: [COMPACT_SLASH_COMMAND],
    probe,
  });

export const buildInitialCommandCodeProviderSnapshot = Effect.fn(
  "CommandCodeProvider.buildInitialCommandCodeProviderSnapshot",
)(function* (
  settings: CommandCodeSettings,
  catalog: CommandCodeCatalog,
): Effect.fn.Return<ServerProviderDraft> {
  const now = yield* DateTime.now;
  return snapshot(settings, catalog, DateTime.formatIso(now), {
    installed: settings.enabled,
    version: null,
    status: "warning",
    auth: { status: "unknown" },
    message: settings.enabled
      ? "Checking Command Code CLI availability..."
      : "Command Code is disabled in Onicode settings.",
  });
});

const runCommandCodeCli = Effect.fn("CommandCodeProvider.runCommandCodeCli")(function* (
  settings: CommandCodeSettings,
  args: ReadonlyArray<string>,
  environment: NodeJS.ProcessEnv,
) {
  const command = settings.binaryPath || COMMAND_CODE_DEFAULT_BINARY;
  const spawnCommand = yield* resolveSpawnCommand(command, args, { env: environment });
  return yield* spawnAndCollect(
    command,
    ChildProcess.make(spawnCommand.command, spawnCommand.args, {
      env: environment,
      shell: spawnCommand.shell,
    }),
  );
});

const isAcpRequestError = Schema.is(EffectAcpErrors.AcpRequestError);

/**
 * Reads ACP `authenticate` with the `command-code-cli` method: success means a
 * stored login or API key, and signed out it answers -32000 "Not authenticated.
 * Run `cmd login` in a terminal, then retry." Anything else leaves auth unknown.
 */
export function commandCodeAuthFromProbe(
  outcome: Result.Result<void, unknown> | undefined,
  environment: NodeJS.ProcessEnv,
): ServerProviderAuth {
  if (outcome === undefined) return { status: "unknown" };
  if (Result.isFailure(outcome)) {
    const cause = outcome.failure;
    return isAcpRequestError(cause) && /not authenticated/i.test(cause.errorMessage)
      ? { status: "unauthenticated" }
      : { status: "unknown" };
  }
  return environment[COMMAND_CODE_API_KEY_ENV]?.trim()
    ? { status: "authenticated", type: "api_key", label: "Command Code API key" }
    : { status: "authenticated", label: "Command Code account" };
}

/**
 * Runs `initialize` and `authenticate` and nothing more: no `session/new`, so
 * no MCP server starts and no session is stored. The process runs in the
 * server's working directory.
 */
const probeCommandCodeAuth = Effect.fn("CommandCodeProvider.probeCommandCodeAuth")(function* (
  settings: CommandCodeSettings,
  environment: NodeJS.ProcessEnv,
) {
  const runtime = yield* makeCommandCodeAcpRuntime({
    settings,
    environment,
    cwd: yield* HostProcess.WorkingDirectory,
    clientInfo: { name: "onicode-provider-probe", version: "0.0.0" },
  });
  yield* runtime.initialize();
  if (runtime.authenticate === undefined) return undefined;
  return yield* runtime.authenticate(COMMAND_CODE_CLI_AUTH_METHOD_ID).pipe(Effect.result);
}, Effect.scoped);

export const checkCommandCodeProviderStatus = Effect.fn("checkCommandCodeProviderStatus")(
  function* (
    settings: CommandCodeSettings,
    environment: NodeJS.ProcessEnv,
    catalog: CommandCodeCatalog,
  ): Effect.fn.Return<
    ServerProviderDraft,
    never,
    ChildProcessSpawner.ChildProcessSpawner | SecureRandom.SecureRandom
  > {
    const checkedAt = DateTime.formatIso(yield* DateTime.now);
    if (!settings.enabled) {
      return snapshot(settings, catalog, checkedAt, {
        installed: false,
        version: null,
        status: "warning",
        auth: { status: "unknown" },
        message: "Command Code is disabled in Onicode settings.",
      });
    }

    const probeEnvironment = providerProbeEnvironment(environment);
    const versionResult = yield* runCommandCodeCli(settings, ["--version"], probeEnvironment).pipe(
      Effect.timeoutOption(
        resolveProviderProbeTimeoutMs("command_code", VERSION_PROBE_TIMEOUT_MS, environment),
      ),
      Effect.result,
    );
    if (Result.isFailure(versionResult) || Option.isNone(versionResult.success)) {
      const missing =
        Result.isFailure(versionResult) && isCommandMissingCause(versionResult.failure);
      return snapshot(settings, catalog, checkedAt, {
        installed: !missing,
        version: null,
        status: "error",
        auth: { status: "unknown" },
        message: missing
          ? "Command Code CLI (`command-code`) is not installed or not on PATH."
          : "Failed to run `command-code --version`.",
      });
    }
    const versionOutput = versionResult.success.value;
    const version = parseCliVersionResult(versionOutput);
    if (versionOutput.code !== 0) {
      return snapshot(settings, catalog, checkedAt, {
        installed: true,
        version,
        status: "error",
        auth: { status: "unknown" },
        message: "Command Code CLI is installed but failed to run.",
      });
    }

    const authOutcome = yield* probeCommandCodeAuth(settings, probeEnvironment).pipe(
      Effect.timeoutOption(
        resolveProviderProbeTimeoutMs("command_code", AUTH_PROBE_TIMEOUT_MS, environment),
      ),
      Effect.map(Option.getOrUndefined),
      Effect.orElseSucceed(() => undefined),
    );
    const auth = commandCodeAuthFromProbe(authOutcome, environment);
    return snapshot(settings, catalog, checkedAt, {
      installed: true,
      version,
      status:
        auth.status === "authenticated"
          ? "ready"
          : auth.status === "unauthenticated"
            ? "error"
            : "warning",
      auth,
      ...(auth.status === "authenticated"
        ? {}
        : auth.status === "unauthenticated"
          ? {
              message: "Command Code CLI is installed but not signed in. Run `command-code login`.",
            }
          : { message: "Could not confirm the Command Code sign-in." }),
    });
  },
);
