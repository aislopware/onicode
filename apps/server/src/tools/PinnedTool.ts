// @effect-diagnostics nodeBuiltinImport:off - Effect has no incremental digest.
import type { ComputerDriverInstall } from "@onicode/contracts";
import { cliArchiveTarCommand } from "@onicode/shared/cliRelease";
import * as HostProcess from "@onicode/shared/HostProcess";
import * as Cause from "effect/Cause";
import * as Clock from "effect/Clock";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as FileSystem from "effect/FileSystem";
import * as Option from "effect/Option";
import * as Path from "effect/Path";
import * as Schema from "effect/Schema";
import * as Semaphore from "effect/Semaphore";
import * as Stream from "effect/Stream";
import { HttpClient, HttpClientRequest, HttpClientResponse } from "effect/http";
import { ChildProcess, ChildProcessSpawner } from "effect/process";
import * as NodeCrypto from "node:crypto";

/** One published build of a tool, pinned by size and hash. */
export interface PinnedToolRelease {
  readonly version: string;
  readonly url: string;
  readonly bytes: number;
  readonly sha256: string;
}

export class PinnedToolInstallError extends Schema.TaggedError<PinnedToolInstallError>()(
  "PinnedToolInstallError",
  { tool: Schema.String, detail: Schema.String, cause: Schema.optional(Schema.Defect()) },
) {
  override get message(): string {
    return `Onicode could not install ${this.tool}: ${this.detail}`;
  }
}
const isInstallError = Schema.is(PinnedToolInstallError);

export class PinnedToolUnsupportedError extends Schema.TaggedError<PinnedToolUnsupportedError>()(
  "PinnedToolUnsupportedError",
  { tool: Schema.String, platform: Schema.String, arch: Schema.String },
) {
  override get message(): string {
    return `${this.tool} has no build for ${this.platform}-${this.arch}.`;
  }
}

export interface PinnedTool {
  /** Never starts an install. */
  readonly state: Effect.Effect<ComputerDriverInstall>;
  /** Installs the tool when it is missing, and waits for that install. A failed install is retried. */
  readonly executable: Effect.Effect<string, PinnedToolInstallError | PinnedToolUnsupportedError>;
}

export interface PinnedToolOptions {
  /** Names the install directory under `<baseDir>/tools` and the tool in messages. */
  readonly name: string;
  readonly baseDir: string;
  readonly executableName: string;
  /** Null when the tool publishes no build for this machine. */
  readonly release: PinnedToolRelease | null;
  /** An archive is unpacked without the entries matching `exclude`; a binary is the download itself. */
  readonly payload:
    | { readonly kind: "archive"; readonly exclude: ReadonlyArray<string> }
    | { readonly kind: "binary" };
}

const DOWNLOAD_TIMEOUT = "15 minutes";
const STAGING_STALE_AFTER_MS = 60 * 60_000;

const wrapFailure = (tool: string, detail: string) => (cause: unknown) =>
  isInstallError(cause) ? cause : new PinnedToolInstallError({ tool, detail, cause });

type InstallState =
  | { readonly _tag: "idle" }
  | {
      readonly _tag: "installing";
      readonly done: Deferred.Deferred<string, PinnedToolInstallError>;
    }
  | { readonly _tag: "installed"; readonly executable: string }
  | { readonly _tag: "failed"; readonly error: PinnedToolInstallError };

