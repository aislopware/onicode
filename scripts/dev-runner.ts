#!/usr/bin/env node

import * as NodeOS from "node:os";

import * as NodeRuntime from "@effect/platform-node/NodeRuntime";
import * as NodeServices from "@effect/platform-node/NodeServices";
import * as NetService from "@onicode/shared/Net";
import { resolveGitWorktreePath, resolveWorktreeOniHome } from "@onicode/shared/devHome";
import * as HostProcess from "@onicode/shared/HostProcess";
import { resolveSpawnCommand } from "@onicode/shared/shell";
import * as Config from "effect/Config";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Hash from "effect/Hash";
import * as Layer from "effect/Layer";
import * as Logger from "effect/Logger";
import * as Option from "effect/Option";
import * as Path from "effect/Path";
import * as Schema from "effect/Schema";
import { Argument, Command, Flag } from "effect/cli";
import { ChildProcess } from "effect/process";

import { loadRepoEnv } from "./lib/public-config.ts";

Object.assign(process.env, loadRepoEnv());

const BASE_SERVER_PORT = 13773;
const BASE_WEB_PORT = 5733;
// Clear of common CDP ports such as 9222 and 9333 that a developer's own browser may hold.
const BASE_DEBUG_PORT = 19222;
const MAX_HASH_OFFSET = 3000;
const MAX_PORT = 65535;
const DESKTOP_DEV_LOOPBACK_HOST = "127.0.0.1";
// Dev binds loopback; other interfaces cannot establish port availability.
const DEV_PORT_PROBE_HOSTS = ["127.0.0.1", "::1"] as const;
// Migration prunes after copying, so large sources also need an automatic-seeding limit.
const LARGE_DEV_DATABASE_BYTES = 500 * 1024 * 1024;

export const DEFAULT_T3_HOME = Effect.map(Effect.service(Path.Path), (path) =>
  path.join(NodeOS.homedir(), ".onicode"),
);

const MODE_ARGS = {
  // The desktop task depends on the server build; Electron starts that server itself.
  dev: ["run", "--filter=@onicode/desktop", "--filter=@onicode/web", "dev"],
  // The server alone, for the mobile app and CLI work.
  "dev:server": ["run", "--filter=onicode", "dev"],
} as const satisfies Record<string, ReadonlyArray<string>>;

type DevMode = keyof typeof MODE_ARGS;
// Only the backend port honors --host/ONICODE_HOST; the others stay loopback.
type PortAvailabilityCheck<R = never> = (
  port: number,
  role?: "server" | "loopback",
) => Effect.Effect<boolean, never, R>;

const DEV_RUNNER_MODES = Object.keys(MODE_ARGS) as Array<DevMode>;

export class DevRunnerConfigurationError extends Schema.TaggedError<DevRunnerConfigurationError>()(
  "DevRunnerConfigurationError",
  {
    configKeys: Schema.Array(Schema.String),
    cause: Schema.Defect(),
  },
) {
  override get message(): string {
    return `Failed to read dev-runner configuration: ${this.configKeys.join(", ")}.`;
  }
}

export class DevRunnerInvalidPortOffsetError extends Schema.TaggedError<DevRunnerInvalidPortOffsetError>()(
  "DevRunnerInvalidPortOffsetError",
  {
    configKey: Schema.Literal("ONICODE_PORT_OFFSET"),
    portOffset: Schema.Number,
    minimum: Schema.Number,
  },
) {
  override get message(): string {
    return `${this.configKey} must be at least ${this.minimum}; received ${this.portOffset}.`;
  }
}

export class DevRunnerPortExhaustedError extends Schema.TaggedError<DevRunnerPortExhaustedError>()(
  "DevRunnerPortExhaustedError",
  {
    startOffset: Schema.Number,
    requireServerPort: Schema.Boolean,
    requireWebPort: Schema.Boolean,
    baseServerPort: Schema.Number,
    baseWebPort: Schema.Number,
    maximumPort: Schema.Number,
  },
) {
  override get message(): string {
    return `No required dev ports were available from offset ${this.startOffset} through maximum port ${this.maximumPort}.`;
  }
}

