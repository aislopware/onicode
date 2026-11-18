import * as NodeCrypto from "@effect/platform-node/NodeCrypto";
import * as NodeServices from "@effect/platform-node/NodeServices";
import * as Layer from "effect/Layer";
import * as ModelManifest from "./ModelManifest.ts";
import { expect, it } from "@effect/vitest";
import * as HostProcess from "@onicode/shared/HostProcess";
import * as Digest from "effect/crypto/Digest";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Hex from "effect/encoding/Hex";
import * as Fiber from "effect/Fiber";
import * as FileSystem from "effect/FileSystem";
import * as Option from "effect/Option";
import * as PlatformError from "effect/PlatformError";
import * as Exit from "effect/Exit";
import * as Queue from "effect/Queue";
import * as Scope from "effect/Scope";
import * as Stream from "effect/Stream";
import { HttpClient, HttpClientError, HttpClientRequest, HttpClientResponse } from "effect/http";
import * as TestClock from "effect/testing/TestClock";
import { ChildProcess, ChildProcessSpawner } from "effect/process";
import * as CodexInstallation from "./CodexInstallation.ts";

const archive = Buffer.from(
  "H4sIAAAAAAAC/+3W0W6DIBQGYB/FcD0sKNSkD7J7aom6tmAQtzXL3n3QZM3qdWVt+n8XoCckJp78wLY3q8bu9Ge2HBbUUp7nYD4H8s9zrNdSsCxnWQLT6JULn8ye09K9h/u2/c0/jSM9xqGzo0+bf3Gdf15WQiD/Kdy61/CA+z8dlO9Wrv2387+c5Z9VZY38p7BY0+Gh8t/sVauLt9Ga9Pnnop7ln1e4/6fxRQ7qZCf/qt3YW0M2OX/JyfvljbCCy3XBSaiGH9VqH4tKuaZbC6qG4aDpTrmP3sQV2nh3Gmxvzqsud0vyjaABAAAAAAAAAAAAAAAk8gOq19rvACgAAA==",
  "base64",
);
const asset = {
  version: "0.156.1",
  target: "aarch64-apple-darwin",
  url: "https://github.com/openai/codex/releases/download/test/package.tar.gz",
  archiveBytes: archive.length,
};
const makeHarness = Effect.fn("test.makeCodexInstallation")(function* (
  input: {
    options?: Partial<CodexInstallation.CodexInstallationOptions>;
    manifestCurrent?: Effect.Effect<ModelManifest.ModelManifestData>;
    body?: Stream.Stream<Uint8Array, HttpClientError.HttpClientError>;
    downloadStatus?: number;
    baseDir?: string;
    local?: { version: string; appServerFails?: boolean; versionFails?: boolean };
    platform?: NodeJS.Platform;
    spawner?: ChildProcessSpawner.ChildProcessSpawner["Service"];
    runtimeRenameFailures?: number;
    /** Runs after the new runtime is renamed into place, before it is activated. */
    afterRuntimeRename?: Effect.Effect<void>;
    /** Served instead of active.json's content, one per read, while any remain. */
    staleActiveReads?: Array<string>;
    /** Runs before a release is moved aside for deletion. */
    beforePruneRename?: Effect.Effect<void>;
    /** Runs before each session lease file is written. */
    beforeLeaseWrite?: Effect.Effect<void>;
    /** Removals of matching paths delete the install record, then fail, like a crash mid-delete. */
    interruptedRemoves?: RegExp;
    /** Served by URL instead of the pinned package. */
    responses?: Readonly<Record<string, () => Response>>;
  } = {},
) {
  const fs = yield* FileSystem.FileSystem;
  const baseDir =
    input.baseDir ?? (yield* fs.makeTempDirectoryScoped({ prefix: "onicodex-install-test-" }));
  const localDirectory = `${baseDir}/local`;
  const localBinaryPath = `${localDirectory}/codex${input.platform === "win32" ? ".exe" : ""}`;
  const probeLog = `${baseDir}/local-probes.txt`;
  if (input.local) {
    yield* fs.makeDirectory(localDirectory, { recursive: true });
    yield* fs.writeFileString(
      localBinaryPath,
      `#!/bin/sh\nprintf '%s\\n' "$*" >> '${probeLog}'\ncase "$1" in\n--version) ${input.local.versionFails ? "exit 1" : `printf '%s\\n' 'codex-cli ${input.local.version}'`};;\napp-server) exit ${input.local.appServerFails ? "1" : "0"};;\nesac\n`,
      { mode: 0o755 },
    );
  }
  let downloads = 0;
  const digest = yield* Digest.Digest;
  const sha256 = Hex.encode(yield* digest.digest(Digest.sha256, archive).pipe(Effect.orDie));
  let runtimeRenameFailures = input.runtimeRenameFailures ?? 0;
  const failedRenames = yield* Queue.unbounded<void>();
  const lockingFs: FileSystem.FileSystem = {
    ...fs,
    // Suspended so each retry of the same effect is a fresh attempt, like the real rename.
    rename: (from, to) =>
      Effect.suspend(() =>
        /[/\\]\.install-pruned-[^/\\]+$/u.test(to)
          ? (input.beforePruneRename ?? Effect.void).pipe(Effect.andThen(fs.rename(from, to)))
          : /[/\\]runtime$/u.test(from) && runtimeRenameFailures-- > 0
            ? Queue.offer(failedRenames, undefined).pipe(
                Effect.andThen(
                  Effect.fail(
                    PlatformError.systemError({
                      _tag: "PermissionDenied",
                      module: "FileSystem",
                      method: "rename",
                      pathOrDescriptor: from,
                    }),
                  ),
                ),
              )
            : fs
                .rename(from, to)
                .pipe(
                  Effect.andThen(
                    /[/\\]runtime$/u.test(from)
                      ? (input.afterRuntimeRename ?? Effect.void)
                      : Effect.void,
                  ),
                ),
      ),
    writeFileString: (file, data, options) =>
      (/[/\\]\.lease-[^/\\]*$/u.test(file) && !file.endsWith("-install")
        ? (input.beforeLeaseWrite ?? Effect.void)
        : Effect.void
      ).pipe(Effect.andThen(fs.writeFileString(file, data, options))),
    remove: (file, options) =>
      input.interruptedRemoves?.test(file)
        ? fs.remove(`${file}/.install-complete.json`, { force: true }).pipe(
            Effect.andThen(
              Effect.fail(
                PlatformError.systemError({
                  _tag: "Busy",
                  module: "FileSystem",
                  method: "remove",
                  pathOrDescriptor: file,
                }),
              ),
            ),
          )
        : fs.remove(file, options),
    readFileString: (file, encoding) =>
      Effect.suspend(() => {
        const stale = file.endsWith("active.json") ? input.staleActiveReads?.shift() : undefined;
        return stale === undefined ? fs.readFileString(file, encoding) : Effect.succeed(stale);
      }),
  };
  const spawner = input.spawner ?? (yield* ChildProcessSpawner.ChildProcessSpawner);
  const installation = yield* CodexInstallation.makeCodexInstallation({
    baseDir,
    releaseAsset: { ...asset, sha256 },
    validate: () => Effect.void,
    ...input.options,
  }).pipe(
    Effect.provideService(ModelManifest.ModelManifest, {
      current: input.manifestCurrent ?? Effect.succeed(ModelManifest.BUNDLED_MODEL_MANIFEST),
      refresh: Effect.succeed(ModelManifest.BUNDLED_MODEL_MANIFEST),
      forceRefresh: Effect.succeed(ModelManifest.BUNDLED_MODEL_MANIFEST),
      refreshOnDemand: Effect.succeed(ModelManifest.BUNDLED_MODEL_MANIFEST),
      refreshInBackground: Effect.void,
    }),
    Effect.provideService(HostProcess.Platform, input.platform ?? "darwin"),
    Effect.provideService(HostProcess.Architecture, "arm64"),
    Effect.provideService(HostProcess.Environment, {
      PATH: input.local ? localDirectory : "",
      SystemRoot: "C:\\Windows",
    }),
    Effect.provideService(FileSystem.FileSystem, lockingFs),
    Effect.provideService(ChildProcessSpawner.ChildProcessSpawner, spawner),
    Effect.provideService(
      HttpClient.HttpClient,
      HttpClient.make((request) =>
        Effect.sync(() => {
          downloads++;
          const respond = input.responses?.[request.url];
          if (respond) return HttpClientResponse.fromWeb(request, respond());
          return Object.defineProperty(
            HttpClientResponse.fromWeb(
              request,
              new Response(null, { status: input.downloadStatus ?? 200 }),
            ),
            "stream",
            {
              value: input.body ?? Stream.succeed(archive),
            },
          );
        }),
      ),
    ),
  );
  return {
    installation,
    fs,
    baseDir,
    localBinaryPath,
    probeLog,
    failedRenames,
    downloads: () => downloads,
  };
});
const terminalState = (installation: CodexInstallation.CodexInstallation["Service"]) =>
  installation.changes.pipe(
    Stream.filter((state) => ["succeeded", "failed", "cancelled"].includes(state.phase)),
    Stream.runHead,
    Effect.map(Option.getOrThrow),
  );

