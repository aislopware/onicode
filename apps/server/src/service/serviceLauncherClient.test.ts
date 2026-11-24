import * as NodeServices from "@effect/platform-node/NodeServices";
import * as FileSystem from "effect/FileSystem";
import * as Path from "effect/Path";
import * as Logger from "effect/Logger";
import * as PlatformError from "effect/PlatformError";
import { assert, describe, expect, it } from "@effect/vitest";
import * as HostProcess from "@onicode/shared/HostProcess";
import * as Effect from "effect/Effect";
import * as Deferred from "effect/Deferred";
import * as Fiber from "effect/Fiber";

import {
  SERVICE_LAUNCHER_CONTEXT_ENV,
  SERVICE_LAUNCHER_PROTOCOL,
  type ServiceLauncherChildMessage,
  type ServiceLauncherParentMessage,
} from "./serviceProtocol.ts";
import { posixLauncherScript } from "./pinnedRuntime.ts";
import * as ServiceLauncherClient from "./serviceLauncherClient.ts";

class FakeLauncherProcess {
  readonly connected: boolean;
  readonly env: Record<string, string | undefined>;
  readonly sent: ServiceLauncherChildMessage[] = [];
  readonly #listeners = new Map<string, Set<(...args: ReadonlyArray<unknown>) => void>>();

  constructor(context: unknown, connected = true) {
    this.connected = connected;
    this.env = { [SERVICE_LAUNCHER_CONTEXT_ENV]: JSON.stringify(context) };
  }

  send = (message: ServiceLauncherChildMessage, callback?: (error: Error | null) => void) => {
    this.sent.push(message);
    callback?.(null);
    return true;
  };

  on = (event: "message" | "disconnect", listener: (...args: ReadonlyArray<unknown>) => void) => {
    const listeners = this.#listeners.get(event) ?? new Set();
    listeners.add(listener);
    this.#listeners.set(event, listeners);
  };

  off = (event: "message" | "disconnect", listener: (...args: ReadonlyArray<unknown>) => void) => {
    this.#listeners.get(event)?.delete(listener);
  };

