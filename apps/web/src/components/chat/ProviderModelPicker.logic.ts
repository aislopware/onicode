import {
  ANTIGRAVITY_DEFAULT_MODEL,
  type ProviderDriverKind,
  type ProviderInstanceId,
} from "@onicode/contracts";

import { isProviderInstancePickerReady, type ProviderInstanceEntry } from "../../providerInstances";
import { hasRuntimeModelCatalog } from "../../providerModels";
import type { ModelEsque } from "./providerIconUtils";

/** Keeps a hidden model visible while its provider is locked or turned off. */
export function retainedModelWhileProviderLocked(
  entry: {
    readonly driverKind: ProviderDriverKind;
    readonly enabled: boolean;
    readonly isAvailable: boolean;
    readonly models: ReadonlyArray<Pick<ModelEsque, "slug" | "name" | "shortName" | "subProvider">>;
  } | null,
  model: string,
): ModelEsque | undefined {
  if (!entry || (entry.enabled && entry.isAvailable) || !model) return undefined;
  if (entry.driverKind === "opencode" || entry.driverKind === "antigravity") return undefined;
  return entry.models.find((candidate) => candidate.slug === model) ?? { slug: model, name: model };
}

export function modelPickerOptionsForInstance<Option extends ModelEsque>(input: {
  readonly entry: ProviderInstanceEntry;
  readonly options: ReadonlyArray<Option>;
  readonly activeInstanceId: ProviderInstanceId;
  readonly activeModel: string;
}): ReadonlyArray<Option> {
  const { entry } = input;
  const ready = isProviderInstancePickerReady(entry);
  return input.options.filter((option) => {
    if (entry.driverKind === "antigravity" && option.slug === ANTIGRAVITY_DEFAULT_MODEL) {
      return false;
    }
    if (ready) return true;
    return (
      entry.enabled &&
      hasRuntimeModelCatalog(entry.driverKind) &&
      entry.instanceId === input.activeInstanceId &&
      option.slug === input.activeModel &&
      option.isUnavailable === true
    );
  });
}