it.effect.each(["0.156.0", "0.156.1", "0.156.2", "0.157.0"])(
  "reuses installed Codex %s without downloading or taking ownership of it",
  (version) =>
    Effect.gen(function* () {
      const h = yield* makeHarness({ local: { version } });
      expect(yield* h.installation.start).toMatchObject({
        source: "local",
        phase: "succeeded",
        installedVersion: version,
        executablePath: h.localBinaryPath,
        canRemove: false,
      });
      expect(yield* h.installation.resolve()).toMatchObject({
        source: "local",
        executablePath: h.localBinaryPath,
        managedVersionDirectory: null,
        version,
      });
      yield* h.installation.acquire().pipe(Effect.scoped);
      expect(h.downloads()).toBe(0);
      expect(yield* h.fs.exists(h.installation.managedDirectory)).toBe(false);
      expect((yield* h.fs.readFileString(h.probeLog)).trim().split("\n")).toEqual([
        "--version",
        "app-server --help",
      ]);
      yield* h.installation.remove();
      expect(yield* h.fs.exists(h.localBinaryPath)).toBe(true);
      expect((yield* h.installation.state).source).toBe("local");
    }).pipe(Effect.scoped, Effect.provide(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer))),
);
it.effect.each([
  { version: "0.128.9" },
  { version: "0.145.0" },
  { version: "0.155.1" },
  { version: "0.155.9" },
  { version: "0.156.1-alpha.1" },
  { version: "unknown" },
  { version: "0.156.1", appServerFails: true },
  { version: "0.156.1", versionFails: true },
])("downloads the pinned release when the local CLI is unsupported or broken: %j", (local) =>
  Effect.gen(function* () {
    const h = yield* makeHarness({ local });
    expect((yield* h.installation.state).installedVersion).toBeNull();
    yield* h.installation.start;
    const installed = yield* terminalState(h.installation);
    expect(installed.phase).toBe("succeeded");
    const executable = yield* h.installation.resolve();
    expect(executable.source).toBe("managed");
    expect(installed.executablePath).toBe(executable.executablePath);
    expect(h.downloads()).toBe(1);
    expect(yield* h.fs.exists(h.localBinaryPath)).toBe(true);
  }).pipe(Effect.scoped, Effect.provide(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer))),
);

const compileNative = Effect.fn("test.compileNative")(function* (
  executablePath: string,
  source: string,
) {
  const spawner = yield* ChildProcessSpawner.ChildProcessSpawner;
  const compiler = yield* spawner.spawn(
    ChildProcess.make("cc", ["-x", "c", "-", "-o", executablePath], {
      stdin: Stream.make(new TextEncoder().encode(source)),
      stdout: "inherit",
      stderr: "inherit",
    }),
  );
  expect(yield* compiler.exitCode).toBe(0);
}, Effect.scoped);

