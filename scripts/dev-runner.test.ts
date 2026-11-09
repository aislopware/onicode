// @effect-diagnostics nodeBuiltinImport:off - builds real worktree layouts on disk.
import * as NodeServices from "@effect/platform-node/NodeServices";
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";
import * as NetService from "@onicode/shared/Net";
import * as HostProcess from "@onicode/shared/HostProcess";
import { assert, describe, it } from "@effect/vitest";
import * as ByteSize from "effect/ByteSize";
import * as ConfigProvider from "effect/ConfigProvider";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Layer from "effect/Layer";
import * as Logger from "effect/Logger";
import * as Path from "effect/Path";
import * as PlatformError from "effect/PlatformError";
import * as Sink from "effect/Sink";
import * as Stream from "effect/Stream";
import { ChildProcessSpawner } from "effect/process";

import {
  checkPortAvailabilityOnHosts,
  createDevRunnerEnv,
  findFirstAvailableOffset,
  resolveModePortOffsets,
  runDevRunnerWithInput,
} from "./dev-runner.ts";

const emptyConfigLayer = ConfigProvider.layer(ConfigProvider.fromEnv({ env: {} }));
const netServiceLayer = Layer.succeed(NetService.NetService, {
  canListenOnHost: () => Effect.succeed(true),
  isPortAvailableOnLoopback: () => Effect.succeed(true),
  hasListenerOnHost: () => Effect.succeed(false),
  reserveLoopbackPort: () => Effect.succeed(49_152),
  findAvailablePort: (port) => Effect.succeed(port),
});

function mockProcess(exit: number | PlatformError.PlatformError) {
  return ChildProcessSpawner.makeHandle({
    pid: ChildProcessSpawner.ProcessId(1),
    exitCode:
      typeof exit === "number"
        ? Effect.succeed(ChildProcessSpawner.ExitCode(exit))
        : Effect.fail(exit),
    isRunning: Effect.succeed(false),
    kill: () => Effect.void,
    unref: Effect.succeed(Effect.void),
    stdin: Sink.drain,
    stdout: Stream.empty,
    stderr: Stream.empty,
    all: Stream.empty,
    getInputFd: () => Sink.drain,
    getOutputFd: () => Stream.empty,
  });
}

const devServerInput = {
  mode: "dev:server",
  oniHome: "/tmp/onicode-dev-runner",
  autoBootstrapProjectFromCwd: undefined,
  logWebSocketEvents: undefined,
  host: undefined,
  port: 13_773,
  devUrl: undefined,
  dryRun: false,
  runArgs: ["--inspect", "secret-token-value"],
} as const;

