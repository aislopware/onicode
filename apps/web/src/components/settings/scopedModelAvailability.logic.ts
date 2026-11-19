import {
  type ProviderInstanceId,
  type ServerProvider,
  textGeneratorForSelection,
  type UnifiedSettings,
} from "@onicode/contracts";

import { mergeEnvironmentSettings } from "../../hooks/useSettings";
import {
  getCustomModelOptionsByInstance,
  getTextGenerationModelOptionsByInstance,
} from "../../modelSelection";
import {
  applyProviderInstanceSettings,
  deriveProviderInstanceEntries,
  type ProviderInstanceEntry,
} from "../../providerInstances";
import type { ScopedSettingsTarget } from "./scopedSettings";

export function createScopedModelDisabledReason(input: {
  readonly settings: UnifiedSettings;
  readonly entries: readonly ProviderInstanceEntry[];
  readonly targets: readonly ScopedSettingsTarget[];
  readonly environments: ReadonlyArray<{
    readonly environmentId: string;
    readonly label: string;
    readonly serverConfig?: unknown;
  }>;
  readonly providersByEnvironment: ReadonlyMap<string, ReadonlyArray<ServerProvider>>;
  readonly textGeneration: boolean;
  readonly unavailableMessage: (environmentLabel: string | undefined) => string;
}): (instanceId: ProviderInstanceId, model: string) => string | null {
  const { settings, entries, targets, environments, providersByEnvironment } = input;
  const optionsForInstance = input.textGeneration
    ? getTextGenerationModelOptionsByInstance
    : getCustomModelOptionsByInstance;
  // The picker asks once per row, so each target's catalog is read once, not once per row.
  const catalogs = new Map<
    ScopedSettingsTarget,
    {
      readonly entryById: ReadonlyMap<ProviderInstanceId, ProviderInstanceEntry>;
      readonly availableSlugsById: ReadonlyMap<ProviderInstanceId, ReadonlySet<string>>;
    }
  >();
  const catalogFor = (target: ScopedSettingsTarget, providers: ReadonlyArray<ServerProvider>) => {
    const cached = catalogs.get(target);
    if (cached) return cached;
    const entryById = new Map<ProviderInstanceId, ProviderInstanceEntry>();
    for (const entry of applyProviderInstanceSettings(
      deriveProviderInstanceEntries(providers),
      target.settings,
      target.projectId,
    )) {
      if (!entryById.has(entry.instanceId)) entryById.set(entry.instanceId, entry);
    }
    const availableSlugsById = new Map(
      [...optionsForInstance(mergeEnvironmentSettings(target.settings, settings), providers)].map(
        ([instanceId, options]) =>
          [
            instanceId,
            new Set(options.flatMap((option) => (option.isUnavailable ? [] : [option.slug]))),
          ] as const,
      ),
    );
    const catalog = { entryById, availableSlugsById };
    catalogs.set(target, catalog);
    return catalog;
  };

  return (instanceId, model) => {
    const sourceEntry = entries.find((entry) => entry.instanceId === instanceId);
    const shownGenerator = textGeneratorForSelection(settings, instanceId);
    for (const candidate of targets) {
      const environment = environments.find(
        (entry) => entry.environmentId === candidate.environmentId,
      );
      const generator = textGeneratorForSelection(candidate.settings, instanceId);
      if (shownGenerator !== undefined || generator !== undefined) {
        // Only the shown environment's model list is loaded; elsewhere it must reach the same API.
        if (
          !generator?.enabled ||
          generator.kind !== shownGenerator?.kind ||
          generator.baseUrl !== shownGenerator.baseUrl
        ) {
          return input.unavailableMessage(environment?.label);
        }
        continue;
      }
      const providers = providersByEnvironment.get(candidate.environmentId);
      if (!environment?.serverConfig || !providers) continue;
      const catalog = catalogFor(candidate, providers);
      const entry = catalog.entryById.get(instanceId);
      if (
        !entry?.enabled ||
        !entry.isAvailable ||
        entry.driverKind !== sourceEntry?.driverKind ||
        !catalog.availableSlugsById.get(instanceId)?.has(model)
      ) {
        return input.unavailableMessage(environment?.label);
      }
    }
    return null;
  };
}