it.effect("rechecks compatibility after the local executable is replaced", () =>
  Effect.gen(function* () {
    const h = yield* makeHarness({ local: { version: "0.156.1" } });
    expect((yield* h.installation.resolve()).source).toBe("local");
    yield* h.fs.remove(h.localBinaryPath);
    yield* h.fs.writeFileString(h.localBinaryPath, "#!/bin/sh\nprintf 'codex-cli 0.128.9\\n'\n", {
      mode: 0o755,
    });
    yield* h.installation.start;
    expect((yield* terminalState(h.installation)).phase).toBe("succeeded");
    expect((yield* h.installation.resolve()).source).toBe("managed");
    expect(h.downloads()).toBe(1);
  }).pipe(Effect.scoped, Effect.provide(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer))),
);

it.effect("rechecks a cached local executable when the shared manifest policy changes", () =>
  Effect.gen(function* () {
    let manifest = ModelManifest.BUNDLED_MODEL_MANIFEST;
    const h = yield* makeHarness({
      local: { version: "0.156.0" },
      manifestCurrent: Effect.sync(() => manifest),
    });
    expect((yield* h.installation.resolve()).source).toBe("local");
    manifest = {
      ...manifest,
      compatibility: [
        {
          driver: "codex",
          onicodeRange: ">=0.0.42",
          ranges: [
            { range: ">=0.156.1", status: "supported" },
            { range: "<0.156.1", status: "broken" },
          ],
        },
      ],
    };
    yield* h.installation.start;
    expect((yield* terminalState(h.installation)).phase).toBe("succeeded");
    expect((yield* h.installation.resolve()).source).toBe("managed");
    expect(h.downloads()).toBe(1);
    expect((yield* h.fs.readFileString(h.probeLog)).trim().split("\n")).toEqual([
      "--version",
      "app-server --help",
    ]);
  }).pipe(Effect.scoped, Effect.provide(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer))),
);

it.effect("uses bundled Codex compatibility when the remote manifest omits its policy", () =>
  Effect.gen(function* () {
    const h = yield* makeHarness({
      local: { version: "0.156.0" },
      manifestCurrent: Effect.succeed({
        ...ModelManifest.BUNDLED_MODEL_MANIFEST,
        compatibility: [],
      }),
    });
    expect((yield* h.installation.start).source).toBe("local");
    expect(h.downloads()).toBe(0);
  }).pipe(Effect.scoped, Effect.provide(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer))),
);

it.effect.each([
  { kind: "script", name: "volta-shim" },
  { kind: "native", name: "volta-shim" },
  { kind: "native", name: "mise" },
  { kind: "native", name: "aqua-proxy" },
])("preserves the invoked name of $kind $name launcher symlinks", ({ kind, name }) =>
  Effect.gen(function* () {
    const h = yield* makeHarness({ local: { version: "0.156.1" } });
    const launcher = `${h.baseDir}/${name}`;
    if (kind === "native") {
      const payload = `${h.baseDir}/codex.js`;
      yield* h.fs.rename(h.localBinaryPath, payload);
      yield* compileNative(
        launcher,
        `#include <string.h>
#include <unistd.h>
int main(int argc, char **argv) {
  const char *name = strrchr(argv[0], '/');
  if (strcmp(name ? name + 1 : argv[0], "codex") != 0) return 1;
  execv(${JSON.stringify(payload)}, argv);
  return 1;
}
`,
      );
    } else {
      yield* h.fs.writeFileString(
        launcher,
        '#!/bin/sh\ncase "$0" in */codex) ;; *) exit 1;; esac\ncase "$1" in --version) printf "codex-cli 0.156.1\\n";; app-server) exit 0;; esac\n',
        { mode: 0o755 },
      );
      yield* h.fs.remove(h.localBinaryPath);
    }
    yield* h.fs.symlink(launcher, h.localBinaryPath);
    expect(yield* h.installation.start).toMatchObject({ source: "local", phase: "succeeded" });
    expect((yield* h.installation.resolve()).executablePath).toBe(h.localBinaryPath);
    expect(h.downloads()).toBe(0);
  }).pipe(Effect.scoped, Effect.provide(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer))),
);

it.effect(
  "installs the complete verified package in tools/codex/version and survives a restart",
  () =>
    Effect.gen(function* () {
      const { installation, fs, baseDir } = yield* makeHarness();
      yield* installation.start;
      expect((yield* terminalState(installation)).phase).toBe("succeeded");
      const executable = yield* installation.resolve();
      expect(executable.executablePath).toBe(`${baseDir}/tools/codex/0.156.1/bin/codex`);
      expect(
        yield* fs.readFileString(`${executable.managedVersionDirectory}/bin/codex-code-mode-host`),
      ).toBe("host");
      expect(yield* fs.readFileString(`${executable.managedVersionDirectory}/codex-path/rg`)).toBe(
        "rg",
      );
      const restarted = yield* makeHarness({ baseDir });
      expect((yield* restarted.installation.resolve()).version).toBe("0.156.1");
    }).pipe(Effect.scoped, Effect.provide(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer))),
);

it.effect("requires an update before running an older activated managed installation", () =>
  Effect.gen(function* () {
    const first = yield* makeHarness();
    yield* first.installation.start;
    yield* terminalState(first.installation);
    const executable = yield* first.installation.resolve();
    const oldDirectory = `${first.installation.managedDirectory}/0.155.1`;
    yield* first.fs.rename(executable.managedVersionDirectory!, oldDirectory);
    for (const name of ["codex-package.json", ".install-complete.json"]) {
      const file = `${oldDirectory}/${name}`;
      yield* first.fs.writeFileString(
        file,
        (yield* first.fs.readFileString(file)).replaceAll("0.156.1", "0.155.1"),
      );
    }
    yield* first.fs.writeFileString(
      `${first.installation.managedDirectory}/active.json`,
      '{"version":"0.155.1"}',
    );
    const restarted = yield* makeHarness({ baseDir: first.baseDir });
    expect((yield* Effect.flip(restarted.installation.resolve())).detail).toContain(
      "outside the supported range",
    );
    yield* restarted.installation.start;
    expect((yield* terminalState(restarted.installation)).phase).toBe("succeeded");
    expect((yield* restarted.installation.resolve()).version).toBe("0.156.1");
    expect(restarted.downloads()).toBe(1);
  }).pipe(Effect.scoped, Effect.provide(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer))),
);

