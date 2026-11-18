import { EnvironmentId, ProviderInstanceId } from "@onicode/contracts";
import * as Schema from "effect/Schema";

import { getLocalStorageItem, setLocalStorageItem } from "~/hooks/useLocalStorage";

const RecentModels = Schema.Array(
  Schema.Struct({ instanceId: ProviderInstanceId, slug: Schema.String }),
);
export type RecentModel = (typeof RecentModels.Type)[number];

/** More than the picker shows, so starring or losing a model still leaves a full section. */
const STORED_LIMIT = 12;
const SHOWN_LIMIT = 4;

const storageKey = (environmentId: EnvironmentId) =>
  `onicode:model-picker:recent:v1:${environmentId}`;

/** The models picked on this device, newest first. Provider instances belong to one environment. */
export function readRecentModels(environmentId: EnvironmentId | null): ReadonlyArray<RecentModel> {
  if (environmentId === null) return [];
  try {
    return getLocalStorageItem(storageKey(environmentId), RecentModels) ?? [];
  } catch {
    return [];
  }
}

export function rememberRecentModel(environmentId: EnvironmentId | null, model: RecentModel) {
  if (environmentId === null) return;
  const rest = readRecentModels(environmentId).filter(
    (recent) => recent.instanceId !== model.instanceId || recent.slug !== model.slug,
  );
  try {
    setLocalStorageItem(
      storageKey(environmentId),
      [model, ...rest].slice(0, STORED_LIMIT),
      RecentModels,
    );
  } catch {
    // Recents are a convenience; a full or blocked store only costs the list.
  }
}

/**
 * The Recent section: the newest picks that are not favorites and can still be picked. The
 * selected model always shows, taking the last place when it is older than the rest.
 */
export function recentModelsToShow<Model extends RecentModel>(input: {
  readonly recents: ReadonlyArray<RecentModel>;
  readonly active: RecentModel | null;
  readonly models: ReadonlyArray<Model>;
  readonly isFavorite: (model: Model) => boolean;
}): Model[] {
  const find = (wanted: RecentModel) =>
    input.models.find(
      (model) =>
        model.instanceId === wanted.instanceId &&
        model.slug === wanted.slug &&
        !input.isFavorite(model),
    );
  const active = input.active ? find(input.active) : undefined;
  const recent = [...new Set(input.recents.flatMap((wanted) => find(wanted) ?? []))];
  // A model picked elsewhere, such as on another device, is the newest pick here too.
  const ordered = active && !recent.includes(active) ? [active, ...recent] : recent;
  const shown = ordered.slice(0, SHOWN_LIMIT);
  if (active && !shown.includes(active)) shown[shown.length - 1] = active;
  return shown;
}
