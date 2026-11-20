import { describe, expect, it } from "vite-plus/test";

import {
  isStoreUpdateSnoozed,
  parseMinimumSupportedVersion,
  parseStoreBuildFeed,
  resolveStoreUpdateOffer,
  type StoreFeed,
} from "./store-update.logic";

const feed = (
  latest: StoreFeed["latest"],
  minimumSupportedVersion: string | null = null,
): StoreFeed => ({ latest, minimumSupportedVersion });

describe("resolveStoreUpdateOffer", () => {
  const running = { appVersion: "0.4.2", runtimeVersion: "old" };

  it.each([
    ["no store build", feed(null), { kind: "none" }],
    [
      "a newer store build on another runtime",
      feed({ version: "0.5.0", runtimeVersion: "new" }),
      { kind: "available", version: "0.5.0" },
    ],
    [
      "a newer version this binary gets over the air",
      feed({ version: "0.5.0", runtimeVersion: "old" }),
      { kind: "none" },
    ],
    [
      "a store build that is not newer",
      feed({ version: "0.4.2", runtimeVersion: "new" }),
      { kind: "none" },
    ],
    [
      "an unsupported version with a supported store build",
      feed({ version: "0.5.0", runtimeVersion: "new" }, "0.5.0"),
      { kind: "required", via: "store", version: "0.5.0" },
    ],
    [
      "an unsupported version whose binary is current",
      feed({ version: "0.5.0", runtimeVersion: "old" }, "0.5.0"),
      { kind: "required", via: "ota" },
    ],
    [
      "an unsupported version whose only store build is unsupported too",
      feed({ version: "0.4.5", runtimeVersion: "new" }, "0.5.0"),
      { kind: "available", version: "0.4.5" },
    ],
    [
      "a supported version",
      feed({ version: "0.5.0", runtimeVersion: "new" }, "0.4.0"),
      { kind: "available", version: "0.5.0" },
    ],
  ] as const)("offers the right update for %s", (_name, storeFeed, offer) => {
    expect(resolveStoreUpdateOffer({ ...running, feed: storeFeed })).toEqual(offer);
  });
});

describe("store update feeds", () => {
  it("reads the store build and ignores anything that is not one", () => {
    expect(
      parseStoreBuildFeed('{"version":"0.5.0","buildNumber":"500","runtimeVersion":"abc"}'),
    ).toEqual({ version: "0.5.0", runtimeVersion: "abc" });
    expect(parseStoreBuildFeed('{"version":"0.5.0","runtimeVersion":null}')).toEqual({
      version: "0.5.0",
      runtimeVersion: null,
    });
    for (const body of [null, "<html>", '{"version":"latest"}', "null"]) {
      expect(parseStoreBuildFeed(body)).toBeNull();
    }
  });

  it("reads the oldest supported version from the desktop feed", () => {
    const yml = 'version: 0.5.1\npath: Onicode.zip\nminimumSupportedVersion: "0.5.0"\n';
    expect(parseMinimumSupportedVersion(yml)).toBe("0.5.0");
    expect(parseMinimumSupportedVersion("version: 0.5.1\npath: Onicode.zip\n")).toBeNull();
    expect(parseMinimumSupportedVersion("minimumSupportedVersion: soon\n")).toBeNull();
    expect(parseMinimumSupportedVersion(null)).toBeNull();
  });

  it("snoozes one version until its time is up", () => {
    const snooze = { version: "0.5.0", until: 1_000 };
    expect(isStoreUpdateSnoozed(snooze, "0.5.0", 999)).toBe(true);
    expect(isStoreUpdateSnoozed(snooze, "0.5.0", 1_000)).toBe(false);
    expect(isStoreUpdateSnoozed(snooze, "0.5.1", 999)).toBe(false);
    expect(isStoreUpdateSnoozed(null, "0.5.0", 0)).toBe(false);
  });
});