it.effect("updates an older supported managed installation to the pinned release on startup", () =>
  Effect.gen(function* () {
    const first = yield* makeHarness();
    yield* first.installation.start;
    yield* terminalState(first.installation);
    const executable = yield* first.installation.resolve();
    const oldDirectory = `${first.installation.managedDirectory}/0.156.0`;
    yield* first.fs.rename(executable.managedVersionDirectory!, oldDirectory);
    for (const name of ["codex-package.json", ".install-complete.json"]) {
      const file = `${oldDirectory}/${name}`;
      yield* first.fs.writeFileString(
        file,
        (yield* first.fs.readFileString(file)).replaceAll("0.156.1", "0.156.0"),
      );
    }
    yield* first.fs.writeFileString(
      `${first.installation.managedDirectory}/active.json`,
      '{"version":"0.156.0"}',
    );
    const restarted = yield* makeHarness({ baseDir: first.baseDir });
    expect((yield* restarted.installation.state).operationId).not.toBeNull();
    expect((yield* terminalState(restarted.installation)).phase).toBe("succeeded");
    expect((yield* restarted.installation.resolve()).version).toBe("0.156.1");
    expect(restarted.downloads()).toBe(1);
  }).pipe(Effect.scoped, Effect.provide(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer))),
);

it.effect("rejects corrupted downloads without publishing a runtime", () =>
  Effect.gen(function* () {
    const { installation, fs } = yield* makeHarness({
      options: { releaseAsset: { ...asset, sha256: "0".repeat(64) } },
    });
    yield* installation.start;
    const state = yield* terminalState(installation);
    expect(state.phase).toBe("failed");
    expect(state.message).toContain("SHA-256");
    expect(yield* fs.exists(`${installation.managedDirectory}/active.json`)).toBe(false);
    expect(yield* fs.exists(`${installation.managedDirectory}/0.156.1`)).toBe(false);
  }).pipe(Effect.scoped, Effect.provide(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer))),
);

it.effect.each(["interrupted body", "HTTP 404"] as const)(
  "reports a download failure for %s without blaming disk access",
  (failure) =>
    Effect.gen(function* () {
      const request = HttpClientRequest.get(asset.url);
      const response = HttpClientResponse.fromWeb(request, new Response(null));
      const h = yield* makeHarness(
        failure === "HTTP 404"
          ? { downloadStatus: 404 }
          : {
              body: Stream.concat(
                Stream.succeed(archive.subarray(0, 10)),
                Stream.fail(
                  new HttpClientError.HttpClientError({
                    reason: new HttpClientError.DecodeError({
                      request,
                      response,
                      cause: new TypeError("terminated"),
                    }),
                  }),
                ),
              ),
            },
      );
      yield* h.installation.start;
      expect(yield* terminalState(h.installation)).toMatchObject({
        phase: "failed",
        message:
          failure === "HTTP 404"
            ? "The Codex download server returned HTTP 404. Try again later."
            : "The Codex download was interrupted. Check your internet connection, then retry installation.",
      });
      expect(h.downloads()).toBe(1);
      expect(yield* h.fs.readDirectory(h.installation.managedDirectory)).toEqual([]);
    }).pipe(Effect.scoped, Effect.provide(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer))),
);

it.effect(
  "preserves the filesystem failure message when installation cannot create its directory",
  () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const directory = yield* fs.makeTempDirectoryScoped({ prefix: "onicodex-install-test-" });
      const baseDir = `${directory}/not-a-directory`;
      yield* fs.writeFileString(baseDir, "existing file");
      const h = yield* makeHarness({ baseDir });
      yield* h.installation.start;
      expect(yield* terminalState(h.installation)).toMatchObject({
        phase: "failed",
        message: "Could not install Codex. Check disk space and directory access, then try again.",
      });
      expect(h.downloads()).toBe(0);
      expect(yield* fs.readFileString(baseDir)).toBe("existing file");
    }).pipe(Effect.scoped, Effect.provide(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer))),
);

it.effect.each(["cached update", "reverify"] as const)(
  "reports a verification timeout during %s and preserves the managed runtime",
  (operation) =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const baseDir = yield* fs.makeTempDirectoryScoped({ prefix: "onicodex-verify-test-" });
      const managed = `${baseDir}/tools/codex`;
      const installedVersion = operation === "cached update" ? "0.156.0" : asset.version;
      const target = "aarch64-pc-windows-msvc";
      const checksum = "a".repeat(64);
      for (const version of new Set([installedVersion, asset.version])) {
        const directory = `${managed}/${version}`;
        yield* fs.makeDirectory(`${directory}/bin`, { recursive: true });
        yield* fs.makeDirectory(`${directory}/codex-path`, { recursive: true });
        for (const name of ["bin/codex.exe", "bin/codex-code-mode-host.exe", "codex-path/rg.exe"]) {
          yield* fs.writeFileString(`${directory}/${name}`, "fixture executable");
        }
        yield* fs.writeFileString(
          `${directory}/codex-package.json`,
          JSON.stringify({ layoutVersion: 1, version, target, entrypoint: "bin/codex.exe" }),
        );
        yield* fs.writeFileString(
          `${directory}/.install-complete.json`,
          JSON.stringify({ version, target, sha256: checksum }),
        );
      }
      const pointer = JSON.stringify({ version: installedVersion });
      yield* fs.writeFileString(`${managed}/active.json`, pointer);
      const entered = yield* Deferred.make<void>();
      const h = yield* makeHarness({
        baseDir,
        platform: "win32",
        manifestCurrent: Effect.succeed({
          ...ModelManifest.BUNDLED_MODEL_MANIFEST,
          compatibility: [
            {
              driver: "codex",
              onicodeRange: ">=0.0.0",
              ranges: [{ range: ">=0.156.0", status: "supported" }],
            },
          ],
        }),
        options: {
          releaseAsset: operation === "reverify" ? null : { ...asset, target, sha256: checksum },
          validate: () => Deferred.succeed(entered, undefined).pipe(Effect.andThen(Effect.never)),
        },
      });
      yield* h.installation.start;
      yield* Deferred.await(entered);
      yield* TestClock.adjust("90 seconds");
      expect(yield* terminalState(h.installation)).toMatchObject({
        phase: "failed",
        installedVersion,
        message:
          "Codex runtime verification timed out after 90 seconds. Check this environment's server logs for startup errors, then retry installation.",
      });
      expect(yield* fs.readFileString(`${managed}/active.json`)).toBe(pointer);
      expect((yield* h.installation.resolve()).version).toBe(installedVersion);
      expect(h.downloads()).toBe(0);
    }).pipe(Effect.scoped, Effect.provide(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer))),
);