export class DevRunnerProcessError extends Schema.TaggedError<DevRunnerProcessError>()(
  "DevRunnerProcessError",
  {
    operation: Schema.Literals(["spawn", "wait-for-exit"]),
    mode: Schema.Literals(["dev", "dev:server"]),
    executable: Schema.Literal("vp"),
    argumentCount: Schema.Number,
    shell: Schema.Boolean,
    cause: Schema.Defect(),
  },
) {
  override get message(): string {
    return `Dev-runner process operation "${this.operation}" failed for mode "${this.mode}".`;
  }
}

export class DevRunnerProcessExitError extends Schema.TaggedError<DevRunnerProcessExitError>()(
  "DevRunnerProcessExitError",
  {
    mode: Schema.Literals(["dev", "dev:server"]),
    executable: Schema.Literal("vp"),
    argumentCount: Schema.Number,
    shell: Schema.Boolean,
    exitCode: Schema.Number,
  },
) {
  override get message(): string {
    return `Dev-runner process exited with code ${this.exitCode} in mode "${this.mode}".`;
  }
}

const optionalStringConfig = (name: string): Config.Config<string | undefined> =>
  Config.String(name).pipe(
    Config.option,
    Config.map((value) => Option.getOrUndefined(value)),
  );
const optionalBooleanConfig = (name: string): Config.Config<boolean | undefined> =>
  Config.Boolean(name).pipe(
    Config.option,
    Config.map((value) => Option.getOrUndefined(value)),
  );
const optionalPortConfig = (name: string): Config.Config<number | undefined> =>
  Config.Port(name).pipe(
    Config.option,
    Config.map((value) => Option.getOrUndefined(value)),
  );
const optionalIntegerConfig = (name: string): Config.Config<number | undefined> =>
  Config.Int(name).pipe(
    Config.option,
    Config.map((value) => Option.getOrUndefined(value)),
  );
const OffsetConfig = Config.all({
  portOffset: optionalIntegerConfig("ONICODE_PORT_OFFSET"),
  devInstance: optionalStringConfig("ONICODE_DEV_INSTANCE"),
});

export function resolveOffset(config: {
  readonly portOffset: number | undefined;
  readonly devInstance: string | undefined;
  readonly worktreePath?: string | undefined;
}): Effect.Effect<
  { readonly offset: number; readonly source: string },
  DevRunnerInvalidPortOffsetError
> {
  if (config.portOffset !== undefined) {
    if (config.portOffset < 0) {
      return Effect.fail(
        new DevRunnerInvalidPortOffsetError({
          configKey: "ONICODE_PORT_OFFSET",
          portOffset: config.portOffset,
          minimum: 0,
        }),
      );
    }
    return Effect.succeed({
      offset: config.portOffset,
      source: `ONICODE_PORT_OFFSET=${config.portOffset}`,
    });
  }

  const seed = config.devInstance?.trim();
  if (seed) {
    if (/^\d+$/.test(seed)) {
      return Effect.succeed({
        offset: Number(seed),
        source: `numeric ONICODE_DEV_INSTANCE=${seed}`,
      });
    }

    const offset = ((Hash.string(seed) >>> 0) % MAX_HASH_OFFSET) + 1;
    return Effect.succeed({ offset, source: `hashed ONICODE_DEV_INSTANCE=${seed}` });
  }

  const worktreePath = config.worktreePath?.trim();
  if (worktreePath) {
    const offset = ((Hash.string(worktreePath) >>> 0) % MAX_HASH_OFFSET) + 1;
    return Effect.succeed({ offset, source: `worktree ${worktreePath}` });
  }

  return Effect.succeed({ offset: 0, source: "default ports" });
}

const resolveBaseDir = Effect.fn("dev-runner.resolveBaseDir")(function* (
  baseDir: string | undefined,
): Effect.fn.Return<string, never, Path.Path> {
  const path = yield* Path.Path;
  const configured = baseDir?.trim();

  if (configured === "~") {
    return yield* HostProcess.HomeDirectory;
  }
  if (configured?.startsWith("~/") || configured?.startsWith("~\\")) {
    return path.join(yield* HostProcess.HomeDirectory, configured.slice(2));
  }
  if (configured) {
    return path.resolve(configured);
  }

  return yield* DEFAULT_T3_HOME;
});

