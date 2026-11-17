import { describe, expect, it } from "vite-plus/test";

import { shardScenarios } from "./scenario-shards.ts";

describe("shardScenarios", () => {
  it("puts every file in exactly one shard and spreads the slow ones", () => {
    const files = ["a", "b", "c", "d", "e", "f", "g"].map((name) => `scenarios/${name}.e2e.ts`);
    const weights = {
      "scenarios/a.e2e.ts": 90,
      "scenarios/b.e2e.ts": 40,
      "scenarios/c.e2e.ts": 40,
    };
    const shards = [1, 2, 3].map((index) => shardScenarios(files, weights, `${index}/3`));
    expect(shards.flat().sort()).toEqual(files);
    expect(shards[0]).toEqual(["scenarios/a.e2e.ts"]);
    expect(shards[1]).toContain("scenarios/b.e2e.ts");
    expect(shards[2]).toContain("scenarios/c.e2e.ts");
  });
});
