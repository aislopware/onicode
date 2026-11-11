import * as NodeServices from "@effect/platform-node/NodeServices";
import { describe, expect, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as ChildProcessSpawner from "effect/process/ChildProcessSpawner";

import * as ProcessRunner from "../processRunner.ts";
import * as DirectorySearch from "./DirectorySearch.ts";

type Tool = (input: ProcessRunner.ProcessRunInput) => { code: number; stdout: string } | "missing";

const ZOXIDE_LIST = "/home/u/work/onicode\n/home/u/notes\n/home/u/work/oni-docs\n";

const find = (tool: Tool, query: string, limit?: number) => {
  const calls: Array<ProcessRunner.ProcessRunInput> = [];
  const runner = ProcessRunner.ProcessRunner.of({
    run: (input) => {
      if (input.args[0] !== "--version") calls.push(input);
      const result = tool(input);
      return result === "missing"
        ? Effect.fail(
            new ProcessRunner.ProcessSpawnError({
              command: input.command,
              argumentCount: input.args.length,
              cause: new Error("ENOENT"),
            }),
          )
        : Effect.succeed({
            stdout: result.stdout,
            stderr: "",
            code: ChildProcessSpawner.ExitCode(result.code),
            timedOut: false,
            stdoutTruncated: false,
            stderrTruncated: false,
            stdoutInvalidUtf8: false,
            stderrInvalidUtf8: false,
          });
    },
  });
  return Effect.gen(function* () {
    const search = yield* DirectorySearch.DirectorySearch;
    const result = yield* search.find({ query, ...(limit === undefined ? {} : { limit }) });
    const isAvailable = yield* search.isAvailable;
    return { result, calls, isAvailable };
  }).pipe(
    Effect.provide(
      DirectorySearch.layer.pipe(
        Layer.provide(Layer.succeed(ProcessRunner.ProcessRunner, runner)),
        Layer.provide(NodeServices.layer),
      ),
    ),
  );
};

describe("DirectorySearch.find", () => {
  it.effect("ranks the zoxide database with fzf", () =>
    Effect.gen(function* () {
      const { result, calls, isAvailable } = yield* find(
        (input) =>
          input.command === "zoxide"
            ? { code: 0, stdout: ZOXIDE_LIST }
            : { code: 0, stdout: "/home/u/work/oni-docs\n/home/u/work/onicode\n" },
        "oni",
        1,
      );
      expect(result.entries).toEqual([{ name: "oni-docs", fullPath: "/home/u/work/oni-docs" }]);
      expect(calls[1]).toMatchObject({ command: "fzf", stdin: ZOXIDE_LIST });
      expect(isAvailable).toBe(true);
      expect(calls[1]?.args[0]).toBe("--filter=oni");
      expect(calls[1]?.env).toEqual({ FZF_DEFAULT_OPTS: "", FZF_DEFAULT_OPTS_FILE: "" });
    }),
  );

  it.effect("finds nothing when fzf matches nothing, without asking zoxide again", () =>
    Effect.gen(function* () {
      const { result, calls } = yield* find(
        (input) =>
          input.command === "zoxide" ? { code: 0, stdout: ZOXIDE_LIST } : { code: 1, stdout: "" },
        "zzz",
      );
      expect(result.entries).toEqual([]);
      expect(calls).toHaveLength(2);
    }),
  );

  it.effect("falls back to zoxide's keyword match when fzf is missing or fails", () =>
    Effect.gen(function* () {
      for (const fzf of ["missing", { code: 2, stdout: "" }] as const) {
        const { result, calls } = yield* find((input) => {
          if (input.command === "fzf") return fzf;
          return input.args.includes("--")
            ? { code: 0, stdout: "/home/u/work/onicode\n" }
            : { code: 0, stdout: ZOXIDE_LIST };
        }, "work  oni");
        expect(result.entries).toEqual([{ name: "onicode", fullPath: "/home/u/work/onicode" }]);
        expect(calls[2]?.args).toEqual(["query", "--list", "--", "work", "oni"]);
      }
    }),
  );

  it.effect("finds nothing and is unavailable when zoxide is missing", () =>
    Effect.gen(function* () {
      const { result, calls, isAvailable } = yield* find(() => "missing", "oni");
      expect(result.entries).toEqual([]);
      expect(isAvailable).toBe(false);
      expect(calls.map((call) => call.command)).toEqual(["zoxide"]);
    }),
  );
});