interface CreateDevRunnerEnvInput {
  readonly mode: DevMode;
  readonly baseEnv: NodeJS.ProcessEnv;
  readonly serverOffset: number;
  readonly webOffset: number;
  readonly oniHome: string | undefined;
  readonly autoBootstrapProjectFromCwd: boolean | undefined;
  readonly logWebSocketEvents: boolean | undefined;
  readonly host: string | undefined;
  readonly port: number | undefined;
  readonly devUrl: URL | undefined;
}

export const createDevRunnerEnv = Effect.fn("dev-runner.createDevRunnerEnv")(function* ({
  mode,
  baseEnv,
  serverOffset,
  webOffset,
  oniHome,
  autoBootstrapProjectFromCwd,
  logWebSocketEvents,
  host,
  port,
  devUrl,
}: CreateDevRunnerEnvInput): Effect.fn.Return<NodeJS.ProcessEnv, never, Path.Path> {
  const { webPort, debugPort } = portsForOffset(webOffset);
  const serverPort = port ?? BASE_SERVER_PORT + serverOffset;
  const configuredBaseDir = oniHome?.trim() || undefined;
  const resolvedBaseDir = yield* resolveBaseDir(configuredBaseDir);

  const output: NodeJS.ProcessEnv = { ...baseEnv, ONICODE_PORT: String(serverPort) };

  if (configuredBaseDir !== undefined) {
    output.ONICODE_HOME = resolvedBaseDir;
  } else {
    delete output.ONICODE_HOME;
  }

  // Managed Onicode service shells leak launcher variables that make dev servers fail version checks.
  // Dev-runner servers are never launcher-managed (serviceLauncherClient.ts resolveStartup).
  delete output.ONICODE_SERVICE_LAUNCHER_CONTEXT;
  delete output.ONICODE_BOOT_SERVICE_UNIT;
  delete output.ONICODE_DESKTOP_WS_URL;

  if (mode === "dev") {
    output.PORT = String(webPort);
    output.VITE_DEV_SERVER_URL =
      devUrl?.toString() ?? `http://${DESKTOP_DEV_LOOPBACK_HOST}:${webPort}`;
    output.ONICODE_DESKTOP_REMOTE_DEBUGGING_PORT = String(debugPort);
    // Electron starts and owns the server, always on loopback, and passes it `--dev` itself.
    delete output.ONICODE_MODE;
    delete output.ONICODE_HOST;
    delete output.ONICODE_DEV;
  } else {
    output.ONICODE_MODE = "standalone";
    output.ONICODE_DEV = "1";
    delete output.VITE_DEV_SERVER_URL;
    if (host !== undefined) {
      output.ONICODE_HOST = host;
    }
  }

  if (autoBootstrapProjectFromCwd !== undefined) {
    output.ONICODE_AUTO_BOOTSTRAP_PROJECT_FROM_CWD = autoBootstrapProjectFromCwd ? "1" : "0";
  } else {
    delete output.ONICODE_AUTO_BOOTSTRAP_PROJECT_FROM_CWD;
  }

  if (logWebSocketEvents !== undefined) {
    output.ONICODE_LOG_WS_EVENTS = logWebSocketEvents ? "1" : "0";
  } else {
    delete output.ONICODE_LOG_WS_EVENTS;
  }

  return output;
});

function portsForOffset(offset: number): {
  readonly serverPort: number;
  readonly webPort: number;
  readonly debugPort: number;
} {
  return {
    serverPort: BASE_SERVER_PORT + offset,
    webPort: BASE_WEB_PORT + offset,
    debugPort: BASE_DEBUG_PORT + offset,
  };
}

export const checkPortAvailabilityOnHosts = Effect.fn("dev-runner.checkPortAvailabilityOnHosts")(
  function* <R>(
    port: number,
    hosts: ReadonlyArray<string>,
    canListenOnHost: (port: number, host: string) => Effect.Effect<boolean, never, R>,
  ): Effect.fn.Return<boolean, never, R> {
    for (const host of hosts) {
      if (!(yield* canListenOnHost(port, host))) {
        return false;
      }
    }

    return true;
  },
);

export function devPortProbeHosts(configuredHost: string | undefined): ReadonlyArray<string> {
  const host = configuredHost?.trim();
  if (!host || DEV_PORT_PROBE_HOSTS.includes(host as (typeof DEV_PORT_PROBE_HOSTS)[number])) {
    return DEV_PORT_PROBE_HOSTS;
  }
  return [...DEV_PORT_PROBE_HOSTS, host];
}

