import { describe, expect, it } from "@effect/vitest";
import * as Clock from "effect/Clock";
import * as Effect from "effect/Effect";
import * as FiberSet from "effect/FiberSet";
import * as Queue from "effect/Queue";
import * as TestClock from "effect/testing/TestClock";

import {
  killProcessDescendant,
  makeProcessDescendantPoll,
  makeSharedProcessDescendantPoll,
  parseProcessTable,
  type ProcessTableRow,
} from "./processDescendants.ts";

describe("processDescendants", () => {
  it("parses ps rows with multi-word lstart timestamps and drops malformed lines", () => {
    const rows = parseProcessTable(
      [
        "  78661   1234  78661 Mon Nov 23 17:00:00 2026",
        "  81100  78661  81100 Mon Nov 23 17:00:05 2026",
        "",
        "not-a-pid 1 1 Mon Nov 23 17:00:00 2026",
        "90000 1 90000",
      ].join("\n"),
    );

    expect(rows).toEqual([
      { pid: 78661, ppid: 1234, pgid: 78661, started: "Mon Nov 23 17:00:00 2026" },
      { pid: 81100, ppid: 78661, pgid: 81100, started: "Mon Nov 23 17:00:05 2026" },
    ]);
  });

  it("signals the negative process group for group leaders, exact pid otherwise, and skips invalid pids", () => {
    const sent: Array<readonly [number, NodeJS.Signals]> = [];
    const signal = (pid: number, sig: NodeJS.Signals) => {
      sent.push([pid, sig]);
      if (pid === 81102) throw Object.assign(new Error("ESRCH"), { code: "ESRCH" });
    };

    killProcessDescendant(
      { pid: 81100, ppid: 1, pgid: 81100, started: "Mon Nov 23 17:00:05 2026" },
      signal,
    );
    killProcessDescendant(
      { pid: 81101, ppid: 1, pgid: 81100, started: "Mon Nov 23 17:00:06 2026" },
      signal,
    );
    killProcessDescendant(
      { pid: 81102, ppid: 1, pgid: 78661, started: "Mon Nov 23 17:00:07 2026" },
      signal,
    );
    killProcessDescendant(
      { pid: 1, ppid: 0, pgid: 1, started: "Mon Nov 23 17:00:00 2026" },
      signal,
    );
    killProcessDescendant(
      { pid: 0, ppid: 0, pgid: 0, started: "Mon Nov 23 17:00:00 2026" },
      signal,
    );

    expect(sent).toEqual([
      [-81100, "SIGKILL"],
      [81101, "SIGKILL"],
      [81102, "SIGKILL"],
    ]);
  });

  it.effect("ignores refresh after leftovers so a reused root pid cannot be adopted", () => {
    let table: ReadonlyArray<ProcessTableRow> = [
      { pid: 1000, ppid: 10, pgid: 1000, started: "Mon Nov 23 17:00:00 2026" },
      { pid: 1100, ppid: 1000, pgid: 1100, started: "Mon Nov 23 17:00:01 2026" },
    ];
    let reads = 0;
    const tracker = makeProcessDescendantPoll(
      Effect.sync(() => {
        reads += 1;
        return table;
      }),
    ).track(1000);

    return Effect.gen(function* () {
      yield* tracker.refresh;
      table = [{ pid: 1100, ppid: 1, pgid: 1100, started: "Mon Nov 23 17:00:01 2026" }];
      expect((yield* tracker.leftovers).map((row) => row.pid)).toEqual([1100]);
      const readsAfterLeftovers = reads;

      table = [
        { pid: 1000, ppid: 10, pgid: 1000, started: "Mon Nov 23 17:00:09 2026" },
        { pid: 1200, ppid: 1000, pgid: 1200, started: "Mon Nov 23 17:00:10 2026" },
      ];
      yield* tracker.refresh;
      expect(reads).toBe(readsAfterLeftovers);
      expect([...tracker.tracked.values()].map((row) => row.pid)).toEqual([1100]);
    });
  });

  it.effect("polls a watched process slowly and a working one at once and often", () =>
    Effect.gen(function* () {
      const readAt = yield* Queue.unbounded<number>();
      const poll = makeSharedProcessDescendantPoll(
        Clock.currentTimeMillis.pipe(
          Effect.tap((now) => Queue.offer(readAt, now)),
          Effect.as([{ pid: 1000, ppid: 10, pgid: 1000, started: "Mon Nov 23 17:00:00 2026" }]),
        ),
        yield* FiberSet.makeRuntime(),
      );
      const first = poll.track(1000);
      first.watch();
      expect(yield* Queue.take(readAt)).toBe(0);
      yield* TestClock.adjust("10 seconds");
      expect(yield* Queue.take(readAt)).toBe(10_000);
      yield* TestClock.adjust("4 seconds");
      first.work();
      expect(yield* Queue.take(readAt)).toBe(14_000);
      yield* TestClock.adjust("1 second");
      expect(yield* Queue.take(readAt)).toBe(15_000);
      yield* first.stop;
      expect(yield* Queue.take(readAt)).toBe(15_000);
      // A frame after the exit does not hold the working cadence for the processes still watched.
      first.work();
      const second = poll.track(2000);
      second.watch();
      expect(yield* Queue.take(readAt)).toBe(15_000);
      yield* TestClock.adjust("10 seconds");
      expect(yield* Queue.take(readAt)).toBe(25_000);
      yield* second.settle;
      expect(yield* Queue.take(readAt)).toBe(25_000);
      yield* TestClock.adjust("20 seconds");
      expect(poll.hasLive()).toBe(false);
      expect(yield* Queue.size(readAt)).toBe(0);
    }),
  );
});
