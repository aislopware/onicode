import {
  AntigravitySettings,
  CodexSettings,
  ProviderDriverKind,
  type ProviderInstallCancelInput,
  type ProviderInstallState,
  type ProviderInstanceId,
  ProviderSetupError,
  type ProviderSetupInput,
} from "@onicode/contracts";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as FiberHandle from "effect/FiberHandle";
import * as Layer from "effect/Layer";
import * as FileSystem from "effect/FileSystem";
import * as Path from "effect/Path";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";
import * as Stream from "effect/Stream";

import * as CodexInstallation from "./CodexInstallation.ts";
import { configuredBinaryPaths } from "./configuredBinaryPaths.ts";
import * as ServerSettings from "../serverSettings.ts";
import * as AntigravityInstallation from "./AntigravityInstallation.ts";
import { deriveProviderInstanceConfigMap } from "./ProviderInstanceRegistryHydration.ts";
import * as ProviderInstanceRegistry from "./ProviderInstanceRegistry.ts";
import * as ProviderRegistry from "./ProviderRegistry.ts";

const ANTIGRAVITY = ProviderDriverKind.make("antigravity");
const decodeCodexSettings = Schema.decodeUnknownEffect(CodexSettings);
const decodeAntigravitySettings = Schema.decodeUnknownEffect(AntigravitySettings);

export class ProviderInstallation extends Context.Service<
  ProviderInstallation,
  {
    readonly start: (
      input: ProviderSetupInput,
    ) => Effect.Effect<ProviderInstallState, ProviderSetupError>;
    readonly cancel: (
      input: ProviderInstallCancelInput,
    ) => Effect.Effect<ProviderInstallState, ProviderSetupError>;
    readonly subscribe: (
      input: ProviderSetupInput,
    ) => Stream.Stream<ProviderInstallState, ProviderSetupError>;
    readonly remove: (
      input: ProviderSetupInput,
    ) => Effect.Effect<ProviderInstallState, ProviderSetupError>;
  }
>()("onicode/provider/providerInstallation") {}