  emit(message: ServiceLauncherParentMessage) {
    for (const listener of this.#listeners.get("message") ?? []) listener(message);
  }
}

// Windows has neither /bin/sh nor the execute bit a PATH search checks.
const posixHost = HostProcess.Platform.defaultValue() !== "win32";

const makeClient = (host: FakeLauncherProcess, currentVersion: string) =>
  ServiceLauncherClient.make({ currentVersion }).pipe(
    Effect.provideService(ServiceLauncherClient.ServiceLauncherHostProcess, host),
    Effect.provideService(HostProcess.Environment, host.env),
    Effect.provide(NodeServices.layer),
  );

// regression: #34502 a remote update never replaces the launcher, so an installed one stays protocol 3
it.effect.each([3, SERVICE_LAUNCHER_PROTOCOL])(
  "waits for a protocol-%i launcher to durably commit the trial update ID, then moves owned launchers",
  (protocol) =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const root = yield* fs.makeTempDirectoryScoped({ prefix: "oni-trial-" });
      const baseDir = path.join(root, ".onicode");
      const exe = (version: string) => path.join(baseDir, "runtime/versions", version, "onicode");
      const installed = path.join(root, ".local/bin/onicode");
      for (const file of [exe("1.0.0"), exe("1.1.0")]) {
        yield* fs.makeDirectory(path.dirname(file), { recursive: true });
        yield* fs.writeFileString(file, "", { mode: 0o755 });
      }
      const cwd = path.join(root, "work");
      const inCwd = path.join(cwd, "onicode");
      for (const link of [installed, inCwd]) {
        yield* fs.makeDirectory(path.dirname(link), { recursive: true });
        yield* fs.symlink(exe("1.0.0"), link);
      }

      const pending = {
        id: "update-1",
        fromVersion: "1.0.0",
        targetVersion: "1.1.0",
        dbPath: "/tmp/state.sqlite",
        status: "pending" as const,
      };
      const host = new FakeLauncherProcess({ protocol, childVersion: "1.1.0", update: pending });
      host.env["HOME"] = root;
      // The launcher reloads the user manager after commit, so oomd's watch must already be mirrored.
      const bin = path.join(root, "bin");
      const calls = path.join(root, "systemctl-calls");
      yield* fs.makeDirectory(bin);
      yield* fs.writeFileString(
        path.join(bin, "systemctl"),
        [
          "#!/bin/sh",
          `echo "$*" >>'${calls}'`,
          'case "$*" in',
          "  *ControlGroup*) echo /user.slice/user-1000.slice/user@1000.service ;;",
          "  *ManagedOOMMemoryPressureLimit*) echo 0 ;;",
          "  *ManagedOOMMemoryPressure\\ *) echo kill ;;",
          "esac",
          "",
        ].join("\n"),
      );
      yield* fs.chmod(path.join(bin, "systemctl"), 0o755);
      host.env["PATH"] = `${bin}:.:/usr/bin:/bin`;
      const sentPrepared = yield* Deferred.make<void>();
      const send = host.send;
      host.send = (message, callback) => {
        Deferred.doneUnsafe(sentPrepared, Effect.void);
        return send(message, callback);
      };
      const client = yield* makeClient(host, "1.1.0").pipe(
        Effect.provideService(HostProcess.Platform, "linux"),
        Effect.provideService(HostProcess.WorkingDirectory, cwd),
      );
      const prepared = yield* Effect.forkChild(client.prepareTrial({ baseDir }), {
        startImmediately: true,
      });
      yield* Deferred.await(sentPrepared);
      // Nothing runs systemctl after "prepared", so the log is what ran before it. Windows has no
      // /bin/sh to run the stub with.
      if (posixHost) {
        expect(yield* fs.readFileString(calls)).toContain(
          "--user set-property --runtime -- -.slice ManagedOOMMemoryPressure=kill",
        );
      }
      expect(host.sent).toEqual([{ type: "prepared", updateId: "update-1" }]);
      assert.equal(yield* fs.readLink(installed), exe("1.0.0"));

      const committed = {
        id: pending.id,
        fromVersion: pending.fromVersion,
        targetVersion: pending.targetVersion,
        status: "committed" as const,
      };
      host.emit({ type: "committed", updateId: committed.id });
      expect(yield* Fiber.join(prepared)).toEqual(committed);
      assert.equal(yield* fs.readFileString(installed), posixLauncherScript(exe("1.1.0")));
      if (posixHost) {
        assert.equal(yield* fs.readFileString(inCwd), posixLauncherScript(exe("1.1.0")));
      }

      // A later start replays the stored outcome; user pins made since then stay put.
      yield* fs.remove(installed);
      yield* fs.symlink(exe("1.0.0"), installed);
      const restarted = new FakeLauncherProcess({
        protocol,
        childVersion: "1.1.0",
        update: committed,
      });
      restarted.env["HOME"] = root;
      const restartedClient = yield* makeClient(restarted, "1.1.0");
      expect(yield* restartedClient.prepareTrial({ baseDir })).toEqual(committed);
      assert.equal(yield* fs.readLink(installed), exe("1.0.0"));
    }).pipe(Effect.scoped, Effect.provide(NodeServices.layer)),
);

it.effect("returns the launcher-generated ID only after update acceptance", () =>
  Effect.gen(function* () {
    const host = new FakeLauncherProcess({
      protocol: SERVICE_LAUNCHER_PROTOCOL,
      childVersion: "1.0.0",
    });
    const client = yield* makeClient(host, "1.0.0");
    const requested = yield* Effect.forkChild(
      client.requestUpdate({ targetVersion: "1.1.0", dbPath: "/tmp/state.sqlite" }),
      { startImmediately: true },
    );
    yield* Effect.yieldNow;
    host.emit({
      type: "update-accepted",
      updateId: "launcher-id",
    });
    expect(yield* Fiber.join(requested)).toBe("launcher-id");
  }),
);

it.effect("preserves a launcher rejection as a distinct error", () =>
  Effect.gen(function* () {
    const host = new FakeLauncherProcess({
      protocol: SERVICE_LAUNCHER_PROTOCOL,
      childVersion: "1.0.0",
    });
    const client = yield* makeClient(host, "1.0.0");
    const requested = yield* Effect.forkChild(
      client.requestUpdate({ targetVersion: "1.1.0", dbPath: "/tmp/state.sqlite" }),
      { startImmediately: true },
    );
    yield* Effect.yieldNow;
    host.emit({ type: "update-rejected", reason: "requires local update" });
    expect(yield* Fiber.join(requested).pipe(Effect.flip)).toMatchObject({
      _tag: "ServiceLauncherRejectedError",
      targetVersion: "1.1.0",
      reason: "requires local update",
    });
  }),
);

