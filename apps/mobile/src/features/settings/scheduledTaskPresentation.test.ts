import { describe, expect, it, vi } from "vite-plus/test";

import { formatNextScheduledTaskRun, formatScheduledTimeOfDay } from "./scheduledTaskPresentation";

// The host locale's clock for a wall time, so these pass in any locale.
function clock(hourCycle: "h12" | "h23", hour: number, minute: number) {
  return new Intl.DateTimeFormat(undefined, {
    hour: "numeric",
    minute: "2-digit",
    hourCycle,
    timeZone: "UTC",
  }).format(Date.UTC(2000, 0, 1, hour, minute));
}

describe("formatNextScheduledTaskRun", () => {
  it("names the rounded unit for a run later today", () => {
    const now = new Date(2026, 10, 3, 10, 0, 0).getTime();
    const at = (minutes: number) => new Date(now + minutes * 60_000).toISOString();
    expect(formatNextScheduledTaskRun(at(59), now)).toBe("Next run in 59 minutes");
    expect(formatNextScheduledTaskRun(at(59.5), now)).toBe("Next run in 1 hour");
    expect(formatNextScheduledTaskRun(at(150), now)).toBe("Next run in 3 hours");
  });

  it("names a later day's run time in the system clock", () => {
    const now = new Date(2026, 10, 3, 10, 0, 0).getTime();
    const tomorrow = new Date(2026, 10, 4, 17, 30);
    const time = tomorrow.toLocaleTimeString(undefined, {
      hour: "numeric",
      minute: "2-digit",
      hourCycle: "h23",
    });
    expect(formatNextScheduledTaskRun(tomorrow.toISOString(), now, undefined, true)).toBe(
      `Next run tomorrow at ${time}`,
    );
  });
});

describe("formatScheduledTimeOfDay", () => {
  it("follows the system clock and keeps midnight at 00", () => {
    expect(formatScheduledTimeOfDay("17:30", true)).toBe(clock("h23", 17, 30));
    expect(formatScheduledTimeOfDay("17:30", false)).toBe(clock("h12", 17, 30));
    expect(formatScheduledTimeOfDay("0:05", true)).toBe(clock("h23", 0, 5));
    expect(formatScheduledTimeOfDay("0:05", true)).not.toContain("24");
  });

  it("keeps a wall time that today's DST gap skips", () => {
    const previous = process.env.TZ;
    // 2:30 AM does not exist in New York on 2026-03-08, when clocks spring forward.
    vi.useFakeTimers({ now: Date.parse("2026-03-08T12:00:00.000Z") });
    try {
      process.env.TZ = "America/New_York";
      expect(formatScheduledTimeOfDay("02:30", false)).toBe(clock("h12", 2, 30));
    } finally {
      vi.useRealTimers();
      if (previous === undefined) delete process.env.TZ;
      else process.env.TZ = previous;
    }
  });
});