const makeDefaultCheckPortAvailability =
  (configuredHost: string | undefined): PortAvailabilityCheck<NetService.NetService> =>
  (port, role) =>
    Effect.gen(function* () {
      const net = yield* NetService.NetService;
      const hosts = role === "loopback" ? DEV_PORT_PROBE_HOSTS : devPortProbeHosts(configuredHost);
      return yield* checkPortAvailabilityOnHosts(port, hosts, (candidatePort, host) =>
        net.canListenOnHost(candidatePort, host),
      );
    });

const defaultCheckPortAvailability = makeDefaultCheckPortAvailability(undefined);

interface FindFirstAvailableOffsetInput<R = NetService.NetService> {
  readonly startOffset: number;
  readonly requireServerPort: boolean;
  readonly requireWebPort: boolean;
  readonly checkPortAvailability?: PortAvailabilityCheck<R>;
}

export const findFirstAvailableOffset = Effect.fn("dev-runner.findFirstAvailableOffset")(function* <
  R = NetService.NetService,
>({
  startOffset,
  requireServerPort,
  requireWebPort,
  checkPortAvailability,
}: FindFirstAvailableOffsetInput<R>): Effect.fn.Return<number, DevRunnerPortExhaustedError, R> {
  const checkPort = (checkPortAvailability ??
    defaultCheckPortAvailability) as PortAvailabilityCheck<R>;

  for (let candidate = startOffset; ; candidate += 1) {
    const { serverPort, webPort, debugPort } = portsForOffset(candidate);
    const serverPortOutOfRange = serverPort > MAX_PORT;
    const webPortOutOfRange = debugPort > MAX_PORT;

    if (
      (requireServerPort && serverPortOutOfRange) ||
      (requireWebPort && webPortOutOfRange) ||
      (!requireServerPort && !requireWebPort && (serverPortOutOfRange || webPortOutOfRange))
    ) {
      break;
    }

    const checks: Array<Effect.Effect<boolean, never, R>> = [];
    if (requireServerPort) {
      checks.push(checkPort(serverPort, "server"));
    }
    if (requireWebPort) {
      checks.push(checkPort(webPort, "loopback"), checkPort(debugPort, "loopback"));
    }

    if (checks.length === 0) {
      return candidate;
    }

    const availability = yield* Effect.all(checks);
    if (availability.every(Boolean)) {
      return candidate;
    }
  }

  return yield* new DevRunnerPortExhaustedError({
    startOffset,
    requireServerPort,
    requireWebPort,
    baseServerPort: BASE_SERVER_PORT,
    baseWebPort: BASE_WEB_PORT,
    maximumPort: MAX_PORT,
  });
});

interface ResolveModePortOffsetsInput<R = NetService.NetService> {
  readonly mode: DevMode;
  readonly startOffset: number;
  readonly hasExplicitServerPort: boolean;
  readonly hasExplicitDevUrl: boolean;
  readonly checkPortAvailability?: PortAvailabilityCheck<R>;
}

export const resolveModePortOffsets = Effect.fn("dev-runner.resolveModePortOffsets")(function* <
  R = NetService.NetService,
>({
  mode,
  startOffset,
  hasExplicitServerPort,
  hasExplicitDevUrl,
  checkPortAvailability,
}: ResolveModePortOffsetsInput<R>): Effect.fn.Return<
  { readonly serverOffset: number; readonly webOffset: number },
  DevRunnerPortExhaustedError,
  R
> {
  const checkPort = (checkPortAvailability ??
    defaultCheckPortAvailability) as PortAvailabilityCheck<R>;

  if (mode === "dev:server") {
    if (hasExplicitServerPort) {
      return { serverOffset: startOffset, webOffset: startOffset };
    }

    const serverOffset = yield* findFirstAvailableOffset({
      startOffset,
      requireServerPort: true,
      requireWebPort: false,
      checkPortAvailability: checkPort,
    });
    return { serverOffset, webOffset: serverOffset };
  }

  const sharedOffset = yield* findFirstAvailableOffset({
    startOffset,
    requireServerPort: !hasExplicitServerPort,
    requireWebPort: !hasExplicitDevUrl,
    checkPortAvailability: checkPort,
  });

  return { serverOffset: sharedOffset, webOffset: sharedOffset };
});

