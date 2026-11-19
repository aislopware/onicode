import type { ProviderInstanceId } from "@onicode/contracts";

const MODEL_KEY_PREFIX = "model:";
const LEGACY_SECTION_KEY_PREFIX = "legacy-models:";
const FAVORITE_KEY_PREFIX = "favorite:";
const RECENT_KEY_PREFIX = "recent:";
const SUB_PROVIDER_SECTION_KEY_PREFIX = "sub-provider:";
/** The row that opens the whole catalogue under Favorites and Recent. */
export const MODEL_PICKER_ALL_MODELS_KEY = "all-models";

export function modelPickerModelKey(instanceId: ProviderInstanceId, slug: string): string {
  return `${MODEL_KEY_PREFIX}${instanceId.length}:${instanceId}${slug}`;
}

/** A model's row in Favorites, apart from its row in its provider's section. */
export function modelPickerFavoriteKey(instanceId: ProviderInstanceId, slug: string): string {
  return `${FAVORITE_KEY_PREFIX}${modelPickerModelKey(instanceId, slug)}`;
}

/** A model's row in Recent, apart from its row in its provider's section. */
export function modelPickerRecentKey(instanceId: ProviderInstanceId, slug: string): string {
  return `${RECENT_KEY_PREFIX}${modelPickerModelKey(instanceId, slug)}`;
}

/** The model a row picks, from its provider row or its Favorites or Recent row. */
export function parseModelPickerModelKey(
  rowKey: string,
): { instanceId: ProviderInstanceId; slug: string } | null {
  const key = rowKey.startsWith(FAVORITE_KEY_PREFIX)
    ? rowKey.slice(FAVORITE_KEY_PREFIX.length)
    : rowKey.startsWith(RECENT_KEY_PREFIX)
      ? rowKey.slice(RECENT_KEY_PREFIX.length)
      : rowKey;
  if (!key.startsWith(MODEL_KEY_PREFIX)) {
    return null;
  }
  const encoded = key.slice(MODEL_KEY_PREFIX.length);
  const separatorIndex = encoded.indexOf(":");
  if (separatorIndex === -1) {
    return null;
  }

  const instanceIdLengthText = encoded.slice(0, separatorIndex);
  if (!/^\d+$/.test(instanceIdLengthText)) {
    return null;
  }

  const instanceIdLength = Number(instanceIdLengthText);
  const value = encoded.slice(separatorIndex + 1);
  if (!Number.isSafeInteger(instanceIdLength) || instanceIdLength > value.length) {
    return null;
  }

  return {
    instanceId: value.slice(0, instanceIdLength) as ProviderInstanceId,
    slug: value.slice(instanceIdLength),
  };
}

export function modelPickerLegacySectionKey(instanceId: ProviderInstanceId): string {
  return `${LEGACY_SECTION_KEY_PREFIX}${instanceId}`;
}

export function parseModelPickerLegacySectionKey(key: string): ProviderInstanceId | null {
  return key.startsWith(LEGACY_SECTION_KEY_PREFIX)
    ? (key.slice(LEGACY_SECTION_KEY_PREFIX.length) as ProviderInstanceId)
    : null;
}

export function modelPickerSubProviderSectionKey(
  instanceId: ProviderInstanceId,
  subProvider: string,
): string {
  return `${SUB_PROVIDER_SECTION_KEY_PREFIX}${instanceId.length}:${instanceId}${subProvider}`;
}
