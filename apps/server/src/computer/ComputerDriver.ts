// @effect-diagnostics nodeBuiltinImport:off - Effect has no incremental digest.
import type { ComputerDriverInstall } from "@onicode/contracts";
import { cliArchiveTarCommand } from "@onicode/shared/cliRelease";
import * as HostProcess from "@onicode/shared/HostProcess";
import * as Cause from "effect/Cause";
import * as Clock from "effect/Clock";
import * as Context from "effect/Context";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as FileSystem from "effect/FileSystem";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Path from "effect/Path";
import * as Schema from "effect/Schema";
import * as Semaphore from "effect/Semaphore";
import * as Stream from "effect/Stream";
import { HttpClient, HttpClientRequest, HttpClientResponse } from "effect/http";
import { ChildProcess, ChildProcessSpawner } from "effect/process";
import * as NodeCrypto from "node:crypto";

import * as ServerConfig from "../config.ts";

// Update the version, sizes and hashes together from the `-binary` assets of
// https://github.com/trycua/cua/releases (tag cua-driver-rs-v<version>). The daemon and
// every MCP proxy run this one binary; the proxy refuses a daemon of another version.
const VERSION = "0.49.11";
const ARCHIVES = {
  "darwin-universal": {
    extension: "tar.gz",
    bytes: 43_201_581,
    sha256: "fcd35de80b31b7144839a227b0dbfb1d691adf2ddcd0a2588be9a0128ec2e582",
  },
  "linux-x86_64": {
    extension: "tar.gz",
    bytes: 29_987_915,
    sha256: "d6566ba4b799fb7a161cfa9a94b3cd95fb1b0f766d98fa4888a9c2dec1ffb076",
  },
  "linux-arm64": {
    extension: "tar.gz",
    bytes: 27_911_664,
    sha256: "ec189e9708436976ea3e6df43aaf35f1c665590fb0141d4da3776e07d0ea1adb",
  },
  "windows-x86_64": {
    extension: "zip",
    bytes: 32_317_826,
    sha256: "e5fe14e2a7da5ba7a2aa116f706bfb7c078ecd922819ee9587d18cc07ae2a24c",
  },
  "windows-arm64": {
    extension: "zip",
    bytes: 30_079_478,
    sha256: "2df64cb4ca0477d176787fc0c5c35ceb9fcb829962a0f20fa07f170529098c85",
  },
} as const;

const driverPlatform = (
  platform: NodeJS.Platform,
  arch: NodeJS.Architecture,
): keyof typeof ARCHIVES | null => {
  switch (platform) {
    case "darwin":
      return arch === "arm64" || arch === "x64" ? "darwin-universal" : null;
    case "linux":
      return arch === "x64" ? "linux-x86_64" : arch === "arm64" ? "linux-arm64" : null;
    case "win32":
      return arch === "x64" ? "windows-x86_64" : arch === "arm64" ? "windows-arm64" : null;
    default:
      return null;
  }
};

export interface ComputerDriverRelease {
  readonly version: string;
  readonly url: string;
  readonly bytes: number;
  readonly sha256: string;
}

const computerDriverRelease = (
  platform: NodeJS.Platform,
  arch: NodeJS.Architecture,
): ComputerDriverRelease | null => {
  const target = driverPlatform(platform, arch);
  if (target === null) return null;
  const { extension, bytes, sha256 } = ARCHIVES[target];
  return {
    version: VERSION,
    url: `https://github.com/trycua/cua/releases/download/cua-driver-rs-v${VERSION}/cua-driver-rs-${VERSION}-${target}-binary.${extension}`,
    bytes,
    sha256,
  };
};

// The archives also carry the SDK library, its Node addon and a C header, which Onicode never loads.
const SKIPPED_ARCHIVE_ENTRIES = ["*cua_driver_sdk*", "*.node", "*.h"];

export class ComputerDriverInstallError extends Schema.TaggedError<ComputerDriverInstallError>()(
  "ComputerDriverInstallError",
  { detail: Schema.String, cause: Schema.optional(Schema.Defect()) },
) {
  override get message(): string {
    return `Onicode could not install cua-driver: ${this.detail}`;
  }
}
const isInstallError = Schema.is(ComputerDriverInstallError);

export class ComputerDriverUnsupportedError extends Schema.TaggedError<ComputerDriverUnsupportedError>()(
  "ComputerDriverUnsupportedError",
  { platform: Schema.String, arch: Schema.String },
) {
  override get message(): string {
    return `cua-driver has no build for ${this.platform}-${this.arch}.`;
  }
}

export class ComputerDriver extends Context.Service<
  ComputerDriver,
  {
    readonly version: string;
    /** Never starts an install. */
    readonly state: Effect.Effect<ComputerDriverInstall>;
    /** Installs the driver when it is missing, and waits for that install. A failed install is retried. */
    readonly executable: Effect.Effect<
      string,
      ComputerDriverInstallError | ComputerDriverUnsupportedError
    >;
  }