/** Best effort: a failed first-start step warns, and dev still starts. */
const runRepoScript = Effect.fn("dev-runner.runRepoScript")(function* (
  script: string,
  args: ReadonlyArray<string>,
  env: NodeJS.ProcessEnv,
) {
  const path = yield* Path.Path;
  const repoRoot = path.dirname(import.meta.dirname);
  const exitCode = yield* ChildProcess.make(
    yield* HostProcess.ExecutablePath,
    [path.join(repoRoot, script), ...args],
    { cwd: repoRoot, stdin: "ignore", stdout: "inherit", stderr: "inherit", env, extendEnv: false },
  ).pipe(
    Effect.flatMap((child) => child.exitCode),
    Effect.scoped,
    Effect.orElseSucceed(() => -1),
  );
  if (exitCode !== 0) {
    yield* Effect.logWarning(`[dev-runner] ${script} failed; starting anyway.`);
  }
});

/**
 * The first start in a worktree seeds its empty database from a small live database
 * and warms the web dependency cache. Each step checks for its own output, so
 * later starts skip it.
 */
const prepareFirstStart = Effect.fn("dev-runner.prepareFirstStart")(function* (input: {
  readonly mode: DevMode;
  readonly seed: boolean;
  readonly worktreeHome: string | undefined;
  readonly env: NodeJS.ProcessEnv;
}) {
  const fs = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  const exists = (file: string) => fs.exists(file).pipe(Effect.orElseSucceed(() => false));

  if (input.worktreeHome !== undefined) {
    const stateDir = path.join(input.worktreeHome, "userdata");
    for (const name of ["statev2.sqlite", "state.sqlite"]) {
      const file = path.join(stateDir, name);
      const info = yield* fs.stat(file).pipe(Effect.option);
      if (Option.isSome(info) && Number(info.value.size) > LARGE_DEV_DATABASE_BYTES) {
        yield* Effect.logWarning(
          `[dev-runner] ${file} is ${Math.round(Number(info.value.size) / 1024 / 1024)} MB. With the dev server stopped, \`vp run migrate-dev-db\` rebuilds statev2.sqlite as a small copy of recent data.`,
        );
      }
    }
    const source = path.join(
      yield* HostProcess.HomeDirectory,
      ".onicode",
      "userdata",
      "statev2.sqlite",
    );
    // A V1 state.sqlite counts as data too: the server imports it only while
    // statev2.sqlite is missing, so seeding would hide it.
    if (
      input.seed &&
      !(yield* exists(path.join(stateDir, "statev2.sqlite"))) &&
      !(yield* exists(path.join(stateDir, "state.sqlite")))
    ) {
      const info = yield* fs.stat(source).pipe(Effect.option);
      if (Option.isSome(info)) {
        if (Number(info.value.size) > LARGE_DEV_DATABASE_BYTES) {
          yield* Effect.logWarning(
            `[dev-runner] ${source} exceeds 500 MiB; skipping automatic database seeding and starting with an empty database. Run \`vp run migrate-dev-db\` manually with dev stopped if needed; it may use substantial disk space before pruning.`,
          );
        } else {
          yield* Effect.logInfo(
            `[dev-runner] seeding ${input.worktreeHome} from ${source} (read-only). --no-seed or ONICODE_DEV_SEED=0 skips this.`,
          );
          yield* runRepoScript(
            "apps/server/scripts/migrate-dev-db.ts",
            ["--base-dir", input.worktreeHome, "--source", source],
            input.env,
          );
        }
      }
    }
  }

  const repoRoot = path.dirname(import.meta.dirname);
  if (
    input.mode !== "dev:server" &&
    !(yield* exists(path.join(repoRoot, "apps/web/node_modules/.vite/deps/_metadata.json")))
  ) {
    yield* runRepoScript("apps/web/scripts/warm-dep-cache.ts", [], input.env);
  }
});

interface DevRunnerCliInput {
  readonly mode: DevMode;
  readonly oniHome: string | undefined;
  readonly autoBootstrapProjectFromCwd: boolean | undefined;
  readonly logWebSocketEvents: boolean | undefined;
  readonly host: string | undefined;
  readonly port: number | undefined;
  readonly devUrl: URL | undefined;
  readonly dryRun: boolean;
  readonly seed?: boolean | undefined;
  readonly runArgs: ReadonlyArray<string>;
}

