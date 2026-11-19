import * as HostProcess from "@onicode/shared/HostProcess";
import { resolveSelfInvocation, type SelfInvocation } from "@onicode/shared/nodeRuntime";
import { type OrchestrationV2ProviderCapabilities } from "@onicode/contracts";
import type * as SecureRandom from "effect/crypto/SecureRandom";
import * as Effect from "effect/Effect";
import type * as FileSystem from "effect/FileSystem";
import * as Option from "effect/Option";
import type * as Path from "effect/Path";
import * as Schema from "effect/Schema";
import type * as Scope from "effect/Scope";
import * as SubscriptionRef from "effect/SubscriptionRef";
import type * as ChildProcessSpawner from "effect/process/ChildProcessSpawner";
import * as EffectAcpErrors from "effect-acp/errors";

import type * as AcpSessionRuntime from "@onicode/provider-acp/server/AcpSessionRuntime";
import {
  AcpProviderCapabilitiesV2,
  makeAcpAdapterV2,
  type AcpAdapterV2Flavor,
  type AcpAdapterV2RuntimeInput,
} from "@onicode/provider-acp/server/adapter";
import { makeAcpNativeLoggerFactory } from "@onicode/provider-acp/server/nativeLogging";
import {
  ProviderAdapterDriverCreateError,
  type ProviderAdapterDriver,
  type ProviderAdapterDriverCreateInput,
} from "@onicode/provider-core/server/adapterDriver";
import { makeProviderFailure } from "@onicode/provider-core/server/failure";
import type * as IdAllocator from "@onicode/provider-core/server/IdAllocator";
import { mergeProviderInstanceEnvironment } from "@onicode/provider-core/server/instanceEnvironment";
import type * as McpProviderSessions from "@onicode/provider-core/server/McpProviderSessions";
import * as ProviderEventLoggers from "@onicode/provider-core/server/ProviderEventLoggers";
import type * as ProviderHost from "@onicode/provider-core/server/ProviderHost";

import { CommandCodeSettings } from "../settings.ts";
import {
  COMMAND_CODE_DRIVER_KIND,
  COMMAND_CODE_MODEL_CONFIG_ID,
  type CommandCodeCatalog,
  commandCodeCurrentModelId,
  commandCodeSessionModeForPolicy,
  learnCommandCodeCatalog,
  makeCommandCodeAcpRuntime,
} from "./acpSupport.ts";
import { COMMAND_CODE_DEFAULT_MODEL_SLUG } from "./status.ts";

const DEFAULT_COMMAND_CODE_SETTINGS = Schema.decodeSync(CommandCodeSettings)({});

const CommandCodeProviderCapabilitiesV2 = {
  ...AcpProviderCapabilitiesV2,
  sessions: {
    ...AcpProviderCapabilitiesV2.sessions,
    supportsModelSwitchInSession: true,
  },
  tools: {
    ...AcpProviderCapabilitiesV2.tools,
    supportsMcpTools: true,
  },
} satisfies OrchestrationV2ProviderCapabilities;

export interface CommandCodeAdapterV2Options {
  readonly instanceId: Parameters<typeof makeAcpAdapterV2>[0]["instanceId"];
  readonly settings: CommandCodeSettings;
  readonly environment: NodeJS.ProcessEnv;
  readonly selfInvocation: SelfInvocation;
  /** Shared with the driver, which publishes what sessions teach it in the snapshot. */
  readonly catalog: SubscriptionRef.SubscriptionRef<CommandCodeCatalog>;
  readonly nativeLogging?: Parameters<typeof makeAcpAdapterV2>[0]["nativeLogging"];
  readonly testHooks?: Parameters<typeof makeAcpAdapterV2>[0]["testHooks"];
  /** Replaces the `command-code acp` launch (replay tests). */
  readonly makeRuntime?: (
    input: AcpAdapterV2RuntimeInput,
  ) => Effect.Effect<
    AcpSessionRuntime.AcpSessionRuntime["Service"],
    EffectAcpErrors.AcpError,
    ChildProcessSpawner.ChildProcessSpawner | SecureRandom.SecureRandom | Scope.Scope
  >;
  readonly assertComplete?: Effect.Effect<void, EffectAcpErrors.AcpError>;
}

/**
 * "Command Code default" is the model `session/new` started on. A loaded
 * session reports its own last model instead, so it falls back to the default
 * an earlier new session recorded. Every model is written, even an unchanged
 * one, because Command Code advertises a model's effort option only after a write.
 */
const applyCommandCodeModelSelection =
  (
    catalog: SubscriptionRef.SubscriptionRef<CommandCodeCatalog>,
  ): NonNullable<AcpAdapterV2Flavor["applyModelSelection"]> =>
  ({ runtime, startResult, modelSelection }) =>
    Effect.gen(function* () {
      const setup = startResult.sessionSetupResult;
      const newSessionDefault =
        "sessionId" in setup ? commandCodeCurrentModelId(setup.configOptions) : undefined;
      if (newSessionDefault !== undefined) {
        yield* SubscriptionRef.updateSome(catalog, (current) =>
          current.defaultModelId === newSessionDefault
            ? Option.none()
            : Option.some({ ...current, defaultModelId: newSessionDefault }),
        );
      }
      const requested = modelSelection.model.trim();
      const model =
        requested.length === 0 || requested === COMMAND_CODE_DEFAULT_MODEL_SLUG
          ? (newSessionDefault ?? (yield* SubscriptionRef.get(catalog)).defaultModelId)
          : requested;
      if (model === undefined) return undefined;
      yield* runtime.setConfigOption(COMMAND_CODE_MODEL_CONFIG_ID, model, {
        allowUnlistedValue: true,
        resendCurrentValue: true,
      });
      return model;
    });

