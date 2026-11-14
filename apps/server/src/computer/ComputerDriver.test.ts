import * as NodeCrypto from "@effect/platform-node/NodeCrypto";
import * as NodeServices from "@effect/platform-node/NodeServices";
import { expect, it } from "@effect/vitest";
import { cliArchiveTarCommand } from "@onicode/shared/cliRelease";
import * as HostProcess from "@onicode/shared/HostProcess";
import * as Digest from "effect/crypto/Digest";
import * as Effect from "effect/Effect";
import * as Hex from "effect/encoding/Hex";
import * as FileSystem from "effect/FileSystem";
import * as Layer from "effect/Layer";
import * as Path from "effect/Path";
import * as Stream from "effect/Stream";
import { HttpClient, HttpClientResponse } from "effect/http";
import { ChildProcess, ChildProcessSpawner } from "effect/process";

import * as ComputerDriver from "./ComputerDriver.ts";

const platform = HostProcess.Platform.defaultValue();
const executableName = platform === "win32" ? "cua-driver.exe" : "cua-driver";

const makeHarness = Effect.fn("test.makeComputerDriver")(function* (
  options: { readonly sha256?: string; readonly unsupported?: boolean } = {},
) {
  const fs = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  const spawner = yield* ChildProcessSpawner.ChildProcessSpawner;
  const baseDir = yield* fs.makeTempDirectoryScoped({ prefix: "oni-computer-driver-test-" });
  const source = path.join(baseDir, "source");
  yield* fs.makeDirectory(source);
  const entries = [executableName, "libcua_driver_sdk.so", "cua_driver_abi.h"];
  yield* Effect.forEach(entries, (name) =>
    fs.writeFileString(path.join(source, name), name, { mode: 0o755 }),
  );
  const archivePath = path.join(baseDir, "release.tar.gz");
  const tar = yield* spawner.spawn(
    ChildProcess.make(
      cliArchiveTarCommand(platform, process.env),
      ["-czf", archivePath, "-C", source, ...entries],
      { shell: false },
    ),
  );
  expect(yield* tar.exitCode).toBe(0);
  const archive = yield* fs.readFile(archivePath);
  const digest = yield* Digest.Digest;
  const requests: Array<string> = [];
  const driver = yield* ComputerDriver.makeComputerDriver({
    baseDir,
    release: options.unsupported
      ? null
      : {
          version: "1.2.3",
          url: "https://github.com/trycua/cua/releases/download/fixture.tar.gz",
          bytes: archive.byteLength,
          sha256:
            options.sha256 ??
            Hex.encode(yield* digest.digest(Digest.sha256, archive).pipe(Effect.orDie)),
        },
  }).pipe(
    Effect.provideService(
      HttpClient.HttpClient,
      HttpClient.make((request) =>
        Effect.sync(() => {
          requests.push(request.url);
          const response = HttpClientResponse.fromWeb(request, new Response(null));
          return Object.defineProperty(response, "stream", {
            value: Stream.make(archive.subarray(0, 40), archive.subarray(40)),
          });
        }),
      ),
    ),
  );
  return { driver, fs, path, installRoot: path.join(baseDir, "tools", "cua-driver"), requests };
});

it.layer(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer))("ComputerDriver", (it) => {
  it.effect("installs the pinned driver without its SDK files and drops older versions", () =>
    Effect.gen(function* () {
      const { driver, fs, path, installRoot, requests } = yield* makeHarness();
      yield* fs.makeDirectory(path.join(installRoot, "1.0.0"), { recursive: true });
      expect(yield* driver.state).toEqual({ state: "missing" });

      const executable = yield* driver.executable;

      expect(executable).toBe(path.join(installRoot, "1.2.3", executableName));
      expect(yield* fs.readDirectory(path.join(installRoot, "1.2.3"))).toEqual([executableName]);
      expect(yield* fs.readDirectory(installRoot)).toEqual(["1.2.3"]);
      expect(yield* driver.state).toEqual({ state: "installed" });
      expect(yield* driver.executable).toBe(executable);
      expect(requests).toHaveLength(1);
    }),
  );

  it.effect(
    "rejects a download that fails its hash, reports it, and retries on the next call",
    () =>
      Effect.gen(function* () {
        const { driver, fs, installRoot, requests } = yield* makeHarness({
          sha256: "0".repeat(64),
        });

        const error = yield* driver.executable.pipe(Effect.flip);

        expect(error._tag).toBe("ComputerDriverInstallError");
        expect(yield* fs.readDirectory(installRoot)).toEqual([]);
        expect(yield* driver.state).toEqual({ state: "failed", message: error.message });
        yield* driver.executable.pipe(Effect.flip);
        expect(requests).toHaveLength(2);
      }),
  );

  it.effect("reports a machine cua-driver has no build for", () =>
    Effect.gen(function* () {
      const { driver, requests } = yield* makeHarness({ unsupported: true });

      expect(yield* driver.state).toEqual({ state: "unsupported" });
      expect((yield* driver.executable.pipe(Effect.flip))._tag).toBe(
        "ComputerDriverUnsupportedError",
      );
      expect(requests).toHaveLength(0);
    }),
  );
});