it.effect("reports a download timeout and cleans the staging directory", () =>
  Effect.gen(function* () {
    const started = yield* Deferred.make<void>();
    const stall = Deferred.succeed(started, undefined).pipe(Effect.andThen(Effect.never));
    const h = yield* makeHarness({
      body: Stream.concat(Stream.succeed(archive.subarray(0, 10)), Stream.fromEffect(stall)),
    });
    yield* h.installation.start;
    yield* Deferred.await(started);
    yield* TestClock.adjust("45 minutes");
    expect(yield* terminalState(h.installation)).toMatchObject({
      phase: "failed",
      message: "The Codex download timed out. Retry installation.",
    });
    expect(h.downloads()).toBe(1);
    expect(yield* h.fs.readDirectory(h.installation.managedDirectory)).toEqual([]);
  }).pipe(Effect.scoped, Effect.provide(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer))),
);

it.effect("cancels an in-flight download and cleans the staging directory", () =>
  Effect.gen(function* () {
    const downloading = yield* Deferred.make<void>();
    const body = Stream.fromEffect(
      Deferred.succeed(downloading, undefined).pipe(Effect.andThen(Effect.never)),
    );
    const { installation, fs } = yield* makeHarness({ body });
    const started = yield* installation.start;
    yield* Deferred.await(downloading);
    expect((yield* installation.cancel(started.operationId!)).phase).toBe("cancelled");
    expect(
      (yield* fs.readDirectory(installation.managedDirectory)).filter((name) =>
        name.startsWith(".install-"),
      ),
    ).toEqual([]);
    expect(yield* fs.exists(`${installation.managedDirectory}/active.json`)).toBe(false);
  }).pipe(Effect.scoped, Effect.provide(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer))),
);

it.effect("protects active process leases and removes the runtime after release", () =>
  Effect.gen(function* () {
    const { installation, fs } = yield* makeHarness();
    yield* installation.start;
    yield* terminalState(installation);
    const leaseScope = yield* Scope.make();
    yield* installation.acquire().pipe(Effect.provideService(Scope.Scope, leaseScope));
    expect((yield* Effect.flip(installation.remove())).detail).toContain("Stop Codex sessions");
    yield* Scope.close(leaseScope, Exit.void);
    yield* installation.remove();
    expect(yield* fs.exists(installation.managedDirectory)).toBe(false);
    expect((yield* installation.state).installedVersion).toBeNull();
    expect((yield* installation.state).executablePath).toBeNull();
  }).pipe(Effect.scoped, Effect.provide(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer))),
);

it.effect("keeps an activated runtime when a later update fails verification", () =>
  Effect.gen(function* () {
    const first = yield* makeHarness();
    yield* first.installation.start;
    yield* terminalState(first.installation);
    const update = yield* makeHarness({
      baseDir: first.baseDir,
      options: { releaseAsset: { ...asset, version: "0.156.2", sha256: "0".repeat(64) } },
    });
    expect((yield* update.installation.state).executablePath).toBe(
      (yield* first.installation.resolve()).executablePath,
    );
    yield* update.installation.start;
    expect((yield* terminalState(update.installation)).phase).toBe("failed");
    expect((yield* update.installation.resolve()).version).toBe("0.156.1");
  }).pipe(Effect.scoped, Effect.provide(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer))),
);

const upstreamVersion = "0.163.0";
const upstreamArchive = Buffer.from(
  "H4sIAAAAAAACA+3WzY7CIBQF4D5Kw1oqtNQmPoh7rKStP9BQdMaYefcBE43T9RQ1nm8B7Q1Jk94cYN3peW026juZDvOqsrzO3nj2yofnUK9KwZKUJREcByet/2TymabuPby29S3/NIz0EIbWDC5u/sXf/PO8EAL5j+G/ew1vuP/TXrp2bpunnf/5KP+syCvkP4bJmg5vlf96JxuVbQej4+efi2qUf17g/h/Hhezl2RzdStmhM5osUz5Lyen+RljGF0XGiK/6H9UoF4pS2rpdCCr7fq/oRtqvTocVSjt77k2nr6vud0vyg6ABAAAAAAAAAAAAAABE8gvVbjvaACgAAA==",
  "base64",
);
const upstreamReleaseUrl = `https://github.com/openai/codex/releases/download/rust-v${upstreamVersion}`;
const upstreamResponses = (checksum: string) => ({
  "https://registry.npmjs.org/@openai/codex/latest": () =>
    Response.json({ version: upstreamVersion }),
  [`${upstreamReleaseUrl}/codex-package_SHA256SUMS`]: () =>
    new Response(
      `${"0".repeat(64)}  codex-package-x86_64-apple-darwin.tar.gz\n${checksum}  codex-package-aarch64-apple-darwin.tar.gz\n`,
    ),
  [`${upstreamReleaseUrl}/codex-package-aarch64-apple-darwin.tar.gz`]: () =>
    new Response(upstreamArchive),
});