export const runDevRunnerWithInput = Effect.fn("dev-runner.runDevRunnerWithInput")(function* (
  input: DevRunnerCliInput,
) {
  const { portOffset, devInstance } = yield* OffsetConfig.pipe(
    Effect.mapError(
      (cause) =>
        new DevRunnerConfigurationError({
          configKeys: ["ONICODE_PORT_OFFSET", "ONICODE_DEV_INSTANCE"],
          cause,
        }),
    ),
  );

  const worktreePath = yield* resolveGitWorktreePath(yield* HostProcess.WorkingDirectory);

  const { offset, source } = yield* resolveOffset({
    portOffset,
    devInstance,
    worktreePath,
  });

  const { serverOffset, webOffset } = yield* resolveModePortOffsets({
    mode: input.mode,
    startOffset: offset,
    hasExplicitServerPort: input.port !== undefined,
    hasExplicitDevUrl: input.devUrl !== undefined,
    checkPortAvailability: makeDefaultCheckPortAvailability(input.host),
  });

  const hostEnvironment = yield* HostProcess.Environment;
  // A dev server started inside a worktree defaults to that worktree's own
  // (gitignored) `.onicode` — see @onicode/shared/devHome for why this must
  // outrank an ambient ONICODE_HOME. `--home-dir` still wins.
  const worktreeHome = yield* resolveWorktreeOniHome(yield* HostProcess.WorkingDirectory);
  // Trim before choosing: `--home-dir ""` is not a selection, and treating it
  // as one would skip the worktree default and land on the shared home —
  // exactly the outcome this precedence exists to prevent.
  const resolvedOniHome =
    (input.oniHome?.trim() || undefined) ??
    worktreeHome ??
    (hostEnvironment.ONICODE_HOME?.trim() || undefined);
  const env = yield* createDevRunnerEnv({
    mode: input.mode,
    baseEnv: hostEnvironment,
    serverOffset,
    webOffset,
    oniHome: resolvedOniHome,
    autoBootstrapProjectFromCwd: input.autoBootstrapProjectFromCwd,
    logWebSocketEvents: input.logWebSocketEvents,
    host: input.host,
    port: input.port,
    devUrl: input.devUrl,
  });

  const selectionSuffix =
    serverOffset !== offset || webOffset !== offset
      ? ` selectedOffset(server=${serverOffset},web=${webOffset})`
      : "";
  const baseDir = env.ONICODE_HOME ?? (yield* DEFAULT_T3_HOME);

  yield* Effect.logInfo(
    `[dev-runner] mode=${input.mode} source=${source}${selectionSuffix} serverPort=${String(env.ONICODE_PORT)}${
      input.mode === "dev"
        ? ` webPort=${String(env.PORT)} cdpPort=${String(env.ONICODE_DESKTOP_REMOTE_DEBUGGING_PORT)}`
        : ""
    } baseDir=${baseDir}`,
  );

  if (input.dryRun) {
    return;
  }

  yield* prepareFirstStart({
    mode: input.mode,
    seed: input.seed !== false,
    worktreeHome: baseDir === worktreeHome ? worktreeHome : undefined,
    env,
  });

  const args = [...MODE_ARGS[input.mode], ...input.runArgs];
  const spawnCommand = yield* resolveSpawnCommand("vp", args, { env });
  const processContext = {
    mode: input.mode,
    executable: "vp" as const,
    argumentCount: args.length,
    shell: spawnCommand.shell,
  } as const;
  const child = yield* ChildProcess.make(spawnCommand.command, spawnCommand.args, {
    stdin: "inherit",
    stdout: "inherit",
    stderr: "inherit",
    env,
    extendEnv: false,
    shell: spawnCommand.shell,
    // Keep Vite+ in the same process group so terminal signals (Ctrl+C)
    // reach it directly. Effect defaults to detached: true on non-Windows,
    // which would put the runner in a new group and require manual forwarding.
    detached: false,
    forceKillAfter: "1500 millis",
  }).pipe(
    Effect.mapError(
      (cause) =>
        new DevRunnerProcessError({
          ...processContext,
          operation: "spawn",
          cause,
        }),
    ),
  );

  const exitCode = yield* child.exitCode.pipe(
    Effect.mapError(
      (cause) =>
        new DevRunnerProcessError({
          ...processContext,
          operation: "wait-for-exit",
          cause,
        }),
    ),
  );
  if (exitCode !== 0) {
    return yield* new DevRunnerProcessExitError({
      ...processContext,
      exitCode,
    });
  }
});

