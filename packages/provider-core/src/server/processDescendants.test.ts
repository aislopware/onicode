import { describe, expect, it } from "@effect/vitest";
import * as Effect from "effect/Effect";

import {
  killProcessDescendant,
  makeProcessDescendantPoll,
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
});