it.effect("stops the server when the launcher asks over IPC", () =>
  Effect.gen(function* () {
    const host = new FakeLauncherProcess({
      protocol: SERVICE_LAUNCHER_PROTOCOL,
      childVersion: "1.0.0",
    });
    const stopped = yield* Effect.forkChild(
      ServiceLauncherClient.awaitServiceLauncherStop({ currentVersion: "1.0.0" }).pipe(
        Effect.provideService(ServiceLauncherClient.ServiceLauncherHostProcess, host),
        Effect.provideService(HostProcess.Environment, host.env),
      ),
      { startImmediately: true },
    );
    host.emit({ type: "stop" });
    yield* Fiber.join(stopped);
  }),
);

it.effect("rejects contradictory trial context instead of leaving activation closed", () =>
  Effect.gen(function* () {
    const host = new FakeLauncherProcess({
      protocol: SERVICE_LAUNCHER_PROTOCOL,
      childVersion: "1.1.0",
      update: {
        id: "update-1",
        fromVersion: "1.0.0",
        targetVersion: "1.2.0",
        dbPath: "/tmp/state.sqlite",
        status: "pending",
      },
    });
    const error = yield* makeClient(host, "1.1.0").pipe(Effect.flip);
    expect(error.message).toBe("The service launcher supplied invalid startup context.");
  }),
);

it.effect("ignores launcher context inherited without an IPC channel", () =>
  Effect.gen(function* () {
    const host = new FakeLauncherProcess(
      { protocol: SERVICE_LAUNCHER_PROTOCOL, childVersion: "1.0.0" },
      false,
    );
    const client = yield* makeClient(host, "1.1.0");
    const error = yield* client
      .requestUpdate({ targetVersion: "1.2.0", dbPath: "/tmp/state.sqlite" })
      .pipe(Effect.flip);
    expect(error).toMatchObject({ _tag: "ServiceLauncherClientError", operation: "unmanaged" });
    expect(host.sent).toEqual([]);
  }),
);