it.layer(NodeServices.layer)("dev-runner", (it) => {
  describe("createDevRunnerEnv", () => {
    it.effect("strips inherited service-launcher context", () =>
      Effect.gen(function* () {
        const env = yield* createDevRunnerEnv({
          mode: "dev",
          baseEnv: {
            ONICODE_SERVICE_LAUNCHER_CONTEXT: '{"childVersion":"9.9.9"}',
            ONICODE_BOOT_SERVICE_UNIT: "onicode.service",
          },
          serverOffset: 0,
          webOffset: 0,
          oniHome: undefined,
          autoBootstrapProjectFromCwd: undefined,
          logWebSocketEvents: undefined,
          host: undefined,
          port: undefined,
          devUrl: undefined,
        });

        assert.equal(env.ONICODE_SERVICE_LAUNCHER_CONTEXT, undefined);
        assert.equal(env.ONICODE_BOOT_SERVICE_UNIT, undefined);
      }),
    );

    it.effect("points the desktop app at its renderer and debug ports in dev mode", () =>
      Effect.gen(function* () {
        const env = yield* createDevRunnerEnv({
          mode: "dev",
          baseEnv: { ONICODE_MODE: "standalone", ONICODE_HOST: "0.0.0.0", ONICODE_DEV: "1" },
          serverOffset: 2,
          webOffset: 2,
          oniHome: undefined,
          autoBootstrapProjectFromCwd: undefined,
          logWebSocketEvents: undefined,
          host: "0.0.0.0",
          port: undefined,
          devUrl: undefined,
        });

        assert.equal(env.ONICODE_PORT, "13775");
        assert.equal(env.VITE_DEV_SERVER_URL, "http://127.0.0.1:5735");
        assert.equal(env.ONICODE_DESKTOP_REMOTE_DEBUGGING_PORT, "19224");
        assert.equal(env.ONICODE_MODE, undefined);
        assert.equal(env.ONICODE_HOST, undefined);
        assert.equal(env.ONICODE_DEV, undefined);
      }),
    );

    it.effect("runs a standalone server on the requested host in dev:server mode", () =>
      Effect.gen(function* () {
        const env = yield* createDevRunnerEnv({
          mode: "dev:server",
          baseEnv: { VITE_DEV_SERVER_URL: "http://127.0.0.1:5733" },
          serverOffset: 0,
          webOffset: 0,
          oniHome: undefined,
          autoBootstrapProjectFromCwd: undefined,
          logWebSocketEvents: undefined,
          host: "0.0.0.0",
          port: undefined,
          devUrl: undefined,
        });

        assert.equal(env.ONICODE_MODE, "standalone");
        assert.equal(env.ONICODE_HOST, "0.0.0.0");
        assert.equal(env.ONICODE_DEV, "1");
        assert.equal(env.VITE_DEV_SERVER_URL, undefined);
        assert.equal(env.ONICODE_DESKTOP_REMOTE_DEBUGGING_PORT, undefined);
      }),
    );
  });

  describe("findFirstAvailableOffset", () => {
    it.effect("advances until all required ports are available", () =>
      Effect.gen(function* () {
        const taken = new Set([13773, 5733, 13774, 5734]);
        const offset = yield* findFirstAvailableOffset({
          startOffset: 0,
          requireServerPort: true,
          requireWebPort: true,
          checkPortAvailability: (port) => Effect.succeed(!taken.has(port)),
        });

        assert.equal(offset, 2);
      }),
    );

    it.effect("probes the renderer and debug ports on loopback only", () =>
      Effect.gen(function* () {
        const probed: Array<{ port: number; role: string | undefined }> = [];
        const offset = yield* findFirstAvailableOffset({
          startOffset: 0,
          requireServerPort: true,
          requireWebPort: true,
          checkPortAvailability: (port, role) => {
            probed.push({ port, role });
            return Effect.succeed(port !== 19_222);
          },
        });

        assert.equal(offset, 1);
        assert.deepStrictEqual(probed.slice(0, 3), [
          { port: 13_773, role: "server" },
          { port: 5733, role: "loopback" },
          { port: 19_222, role: "loopback" },
        ]);
      }),
    );
  });

  describe("checkPortAvailabilityOnHosts", () => {
    it.effect("checks overlapping hosts sequentially to avoid self-interference", () =>
      Effect.gen(function* () {
        let inFlightCount = 0;
        const calls: Array<[number, string]> = [];

        const available = yield* checkPortAvailabilityOnHosts(
          13_773,
          ["127.0.0.1", "0.0.0.0", "::"],
          (port, host) =>
            Effect.promise(async () => {
              calls.push([port, host]);
              inFlightCount += 1;
              const overlapped = inFlightCount > 1;
              await Promise.resolve();
              inFlightCount -= 1;
              return !overlapped;
            }),
        );

        assert.equal(available, true);
        assert.deepStrictEqual(calls, [
          [13_773, "127.0.0.1"],
          [13_773, "0.0.0.0"],
          [13_773, "::"],
        ]);
      }),
    );
  });

  describe("devPortProbeHosts", () => {
    it.effect("passes the port role so only the server port sees the bind host", () =>
      Effect.gen(function* () {
        const probed: Array<{ port: number; role: string | undefined }> = [];

        yield* resolveModePortOffsets({
          mode: "dev",
          startOffset: 0,
          hasExplicitServerPort: false,
          hasExplicitDevUrl: false,
          checkPortAvailability: (port, role) => {
            probed.push({ port, role });
            return Effect.succeed(true);
          },
        });

        assert.deepStrictEqual(probed, [
          { port: 13_773, role: "server" },
          { port: 5733, role: "loopback" },
          { port: 19_222, role: "loopback" },
        ]);
      }),
    );
  });

  describe("resolveModePortOffsets", () => {
    it.effect("uses a shared fallback offset for dev mode", () =>
      Effect.gen(function* () {
        const taken = new Set([13773, 5733]);
        const offsets = yield* resolveModePortOffsets({
          mode: "dev",
          startOffset: 0,
          hasExplicitServerPort: false,
          hasExplicitDevUrl: false,
          checkPortAvailability: (port) => Effect.succeed(!taken.has(port)),
        });

        assert.deepStrictEqual(offsets, { serverOffset: 1, webOffset: 1 });
      }),
    );

    it.effect("shifts only server offset for dev:server", () =>
      Effect.gen(function* () {
        const taken = new Set([13773]);
        const offsets = yield* resolveModePortOffsets({
          mode: "dev:server",
          startOffset: 0,
          hasExplicitServerPort: false,
          hasExplicitDevUrl: false,
          checkPortAvailability: (port) => Effect.succeed(!taken.has(port)),
        });

        assert.deepStrictEqual(offsets, { serverOffset: 1, webOffset: 1 });
      }),
    );
  });

  describe("runDevRunnerWithInput", () => {
    it.effect("preserves process spawn context and the exact platform cause", () => {
      const cause = PlatformError.systemError({
        _tag: "NotFound",
        module: "ChildProcess",
        method: "spawn",
        description: "vp was not found",
      });
      const spawnerLayer = Layer.succeed(
        ChildProcessSpawner.ChildProcessSpawner,
        ChildProcessSpawner.make(() => Effect.fail(cause)),
      );

      return Effect.gen(function* () {
        const error = yield* runDevRunnerWithInput(devServerInput).pipe(
          Effect.provide(Layer.mergeAll(emptyConfigLayer, netServiceLayer, spawnerLayer)),
          Effect.provideService(HostProcess.Platform, "linux"),
          Effect.flip,
        );

        if (error._tag !== "DevRunnerProcessError") {
          assert.fail(`Unexpected error: ${error._tag}`);
        }
        assert.equal(error.operation, "spawn");
        assert.equal(error.mode, "dev:server");
        assert.equal(error.executable, "vp");
        assert.equal(error.argumentCount, 5);
        assert.equal(error.shell, false);
        assert.equal(error.cause, cause);
        assert.ok(!error.message.includes(cause.message));
        assert.notProperty(error, "args");
        assert.notInclude(error.message, "secret-token-value");
      });
    });

    it.effect("ignores an ambient ONICODE_HOME when no home is resolved", () =>
      Effect.gen(function* () {
        const env = yield* createDevRunnerEnv({
          mode: "dev",
          baseEnv: { ONICODE_HOME: "/home/user/.onicode" },
          serverOffset: 0,
          webOffset: 0,
          oniHome: undefined,
          autoBootstrapProjectFromCwd: undefined,
          logWebSocketEvents: undefined,
          host: undefined,
          port: undefined,
          devUrl: undefined,
        });

        assert.equal(env.ONICODE_HOME, undefined);
      }),
    );

    it.effect("expands a home-relative Onicode home the way the server does", () =>
      Effect.gen(function* () {
        const path = yield* Path.Path;
        const homeFor = (oniHome: string) =>
          createDevRunnerEnv({
            mode: "dev",
            baseEnv: {},
            serverOffset: 0,
            webOffset: 0,
            oniHome,
            autoBootstrapProjectFromCwd: undefined,
            logWebSocketEvents: undefined,
            host: undefined,
            port: undefined,
            devUrl: undefined,
          }).pipe(
            Effect.provideService(HostProcess.HomeDirectory, "/home/dev"),
            Effect.map((env) => env.ONICODE_HOME),
          );

        assert.equal(yield* homeFor("~/oni-dev"), path.join("/home/dev", "oni-dev"));
        assert.equal(yield* homeFor("~"), "/home/dev");
        assert.equal(yield* homeFor("~\\oni-dev"), path.join("/home/dev", "oni-dev"));
        assert.equal(yield* homeFor("  ~/oni-dev  "), path.join("/home/dev", "oni-dev"));
        assert.equal(yield* homeFor("~someone/oni-dev"), path.resolve("~someone/oni-dev"));
        assert.equal(yield* homeFor("relative/oni-dev"), path.resolve("relative/oni-dev"));
        assert.equal(yield* homeFor("/tmp/oni-dev"), path.resolve("/tmp/oni-dev"));
      }),
    );

    it.effect("reports non-zero exits without manufacturing a cause", () => {
      const spawnerLayer = Layer.succeed(
        ChildProcessSpawner.ChildProcessSpawner,
        ChildProcessSpawner.make(() => Effect.succeed(mockProcess(17))),
      );

      return Effect.gen(function* () {
        const error = yield* runDevRunnerWithInput(devServerInput).pipe(
          Effect.provide(Layer.mergeAll(emptyConfigLayer, netServiceLayer, spawnerLayer)),
          Effect.provideService(HostProcess.Platform, "linux"),
          Effect.flip,
        );

        if (error._tag !== "DevRunnerProcessExitError") {
          assert.fail(`Unexpected error: ${error._tag}`);
        }
        assert.equal(error.mode, "dev:server");
        assert.equal(error.executable, "vp");
        assert.equal(error.argumentCount, 5);
        assert.equal(error.shell, false);
        assert.equal(error.exitCode, 17);
        assert.ok(!("cause" in error));
        assert.notProperty(error, "args");
        assert.notInclude(error.message, "secret-token-value");
      });
    });

    describe("onicode home precedence", () => {
      const makeWorktree = Effect.acquireRelease(
        Effect.sync(() => {
          const root = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "oni-devrunner-"));
          NodeFS.writeFileSync(
            NodePath.join(root, ".git"),
            "gitdir: /elsewhere/.git/worktrees/x\n",
          );
          return root;
        }),
        (root) => Effect.sync(() => NodeFS.rmSync(root, { recursive: true, force: true })),
      );

      const spawnedHome = (input: {
        readonly oniHome: string | undefined;
        readonly cwd: string;
        readonly ambientHome: string | undefined;
      }) =>
        Effect.gen(function* () {
          let captured: Record<string, string | undefined> | undefined;
          const spawnerLayer = Layer.succeed(
            ChildProcessSpawner.ChildProcessSpawner,
            ChildProcessSpawner.make((command) => {
              captured = (
                command as {
                  readonly options?: { readonly env?: Record<string, string | undefined> };
                }
              ).options?.env;
              return Effect.succeed(mockProcess(0));
            }),
          );

          yield* runDevRunnerWithInput({ ...devServerInput, oniHome: input.oniHome }).pipe(
            Effect.provide(Layer.mergeAll(emptyConfigLayer, netServiceLayer, spawnerLayer)),
            Effect.provideService(HostProcess.Platform, "linux"),
            Effect.provideService(HostProcess.WorkingDirectory, input.cwd),
            // No ~/.onicode here, so these runs never seed from this machine's install.
            Effect.provideService(HostProcess.HomeDirectory, input.cwd),
            Effect.provideService(
              HostProcess.Environment,
              input.ambientHome === undefined ? {} : { ONICODE_HOME: input.ambientHome },
            ),
          );

          return captured?.ONICODE_HOME;
        });

      it.effect("prefers an explicit --home-dir over the worktree default", () =>
        Effect.gen(function* () {
          const path = yield* Path.Path;
          const root = yield* makeWorktree;
          const home = yield* spawnedHome({
            oniHome: "/tmp/explicit-home",
            cwd: root,
            ambientHome: "/home/user/.onicode",
          });
          assert.equal(home, path.resolve("/tmp/explicit-home"));
        }).pipe(Effect.scoped),
      );

      it.effect("treats a blank --home-dir as unset rather than as a selection", () =>
        Effect.gen(function* () {
          const path = yield* Path.Path;
          const root = yield* makeWorktree;
          const home = yield* spawnedHome({
            oniHome: "   ",
            cwd: root,
            ambientHome: "/home/user/.onicode",
          });
          assert.equal(home, path.join(path.resolve(root), ".onicode"));
        }).pipe(Effect.scoped),
      );

      it.effect("prefers the worktree .onicode over an ambient ONICODE_HOME", () =>
        Effect.gen(function* () {
          const path = yield* Path.Path;
          const root = yield* makeWorktree;
          const home = yield* spawnedHome({
            oniHome: undefined,
            cwd: root,
            ambientHome: "/home/user/.onicode",
          });
          assert.equal(home, path.join(path.resolve(root), ".onicode"));
        }).pipe(Effect.scoped),
      );

      it.effect("seeds a worktree database from ~/.onicode only when it has none", () =>
        Effect.gen(function* () {
          const path = yield* Path.Path;
          const root = yield* makeWorktree;
          const home = path.join(root, "home");
          NodeFS.mkdirSync(path.join(home, ".onicode", "userdata"), { recursive: true });
          NodeFS.writeFileSync(path.join(home, ".onicode", "userdata", "statev2.sqlite"), "");

          const spawnedArgs = (seed: boolean | undefined) =>
            Effect.gen(function* () {
              const spawned: Array<ReadonlyArray<string>> = [];
              const spawnerLayer = Layer.succeed(
                ChildProcessSpawner.ChildProcessSpawner,
                ChildProcessSpawner.make((command) => {
                  spawned.push(
                    (command as unknown as { readonly args: ReadonlyArray<string> }).args,
                  );
                  return Effect.succeed(mockProcess(0));
                }),
              );
              yield* runDevRunnerWithInput({ ...devServerInput, oniHome: undefined, seed }).pipe(
                Effect.provide(Layer.mergeAll(emptyConfigLayer, netServiceLayer, spawnerLayer)),
                Effect.provideService(HostProcess.Platform, "linux"),
                Effect.provideService(HostProcess.WorkingDirectory, root),
                Effect.provideService(HostProcess.HomeDirectory, home),
                Effect.provideService(HostProcess.Environment, {}),
              );
              return spawned.filter((args) =>
                args.some((arg) => arg.endsWith("migrate-dev-db.ts")),
              );
            });

          assert.deepStrictEqual(yield* spawnedArgs(false), []);
          const [seedArgs] = yield* spawnedArgs(undefined);
          assert.includeMembers([...(seedArgs ?? [])], ["--base-dir", path.join(root, ".onicode")]);

          // Either database means the worktree already has data.
          for (const name of ["state.sqlite", "statev2.sqlite"]) {
            const file = path.join(root, ".onicode", "userdata", name);
            NodeFS.mkdirSync(path.dirname(file), { recursive: true });
            NodeFS.writeFileSync(file, "");
            assert.deepStrictEqual(yield* spawnedArgs(undefined), []);
            NodeFS.rmSync(file);
          }
        }).pipe(Effect.scoped),
      );

      it.effect.each([
        { sourceBytes: 500 * 1024 * 1024 - 1, shouldSeed: true },
        { sourceBytes: 500 * 1024 * 1024, shouldSeed: true },
        { sourceBytes: 500 * 1024 * 1024 + 1, shouldSeed: false },
      ])(
        "checks source size before seeding an empty worktree: $sourceBytes bytes",
        ({ sourceBytes, shouldSeed }) =>
          Effect.gen(function* () {
            const fs = yield* FileSystem.FileSystem;
            const path = yield* Path.Path;
            const root = yield* makeWorktree;
            const home = path.join(root, "home");
            const source = path.join(home, ".onicode", "userdata", "statev2.sqlite");
            NodeFS.mkdirSync(path.dirname(source), { recursive: true });
            NodeFS.writeFileSync(source, "");
            const spawned: Array<ReadonlyArray<string>> = [];
            const warnings: Array<string> = [];
            const spawnerLayer = Layer.succeed(
              ChildProcessSpawner.ChildProcessSpawner,
              ChildProcessSpawner.make((command) => {
                spawned.push((command as unknown as { readonly args: ReadonlyArray<string> }).args);
                return Effect.succeed(mockProcess(0));
              }),
            );

            yield* runDevRunnerWithInput({ ...devServerInput, oniHome: undefined }).pipe(
              Effect.provide([
                emptyConfigLayer,
                netServiceLayer,
                spawnerLayer,
                Logger.layer([
                  Logger.make((options) => {
                    if (options.logLevel === "Warn") warnings.push(String(options.message));
                  }),
                ]),
              ]),
              Effect.provideService(FileSystem.FileSystem, {
                ...fs,
                stat: (file) =>
                  fs
                    .stat(file)
                    .pipe(
                      Effect.map((info) =>
                        file === source ? { ...info, size: ByteSize.bytes(sourceBytes) } : info,
                      ),
                    ),
              }),
              Effect.provideService(HostProcess.Platform, "linux"),
              Effect.provideService(HostProcess.WorkingDirectory, root),
              Effect.provideService(HostProcess.HomeDirectory, home),
              Effect.provideService(HostProcess.Environment, {}),
            );

            assert.equal(
              spawned.some((args) => args.some((arg) => arg.endsWith("migrate-dev-db.ts"))),
              shouldSeed,
            );
            assert.deepStrictEqual(spawned.at(-1), [
              "run",
              "--filter=onicode",
              "dev",
              ...devServerInput.runArgs,
            ]);
            if (shouldSeed) {
              assert.deepStrictEqual(warnings, []);
            } else {
              assert.lengthOf(warnings, 1);
              assert.include(warnings[0]!, source);
              assert.include(warnings[0]!, "skipping automatic database seeding");
              assert.include(warnings[0]!, "vp run migrate-dev-db");
            }
          }).pipe(Effect.scoped),
      );

      it.effect("falls back to an ambient ONICODE_HOME outside a worktree", () =>
        Effect.gen(function* () {
          const path = yield* Path.Path;
          const home = yield* spawnedHome({
            oniHome: undefined,
            cwd: NodeOS.tmpdir(),
            ambientHome: "/home/user/.onicode",
          });
          assert.equal(home, path.resolve("/home/user/.onicode"));
        }),
      );
    });
  });
});
