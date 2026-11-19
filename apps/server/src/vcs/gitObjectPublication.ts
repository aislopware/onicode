import * as Effect from "effect/Effect";
import type * as FileSystem from "effect/FileSystem";
import type * as Path from "effect/Path";

/**
 * Publishes objects a Git writer put in a private object directory into the repository's
 * store, as Git's own quarantine does. Call it after every writer into `privateObjects` exited.
 */
export const publishPrivateObjects = Effect.fn("GitVcsDriver.publishPrivateObjects")(function* (
  fileSystem: FileSystem.FileSystem,
  path: Path.Path,
  privateObjects: string,
  sharedObjects: string,
) {
  const directories = yield* fileSystem.readDirectory(privateObjects);
  yield* Effect.forEach(
    directories.filter((directory) => directory === "pack" || /^[0-9a-f]{2}$/.test(directory)),
    (directory) =>
      Effect.gen(function* () {
        const source = path.join(privateObjects, directory);
        const destination = path.join(sharedObjects, directory);
        // Most fanouts already exist; avoid constructing an EEXIST error
        // for each of them on every publication. mkdir still handles the race.
        if (!(yield* fileSystem.exists(destination)))
          yield* fileSystem.makeDirectory(destination).pipe(
            Effect.andThen(
              Effect.gen(function* () {
                // Git has already applied core.sharedRepository to the source.
                // chmod bypasses umask, but never changes a pre-existing bucket.
                const { mode } = yield* fileSystem.stat(source);
                yield* fileSystem.chmod(destination, mode & 0o7777);
              }),
            ),
            Effect.catchIf(
              (cause) => cause.reason._tag === "AlreadyExists",
              () => Effect.void,
            ),
            Effect.uninterruptible,
          );
        const names = yield* fileSystem.readDirectory(source);
        // Per-object spans would swamp the caller's span for large publications.
        const linkObject = Effect.fnUntraced(function* (name: string) {
          const from = path.join(source, name);
          const to = path.join(destination, name);
          yield* fileSystem.link(from, to).pipe(
            Effect.catchIf(
              (cause) =>
                cause.reason._tag === "PermissionDenied" || cause.reason._tag === "Unknown",
              () =>
                Effect.gen(function* () {
                  if (yield* fileSystem.exists(to)) return;
                  // Git also falls back to rename without hardlinks; identical object names are interchangeable.
                  yield* fileSystem.rename(from, to);
                }),
            ),
            Effect.catchIf(
              (cause) => cause.reason._tag === "AlreadyExists",
              () => Effect.void,
            ),
          );
        });
        if (directory !== "pack") {
          yield* Effect.forEach(
            names.filter((name) => /^(?:[0-9a-f]{38}|[0-9a-f]{62})$/.test(name)),
            linkObject,
            { discard: true },
          );
          return;
        }
        const files = new Set(names);
        for (const name of names) {
          if (!/^pack-(?:[0-9a-f]{40}|[0-9a-f]{64})\.pack$/.test(name)) continue;
          const base = name.slice(0, -5);
          if (!files.has(`${base}.idx`)) continue;
          // Link metadata before the pack so an I/O error cannot strand a
          // large unindexed pack. Defer cancellation across this short group
          // of links. Never roll back shared links: a concurrent writer may
          // already depend on them. Ref publication still happens last.
          yield* Effect.forEach(
            [".keep", ".promisor", ".rev", ".idx", ".pack"]
              .map((extension) => `${base}${extension}`)
              .filter((file) => files.has(file)),
            linkObject,
            { discard: true },
          ).pipe(Effect.uninterruptible);
        }
      }),
    { concurrency: 16, discard: true },
  );
});
