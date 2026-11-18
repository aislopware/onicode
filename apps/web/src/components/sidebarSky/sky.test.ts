import { describe, expect, it } from "vite-plus/test";

import { moonPhaseAt, skyBodyAt, skyEventsAt, skyPaletteAt, type SkyEventKind } from "./sky";

/** Every kind of event that is in the air at some point in a stretch of local time. */
function kindsBetween(from: Date, hours: number): Set<SkyEventKind> {
  const kinds = new Set<SkyEventKind>();
  const start = from.getTime() / 1000;
  for (let seconds = 0; seconds < hours * 3600; seconds += 4) {
    for (const event of skyEventsAt(start + seconds)) kinds.add(event.kind);
  }
  return kinds;
}

describe("sky", () => {
  it("joins midnight to midnight and keeps night and day apart", () => {
    expect(skyPaletteAt(24)).toEqual(skyPaletteAt(0));
    expect(skyPaletteAt(-1)).toEqual(skyPaletteAt(23));
    expect(skyPaletteAt(1).night).toBe(1);
    expect(skyPaletteAt(13).night).toBe(0);
    expect(skyBodyAt(12.25)).toEqual({ sun: true, progress: 0.5 });
    expect(skyBodyAt(0.25)).toEqual({ sun: false, progress: 0.5 });
  });

  it("knows the moon's phase", () => {
    expect(moonPhaseAt(Date.UTC(2000, 0, 6, 18, 14))).toBe(0);
    // The full moon of 17 October 2024, 11:26 UTC.
    expect(moonPhaseAt(Date.UTC(2024, 9, 17, 11, 26))).toBeCloseTo(0.5, 1);
  });

  it("gives a moment the same events every time it is asked", () => {
    const dusk = new Date(2026, 5, 10, 19, 0).getTime() / 1000;
    for (let seconds = 0; seconds < 1800; seconds += 7) {
      expect(skyEventsAt(dusk + seconds)).toEqual(skyEventsAt(dusk + seconds));
    }
  });

  it("keeps each event to its hours and season", () => {
    const summerNight = kindsBetween(new Date(2026, 5, 10, 23, 0), 4);
    expect(summerNight).toContain("meteor");
    expect([...summerNight].filter((kind) => kind !== "meteor" && kind !== "aurora")).toEqual(
      expect.not.arrayContaining(["flock", "balloon", "petals", "leaves", "snow", "fireworks"]),
    );

    const summerDay = kindsBetween(new Date(2026, 5, 10, 9, 0), 8);
    expect(summerDay).toContain("flock");
    expect(summerDay).toContain("wind");
    for (const kind of ["meteor", "satellite", "aurora", "snow", "petals", "leaves"] as const) {
      expect(summerDay).not.toContain(kind);
    }

    expect(kindsBetween(new Date(2026, 5, 10, 19, 0), 3)).toContain("fireflies");
    expect(kindsBetween(new Date(2026, 2, 20, 9, 0), 3)).toContain("petals");
    expect(kindsBetween(new Date(2026, 9, 20, 9, 0), 3)).toContain("leaves");
    expect(kindsBetween(new Date(2026, 0, 20, 9, 0), 6)).toContain("snow");
  });

  it("celebrates the new year and the lantern nights", () => {
    expect(kindsBetween(new Date(2027, 0, 1, 0, 0), 0.2)).toContain("fireworks");
    expect(kindsBetween(new Date(2027, 0, 1, 0, 30), 2)).not.toContain("fireworks");
    // Lunar new year 2026 falls on 17 February; mid-autumn 2026 on 25 September.
    expect(kindsBetween(new Date(2026, 1, 17, 0, 0), 0.2)).toContain("fireworks");
    expect(kindsBetween(new Date(2026, 8, 25, 19, 30), 1)).toContain("lanterns");
    expect(kindsBetween(new Date(2026, 8, 24, 19, 30), 1)).not.toContain("lanterns");
  });
});