>()("onicode/computer/ComputerDriver") {}

export interface ComputerDriverOptions {
  readonly baseDir: string;
  readonly release?: ComputerDriverRelease | null;
}

const DOWNLOAD_TIMEOUT = "15 minutes";
const STAGING_STALE_AFTER_MS = 60 * 60_000;

const wrapFailure = (detail: string) => (cause: unknown) =>
  isInstallError(cause) ? cause : new ComputerDriverInstallError({ detail, cause });

type InstallState =
  | { readonly _tag: "idle" }
  | {
      readonly _tag: "installing";
      readonly done: Deferred.Deferred<string, ComputerDriverInstallError>;
    }
  | { readonly _tag: "installed"; readonly executable: string }
  | { readonly _tag: "failed"; readonly error: ComputerDriverInstallError };

export const makeComputerDriver = Effect.fn("ComputerDriver.make")(function* (
  options: ComputerDriverOptions,
) {
  const fs = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  const http = yield* HttpClient.HttpClient;
  const spawner = yield* ChildProcessSpawner.ChildProcessSpawner;
  // Installs belong to the service, so they finish even when no caller is still waiting.
  const serviceScope = yield* Effect.scope;
  const platform = yield* HostProcess.Platform;
  const arch = yield* HostProcess.Architecture;
  const environment = yield* HostProcess.Environment;
  const release =
    options.release === undefined ? computerDriverRelease(platform, arch) : options.release;
  const installRoot = path.join(options.baseDir, "tools", "cua-driver");
  const executableName = platform === "win32" ? "cua-driver.exe" : "cua-driver";
  const gate = yield* Semaphore.make(1);
  let state: InstallState = { _tag: "idle" };
  let downloadedBytes = 0;

  const installedExecutable = (release: ComputerDriverRelease) => {
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

  const download = Effect.fn("ComputerDriver.download")(
    function* (release: ComputerDriverRelease, archivePath: string) {
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
                new ComputerDriverInstallError({
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
        return yield* new ComputerDriverInstallError({
          detail: "The download failed its size or SHA-256 check. Nothing was installed.",
        });
      }
    },
    Effect.timeout(DOWNLOAD_TIMEOUT),
    Effect.mapError(
      wrapFailure("The download from github.com failed. Check this machine's network access."),
    ),
  );

  // The archive passed its pinned hash, so its entries are the ones upstream published.
  const extract = Effect.fn("ComputerDriver.extract")(
    function* (archivePath: string, destination: string) {
      yield* fs.makeDirectory(destination);
      const child = yield* spawner.spawn(
        ChildProcess.make(
          cliArchiveTarCommand(platform, environment),
          [
            "-xf",
            archivePath,
            "-C",
            destination,
            ...SKIPPED_ARCHIVE_ENTRIES.flatMap((pattern) => ["--exclude", pattern]),
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
        return yield* new ComputerDriverInstallError({
          detail: `Could not unpack the driver. ${stderr.trim().slice(-300)}`.trim(),
        });
      }
    },
    Effect.scoped,
    Effect.mapError(
      wrapFailure(
        "Could not unpack the driver. Check free disk space in Onicode's home directory.",
      ),
    ),
  );

  const install = Effect.fn("ComputerDriver.install")(
    function* (release: ComputerDriverRelease) {
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
        yield* extract(archivePath, unpacked);
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
              Effect.logWarning("Could not remove an old cua-driver.", { name, cause }),
            ),
          ),
        { discard: true },
      );
      return path.join(installRoot, release.version, executableName);
    },
    Effect.mapError(wrapFailure("Could not save the driver in Onicode's home directory.")),
  );

  const join = (release: ComputerDriverRelease) =>
    gate.withPermit(
      Effect.gen(function* () {
        if (state._tag === "installing") return state;
        const installed = yield* installedExecutable(release);
        if (Option.isSome(installed)) {
          state = { _tag: "installed", executable: installed.value };
          return state;
        }
        const done = yield* Deferred.make<string, ComputerDriverInstallError>();
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
                          () => new ComputerDriverInstallError({ detail: "The install stopped." }),
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
      return yield* new ComputerDriverUnsupportedError({ platform, arch });
    }
    const current = yield* join(release);
    switch (current._tag) {
      case "installed":
        return current.executable;
      case "installing":
        return yield* Deferred.await(current.done);
    }
  }).pipe(Effect.withSpan("ComputerDriver.executable"));

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

  return ComputerDriver.of({
    version: release?.version ?? VERSION,
    state: currentState,
    executable,
  });
});

export const layer = Layer.effect(
  ComputerDriver,
  Effect.gen(function* () {
    const config = yield* ServerConfig.ServerConfig;
    return yield* makeComputerDriver({ baseDir: config.baseDir });
  }),
);
