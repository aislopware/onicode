import type {
  AgentComputerPermissionMode,
  ComputerAccessStatus,
  ServerSettings as ServerSettingsSnapshot,
} from "@onicode/contracts";
import type { McpStdioServer } from "@onicode/provider-core/server/mcpSession";
import * as HostProcess from "@onicode/shared/HostProcess";
import * as Context from "effect/Context";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as FileSystem from "effect/FileSystem";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Path from "effect/Path";
import * as Schema from "effect/Schema";
import * as Scope from "effect/Scope";
import * as Semaphore from "effect/Semaphore";
import * as Stream from "effect/Stream";
import { ChildProcess, ChildProcessSpawner } from "effect/process";

import * as ServerSettings from "../serverSettings.ts";
import * as ComputerDriver from "./ComputerDriver.ts";

export class ComputerDriverStartError extends Schema.TaggedError<ComputerDriverStartError>()(
  "ComputerDriverStartError",
  { detail: Schema.String, cause: Schema.optional(Schema.Defect()) },
) {
  override get message(): string {
    return `cua-driver did not start: ${this.detail}`;
  }
}

export class ComputerService extends Context.Service<
  ComputerService,
  {
    /** Installs and starts the driver when needed. None when it cannot run on this machine. */
    readonly mcpServer: Effect.Effect<Option.Option<McpStdioServer>>;
    readonly status: Effect.Effect<ComputerAccessStatus>;
  }
>()("onicode/computer/ComputerService") {}

const DRIVER_ENVIRONMENT = {
  CUA_DRIVER_RS_TELEMETRY_ENABLED: "false",
  CUA_DRIVER_RS_UPDATE_CHECK: "false",
};
const START_TIMEOUT = "20 seconds";
const SESSION_WAIT = "45 seconds";

const DriverPermissions = Schema.Struct({
  accessibility: Schema.Boolean,
  screen_recording: Schema.Boolean,
});
const decodeDriverPermissions = Schema.decodeUnknownEffect(
  Schema.fromJsonString(DriverPermissions),
);

type Permissions = { readonly accessibility: boolean; readonly screenRecording: boolean } | null;

type DaemonState =
  | { readonly _tag: "stopped" }
  | {
      readonly _tag: "running";
      readonly scope: Scope.Closeable;
      readonly executable: string;
      readonly endpoint: string;
      readonly permissionMode: AgentComputerPermissionMode;
      readonly permissions: Permissions;
    }
  | { readonly _tag: "failed"; readonly message: string };

/** True when the environment or any project turns computer access on. */
const computerAccessEnabledAnywhere = (settings: ServerSettingsSnapshot) =>
  settings.enableAgentComputerAccess ||
  Object.values(settings.projectSettingsOverrides).some(
    (overrides) => overrides.enableAgentComputerAccess === true,
  );

