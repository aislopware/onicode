import { describe, expect, it } from "vite-plus/test";

import {
  competingBetween,
  descendantProcesses,
  describeFailure,
  parsePsProcessTable,
  percentile,
  unchangedProcesses,
} from "./report.ts";

describe("percentile", () => {
  it("takes the nearest-rank p90, a value that was measured", () => {
    expect(percentile([10, 20, 30, 40, 50, 60, 70, 80, 90, 100], 90)).toBe(90);
    expect(percentile([5, 1, 3], 90)).toBe(5);
    expect(percentile([], 90)).toBeNull();
  });
});

describe("competingBetween", () => {
  it.each([
    ["a process that kept running", [["10:500", 900]], [["10:500", 960]], 60],
    ["a new process, counted whole", [], [["11:700", 30]], 30],
    ["a reused pid is a new process", [["10:500", 900]], [["10:800", 300]], 300],
    ["a process that exited is lost", [["10:500", 900]], [], 0],
  ] as const)("%s", (_name, before, after, seconds) => {
    expect(competingBetween(new Map(before), new Map(after))).toBe(seconds);
  });
});

describe("describeFailure", () => {
  it.each([
    ["a plain error", new Error("boom"), "boom"],
    [
      "an RPC error wrapping the server's",
      new Error("RpcClientError", { cause: new Error("Command x was already handled") }),
      "RpcClientError: Command x was already handled",
    ],
    ["a tagged error with no message", { _tag: "TimeoutError" }, "TimeoutError"],
    [
      "a cause that repeats its wrapper",
      new Error("lost: gone", { cause: new Error("gone") }),
      "lost: gone",
    ],
    ["a string", "exit 1", "exit 1"],
  ])("%s", (_name, error, expected) => {
    expect(describeFailure(error)).toBe(expected);
  });
});

describe("process cleanup", () => {
  const table = parsePsProcessTable(
    [
      "  100     1 Mon Nov 24 10:00:00 2026",
      "  200   100 Mon Nov 24 10:00:01 2026",
      "  300   200 Mon Nov 24 10:00:02 2026",
      "  400     1 Mon Nov 24 10:00:03 2026",
      "",
    ].join("\n"),
  );

  it("lists a process's whole tree", () => {
    expect(descendantProcesses(table, 100).map(({ pid }) => pid)).toEqual([200, 300]);
  });

  it("kills only descendants still running as themselves", () => {
    const later = parsePsProcessTable(
      // 200 kept running after its parent exited; 300 exited and its PID went to a new process.
      ["  200     1 Mon Nov 24 10:00:01 2026", "  300   400 Mon Nov 24 10:05:00 2026"].join("\n"),
    );
    expect(unchangedProcesses(descendantProcesses(table, 100), later)).toEqual([200]);
  });
});
