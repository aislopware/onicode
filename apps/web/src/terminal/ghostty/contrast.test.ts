import { describe, expect, it } from "vite-plus/test";

import { ensureContrast, isContrastExemptText } from "./contrast";

const white = { r: 255, g: 255, b: 255 };
const dark = { r: 14, g: 18, b: 24 };

describe("ensureContrast", () => {
  it.each([
    [
      "pastel yellow on white darkens and keeps its hue",
      { r: 255, g: 255, b: 128 },
      white,
      4.5,
      { r: 121, g: 121, b: 61 },
    ],
    [
      "pastel green on white darkens and keeps its hue",
      { r: 138, g: 255, b: 128 },
      white,
      4.5,
      { r: 71, g: 132, b: 66 },
    ],
    [
      "a near miss moves only slightly",
      { r: 121, g: 112, b: 169 },
      white,
      4.5,
      { r: 120, g: 111, b: 168 },
    ],
    [
      "ANSI black on a dark theme lightens",
      { r: 0, g: 0, b: 0 },
      dark,
      4.5,
      { r: 125, g: 125, b: 125 },
    ],
    [
      "a softer floor moves less",
      { r: 255, g: 255, b: 128 },
      white,
      2.25,
      { r: 177, g: 177, b: 89 },
    ],
    [
      "the other direction is used when the preferred one cannot reach",
      { r: 120, g: 120, b: 120 },
      { r: 119, g: 119, b: 119 },
      4.5,
      { r: 6, g: 6, b: 6 },
    ],
  ])("%s", (_name, foreground, background, ratio, expected) => {
    expect(ensureContrast(foreground, background, ratio)).toEqual(expected);
  });

  it("returns legible colors untouched", () => {
    const foreground = { r: 28, g: 33, b: 41 };
    expect(ensureContrast(foreground, white, 4.5)).toBe(foreground);
  });
});

describe("isContrastExemptText", () => {
  it.each([
    ["", true],
    ["", true],
    ["█", true],
    ["▄", true],
    ["─", false],
    ["a", false],
  ])("%s -> %s", (text, exempt) => {
    expect(isContrastExemptText(text)).toBe(exempt);
  });
});