const isAcpRequestError = Schema.is(EffectAcpErrors.AcpRequestError);

/**
 * Command Code's prompt errors carry a message meant for the user (1.88.5: "POST
 * /alpha/generate → 403 error: MODEL_NOT_IN_PLAN: Claude Haiku 5.5 available in GOAT
 * and above plans …"). makeProviderFailure bounds and redacts it.
 */
function commandCodePromptFailure(cause: unknown) {
  return makeProviderFailure({
    cause,
    ...(isAcpRequestError(cause) ? { message: cause.errorMessage, code: String(cause.code) } : {}),
    class: "provider_error",
  });
}

function makeCommandCodeAcpAdapterFlavor(options: CommandCodeAdapterV2Options): AcpAdapterV2Flavor {
  const makeRuntime =
    options.makeRuntime ??
    (({ runtimePolicy: _runtimePolicy, processEnvironment, ...input }: AcpAdapterV2RuntimeInput) =>
      makeCommandCodeAcpRuntime({
        ...input,
        settings: options.settings,
        environment: { ...options.environment, ...processEnvironment },
      }));
  return {
    driver: COMMAND_CODE_DRIVER_KIND,
    runtimeHarness: "Command Code",
    capabilities: CommandCodeProviderCapabilitiesV2,
    makeRuntime,
    promptFailure: commandCodePromptFailure,
    mcpOverHttp: true,
    applyModelSelection: applyCommandCodeModelSelection(options.catalog),
    lockSessionModeToPolicy: true,
    sessionModeForPolicy: commandCodeSessionModeForPolicy,
    compactViaAdvertisedCommand: true,
    onSessionConfigurationUpdate: (configOptions) =>
      SubscriptionRef.updateSome(options.catalog, (current) => {
        const next = learnCommandCodeCatalog(current, configOptions);
        return next === current ? Option.none() : Option.some(next);
      }),
    ...(options.assertComplete === undefined ? {} : { assertComplete: options.assertComplete }),
  };
}

const makeCommandCodeAdapterV2 = Effect.fn("makeCommandCodeAdapterV2")(function* (
  options: CommandCodeAdapterV2Options,
) {
  return yield* makeAcpAdapterV2({
    instanceId: options.instanceId,
    flavor: makeCommandCodeAcpAdapterFlavor(options),
    selfInvocation: options.selfInvocation,
    ...(options.nativeLogging === undefined ? {} : { nativeLogging: options.nativeLogging }),
    ...(options.testHooks === undefined ? {} : { testHooks: options.testHooks }),
  });
});

export type CommandCodeAdapterV2DriverEnv =
  | ChildProcessSpawner.ChildProcessSpawner
  | SecureRandom.SecureRandom
  | FileSystem.FileSystem
  | IdAllocator.IdAllocatorV2
  | Path.Path
  | McpProviderSessions.McpProviderSessions
  | ProviderEventLoggers.ProviderEventLoggers
  | ProviderHost.ProviderHost;

export const makeCommandCodeAdapterV2Driver = (
  catalog: SubscriptionRef.SubscriptionRef<CommandCodeCatalog>,
): ProviderAdapterDriver<CommandCodeSettings, CommandCodeAdapterV2DriverEnv> => ({
  driverKind: COMMAND_CODE_DRIVER_KIND,
  configSchema: CommandCodeSettings,
  defaultConfig: (): CommandCodeSettings => DEFAULT_COMMAND_CODE_SETTINGS,
  create: Effect.fn("CommandCodeAdapterV2Driver.create")(
    function* (input: ProviderAdapterDriverCreateInput<CommandCodeSettings>) {
      const hostEnvironment = yield* HostProcess.Environment;
      const selfInvocation = yield* resolveSelfInvocation();
      const providerEventLoggers = yield* ProviderEventLoggers.ProviderEventLoggers;
      const makeNativeLogger = yield* makeAcpNativeLoggerFactory();
      return yield* makeCommandCodeAdapterV2({
        instanceId: input.instanceId,
        settings: { ...input.config, enabled: input.enabled },
        environment: yield* mergeProviderInstanceEnvironment(input.environment, hostEnvironment),
        selfInvocation,
        catalog,
        nativeLogging: (threadId) =>
          makeNativeLogger({
            nativeEventLogger: providerEventLoggers.native,
            provider: COMMAND_CODE_DRIVER_KIND,
            threadId,
          }),
      });
    },
    (effect, input) =>
      effect.pipe(
        Effect.mapError(
          (cause) =>
            new ProviderAdapterDriverCreateError({
              driver: COMMAND_CODE_DRIVER_KIND,
              instanceId: input.instanceId,
              detail: "Failed to create the Command Code ACP adapter.",
              cause,
            }),
        ),
      ),
  ),
});