const make = Effect.fn("makeProviderInstallation")(function* () {
  const antigravityInstallation = yield* AntigravityInstallation.AntigravityInstallation;
  const codexInstallation = yield* CodexInstallation.CodexInstallation;
  const instances = yield* ProviderInstanceRegistry.ProviderInstanceRegistry;
  const providers = yield* ProviderRegistry.ProviderRegistry;
  const settings = yield* ServerSettings.ServerSettingsService;
  const fileSystem = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  const scope = yield* Effect.scope;
  // A start during a running install joins it, so one operation is checked once whichever instance asked.
  let accountCheckOperation: string | undefined;
  // A newer install's check replaces an older one, so an older runtime cannot publish last.
  const managedRuntimeCheck = yield* FiberHandle.make();

  const readEntries = Effect.fn("ProviderInstallation.readEntries")(function* (
    instanceId: ProviderInstanceId,
    operation: string,
  ) {
    const current = yield* settings.getSettings.pipe(
      Effect.mapError(
        () =>
          new ProviderSetupError({
            instanceId,
            operation,
            detail: "Could not read provider installation settings.",
          }),
      ),
    );
    return deriveProviderInstanceConfigMap(current);
  });

  const requireInstance = Effect.fn("ProviderInstallation.requireInstance")(function* (
    instanceId: ProviderInstanceId,
    operation: string,
    managedOnly = false,
  ) {
    const instance = yield* instances.getInstance(instanceId);
    const isCodex = instance?.driverKind === ProviderDriverKind.make("codex");
    if (instance?.driverKind !== ANTIGRAVITY && !isCodex) {
      return yield* new ProviderSetupError({
        instanceId,
        operation,
        detail: "Managed installation is not available for this provider instance.",
      });
    }
    const installation = isCodex ? codexInstallation : antigravityInstallation;
    const entries = yield* readEntries(instanceId, operation);
    const invalidConfig = () =>
      new ProviderSetupError({
        instanceId,
        operation,
        detail: "The provider instance configuration is invalid.",
      });
    const config = isCodex
      ? yield* decodeCodexSettings(entries[instanceId]?.config ?? {}).pipe(
          Effect.mapError(invalidConfig),
        )
      : yield* decodeAntigravitySettings(entries[instanceId]?.config ?? {}).pipe(
          Effect.mapError(invalidConfig),
        );
    if (isCodex && (!("setupMode" in config) || config.setupMode !== "managed")) {
      return yield* new ProviderSetupError({
        instanceId,
        operation,
        detail: "Choose managed setup to install Codex in Onicode.",
      });
    }
    if (managedOnly && config.binaryPath && (!isCodex || config.binaryPath !== "codex")) {
      return yield* new ProviderSetupError({
        instanceId,
        operation,
        detail:
          "This instance uses a custom executable. Clear its binary path to manage installation in Onicode.",
      });
    }
    return { installation, driver: instance.driverKind };
  });

  const failure =
    (instanceId: ProviderInstanceId) =>
    (
      error:
        | AntigravityInstallation.AntigravityInstallationError
        | CodexInstallation.CodexInstallationError,
    ) =>
      new ProviderSetupError({ instanceId, operation: error.operation, detail: error.detail });

  // Every instance without a custom executable runs the one managed runtime, so an install
  // replaces the runtime behind all of their model lists, not only the instance that asked.
  const checkManagedRuntimeInstances = Effect.fn(
    "ProviderInstallation.checkManagedRuntimeInstances",
  )(function* (instanceId: ProviderInstanceId) {
    const entries = yield* readEntries(instanceId, "install");
    const listed = yield* instances.listInstances;
    // Each check launches a runtime that unpacks about 1 GB, so they run one at a time,
    // starting with the instance whose user asked for the install.
    const ordered = [
      ...listed.filter((instance) => instance.instanceId === instanceId),
      ...listed.filter((instance) => instance.instanceId !== instanceId),
    ];
    for (const instance of ordered) {
      // An instance missing from settings was deleted and is about to be retired.
      const entry = entries[instance.instanceId];
      if (
        entry === undefined ||
        instance.driverKind !== ANTIGRAVITY ||
        !instance.enabled ||
        !instance.refreshModels
      ) {
        continue;
      }
      const config = yield* decodeAntigravitySettings(entry.config ?? {}).pipe(Effect.option);
      if (Option.isNone(config) || config.value.binaryPath) continue;
      // A sign-out or disable interrupts this instance's check; the others still need theirs.
      yield* instance.refreshModels().pipe(Effect.ignoreCause({ log: true }));
      yield* providers.refreshInstance(instance.instanceId);
    }
  });

  const start = Effect.fn("ProviderInstallation.start")(function* (input: ProviderSetupInput) {
    const { installation, driver } = yield* requireInstance(input.instanceId, "install", true);
    const started = yield* installation.start.pipe(Effect.mapError(failure(input.instanceId)));
    if (
      driver === ANTIGRAVITY &&
      started.operationId !== null &&
      accountCheckOperation !== started.operationId
    ) {
      accountCheckOperation = started.operationId;
      // Installation outlives the request and may finish while every client is disconnected.
      yield* installation.changes.pipe(
        Stream.filter(
          (state) =>
            state.operationId !== started.operationId ||
            state.phase === "succeeded" ||
            state.phase === "failed" ||
            state.phase === "cancelled",
        ),
        Stream.runHead,
        Effect.flatMap((result) =>
          Option.isSome(result) &&
          result.value.operationId === started.operationId &&
          result.value.phase === "succeeded"
            ? FiberHandle.run(
                managedRuntimeCheck,
                checkManagedRuntimeInstances(input.instanceId).pipe(Effect.ignore({ log: true })),
              )
            : Effect.void,
        ),
        Effect.ignore({ log: true }),
        Effect.forkIn(scope),
      );
    }
    return started;
  });

  const cancel = Effect.fn("ProviderInstallation.cancel")(function* (
    input: ProviderInstallCancelInput,
  ) {
    const { installation } = yield* requireInstance(input.instanceId, "cancel-install");
    return yield* installation
      .cancel(input.operationId)
      .pipe(Effect.mapError(failure(input.instanceId)));
  });

  // Watching an installation is when its owner can act on an update, so the
  // lookup runs then, in the background, unless update checks are off.
  const subscribe = (input: ProviderSetupInput) =>
    Stream.unwrap(
      requireInstance(input.instanceId, "observe-install").pipe(
        Effect.tap(({ installation }) =>
          settings.getSettings.pipe(
            Effect.flatMap((current) =>
              current.enableProviderUpdateChecks
                ? Effect.forkDetach(installation.checkForUpdate)
                : Effect.void,
            ),
            Effect.ignore,
          ),
        ),
        Effect.map(({ installation }) => installation.changes),
      ),
    );

  const remove = Effect.fn("ProviderInstallation.remove")(function* (input: ProviderSetupInput) {
    const { installation, driver } = yield* requireInstance(
      input.instanceId,
      "remove-install",
      true,
    );
    const entries = yield* readEntries(input.instanceId, "remove-install");
    const protectedPaths = yield* configuredBinaryPaths(Object.values(entries)).pipe(
      Effect.provideService(FileSystem.FileSystem, fileSystem),
      Effect.provideService(Path.Path, path),
    );
    yield* installation.remove(protectedPaths).pipe(Effect.mapError(failure(input.instanceId)));
    const allInstances = yield* instances.listInstances;
    yield* Effect.forEach(
      allInstances.filter((instance) => instance.driverKind === driver),
      (instance) => providers.refreshInstance(instance.instanceId),
      { discard: true },
    );
    return yield* installation.state;
  });

  return { start, cancel, subscribe, remove };
});

export const layer = Layer.effect(ProviderInstallation, make());
