import { TextGenerationError } from "@onicode/contracts";
import * as HostProcess from "@onicode/shared/HostProcess";
import * as SecureRandom from "effect/crypto/SecureRandom";
import * as Effect from "effect/Effect";
import * as Schema from "effect/Schema";
import * as Stream from "effect/Stream";
import * as SubscriptionRef from "effect/SubscriptionRef";
import * as ChildProcessSpawner from "effect/process/ChildProcessSpawner";

import {
  defaultProviderContinuationIdentity,
  type ProviderDriver,
  type ProviderInstance,
} from "@onicode/provider-core/server/driver";
import { ProviderDriverError } from "@onicode/provider-core/server/errors";
import { mergeProviderInstanceEnvironment } from "@onicode/provider-core/server/instanceEnvironment";
import { withInstanceIdentity } from "@onicode/provider-core/server/instanceIdentity";
import { makeManualOnlyProviderMaintenanceCapabilities } from "@onicode/provider-core/server/maintenanceResolver";
import { makeManagedServerProvider } from "@onicode/provider-core/server/managedProvider";
import { expandHomePath } from "@onicode/provider-core/server/pathExpansion";
import type * as ProviderHost from "@onicode/provider-core/server/ProviderHost";
import {
  haveProviderSnapshotSettingsChanged,
  makeProviderSnapshotSettingsSource,
  type ProviderSnapshotSettings,
} from "@onicode/provider-core/server/snapshotSettings";
import type { ProviderTextGeneration } from "@onicode/provider-core/server/textGeneration";

import { CommandCodeSettings } from "../settings.ts";
import { COMMAND_CODE_DRIVER_KIND, EMPTY_COMMAND_CODE_CATALOG } from "./acpSupport.ts";
import { type CommandCodeAdapterV2DriverEnv, makeCommandCodeAdapterV2Driver } from "./adapter.ts";
import {
  buildInitialCommandCodeProviderSnapshot,
  checkCommandCodeProviderStatus,
  commandCodeModels,
} from "./status.ts";

const decodeCommandCodeSettings = Schema.decodeSync(CommandCodeSettings);
// Command Code updates itself, so Onicode offers no one-click update.
const MAINTENANCE = makeManualOnlyProviderMaintenanceCapabilities({
  provider: COMMAND_CODE_DRIVER_KIND,
  packageName: null,
});

const unsupportedTextGeneration: ProviderTextGeneration = (() => {
  const unsupported = (operation: string) =>
    Effect.fail(
      new TextGenerationError({
        operation,
        detail: "Command Code does not provide application text generation.",
      }),
    );
  return {
    generateCommitMessage: () => unsupported("generateCommitMessage"),
    generatePrContent: () => unsupported("generatePrContent"),
    generateBranchName: () => unsupported("generateBranchName"),
    generateThreadTitle: () => unsupported("generateThreadTitle"),
    generateReviewGuide: () => unsupported("generateReviewGuide"),
    generateTranscriptionPostProcessing: () => unsupported("generateTranscriptionPostProcessing"),
  };
})();

export type CommandCodeDriverEnv =
  | CommandCodeAdapterV2DriverEnv
  | ChildProcessSpawner.ChildProcessSpawner
  | SecureRandom.SecureRandom
  | ProviderHost.ProviderHost;

export const CommandCodeDriver: ProviderDriver<CommandCodeSettings, CommandCodeDriverEnv> = {
  driverKind: COMMAND_CODE_DRIVER_KIND,
  metadata: {
    displayName: "Command Code",
    supportsMultipleInstances: true,
  },
  configSchema: CommandCodeSettings,
  defaultConfig: (): CommandCodeSettings => decodeCommandCodeSettings({}),
  create: Effect.fn("CommandCodeDriver.create")(function* ({
    instanceId,
    displayName,
    accentColor,
    environment,
    enabled,
    config,
  }) {
    const spawner = yield* ChildProcessSpawner.ChildProcessSpawner;
    const crypto = yield* SecureRandom.SecureRandom;
    const processEnv = yield* mergeProviderInstanceEnvironment(
      environment,
      yield* HostProcess.Environment,
    );
    const continuationIdentity = defaultProviderContinuationIdentity({
      driverKind: COMMAND_CODE_DRIVER_KIND,
      instanceId,
    });
    const stampIdentity = withInstanceIdentity({
      instanceId,
      driverKind: COMMAND_CODE_DRIVER_KIND,
      displayName,
      accentColor,
      continuationGroupKey: continuationIdentity.continuationKey,
    });
    const effectiveConfig = {
      ...config,
      enabled,
      binaryPath: expandHomePath(config.binaryPath, yield* HostProcess.HomeDirectory),
    } satisfies CommandCodeSettings;
    const catalog = yield* SubscriptionRef.make(EMPTY_COMMAND_CODE_CATALOG);
    const snapshotSettings = yield* makeProviderSnapshotSettingsSource(effectiveConfig);
    const snapshot = yield* makeManagedServerProvider<
      ProviderSnapshotSettings<CommandCodeSettings>
    >({
      resolveMaintenance: () => Effect.succeed(MAINTENANCE),
      getSettings: snapshotSettings.getSettings,
      streamSettings: snapshotSettings.streamSettings,
      haveSettingsChanged: haveProviderSnapshotSettingsChanged,
      initialSnapshot: (settings) =>
        SubscriptionRef.get(catalog).pipe(
          Effect.flatMap((current) =>
            buildInitialCommandCodeProviderSnapshot(settings.provider, current),
          ),
          Effect.map(stampIdentity),
        ),
      checkProvider: SubscriptionRef.get(catalog).pipe(
        Effect.flatMap((current) =>
          checkCommandCodeProviderStatus(effectiveConfig, processEnv, current),
        ),
        Effect.map(stampIdentity),
        Effect.provideService(ChildProcessSpawner.ChildProcessSpawner, spawner),
        Effect.provideService(SecureRandom.SecureRandom, crypto),
      ),
      enrichSnapshot: ({ getSnapshot, publishSnapshot }) =>
        SubscriptionRef.changes(catalog).pipe(
          Stream.runForEach((current) =>
            getSnapshot.pipe(
              Effect.flatMap((latest) =>
                publishSnapshot({
                  ...latest,
                  models: commandCodeModels(effectiveConfig, current),
                }),
              ),
            ),
          ),
        ),
    }).pipe(
      Effect.mapError(
        (cause) =>
          new ProviderDriverError({
            driver: COMMAND_CODE_DRIVER_KIND,
            instanceId,
            detail: "Failed to build the Command Code provider snapshot.",
            cause,
          }),
      ),
    );

    const orchestrationAdapter = yield* makeCommandCodeAdapterV2Driver(catalog)
      .create({
        instanceId,
        displayName,
        accentColor,
        environment,
        enabled,
        config: effectiveConfig,
      })
      .pipe(
        Effect.mapError(
          (cause) =>
            new ProviderDriverError({
              driver: COMMAND_CODE_DRIVER_KIND,
              instanceId,
              detail: "Failed to build the Command Code orchestration adapter.",
              cause,
            }),
        ),
      );

    return {
      instanceId,
      driverKind: COMMAND_CODE_DRIVER_KIND,
      continuationIdentity,
      displayName,
      accentColor,
      enabled,
      snapshot,
      orchestrationAdapter,
      textGeneration: unsupportedTextGeneration,
    } satisfies ProviderInstance;
  }),
};