const devRunnerCli = Command.make("dev-runner", {
  mode: Argument.Literals("mode", DEV_RUNNER_MODES).pipe(
    Argument.withDescription("Development mode to run."),
  ),
  oniHome: Flag.String("home-dir").pipe(
    Flag.withDescription(
      "Explicit Onicode data directory; runtime state is stored under userdata (equivalent to ONICODE_HOME). Inside a git worktree this defaults to that worktree's own .onicode so dev state stays off the shared home.",
    ),
    Flag.optional,
    Flag.map(Option.getOrUndefined),
  ),
  autoBootstrapProjectFromCwd: Flag.Boolean("auto-bootstrap-project-from-cwd").pipe(
    Flag.withDescription(
      "Auto-bootstrap toggle (equivalent to ONICODE_AUTO_BOOTSTRAP_PROJECT_FROM_CWD).",
    ),
    Flag.withFallbackConfig(optionalBooleanConfig("ONICODE_AUTO_BOOTSTRAP_PROJECT_FROM_CWD")),
  ),
  logWebSocketEvents: Flag.Boolean("log-websocket-events").pipe(
    Flag.withDescription("WebSocket event logging toggle (equivalent to ONICODE_LOG_WS_EVENTS)."),
    Flag.withAlias("log-ws-events"),
    Flag.withFallbackConfig(optionalBooleanConfig("ONICODE_LOG_WS_EVENTS")),
  ),
  host: Flag.String("host").pipe(
    Flag.withDescription(
      "Server host/interface override for `dev:server` (forwards to ONICODE_HOST). The desktop app always binds its server to loopback.",
    ),
    Flag.withFallbackConfig(optionalStringConfig("ONICODE_HOST")),
  ),
  port: Flag.Int("port").pipe(
    Flag.withSchema(Schema.Int.check(Schema.isBetween({ minimum: 1, maximum: 65535 }))),
    Flag.withDescription("Server port override (forwards to ONICODE_PORT)."),
    Flag.withFallbackConfig(optionalPortConfig("ONICODE_PORT")),
  ),
  devUrl: Flag.String("dev-url").pipe(
    Flag.withSchema(Schema.URLFromString),
    Flag.withDescription(
      "Renderer dev server URL override for `dev` (forwards to VITE_DEV_SERVER_URL). Ambient VITE_DEV_SERVER_URL values are ignored so a parent dev app cannot redirect the child runner.",
    ),
    Flag.optional,
    Flag.map(Option.getOrUndefined),
  ),
  dryRun: Flag.Boolean("dry-run").pipe(
    Flag.withDescription("Resolve mode/ports/env and print, but do not spawn Vite+."),
    Flag.withDefault(false),
  ),
  seed: Flag.Boolean("seed").pipe(
    Flag.withDescription(
      "Seed an empty worktree database from ~/.onicode on first start (read-only, via migrate-dev-db) if the source is at most 500 MiB. On by default; --no-seed or ONICODE_DEV_SEED=0 skips it.",
    ),
    Flag.withFallbackConfig(optionalBooleanConfig("ONICODE_DEV_SEED")),
  ),
  runArgs: Argument.String("run-arg").pipe(
    Argument.withDescription("Additional Vite+ run args (pass after `--`)."),
    Argument.variadic(),
  ),
}).pipe(
  Command.withDescription("Run monorepo development modes with deterministic port/env wiring."),
  Command.withHandler((input) => runDevRunnerWithInput(input)),
);

const cliRuntimeLayer = Layer.mergeAll(
  Logger.layer([Logger.consolePretty()]),
  NodeServices.layer,
  NetService.layer,
);

if (import.meta.main) {
  Command.run(devRunnerCli, { version: "0.0.0" }).pipe(
    Effect.scoped,
    Effect.provide(cliRuntimeLayer),
    NodeRuntime.runMain,
  );
}