it.layer(NodeServices.layer)("self-update launchers", (it) => {
  it.effect.skipIf(!posixHost)(
    "updates custom install and PATH launchers, including versions older than fromVersion",
    () =>
      Effect.gen(function* () {
        const fs = yield* FileSystem.FileSystem;
        const path = yield* Path.Path;
        const root = yield* fs.makeTempDirectoryScoped({ prefix: "oni-update-" });
        const baseDir = path.join(root, ".onicode");
        const exe = (version: string) => path.join(baseDir, "runtime/versions", version, "onicode");
        const installed = path.join(root, "custom/bin/onicode");
        const onPath = path.join(root, "bin/onicode");
        for (const file of [exe("1.0.0"), exe("1.5.0"), exe("2.0.0")]) {
          yield* fs.makeDirectory(path.dirname(file), { recursive: true });
          yield* fs.writeFileString(file, "", { mode: 0o755 });
        }
        yield* fs.makeDirectory(path.dirname(installed), { recursive: true });
        yield* fs.writeFileString(installed, posixLauncherScript(exe("1.5.0")), { mode: 0o755 });
        yield* fs.makeDirectory(path.dirname(onPath), { recursive: true });
        yield* fs.symlink(exe("1.0.0"), onPath);

        yield* ServiceLauncherClient.repointLaunchers({ baseDir, targetVersion: "2.0.0" }).pipe(
          Effect.provideService(HostProcess.Environment, {
            HOME: root,
            ONICODE_INSTALL_BIN_DIR: path.dirname(installed),
            PATH: path.dirname(onPath),
          }),
        );

        // The script is rewritten and the legacy symlink becomes one.
        assert.equal(yield* fs.readFileString(installed), posixLauncherScript(exe("2.0.0")));
        assert.equal(yield* fs.readFileString(onPath), posixLauncherScript(exe("2.0.0")));
      }).pipe(Effect.provideService(HostProcess.Platform, "linux")),
  );

  it.effect.skipIf(!posixHost)(
    "still moves the PATH launcher when the install bin one cannot be rewritten",
    () =>
      Effect.gen(function* () {
        const fs = yield* FileSystem.FileSystem;
        const path = yield* Path.Path;
        const root = yield* fs.makeTempDirectoryScoped({ prefix: "oni-update-" });
        const baseDir = path.join(root, ".onicode");
        const exe = (version: string) => path.join(baseDir, "runtime/versions", version, "onicode");
        const installed = path.join(root, ".local/bin/onicode");
        const onPath = path.join(root, "bin/onicode");
        for (const file of [exe("1.0.0"), exe("2.0.0")]) {
          yield* fs.makeDirectory(path.dirname(file), { recursive: true });
          yield* fs.writeFileString(file, "", { mode: 0o755 });
        }
        for (const link of [installed, onPath]) {
          yield* fs.makeDirectory(path.dirname(link), { recursive: true });
          yield* fs.symlink(exe("1.0.0"), link);
        }
        const warnings: unknown[] = [];
        const logger = Logger.make(({ logLevel, message }) => {
          if (logLevel === "Warn") warnings.push(message);
        });
        const failingFs = FileSystem.FileSystem.of({
          ...fs,
          rename: (source, destination) =>
            destination === installed
              ? Effect.fail(
                  new PlatformError.PlatformError(
                    new PlatformError.SystemError({
                      _tag: "PermissionDenied",
                      module: "FileSystem",
                      method: "rename",
                      pathOrDescriptor: destination,
                    }),
                  ),
                )
              : fs.rename(source, destination),
        });

        yield* ServiceLauncherClient.repointLaunchers({ baseDir, targetVersion: "2.0.0" }).pipe(
          Effect.provideService(HostProcess.Environment, {
            HOME: root,
            PATH: path.dirname(onPath),
          }),
          Effect.provideService(FileSystem.FileSystem, failingFs),
          Effect.provide(Logger.layer([logger], { mergeWithExisting: false })),
        );

        assert.lengthOf(warnings, 1);
        assert.include(JSON.stringify(warnings), installed);
        assert.equal(yield* fs.readLink(installed), exe("1.0.0"));
        assert.equal(yield* fs.readFileString(onPath), posixLauncherScript(exe("2.0.0")));
      }).pipe(Effect.provideService(HostProcess.Platform, "linux")),
  );

  it.effect("leaves unowned install files and PATH symlinks untouched after self-update", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const root = yield* fs.makeTempDirectoryScoped({ prefix: "oni-update-" });
      const baseDir = path.join(root, ".onicode");
      const installed = path.join(root, ".local/bin/onicode");
      const onPath = path.join(root, "bin/onicode");
      const foreign = path.join(root, "other-home/runtime/versions/1.0.0/onicode");
      const target = path.join(baseDir, "runtime/versions/2.0.0/onicode");
      for (const file of [installed, foreign, target]) {
        yield* fs.makeDirectory(path.dirname(file), { recursive: true });
        yield* fs.writeFileString(file, "user-owned file", { mode: 0o755 });
      }
      yield* fs.makeDirectory(path.dirname(onPath), { recursive: true });
      yield* fs.symlink(foreign, onPath);

      yield* ServiceLauncherClient.repointLaunchers({ baseDir, targetVersion: "2.0.0" }).pipe(
        Effect.provideService(HostProcess.Environment, {
          HOME: root,
          PATH: path.dirname(onPath),
        }),
      );

      assert.equal(yield* fs.readFileString(installed), "user-owned file");
      assert.equal(yield* fs.readLink(onPath), foreign);
    }).pipe(Effect.provideService(HostProcess.Platform, "linux")),
  );

  describe.skipIf(!posixHost)("a POSIX PATH", () => {
    it.effect.each([
      { name: "leading", searchPath: (missing: string) => `:${missing}`, moved: true },
      { name: "trailing", searchPath: (missing: string) => `${missing}:`, moved: true },
      { name: "whole", searchPath: () => "", moved: false },
    ])("treats a $name empty PATH entry like the shell does", ({ searchPath, moved }) =>
      Effect.gen(function* () {
        const fs = yield* FileSystem.FileSystem;
        const path = yield* Path.Path;
        const root = yield* fs.makeTempDirectoryScoped({ prefix: "oni-update-" });
        const baseDir = path.join(root, ".onicode");
        const exe = (version: string) => path.join(baseDir, "runtime/versions", version, "onicode");
        const cwd = path.join(root, "work");
        const inCwd = path.join(cwd, "onicode");
        for (const file of [exe("1.0.0"), exe("2.0.0")]) {
          yield* fs.makeDirectory(path.dirname(file), { recursive: true });
          yield* fs.writeFileString(file, "", { mode: 0o755 });
        }
        yield* fs.makeDirectory(cwd);
        yield* fs.symlink(exe("1.0.0"), inCwd);

        yield* ServiceLauncherClient.repointLaunchers({ baseDir, targetVersion: "2.0.0" }).pipe(
          Effect.provideService(HostProcess.Environment, {
            HOME: root,
            PATH: searchPath(path.join(root, "missing")),
          }),
          Effect.provideService(HostProcess.WorkingDirectory, cwd),
        );

        // A nonempty PATH searches the working directory there; an empty PATH searches nothing.
        if (moved) {
          assert.equal(yield* fs.readFileString(inCwd), posixLauncherScript(exe("2.0.0")));
        } else {
          assert.equal(yield* fs.readLink(inCwd), exe("1.0.0"));
        }
      }).pipe(Effect.provideService(HostProcess.Platform, "linux")),
    );

    it.effect.each([
      {
        name: "past a directory in an empty entry",
        cwd: "blocked",
        searchPath: (root: string) => `:${root}/bin`,
        moved: "bin",
      },
      {
        name: "past a directory in an absolute entry",
        cwd: "work",
        searchPath: (root: string) => `${root}/blocked:${root}/bin`,
        moved: "bin",
      },
      {
        name: "past a file it cannot execute",
        cwd: "work",
        searchPath: (root: string) => `${root}/unrunnable:${root}/bin`,
        moved: "bin",
      },
      { name: "in the . entry", cwd: "work", searchPath: () => ".", moved: "work" },
      { name: "in a relative entry", cwd: ".", searchPath: () => "work", moved: "work" },
      {
        name: "through a symlinked directory",
        cwd: "work",
        searchPath: (root: string) => `${root}/linked`,
        moved: "real/bin",
      },
    ])("moves the onicode the shell finds $name", ({ cwd, searchPath, moved }) =>
      Effect.gen(function* () {
        const fs = yield* FileSystem.FileSystem;
        const path = yield* Path.Path;
        const root = yield* fs.makeTempDirectoryScoped({ prefix: "oni-update-" });
        const baseDir = path.join(root, ".onicode");
        const exe = (version: string) => path.join(baseDir, "runtime/versions", version, "onicode");
        for (const file of [exe("1.0.0"), exe("2.0.0")]) {
          yield* fs.makeDirectory(path.dirname(file), { recursive: true });
          yield* fs.writeFileString(file, "", { mode: 0o755 });
        }
        yield* fs.makeDirectory(path.join(root, "blocked/onicode"), { recursive: true });
        yield* fs.makeDirectory(path.join(root, "unrunnable"));
        yield* fs.writeFileString(path.join(root, "unrunnable/onicode"), "", { mode: 0o644 });
        const launcher = path.join(root, moved, "onicode");
        yield* fs.makeDirectory(path.dirname(launcher), { recursive: true });
        yield* fs.symlink(path.relative(path.dirname(launcher), exe("1.0.0")), launcher);
        yield* fs.symlink("real/bin", path.join(root, "linked"));

        yield* ServiceLauncherClient.repointLaunchers({ baseDir, targetVersion: "2.0.0" }).pipe(
          Effect.provideService(HostProcess.Environment, { HOME: root, PATH: searchPath(root) }),
          Effect.provideService(HostProcess.WorkingDirectory, path.join(root, cwd)),
        );

        assert.equal(yield* fs.readFileString(launcher), posixLauncherScript(exe("2.0.0")));
      }).pipe(Effect.provideService(HostProcess.Platform, "linux")),
    );
  });
});
