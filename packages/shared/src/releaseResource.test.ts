import { describe, expect, it } from "@effect/vitest";

import { releaseResourceAttributes } from "./releaseResource.ts";

describe("releaseResourceAttributes", () => {
  it.each([
    ["dev", {}, "dev", "development"],
    ["stable", {}, "stable", "production"],
    [
      "stable",
      { "deployment.environment.name": "staging", "onicode.release.channel": "" },
      "stable",
      "staging",
    ],
  ] as const)(
    "%s (operator: %o) reports %s in %s",
    (channel, operatorAttributes, expectedChannel, environment) => {
      expect(releaseResourceAttributes(channel, operatorAttributes)).toEqual({
        "onicode.release.channel": expectedChannel,
        "deployment.environment.name": environment,
      });
    },
  );
});
