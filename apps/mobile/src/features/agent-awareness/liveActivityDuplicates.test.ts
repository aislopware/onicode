import { describe, expect, it } from "vite-plus/test";

import { duplicateActivityIds } from "./liveActivityDuplicates";

describe("duplicateActivityIds", () => {
  it("keeps the freshest card of each environment", () => {
    const card = (id: string, environmentId: string, minute: number) => ({
      id,
      environmentId,
      updatedAt: `2026-11-12T13:${minute}:00.000Z`,
    });

    expect([
      ...duplicateActivityIds([
        card("first", "mac-mini", 38),
        card("updated", "mac-mini", 44),
        card("second", "mac-mini", 39),
        card("same-time", "mac-mini", 44),
        card("other", "macbook", 10),
      ]),
    ]).toEqual(["first", "second", "same-time"]);
    expect(duplicateActivityIds([card("only", "mac-mini", 38)]).size).toBe(0);
  });
});