it.effect("updates a managed install to OpenAI's newest release and never downgrades it", () =>
  Effect.gen(function* () {
    const digest = yield* Digest.Digest;
    const checksum = Hex.encode(
      yield* digest.digest(Digest.sha256, upstreamArchive).pipe(Effect.orDie),
    );
    const h = yield* makeHarness({ responses: upstreamResponses(checksum) });
    yield* h.installation.checkForUpdate;
    // A fresh install still gets the pinned release.
    expect((yield* h.installation.state).version).toBe("0.156.1");
    yield* h.installation.start;
    expect(yield* terminalState(h.installation)).toMatchObject({
      phase: "succeeded",
      installedVersion: "0.156.1",
      version: upstreamVersion,
    });

    const sessionScope = yield* Scope.make();
    const previous = yield* h.installation.acquire().pipe(Scope.provide(sessionScope));
    yield* h.installation.start;
    expect(yield* terminalState(h.installation)).toMatchObject({
      phase: "succeeded",
      installedVersion: upstreamVersion,
      version: upstreamVersion,
    });
    expect((yield* h.installation.resolve()).version).toBe(upstreamVersion);
    // The running session keeps its release until the next start prunes it.
    expect(yield* h.fs.exists(previous.managedVersionDirectory!)).toBe(true);
    yield* Scope.close(sessionScope, Exit.void);

    // Without upstream information the older pin is not offered as a reinstall.
    const restarted = yield* makeHarness({ baseDir: h.baseDir });
    expect(yield* h.fs.exists(previous.managedVersionDirectory!)).toBe(false);
    expect(yield* restarted.installation.state).toMatchObject({
      installedVersion: upstreamVersion,
      version: upstreamVersion,
    });
    yield* restarted.installation.start;
    expect(yield* terminalState(restarted.installation)).toMatchObject({
      phase: "succeeded",
      installedVersion: upstreamVersion,
    });
    expect(restarted.downloads()).toBe(0);
  }).pipe(Effect.scoped, Effect.provide(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer))),
);

const upstreamChecksum = Effect.gen(function* () {
  const digest = yield* Digest.Digest;
  return Hex.encode(yield* digest.digest(Digest.sha256, upstreamArchive).pipe(Effect.orDie));
});
const updateToUpstream = Effect.fn("test.updateToUpstream")(function* (
  installation: CodexInstallation.CodexInstallation["Service"],
) {
  yield* installation.checkForUpdate;
  yield* installation.start;
  expect(yield* terminalState(installation)).toMatchObject({
    phase: "succeeded",
    installedVersion: upstreamVersion,
  });
});

it.effect("keeps a superseded release while another server sharing its home runs it", () =>
  Effect.gen(function* () {
    const h = yield* makeHarness({ responses: upstreamResponses(yield* upstreamChecksum) });
    yield* h.installation.start;
    yield* terminalState(h.installation);
    const other = yield* makeHarness({ baseDir: h.baseDir });
    const sessionScope = yield* Scope.make();
    const previous = yield* other.installation.acquire().pipe(Scope.provide(sessionScope));
    const previousDirectory = previous.managedVersionDirectory!;
    expect((yield* Effect.flip(h.installation.remove())).detail).toContain("Stop Codex sessions");
    yield* updateToUpstream(h.installation);
    yield* makeHarness({ baseDir: h.baseDir });
    expect(yield* h.fs.exists(previousDirectory)).toBe(true);

    yield* Scope.close(sessionScope, Exit.void);
    // A lease left by a server that has exited no longer keeps the release.
    yield* h.fs.writeFileString(`${previousDirectory}/.lease-2147483646-exited`, "");
    // So is one written before the machine last booted, whatever process now has its pid.
    yield* h.fs.writeFileString(`${previousDirectory}/.lease-${process.pid}-before-boot`, "");
    yield* TestClock.setTime(Date.UTC(2100, 0, 1));
    yield* makeHarness({ baseDir: h.baseDir });
    expect(yield* h.fs.exists(previousDirectory)).toBe(false);
  }).pipe(Effect.scoped, Effect.provide(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer))),
);

it.effect("keeps a new release another server starts up beside before it is activated", () =>
  Effect.gen(function* () {
    const renamed = yield* Deferred.make<void>();
    const proceed = yield* Deferred.make<void>();
    let pauseAfterRename = false;
    const h = yield* makeHarness({
      responses: upstreamResponses(yield* upstreamChecksum),
      afterRuntimeRename: Effect.suspend(() =>
        pauseAfterRename
          ? Deferred.succeed(renamed, undefined).pipe(Effect.andThen(Deferred.await(proceed)))
          : Effect.void,
      ),
    });
    yield* h.installation.start;
    yield* terminalState(h.installation);
    pauseAfterRename = true;
    yield* h.installation.checkForUpdate;
    yield* h.installation.start;
    yield* Deferred.await(renamed);
    yield* makeHarness({ baseDir: h.baseDir });
    yield* Deferred.succeed(proceed, undefined);
    expect(yield* terminalState(h.installation)).toMatchObject({
      phase: "succeeded",
      installedVersion: upstreamVersion,
    });
    expect((yield* h.installation.resolve()).version).toBe(upstreamVersion);
  }).pipe(Effect.scoped, Effect.provide(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer))),
);

it.effect("keeps a release another server activates while this one is pruning", () =>
  Effect.gen(function* () {
    const h = yield* makeHarness({ responses: upstreamResponses(yield* upstreamChecksum) });
    yield* h.installation.start;
    yield* terminalState(h.installation);
    const entered = yield* Deferred.make<void>();
    const proceed = yield* Deferred.make<void>();
    const pruning = yield* makeHarness({
      baseDir: h.baseDir,
      options: {
        protectedBinaryPaths: Deferred.succeed(entered, undefined).pipe(
          Effect.andThen(Deferred.await(proceed)),
          Effect.as([]),
        ),
      },
    }).pipe(Effect.forkScoped);
    yield* Deferred.await(entered);
    yield* updateToUpstream(h.installation);
    yield* Deferred.succeed(proceed, undefined);
    yield* Fiber.join(pruning);
    expect((yield* h.installation.resolve()).version).toBe(upstreamVersion);
  }).pipe(Effect.scoped, Effect.provide(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer))),
);

