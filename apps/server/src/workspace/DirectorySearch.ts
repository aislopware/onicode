import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Path from "effect/Path";

import type {
  FilesystemFindDirectoriesInput,
  FilesystemFindDirectoriesResult,
} from "@onicode/contracts";

import * as ProcessRunner from "../processRunner.ts";

export class DirectorySearch extends Context.Service<
  DirectorySearch,
  {
    /** Whether zoxide runs on this host; rechecked at most once a minute. */
    readonly isAvailable: Effect.Effect<boolean>;
    readonly find: (
      input: FilesystemFindDirectoriesInput,
    ) => Effect.Effect<FilesystemFindDirectoriesResult>;
  }
>()("onicode/workspace/DirectorySearch") {}

const DEFAULT_LIMIT = 20;
const TOOL_TIMEOUT = "3 seconds";
const AVAILABILITY_TTL = "1 minute";
const MAX_OUTPUT_BYTES = 4 * 1024 * 1024;
// fzf exits 1 when nothing matches; 2 is its own error, such as an unknown option.
const FZF_NO_MATCH = 1;

const splitLines = (stdout: string) =>
  stdout
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line.length > 0);

/** @public */
export const make = Effect.gen(function* () {
  const runner = yield* ProcessRunner.ProcessRunner;
  const path = yield* Path.Path;

  // A missing or failing tool is not an error to the client: it just finds nothing.
  const runTool = (
    command: string,
    args: ReadonlyArray<string>,
    options?: { readonly stdin: string; readonly env: NodeJS.ProcessEnv },
  ) =>
    runner
      .run({
        command,
        args,
        ...options,
        timeout: TOOL_TIMEOUT,
        maxOutputBytes: MAX_OUTPUT_BYTES,
        outputMode: "truncate",
        truncatedMarker: "",
      })
      .pipe(
        Effect.map((output) => (output.timedOut ? null : output)),
        Effect.orElseSucceed(() => null),
      );

  const isAvailable = yield* Effect.cachedWithTTL(
    runTool("zoxide", ["--version"]).pipe(Effect.map((output) => output?.code === 0)),
    AVAILABILITY_TTL,
  );

  const rankWithFzf = Effect.fn("DirectorySearch.rankWithFzf")(function* (
    query: string,
    candidates: string,
  ) {
    // Ties keep zoxide's frecency order. A user's default options such as --read0 or --tac
    // would break the line protocol or the ranking, so they are cleared.
    const output = yield* runTool(
      "fzf",
      [`--filter=${query}`, "--scheme=path", "--tiebreak=index"],
      { stdin: candidates, env: { FZF_DEFAULT_OPTS: "", FZF_DEFAULT_OPTS_FILE: "" } },
    );
    if (output === null) return null;
    if (output.code === 0) return splitLines(output.stdout);
    return output.code === FZF_NO_MATCH ? [] : null;
  });

  const find: DirectorySearch["Service"]["find"] = Effect.fn("DirectorySearch.find")(
    function* (input) {
      const limit = input.limit ?? DEFAULT_LIMIT;
      const listed = yield* runTool("zoxide", ["query", "--list"]);
      if (listed === null || listed.code !== 0) return { entries: [] };

      let matches = yield* rankWithFzf(input.query, listed.stdout);
      if (matches === null) {
        // Without fzf, zoxide's own keyword match: terms in order, the last in the leaf.
        const terms = input.query.split(/\s+/).filter((term) => term.length > 0);
        const queried = yield* runTool("zoxide", ["query", "--list", "--", ...terms]);
        matches = queried !== null && queried.code === 0 ? splitLines(queried.stdout) : [];
      }

      return {
        entries: matches.slice(0, limit).map((fullPath) => ({
          name: path.basename(fullPath) || fullPath,
          fullPath,
        })),
      };
    },
  );

  return DirectorySearch.of({ isAvailable, find });
});

export const layer = Layer.effect(DirectorySearch, make);