export const make = Effect.gen(function* () {
  const driver = yield* ComputerDriver.ComputerDriver;
  const settings = yield* ServerSettings.ServerSettingsService;
  const spawner = yield* ChildProcessSpawner.ChildProcessSpawner;
  const fs = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  const platform = yield* HostProcess.Platform;
  const serviceScope = yield* Effect.scope;
  // A short path: macOS caps a unix socket path near 104 bytes.
  const runtimeDirectory = yield* fs.makeTempDirectoryScoped({ prefix: "oni-cua-" });
  // One endpoint per daemon: an MCP proxy never reconnects, so a session must see its
  // proxy's daemon is gone and launch a new proxy.
  let generation = 0;
  const nextEndpoint = () => {
    generation += 1;
    return platform === "win32"
      ? `\\\\.\\pipe\\${path.basename(runtimeDirectory)}-${generation}`
      : path.join(runtimeDirectory, `d${generation}.sock`);
  };
  // Without its own pid file the daemon overwrites the one a standalone cua-driver uses.
  const pidFile = path.join(runtimeDirectory, "d.pid");
  const gate = yield* Semaphore.make(1);
  let daemon: DaemonState = { _tag: "stopped" };

  const readPermissions = Effect.fn("ComputerService.readPermissions")(
    function* (executable: string, endpoint: string) {
      const child = yield* spawner.spawn(
        ChildProcess.make(executable, ["call", "check_permissions", "--socket", endpoint], {
          shell: false,
          stdin: "ignore",
          stdout: "pipe",
          stderr: "ignore",
          env: DRIVER_ENVIRONMENT,
          extendEnv: true,
        }),
      );
      const [output] = yield* Effect.all(
        [child.stdout.pipe(Stream.decodeText(), Stream.mkString), child.exitCode],
        { concurrency: "unbounded" },
      );
      const decoded = yield* decodeDriverPermissions(output);
      return { accessibility: decoded.accessibility, screenRecording: decoded.screen_recording };
    },
    Effect.scoped,
    Effect.timeout("10 seconds"),
    Effect.mapError(
      (cause) =>
        new ComputerDriverStartError({ detail: "It did not report its permissions.", cause }),
    ),
  );

  const start = Effect.fn("ComputerService.start")(function* (
    executable: string,
    permissionMode: AgentComputerPermissionMode,
  ) {
    const scope = yield* Scope.make();
    const endpoint = nextEndpoint();
    return yield* Effect.gen(function* () {
      const child = yield* spawner
        .spawn(
          ChildProcess.make(
            executable,
            [
              "serve",
              "--embedded",
              "--no-permissions-gate",
              "--socket",
              endpoint,
              "--pid-file",
              pidFile,
              "--permission-mode",
              permissionMode,
              ...(permissionMode === "unrestricted" ? ["--dangerously-bypass-approvals"] : []),
            ],
            {
              shell: false,
              detached: false,
              // The daemon exits when this pipe closes, so it cannot outlive a crashed server.
              stdin: "pipe",
              stdout: "ignore",
              stderr: "pipe",
              env: {
                ...DRIVER_ENVIRONMENT,
                CUA_DRIVER_EMBEDDED: "1",
                CUA_DRIVER_PARENT_LIVENESS_STDIN: "1",
              },
              extendEnv: true,
              forceKillAfter: "3 seconds",
            },
          ),
        )
        .pipe(Effect.provideService(Scope.Scope, scope));
      const listening = yield* Deferred.make<void>();
      const lastOutput: Array<string> = [];
      yield* child.stderr.pipe(
        Stream.decodeText(),
        Stream.splitLines,
        Stream.runForEach((line) =>
          Effect.suspend(() => {
            lastOutput.push(line);
            if (lastOutput.length > 5) lastOutput.shift();
            return line.includes("daemon listening on")
              ? Deferred.succeed(listening, undefined)
              : Effect.logDebug("cua-driver output", { output: line });
          }),
        ),
        Effect.ignoreCause,
        Effect.forkIn(scope),
      );
      const exited = Effect.result(child.exitCode);
      yield* Effect.raceFirst(
        Deferred.await(listening),
        exited.pipe(
          Effect.andThen(
            Effect.failSync(
              () =>
                new ComputerDriverStartError({
                  detail: lastOutput.join(" ").slice(-500) || "It exited at once.",
                }),
            ),
          ),
        ),
      ).pipe(
        Effect.timeoutOrElse({
          duration: START_TIMEOUT,
          orElse: () =>
            Effect.fail(new ComputerDriverStartError({ detail: "It never started listening." })),
        }),
      );
      const permissions =
        platform === "darwin" ? yield* readPermissions(executable, endpoint) : null;
      yield* exited.pipe(
        Effect.andThen(
          gate.withPermit(
            Effect.sync(() => {
              if (daemon._tag === "running" && daemon.scope === scope) {
                daemon = { _tag: "failed", message: "cua-driver stopped unexpectedly." };
              }
            }),
          ),
        ),
        Effect.forkIn(scope),
      );
      yield* Effect.logInfo("cua-driver started", { pid: Number(child.pid), permissionMode });
      return {
        _tag: "running",
        scope,
        executable,
        endpoint,
        permissionMode,
        permissions,
      } as const;
    }).pipe(Effect.onError(() => Scope.close(scope, Exit.void)));
  });

  const stopLocked = Effect.suspend(() => {
    const current = daemon;
    daemon = { _tag: "stopped" };
    return current._tag === "running" ? Scope.close(current.scope, Exit.void) : Effect.void;
  });

  const ensureRunning = Effect.fn("ComputerService.ensureRunning")(function* () {
    const executable = yield* driver.executable;
    return yield* gate.withPermit(
      Effect.gen(function* () {
        const permissionMode = (yield* settings.getSettings).agentComputerPermissionMode;
        if (
          daemon._tag === "running" &&
          daemon.executable === executable &&
          daemon.permissionMode === permissionMode &&
          // macOS answers a process from the grants it had at launch; a new daemon sees new grants.
          (daemon.permissions === null ||
            (daemon.permissions.accessibility && daemon.permissions.screenRecording))
        ) {
          return daemon;
        }
        yield* stopLocked;
        const started = yield* start(executable, permissionMode).pipe(
          Effect.tapError((error) =>
            Effect.sync(() => {
              daemon = { _tag: "failed", message: error.message };
            }),
          ),
        );
        daemon = started;
        return started;
      }),
    );
  });

  const reconcile = Effect.fn("ComputerService.reconcile")(
    function* (next: ServerSettingsSnapshot) {
      if (!computerAccessEnabledAnywhere(next)) return yield* gate.withPermit(stopLocked);
      if (daemon._tag === "running" && daemon.permissionMode !== next.agentComputerPermissionMode) {
        return yield* ensureRunning().pipe(Effect.asVoid);
      }
      // Download ahead of the first session that needs the driver.
      yield* driver.executable;
    },
    Effect.catch((cause) => Effect.logWarning("Could not prepare cua-driver.", { cause })),
  );

  yield* Stream.concat(
    Stream.fromEffect(settings.getSettings).pipe(Stream.catch(() => Stream.empty)),
    settings.streamChanges,
  ).pipe(
    Stream.map(
      (next) =>
        [computerAccessEnabledAnywhere(next), next.agentComputerPermissionMode, next] as const,
    ),
    Stream.changesWith((left, right) => left[0] === right[0] && left[1] === right[1]),
    Stream.runForEach(([, , next]) => reconcile(next)),
    Effect.forkScoped,
  );
  yield* Effect.addFinalizer(() => gate.withPermit(stopLocked));

  const mcpServer = ensureRunning().pipe(
    Effect.map((running) =>
      Option.some<McpStdioServer>({
        command: running.executable,
        args: ["mcp", "--embedded", "--socket", running.endpoint],
        env: DRIVER_ENVIRONMENT,
      }),
    ),
    Effect.timeoutOrElse({
      duration: SESSION_WAIT,
      orElse: () =>
        Effect.fail(new ComputerDriverStartError({ detail: "It is still being installed." })),
    }),
    Effect.catch((cause) =>
      Effect.logWarning("Computer tools are unavailable for this session.", { cause }).pipe(
        Effect.as(Option.none<McpStdioServer>()),
      ),
    ),
  );

  const status = Effect.gen(function* () {
    const enabled = yield* settings.getSettings.pipe(
      Effect.map(computerAccessEnabledAnywhere),
      Effect.orElseSucceed(() => false),
    );
    let install = yield* driver.state;
    if (enabled && install.state === "missing") {
      // A client asks right after turning this on, before `reconcile` sees the change.
      yield* driver.executable.pipe(
        Effect.ignore,
        Effect.forkIn(serviceScope, { startImmediately: true }),
      );
      install = yield* driver.state;
    }
    // A failed start is kept in `daemon`, which is what this reports.
    if (enabled && install.state === "installed") yield* Effect.ignore(ensureRunning());
    const current = daemon;
    return {
      driverVersion: driver.version,
      install,
      daemon:
        current._tag === "running"
          ? {
              state: "running",
              permissionMode: current.permissionMode,
              permissions: current.permissions,
            }
          : current._tag === "failed"
            ? { state: "failed", message: current.message }
            : { state: "stopped" },
    } satisfies ComputerAccessStatus;
  }).pipe(Effect.withSpan("ComputerService.status"));

  return ComputerService.of({ mcpServer, status });
});

export const layer = Layer.effect(ComputerService, make);