it.effect("leases the active release when another server prunes the one it resolved", () =>
  Effect.gen(function* () {
    const staleActiveReads: Array<string> = [];
    let pruneBeforeLease: Effect.Effect<void> = Effect.void;
    const h = yield* makeHarness({
      responses: upstreamResponses(yield* upstreamChecksum),
      staleActiveReads,
      beforeLeaseWrite: Effect.suspend(() => {
        const prune = pruneBeforeLease;
        pruneBeforeLease = Effect.void;
        return prune;
      }),
    });
    yield* h.installation.start;
    yield* terminalState(h.installation);
    const firstScope = yield* Scope.make();
    const previous = yield* h.installation.acquire().pipe(Scope.provide(firstScope));
    yield* updateToUpstream(h.installation);
    yield* Scope.close(firstScope, Exit.void);
    staleActiveReads.push(JSON.stringify({ version: asset.version }));
    pruneBeforeLease = h.fs
      .remove(previous.managedVersionDirectory!, { recursive: true })
      .pipe(Effect.orDie);
    const executable = yield* h.installation.acquire();
    expect(executable.version).toBe(upstreamVersion);
    expect(yield* h.fs.exists(executable.executablePath)).toBe(true);
  }).pipe(Effect.scoped, Effect.provide(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer))),
);

it.effect("leaves no record-less release behind when a prune is interrupted", () =>
  Effect.gen(function* () {
    const h = yield* makeHarness({
      responses: upstreamResponses(yield* upstreamChecksum),
      interruptedRemoves: /[/\\](?:0\.156\.1|\.install-pruned-[^/\\]+)$/u,
    });
    yield* h.installation.start;
    yield* terminalState(h.installation);
    const previous = yield* h.installation.resolve();
    yield* updateToUpstream(h.installation);
    expect(yield* h.fs.exists(previous.managedVersionDirectory!)).toBe(false);
  }).pipe(Effect.scoped, Effect.provide(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer))),
);

it.effect("keeps a release another server leases while this one is pruning it", () =>
  Effect.gen(function* () {
    const h = yield* makeHarness({ responses: upstreamResponses(yield* upstreamChecksum) });
    yield* h.installation.start;
    yield* terminalState(h.installation);
    const firstScope = yield* Scope.make();
    yield* h.installation.acquire().pipe(Scope.provide(firstScope));
    yield* updateToUpstream(h.installation);
    const staleActiveReads: Array<string> = [];
    const other = yield* makeHarness({ baseDir: h.baseDir, staleActiveReads });
    yield* Scope.close(firstScope, Exit.void);
    let leased: CodexInstallation.CodexExecutable | undefined;
    const sessionScope = yield* Effect.scope;
    yield* makeHarness({
      baseDir: h.baseDir,
      beforePruneRename: Effect.suspend(() => {
        staleActiveReads.push(JSON.stringify({ version: asset.version }));
        return other.installation.acquire().pipe(
          Scope.provide(sessionScope),
          Effect.tap((executable) => Effect.sync(() => (leased = executable))),
          Effect.orDie,
          Effect.asVoid,
        );
      }),
    });
    expect(leased?.version).toBe(asset.version);
    expect(yield* h.fs.exists(leased!.executablePath)).toBe(true);
  }).pipe(Effect.scoped, Effect.provide(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer))),
);

it.effect("refuses to remove the runtime while another server is installing", () =>
  Effect.gen(function* () {
    const h = yield* makeHarness();
    const verifying = yield* Deferred.make<void>();
    const installing = yield* makeHarness({
      baseDir: h.baseDir,
      options: {
        validate: () => Deferred.succeed(verifying, undefined).pipe(Effect.andThen(Effect.never)),
      },
    });
    yield* installing.installation.start;
    yield* Deferred.await(verifying);
    expect((yield* Effect.flip(h.installation.remove())).detail).toContain("Stop Codex sessions");
  }).pipe(Effect.scoped, Effect.provide(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer))),
);

it.effect("keeps releases it did not install and releases a provider instance points at", () =>
  Effect.gen(function* () {
    const fs = yield* FileSystem.FileSystem;
    const baseDir = yield* fs.makeTempDirectoryScoped({ prefix: "onicodex-prune-test-" });
    const pinned = `${baseDir}/tools/codex/${asset.version}`;
    const backup = `${baseDir}/tools/codex/0.166.0-backup`;
    const options = { protectedBinaryPaths: Effect.succeed([`${pinned}/bin/codex`]) };
    const h = yield* makeHarness({
      baseDir,
      options,
      responses: upstreamResponses(yield* upstreamChecksum),
    });
    yield* h.installation.start;
    yield* terminalState(h.installation);
    yield* fs.makeDirectory(backup);
    yield* fs.writeFileString(`${backup}/notes.txt`, "kept by hand");
    yield* updateToUpstream(h.installation);
    yield* makeHarness({ baseDir, options });
    expect(yield* fs.exists(`${pinned}/codex-path/rg`)).toBe(true);
    expect(yield* fs.readFileString(`${backup}/notes.txt`)).toBe("kept by hand");
  }).pipe(Effect.scoped, Effect.provide(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer))),
);

it.effect("keeps complete releases when the active one is broken so a reinstall reuses them", () =>
  Effect.gen(function* () {
    const h = yield* makeHarness();
    yield* h.installation.start;
    yield* terminalState(h.installation);
    const installed = yield* h.installation.resolve();
    yield* h.fs.writeFileString(
      `${h.installation.managedDirectory}/active.json`,
      JSON.stringify({ version: "0.156.2" }),
    );
    const restarted = yield* makeHarness({ baseDir: h.baseDir });
    expect((yield* restarted.installation.state).phase).toBe("failed");
    expect(yield* h.fs.exists(installed.managedVersionDirectory!)).toBe(true);
    yield* restarted.installation.start;
    expect((yield* terminalState(restarted.installation)).phase).toBe("succeeded");
    expect(restarted.downloads()).toBe(0);
  }).pipe(Effect.scoped, Effect.provide(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer))),
);