export const makePinnedTool = Effect.fn("PinnedTool.make")(function* (options: PinnedToolOptions) {
  const fs = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  const http = yield* HttpClient.HttpClient;
  const spawner = yield* ChildProcessSpawner.ChildProcessSpawner;
  // Installs belong to the service, so they finish even when no caller is still waiting.
  const serviceScope = yield* Effect.scope;
  const platform = yield* HostProcess.Platform;
  const arch = yield* HostProcess.Architecture;
  const environment = yield* HostProcess.Environment;
  const { name: tool, release, executableName, payload } = options;
  const installRoot = path.join(options.baseDir, "tools", tool);
  const gate = yield* Semaphore.make(1);
  let state: InstallState = { _tag: "idle" };
  let downloadedBytes = 0;

  const installedExecutable = (release: PinnedToolRelease) => {
    const executable = path.join(installRoot, release.version, executableName);
    return fs.stat(executable).pipe(
      Effect.map((info) =>
        info.type === "File" && (platform === "win32" || (info.mode & 0o111) !== 0)
          ? Option.some(executable)
          : Option.none<string>(),
      ),
      Effect.orElseSucceed(() => Option.none<string>()),
    );
  };

  const download = Effect.fn("PinnedTool.download")(
    function* (release: PinnedToolRelease, archivePath: string) {
      const response = yield* http
        .execute(HttpClientRequest.get(release.url))
        .pipe(Effect.flatMap(HttpClientResponse.filterStatusOk));
      const hash = NodeCrypto.createHash("sha256");
      yield* response.stream.pipe(
        Stream.tap((chunk) =>
          Effect.suspend(() => {
            downloadedBytes += chunk.byteLength;
            if (downloadedBytes > release.bytes) {
              return Effect.fail(
                new PinnedToolInstallError({
                  tool,
                  detail: "The download was larger than the pinned release.",
                }),
              );
            }
            hash.update(chunk);
            return Effect.void;
          }),
        ),
        Stream.run(fs.sink(archivePath, { flag: "wx", mode: 0o600 })),
      );
      if (downloadedBytes !== release.bytes || hash.digest("hex") !== release.sha256) {
        return yield* new PinnedToolInstallError({
          tool,
          detail: "The download failed its size or SHA-256 check. Nothing was installed.",
        });
      }
    },
    Effect.timeout(DOWNLOAD_TIMEOUT),
    Effect.mapError(
      wrapFailure(
        tool,
        "The download from github.com failed. Check this machine's network access.",
      ),
    ),
  );

  // The archive passed its pinned hash, so its entries are the ones upstream published.
  const extract = Effect.fn("PinnedTool.extract")(
    function* (archivePath: string, destination: string, exclude: ReadonlyArray<string>) {
      yield* fs.makeDirectory(destination);
      const child = yield* spawner.spawn(
        ChildProcess.make(
          cliArchiveTarCommand(platform, environment),
          [
            "-xf",
            archivePath,
            "-C",
            destination,
            ...exclude.flatMap((pattern) => ["--exclude", pattern]),
          ],
          { shell: false, stdout: "ignore", stderr: "pipe" },
        ),
      );
      const [stderr, exitCode] = yield* Effect.all(
        [child.stderr.pipe(Stream.decodeText(), Stream.mkString), child.exitCode],
        { concurrency: "unbounded" },
      );
      const executable = yield* fs.stat(path.join(destination, executableName)).pipe(Effect.option);
      if (exitCode !== 0 || Option.isNone(executable) || executable.value.type !== "File") {
        return yield* new PinnedToolInstallError({
          tool,
          detail: `Could not unpack it. ${stderr.trim().slice(-300)}`.trim(),
        });
      }
    },
    Effect.scoped,
    Effect.mapError(
      wrapFailure(tool, "Could not unpack it. Check free disk space in Onicode's home directory."),
    ),
  );

  const install = Effect.fn("PinnedTool.install")(
    function* (release: PinnedToolRelease) {
      yield* Effect.gen(function* () {
        yield* fs.makeDirectory(installRoot, { recursive: true });
        // Staging shares the install root's filesystem so publishing is one rename.
        const staging = yield* fs.makeTempDirectoryScoped({
          directory: installRoot,
          prefix: ".install-",
        });
        const archivePath = path.join(staging, "download");
        const unpacked = path.join(staging, "driver");
        yield* download(release, archivePath);
        if (payload.kind === "archive") {
          yield* extract(archivePath, unpacked, payload.exclude);
        } else {
          yield* fs.makeDirectory(unpacked);
          const executable = path.join(unpacked, executableName);
          yield* fs.rename(archivePath, executable);
          yield* fs.chmod(executable, 0o755);
        }
        const destination = path.join(installRoot, release.version);
        yield* fs.remove(destination, { recursive: true, force: true });
        yield* fs.rename(unpacked, destination);
      }).pipe(Effect.scoped);
      const now = yield* Clock.currentTimeMillis;
      yield* Effect.forEach(
        (yield* fs.readDirectory(installRoot)).filter((name) => name !== release.version),
        (name) =>
          Effect.gen(function* () {
            const target = path.join(installRoot, name);
            // Another server sharing this home may be installing into a fresh staging directory.
            if (name.startsWith(".install-")) {
              const info = yield* fs.stat(target);
              const modifiedAt = Option.match(info.mtime, {
                onNone: () => now,
                onSome: (date) => date.getTime(),
              });
              if (now - modifiedAt < STAGING_STALE_AFTER_MS) return;
            }
            yield* fs.remove(target, { recursive: true, force: true });
          }).pipe(
            Effect.catch((cause) =>
              Effect.logWarning("Could not remove an old install.", { tool, name, cause }),
            ),
          ),
        { discard: true },
      );
      return path.join(installRoot, release.version, executableName);
    },
    Effect.mapError(wrapFailure(tool, "Could not save it in Onicode's home directory.")),
  );

  const join = (release: PinnedToolRelease) =>
    gate.withPermit(
      Effect.gen(function* () {
        if (state._tag === "installing") return state;
        const installed = yield* installedExecutable(release);
        if (Option.isSome(installed)) {
          state = { _tag: "installed", executable: installed.value };
          return state;
        }
        const done = yield* Deferred.make<string, PinnedToolInstallError>();
        downloadedBytes = 0;
        const installing: InstallState = { _tag: "installing", done };
        state = installing;
        yield* install(release).pipe(
          Effect.onExit((exit) =>
            gate
              .withPermit(
                Effect.sync(() => {
                  state = Exit.isSuccess(exit)
                    ? { _tag: "installed", executable: exit.value }
                    : {
                        _tag: "failed",
                        error: Option.getOrElse(
                          Cause.findErrorOption(exit.cause),
                          () =>
                            new PinnedToolInstallError({ tool, detail: "The install stopped." }),
                        ),
                      };
                }),
              )
              .pipe(Effect.andThen(Deferred.done(done, exit))),
          ),
          Effect.ignoreCause,
          Effect.interruptible,
          Effect.forkIn(serviceScope),
        );
        return installing;
      }),
    );

  const executable = Effect.gen(function* () {
    if (release === null) {
      return yield* new PinnedToolUnsupportedError({ tool, platform, arch });
    }
    const current = yield* join(release);
    switch (current._tag) {
      case "installed":
        return current.executable;
      case "installing":
        return yield* Deferred.await(current.done);
    }
  }).pipe(Effect.withSpan("PinnedTool.executable", { attributes: { tool } }));

  const currentState: Effect.Effect<ComputerDriverInstall> =
    release === null
      ? Effect.succeed({ state: "unsupported" })
      : gate.withPermit(
          Effect.gen(function* () {
            switch (state._tag) {
              case "installing":
                return { state: "installing", downloadedBytes, totalBytes: release.bytes };
              case "failed":
                return { state: "failed", message: state.error.message };
              case "installed":
              case "idle":
                return Option.isSome(yield* installedExecutable(release))
                  ? { state: "installed" }
                  : { state: "missing" };
            }
          }),
        );

  return { state: currentState, executable } satisfies PinnedTool;
});
