import { compareSemverVersions, parseSemver } from "@onicode/shared/semver";

/** What the release feeds say about this platform's store binary. */
export interface StoreFeed {
  /** The newest build the store has; null when the feed lists none. */
  readonly latest: { readonly version: string; readonly runtimeVersion: string | null } | null;
  /** Versions below this one are no longer supported; null when every version is. */
  readonly minimumSupportedVersion: string | null;
}

export type StoreUpdateOffer =
  | { readonly kind: "none" }
  | { readonly kind: "available"; readonly version: string }
  | { readonly kind: "required"; readonly via: "store"; readonly version: string }
  /** The binary is current, so an over-the-air update is what lifts the app to a supported version. */
  | { readonly kind: "required"; readonly via: "ota" };

export interface StoreUpdateSnooze {
  readonly version: string;
  readonly until: number;
}

export const STORE_UPDATE_SNOOZE_MS = 4 * 60 * 60 * 1000;

function isBelow(version: string, other: string): boolean {
  return (
    parseSemver(version) !== null &&
    parseSemver(other) !== null &&
    compareSemverVersions(version, other) < 0
  );
}

export function resolveStoreUpdateOffer(input: {
  readonly appVersion: string;
  /** The running binary's runtime version; a store build with another one cannot arrive over the air. */
  readonly runtimeVersion: string | null;
  readonly feed: StoreFeed;
}): StoreUpdateOffer {
  const { appVersion, runtimeVersion, feed } = input;
  const { latest, minimumSupportedVersion } = feed;
  const storeVersion =
    latest !== null &&
    isBelow(appVersion, latest.version) &&
    (latest.runtimeVersion === null || latest.runtimeVersion !== runtimeVersion)
      ? latest.version
      : null;
  if (minimumSupportedVersion !== null && isBelow(appVersion, minimumSupportedVersion)) {
    if (storeVersion === null) return { kind: "required", via: "ota" };
    // A store build that is itself unsupported would only trade one blocked app for another.
    if (!isBelow(storeVersion, minimumSupportedVersion)) {
      return { kind: "required", via: "store", version: storeVersion };
    }
  }
  return storeVersion === null ? { kind: "none" } : { kind: "available", version: storeVersion };
}

export function isStoreUpdateSnoozed(
  snooze: StoreUpdateSnooze | null,
  version: string,
  now: number,
): boolean {
  return snooze !== null && snooze.version === version && now < snooze.until;
}

export function parseStoreBuildFeed(body: string | null): StoreFeed["latest"] {
  if (body === null) return null;
  try {
    const value: unknown = JSON.parse(body);
    if (typeof value !== "object" || value === null) return null;
    const { version, runtimeVersion } = value as Record<string, unknown>;
    if (typeof version !== "string" || parseSemver(version) === null) return null;
    return { version, runtimeVersion: typeof runtimeVersion === "string" ? runtimeVersion : null };
  } catch {
    return null;
  }
}

/** Reads the one key this app needs from the desktop release feed, which shares its version. */
export function parseMinimumSupportedVersion(body: string | null): string | null {
  if (body === null) return null;
  const version = /^minimumSupportedVersion:\s*["']?([^"'\s]+)["']?\s*$/mu.exec(body)?.[1];
  return version !== undefined && parseSemver(version) !== null ? version : null;
}