it.effect(
  "resolves the new release when an update lands between reading the pointer and its release",
  () =>
    Effect.gen(function* () {
      const staleActiveReads: Array<string> = [];
      const h = yield* makeHarness({
        responses: upstreamResponses(yield* upstreamChecksum),
        staleActiveReads,
      });
      yield* h.installation.start;
      yield* terminalState(h.installation);
      yield* updateToUpstream(h.installation);
      staleActiveReads.push(JSON.stringify({ version: asset.version }));
      expect((yield* h.installation.resolve()).version).toBe(upstreamVersion);
    }).pipe(Effect.scoped, Effect.provide(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer))),
);

it.effect("keeps the managed install when an update fails its published checksum", () =>
  Effect.gen(function* () {
    const h = yield* makeHarness({ responses: upstreamResponses("f".repeat(64)) });
    yield* h.installation.start;
    yield* terminalState(h.installation);
    yield* h.installation.checkForUpdate;
    yield* h.installation.start;
    expect(yield* terminalState(h.installation)).toMatchObject({
      phase: "failed",
      installedVersion: "0.156.1",
      message: expect.stringContaining("SHA-256"),
    });
    expect((yield* h.installation.resolve()).version).toBe("0.156.1");
  }).pipe(Effect.scoped, Effect.provide(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer))),
);

it.effect("does not install a cached update the manifest has since withdrawn", () =>
  Effect.gen(function* () {
    const digest = yield* Digest.Digest;
    const checksum = Hex.encode(
      yield* digest.digest(Digest.sha256, upstreamArchive).pipe(Effect.orDie),
    );
    let manifest = ModelManifest.BUNDLED_MODEL_MANIFEST;
    const h = yield* makeHarness({
      responses: upstreamResponses(checksum),
      manifestCurrent: Effect.sync(() => manifest),
    });
    yield* h.installation.start;
    yield* terminalState(h.installation);
    yield* h.installation.checkForUpdate;
    expect((yield* h.installation.state).version).toBe(upstreamVersion);
    manifest = {
      ...manifest,
      compatibility: [
        {
          driver: "codex",
          onicodeRange: ">=0.0.42",
          ranges: [
            { range: ">=0.160.0", status: "unsupported" },
            { range: ">=0.156.0 <0.160.0", status: "supported" },
          ],
        },
      ],
    };
    const downloads = h.downloads();
    yield* h.installation.start;
    expect(yield* terminalState(h.installation)).toMatchObject({
      phase: "succeeded",
      installedVersion: "0.156.1",
      version: "0.156.1",
    });
    expect(h.downloads()).toBe(downloads);
    expect((yield* h.installation.resolve()).version).toBe("0.156.1");
  }).pipe(Effect.scoped, Effect.provide(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer))),
);

it.effect("returns a managed install the manifest withdraws to the pinned release", () =>
  Effect.gen(function* () {
    const digest = yield* Digest.Digest;
    const checksum = Hex.encode(
      yield* digest.digest(Digest.sha256, upstreamArchive).pipe(Effect.orDie),
    );
    let manifest = ModelManifest.BUNDLED_MODEL_MANIFEST;
    const h = yield* makeHarness({
      responses: upstreamResponses(checksum),
      manifestCurrent: Effect.sync(() => manifest),
    });
    yield* h.installation.checkForUpdate;
    yield* h.installation.start;
    yield* terminalState(h.installation);
    yield* h.installation.start;
    expect(yield* terminalState(h.installation)).toMatchObject({
      installedVersion: upstreamVersion,
    });

    manifest = {
      ...manifest,
      compatibility: [
        {
          driver: "codex",
          onicodeRange: ">=0.0.42",
          ranges: [
            { range: ">=0.160.0", status: "unsupported" },
            { range: ">=0.156.0 <0.160.0", status: "supported" },
          ],
        },
      ],
    };
    yield* h.installation.checkForUpdate;
    expect(yield* h.installation.state).toMatchObject({
      installedVersion: upstreamVersion,
      version: "0.156.1",
    });
    yield* h.installation.start;
    expect(yield* terminalState(h.installation)).toMatchObject({
      phase: "succeeded",
      installedVersion: "0.156.1",
      version: "0.156.1",
    });
    expect((yield* h.installation.resolve()).version).toBe("0.156.1");
  }).pipe(Effect.scoped, Effect.provide(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer))),
);

it.effect.each(["update", "reinstall"] as const)(
  "rejects withdrawal during $0 validation",
  (operation) =>
    Effect.gen(function* () {
      const digest = yield* Digest.Digest;
      const checksum = Hex.encode(
        yield* digest.digest(Digest.sha256, upstreamArchive).pipe(Effect.orDie),
      );
      let manifest = ModelManifest.BUNDLED_MODEL_MANIFEST;
      let withdraw = operation === "update";
      const h = yield* makeHarness({
        responses: upstreamResponses(checksum),
        manifestCurrent: Effect.sync(() => manifest),
        options: {
          validate: (_executable, version) =>
            Effect.sync(() => {
              if (withdraw && version === upstreamVersion)
                manifest = {
                  ...ModelManifest.BUNDLED_MODEL_MANIFEST,
                  compatibility: [
                    {
                      driver: "codex",
                      onicodeRange: ">=0.0.42",
                      ranges: [
                        { range: ">=0.160.0", status: "unsupported" },
                        { range: ">=0.156.0 <0.160.0", status: "supported" },
                      ],
                    },
                  ],
                };
            }),
        },
      });
      yield* h.installation.start;
      yield* terminalState(h.installation);
      yield* h.installation.checkForUpdate;
      if (operation === "reinstall") {
        yield* h.installation.start;
        expect((yield* terminalState(h.installation)).phase).toBe("succeeded");
        withdraw = true;
      }
      yield* h.installation.start;
      expect((yield* terminalState(h.installation)).phase).toBe("failed");
      if (operation === "update") {
        expect((yield* h.installation.resolve()).version).toBe("0.156.1");
      } else {
        expect((yield* h.installation.state).version).toBe("0.156.1");
        yield* Effect.flip(h.installation.resolve());
      }
    }).pipe(Effect.scoped, Effect.provide(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer))),
);
