import { describe, expect, it } from "vite-plus/test";

import { effortHue, effortTone } from "./EffortSliderField";

describe("effortTone", () => {
  it.each([
    { name: "the default level is the accent", index: 1, last: 4, defaultIndex: 1, tone: 0.5 },
    {
      name: "levels under the default share the fast half",
      index: 0,
      last: 4,
      defaultIndex: 2,
      tone: 0,
    },
    {
      name: "levels over the default share the smart half",
      index: 3,
      last: 4,
      defaultIndex: 1,
      tone: 0.5 + 1 / 3,
    },
    { name: "the top level is the smart end", index: 4, last: 4, defaultIndex: 1, tone: 1 },
    {
      name: "a default at the fast end spreads the levels evenly",
      index: 1,
      last: 4,
      defaultIndex: 0,
      tone: 0.25,
    },
    {
      name: "a default at the smart end spreads the levels evenly",
      index: 2,
      last: 4,
      defaultIndex: 4,
      tone: 0.5,
    },
    {
      name: "no default spreads the levels evenly",
      index: 3,
      last: 4,
      defaultIndex: -1,
      tone: 0.75,
    },
    {
      name: "a level the slider does not offer sits at the fast end",
      index: -1,
      last: 4,
      defaultIndex: 1,
      tone: 0,
    },
    { name: "a single level is the accent", index: 0, last: 0, defaultIndex: 0, tone: 0.5 },
  ])("$name", ({ index, last, defaultIndex, tone }) => {
    expect(effortTone(index, last, defaultIndex)).toBeCloseTo(tone);
  });

  it("keeps the accent's own hue at the default level and turns away from it on both sides", () => {
    expect(effortHue(0.5)).toBeCloseTo(0);
    expect(effortHue(0)).toBeLessThan(0);
    expect(effortHue(1)).toBeGreaterThan(0);
  });
});
