import * as HostProcess from "@onicode/shared/HostProcess";
import { fromLenientJson } from "@onicode/shared/schemaJson";
import { resolveCommandPath } from "@onicode/shared/shell";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Option from "effect/Option";
import * as Path from "effect/Path";
import * as Schema from "effect/Schema";

// A managed Claude launcher leaves this marker beside itself, and records the build its channel
// publishes under the user's data directory each time it reads the channel.
const MANAGED_LAUNCHER_MARKER = ".claude-managed";
const MANAGED_PUBLISHED_PATH = [".local", "share", "claude-managed", "published.json"];

const decodeManagedPublished = Schema.decodeUnknownEffect(
  fromLenientJson(Schema.Struct({ build: Schema.Struct({ published: Schema.String }) })),
);

export interface ClaudeInstalledBuild {
  /**
   * The file `claude` runs. Self-updating installs (the native versions symlink, a managed
   * launcher's rename over itself) swap it under a live process.
   */
  readonly executable: string | undefined;
  /** The build a managed launcher starts on its next launch. */
  readonly managedPublishedVersion: string | undefined;
}

/** What the next `claude` process would run. Anything unreadable is undefined. */
export const readClaudeInstalledBuild = Effect.fn("ClaudeInstalledBuild.read")(function* (
  binaryPath: string,
  environment: NodeJS.ProcessEnv,
): Effect.fn.Return<ClaudeInstalledBuild, never, FileSystem.FileSystem | Path.Path> {
  const resolved = yield* resolveCommandPath(binaryPath, { env: environment });
  if (resolved === null) return { executable: undefined, managedPublishedVersion: undefined };
  const fileSystem = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  const [info, managed] = yield* Effect.all(
    [
      fileSystem.stat(resolved).pipe(Effect.option),
      fileSystem
        .exists(path.join(path.dirname(resolved), MANAGED_LAUNCHER_MARKER))
        .pipe(Effect.orElseSucceed(() => false)),
    ],
    { concurrency: "unbounded" },
  );
  const executable = Option.match(info, {
    onNone: () => undefined,
    onSome: ({ dev, ino, mtime, size }) =>
      [
        resolved,
        dev,
        Option.getOrElse(ino, () => ""),
        Option.match(mtime, { onNone: () => "", onSome: (date) => date.getTime() }),
        size,
      ].join("\u0000"),
  });
  if (!managed) return { executable, managedPublishedVersion: undefined };
  const home = yield* HostProcess.HomeDirectory;
  const published = yield* fileSystem
    .readFileString(path.join(home, ...MANAGED_PUBLISHED_PATH))
    .pipe(
      Effect.flatMap(decodeManagedPublished),
      Effect.map((decoded) => decoded.build.published.trim()),
      Effect.orElseSucceed(() => ""),
    );
  return {
    executable,
    managedPublishedVersion: published.length > 0 ? published : undefined,
  };
});

export type ClaudeOutdatedBuild =
  | { readonly reason: "executable_replaced" }
  | { readonly reason: "build_published"; readonly running: string; readonly published: string };

/**
 * Why a live Claude process would no longer be the one a new launch starts. A publication the
 * process already started under is not a reason: a launch that fell back to the old build would
 * otherwise be replaced again on every turn.
 */
export function claudeOutdatedBuild(input: {
  readonly atInit: ClaudeInstalledBuild;
  readonly current: ClaudeInstalledBuild;
  readonly runningVersion: string | undefined;
}): ClaudeOutdatedBuild | undefined {
  const { atInit, current, runningVersion } = input;
  if (
    atInit.executable !== undefined &&
    current.executable !== undefined &&
    atInit.executable !== current.executable
  ) {
    return { reason: "executable_replaced" };
  }
  const published = current.managedPublishedVersion;
  if (
    published !== undefined &&
    runningVersion !== undefined &&
    published !== runningVersion &&
    published !== atInit.managedPublishedVersion
  ) {
    return { reason: "build_published", running: runningVersion, published };
  }
  return undefined;
}
