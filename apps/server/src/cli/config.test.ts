// @effect-diagnostics nodeBuiltinImport:off
import * as NodeChildProcess from "node:child_process";
import * as NodeEvents from "node:events";
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";

import { assert, expect, it } from "@effect/vitest";
import * as ConfigProvider from "effect/ConfigProvider";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Path from "effect/Path";
import * as Result from "effect/Result";
import * as Schema from "effect/Schema";

import {
  DesktopBackendBootstrap,
  type DesktopBackendBootstrap as DesktopBackendBootstrapValue,
} from "@onicode/contracts";
import * as NetService from "@onicode/shared/Net";
import { DEFAULT_SIGNAL_EXPORT } from "@onicode/shared/observability";
import * as OtelEnvironment from "@onicode/shared/otelEnvironment";
import * as NodeServices from "@effect/platform-node/NodeServices";
import * as HostProcess from "@onicode/shared/HostProcess";
import packageJson from "../../package.json" with { type: "json" };
import { ServiceLauncherHostProcess } from "../service/serviceLauncherClient.ts";
import {
  SERVICE_LAUNCHER_CONTEXT_ENV,
  SERVICE_LAUNCHER_PROTOCOL,
} from "../service/serviceProtocol.ts";
import { deriveServerPaths } from "../config.ts";
import { resolveServerConfig } from "./config.ts";

const deriveExplicitServerPaths = (baseDir: string, dev: boolean) =>
  deriveServerPaths(baseDir, dev, { baseDirIsExplicit: true });

const minimalDesktopFlags = (baseDir: string) => ({
  mode: Option.some("desktop" as const),
  port: Option.some(4888),
  host: Option.none<string>(),
  baseDir: Option.some(baseDir),
  cwd: Option.none<string>(),
  dev: Option.none<boolean>(),
  bootstrapFd: Option.none<number>(),
  autoBootstrapProjectFromCwd: Option.none<boolean>(),
  logWebSocketEvents: Option.none<boolean>(),
  tailscaleServeEnabled: Option.none<boolean>(),
  tailscaleServePort: Option.none<number>(),
  lan: Option.none<boolean>(),
});

const encodeDesktopBootstrap = Schema.encodeEffect(Schema.fromJsonString(DesktopBackendBootstrap));
const encodeUnknownJson = Schema.encodeEffect(Schema.fromJsonString(Schema.Unknown));

const makeDesktopBootstrap = (
  overrides: Partial<DesktopBackendBootstrapValue> = {},
): DesktopBackendBootstrapValue => ({
  mode: "desktop",
  port: 4888,
  oniHome: "/tmp/oni-bootstrap-home",
  host: "127.0.0.1",
  desktopBootstrapToken: "desktop-bootstrap-token",
  tailscaleServeEnabled: false,
  tailscaleServePort: 443,
  ...overrides,
});

const liveProcess = Effect.acquireRelease(
  Effect.promise(async () => {
    const child = NodeChildProcess.spawn(process.execPath, ["-e", "process.stdin.resume()"], {
      stdio: ["pipe", "ignore", "ignore"],
    });
    await NodeEvents.EventEmitter.once(child, "spawn");
    return child;
  }),
  (child) =>
    Effect.promise(async () => {
      const exited = NodeEvents.EventEmitter.once(child, "exit");
      child.kill();
      await exited;
    }),
);

it.layer(NodeServices.layer)("cli config resolution", (it) => {
  const defaultObservabilityConfig = {
    traceMinLevel: "Info",
    traceTimingEnabled: true,
    traceBatchWindowMs: 1_000,
    traceMaxBytes: 10 * 1024 * 1024,
    traceMaxFiles: 10,
    otlpTracesUrl: undefined,
    otlpMetricsUrl: undefined,
    otlpLogsUrl: undefined,
    otlpTracesExport: DEFAULT_SIGNAL_EXPORT,
    otlpMetricsExport: DEFAULT_SIGNAL_EXPORT,
    otlpLogsExport: DEFAULT_SIGNAL_EXPORT,
    otelEnvironment: OtelEnvironment.none,
  } as const;

  const openBootstrapFd = Effect.fn(function* (payload: DesktopBackendBootstrapValue) {
    const fs = yield* FileSystem.FileSystem;
    const filePath = yield* fs.makeTempFileScoped({ prefix: "oni-bootstrap-", suffix: ".ndjson" });
    const encoded = yield* encodeDesktopBootstrap(payload);
    yield* fs.writeFileString(filePath, `${encoded}\n`);
    return yield* Effect.acquireRelease(
      Effect.sync(() => NodeFS.openSync(filePath, "r")),
      // Windows cannot reopen the fd through /proc or /dev/fd, so the reader closes it.
      (fd) =>
        Effect.sync(() => {
          try {
            NodeFS.closeSync(fd);
          } catch (error) {
            if ((error as NodeJS.ErrnoException).code !== "EBADF") throw error;
          }
        }),
    );
  });

  it.effect("keeps stale records and supervised startup out of the manual launch preflight", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const root = yield* fs.makeTempDirectoryScoped({ prefix: "oni-cli-preflight-" });
      const child = yield* liveProcess;
      assert(child.pid !== undefined);
      for (const [name, pid, mode, rejectRunningServer] of [
        ["stale", 2_147_483_647, "standalone", true],
        ["own-pid", process.pid, "standalone", true],
        ["desktop", child.pid, "desktop", true],
        ["serve", child.pid, "standalone", false],
      ] as const) {
        const baseDir = path.join(root, name);
        const stateDir = path.join(baseDir, "userdata");
        yield* fs.makeDirectory(stateDir, { recursive: true });
        yield* fs.writeFileString(
          path.join(stateDir, "server-runtime.json"),
          yield* encodeUnknownJson({
            version: 1,
            pid,
            port: 3773,
            origin: "http://127.0.0.1:3773",
            startedAt: "2026-10-01T00:00:00.000Z",
          }),
        );
        const cwd = path.join(root, `${name}-project`);
        const configResult = yield* resolveServerConfig(
          {
            ...minimalWebFlags(baseDir),
            mode: Option.some(mode),
            port: Option.some(8788),
            cwd: Option.some(cwd),
          },
          Option.none(),
          { rejectRunningServer },
        ).pipe(
          Effect.provide(
            Layer.merge(
              NetService.layer,
              ConfigProvider.layer(ConfigProvider.fromEnv({ env: {} })),
            ),
          ),
          Effect.result,
        );
        assert(Result.isSuccess(configResult));
        expect(configResult.success.cwd).toBe(cwd);
        expect(yield* fs.exists(cwd)).toBe(true);
      }
    }),
  );

  it.effect(
    "refuses a manual web launch when a runtime record belongs to another live process",
    () =>
      Effect.gen(function* () {
        const fs = yield* FileSystem.FileSystem;
        const path = yield* Path.Path;
        const baseDir = yield* fs.makeTempDirectoryScoped({ prefix: "oni-cli-preflight-live-" });
        const child = yield* liveProcess;
        assert(child.pid !== undefined);
        const stateDir = path.join(baseDir, "userdata");
        yield* fs.makeDirectory(stateDir, { recursive: true });
        yield* fs.writeFileString(
          path.join(stateDir, "server-runtime.json"),
          yield* encodeUnknownJson({
            version: 1,
            pid: child.pid,
            port: 3773,
            origin: "http://127.0.0.1:3773",
            startedAt: "2026-10-01T00:00:00.000Z",
          }),
        );
        const cwd = path.join(baseDir, "project");
        const error = yield* resolveServerConfig(
          { ...minimalWebFlags(baseDir), cwd: Option.some(cwd) },
          Option.none(),
          { rejectRunningServer: true },
        ).pipe(
          Effect.provide(
            Layer.merge(
              NetService.layer,
              ConfigProvider.layer(ConfigProvider.fromEnv({ env: {} })),
            ),
          ),
          Effect.flip,
        );
        expect(String(error)).toContain("A Onicode server is already running");
        expect(String(error)).toContain(`pid ${child.pid}`);
        expect(yield* fs.exists(cwd)).toBe(false);
      }),
  );

  it.effect("preserves explicit false CLI boolean flags over env and bootstrap values", () =>
    Effect.gen(function* () {
      const { join } = yield* Path.Path;
      const baseDir = join(NodeOS.tmpdir(), "oni-cli-config-false-flags");
      const fd = yield* openBootstrapFd(
        makeDesktopBootstrap({
          tailscaleServeEnabled: false,
          tailscaleServePort: 443,
        }),
      );
      const derivedPaths = yield* deriveExplicitServerPaths(baseDir, true);

      const resolved = yield* resolveServerConfig(
        {
          mode: Option.some("standalone"),
          port: Option.some(8788),
          host: Option.some("127.0.0.1"),
          baseDir: Option.some(baseDir),
          cwd: Option.none(),
          dev: Option.some(true),
          bootstrapFd: Option.none(),
          autoBootstrapProjectFromCwd: Option.some(false),
          logWebSocketEvents: Option.some(false),
          tailscaleServeEnabled: Option.none(),
          tailscaleServePort: Option.none(),
          lan: Option.none(),
        },
        Option.none(),
      ).pipe(
        Effect.provide(
          Layer.mergeAll(
            ConfigProvider.layer(
              ConfigProvider.fromEnv({
                env: {
                  ONICODE_BOOTSTRAP_FD: String(fd),
                  ONICODE_AUTO_BOOTSTRAP_PROJECT_FROM_CWD: "true",
                  ONICODE_LOG_WS_EVENTS: "true",
                },
              }),
            ),
            NetService.layer,
          ),
        ),
      );

      expect(resolved).toEqual({
        logLevel: "Info",
        ...defaultObservabilityConfig,
        mode: "standalone",
        port: 8788,
        cwd: process.cwd(),
        baseDir,
        ...derivedPaths,
        host: "127.0.0.1",
        dev: true,
        desktopBootstrapToken: "desktop-bootstrap-token",
        autoBootstrapProjectFromCwd: false,
        logWebSocketEvents: false,
        tailscaleServeEnabled: false,
        tailscaleServePort: 443,
      });
    }),
  );

  it.effect("applies flag then env precedence over bootstrap envelope values", () =>
    Effect.gen(function* () {
      const { join } = yield* Path.Path;
      const baseDir = join(NodeOS.tmpdir(), "oni-cli-config-env-wins");
      const fd = yield* openBootstrapFd(
        makeDesktopBootstrap({
          port: 4888,
          host: "127.0.0.2",
          oniHome: "/tmp/oni-bootstrap-home",
          desktopBootstrapToken: "desktop-token",
          tailscaleServeEnabled: false,
          tailscaleServePort: 443,
        }),
      );
      const derivedPaths = yield* deriveExplicitServerPaths(baseDir, true);

      const resolved = yield* resolveServerConfig(
        {
          mode: Option.none(),
          port: Option.some(8788),
          host: Option.some("127.0.0.1"),
          baseDir: Option.none(),
          cwd: Option.none(),
          dev: Option.some(true),
          bootstrapFd: Option.none(),
          autoBootstrapProjectFromCwd: Option.none(),
          logWebSocketEvents: Option.none(),
          tailscaleServeEnabled: Option.none(),
          tailscaleServePort: Option.none(),
          lan: Option.none(),
        },
        Option.some("Debug"),
      ).pipe(
        Effect.provide(
          Layer.mergeAll(
            ConfigProvider.layer(
              ConfigProvider.fromEnv({
                env: {
                  ONICODE_MODE: "standalone",
                  ONICODE_BOOTSTRAP_FD: String(fd),
                  ONICODE_HOME: baseDir,
                  ONICODE_AUTO_BOOTSTRAP_PROJECT_FROM_CWD: "true",
                  ONICODE_LOG_WS_EVENTS: "true",
                },
              }),
            ),
            NetService.layer,
          ),
        ),
      );

      expect(resolved).toEqual({
        logLevel: "Debug",
        ...defaultObservabilityConfig,
        mode: "standalone",
        port: 8788,
        cwd: process.cwd(),
        baseDir,
        ...derivedPaths,
        host: "127.0.0.1",
        dev: true,
        desktopBootstrapToken: "desktop-token",
        autoBootstrapProjectFromCwd: true,
        logWebSocketEvents: true,
        tailscaleServeEnabled: false,
        tailscaleServePort: 443,
      });
    }),
  );

  it.effect("reports the desktop bootstrap as the source of endpoints it supplied", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const baseDir = yield* fs.makeTempDirectoryScoped({
        prefix: "oni-cli-config-otlp-bootstrap-",
      });
      const fd = yield* openBootstrapFd(
        makeDesktopBootstrap({
          otlpTracesUrl: "http://localhost:4318/v1/traces",
          otlpMetricsUrl: "http://localhost:4318/v1/metrics",
          otlpLogsUrl: "http://localhost:4318/v1/logs",
        }),
      );

      const resolved = yield* resolveServerConfig(
        { ...minimalDesktopFlags(baseDir), bootstrapFd: Option.some(fd) },
        Option.none(),
      ).pipe(
        Effect.provide(
          Layer.mergeAll(
            ConfigProvider.layer(ConfigProvider.fromEnv({ env: {} })),
            NetService.layer,
          ),
        ),
      );

      expect(resolved).toMatchObject({
        otlpTracesUrl: "http://localhost:4318/v1/traces",
        otlpTracesSource: "bootstrap",
        otlpMetricsSource: "bootstrap",
        otlpLogsSource: "bootstrap",
      });
    }),
  );

  it.effect("reports saved settings as the source of endpoints read from settings.json", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const baseDir = yield* fs.makeTempDirectoryScoped({
        prefix: "oni-cli-config-otlp-settings-",
      });
      const derivedPaths = yield* deriveExplicitServerPaths(baseDir, false);
      yield* fs.makeDirectory(path.dirname(derivedPaths.settingsPath), { recursive: true });
      yield* fs.writeFileString(
        derivedPaths.settingsPath,
        `${JSON.stringify({
          observability: {
            otlpTracesUrl: "http://localhost:4318/v1/traces",
            otlpMetricsUrl: "http://localhost:4318/v1/metrics",
            otlpLogsUrl: "http://localhost:4318/v1/logs",
          },
        })}\n`,
      );

      const resolved = yield* resolveServerConfig(minimalDesktopFlags(baseDir), Option.none()).pipe(
        Effect.provide(
          Layer.mergeAll(
            ConfigProvider.layer(ConfigProvider.fromEnv({ env: {} })),
            NetService.layer,
          ),
        ),
      );

      expect(resolved).toMatchObject({
        otlpTracesUrl: "http://localhost:4318/v1/traces",
        otlpTracesSource: "settings",
        otlpMetricsSource: "settings",
        otlpLogsSource: "settings",
      });
    }),
  );

  it.effect("zeroes an endpoint stored in Settings when the SDK is disabled", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const baseDir = yield* fs.makeTempDirectoryScoped({ prefix: "oni-cli-config-otel-off-" });
      const derivedPaths = yield* deriveExplicitServerPaths(baseDir, false);
      yield* fs.makeDirectory(path.dirname(derivedPaths.settingsPath), { recursive: true });
      yield* fs.writeFileString(
        derivedPaths.settingsPath,
        `${JSON.stringify({
          observability: {
            otlpTracesUrl: "http://localhost:4318/v1/traces",
            otlpMetricsUrl: "http://localhost:4318/v1/metrics",
            otlpLogsUrl: "http://localhost:4318/v1/logs",
          },
        })}\n`,
      );

      const resolved = yield* resolveServerConfig(
        {
          mode: Option.some("desktop"),
          port: Option.some(4888),
          host: Option.none(),
          baseDir: Option.some(baseDir),
          cwd: Option.none(),
          dev: Option.none(),
          bootstrapFd: Option.none(),
          autoBootstrapProjectFromCwd: Option.none(),
          logWebSocketEvents: Option.none(),
          tailscaleServeEnabled: Option.none(),
          tailscaleServePort: Option.none(),
          lan: Option.none(),
        },
        Option.none(),
      ).pipe(
        Effect.provide(
          Layer.mergeAll(
            ConfigProvider.layer(ConfigProvider.fromEnv({ env: { OTEL_SDK_DISABLED: "true" } })),
            NetService.layer,
          ),
        ),
      );

      expect(resolved.otlpTracesUrl).toBeUndefined();
      expect(resolved.otlpMetricsUrl).toBeUndefined();
      expect(resolved.otlpLogsUrl).toBeUndefined();
      expect(resolved.otelEnvironment.disabled).toBe(true);
    }),
  );

  it.effect("lets ONICODE_OTEL_SDK_DISABLED=false override an ambient OTEL_SDK_DISABLED=true", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const baseDir = yield* fs.makeTempDirectoryScoped({ prefix: "oni-cli-config-otel-on-" });
      const derivedPaths = yield* deriveExplicitServerPaths(baseDir, false);
      yield* fs.makeDirectory(path.dirname(derivedPaths.settingsPath), { recursive: true });
      yield* fs.writeFileString(
        derivedPaths.settingsPath,
        `${JSON.stringify({
          observability: {
            otlpTracesUrl: "http://localhost:4318/v1/traces",
          },
        })}\n`,
      );

      const resolved = yield* resolveServerConfig(
        {
          mode: Option.some("desktop"),
          port: Option.some(4888),
          host: Option.none(),
          baseDir: Option.some(baseDir),
          cwd: Option.none(),
          dev: Option.none(),
          bootstrapFd: Option.none(),
          autoBootstrapProjectFromCwd: Option.none(),
          logWebSocketEvents: Option.none(),
          tailscaleServeEnabled: Option.none(),
          tailscaleServePort: Option.none(),
          lan: Option.none(),
        },
        Option.none(),
      ).pipe(
        Effect.provide(
          Layer.mergeAll(
            ConfigProvider.layer(
              ConfigProvider.fromEnv({
                env: { ONICODE_OTEL_SDK_DISABLED: "false", OTEL_SDK_DISABLED: "true" },
              }),
            ),
            NetService.layer,
          ),
        ),
      );

      expect(resolved.otelEnvironment.disabled).toBe(false);
      expect(resolved.otlpTracesUrl).toBe("http://localhost:4318/v1/traces");
    }),
  );

  it.effect("keeps whitespace-separated pairs and literal equals signs in OTLP headers", () =>
    Effect.gen(function* () {
      const { join } = yield* Path.Path;
      const baseDir = join(NodeOS.tmpdir(), "oni-cli-config-otlp-headers-loose-base");

      const resolved = yield* resolveServerConfig(
        {
          mode: Option.some("standalone"),
          port: Option.some(3773),
          host: Option.none(),
          baseDir: Option.some(baseDir),
          cwd: Option.none(),
          dev: Option.none(),
          bootstrapFd: Option.none(),
          autoBootstrapProjectFromCwd: Option.none(),
          logWebSocketEvents: Option.none(),
          tailscaleServeEnabled: Option.none(),
          tailscaleServePort: Option.none(),
          lan: Option.none(),
        },
        Option.none(),
      ).pipe(
        Effect.provide(
          Layer.mergeAll(
            ConfigProvider.layer(
              ConfigProvider.fromEnv({
                env: {
                  ONICODE_OTLP_HEADERS: "authorization=Bearer abc==, x-tenant=onicode",
                  ONICODE_OTLP_TRACES_URL: "http://collector.internal:4318",
                },
              }),
            ),
            NetService.layer,
          ),
        ),
      );

      expect(resolved.otlpTracesExport.headers).toEqual({
        authorization: "Bearer abc==",
        "x-tenant": "onicode",
      });
      expect(resolved.otlpTracesUrl).toBe("http://collector.internal:4318");
    }),
  );

  const minimalWebFlags = (baseDir: string) => ({
    mode: Option.some("standalone" as const),
    port: Option.some(3773),
    host: Option.none<string>(),
    baseDir: Option.some(baseDir),
    cwd: Option.none<string>(),
    dev: Option.none<boolean>(),
    bootstrapFd: Option.none<number>(),
    autoBootstrapProjectFromCwd: Option.none<boolean>(),
    logWebSocketEvents: Option.none<boolean>(),
    tailscaleServeEnabled: Option.none<boolean>(),
    tailscaleServePort: Option.none<number>(),
    lan: Option.none<boolean>(),
  });

  it.effect("uses the automatic port selector only when no port is configured", () =>
    Effect.gen(function* () {
      const baseDir = yield* FileSystem.FileSystem.pipe(
        Effect.flatMap((fs) => fs.makeTempDirectoryScoped({ prefix: "oni-cli-auto-port-" })),
      );
      const selected: number[] = [];
      const selectAutomaticPort = (preferred: number) =>
        Effect.sync(() => {
          selected.push(preferred);
          return 50_123;
        });
      const resolvePort = (flags: {
        readonly mode?: Option.Option<"standalone" | "desktop">;
        readonly port: Option.Option<number>;
      }) =>
        resolveServerConfig({ ...minimalWebFlags(baseDir), ...flags }, Option.none(), {
          selectAutomaticPort,
        }).pipe(
          Effect.map((config) => config.port),
          Effect.provide(
            Layer.merge(
              NetService.layer,
              ConfigProvider.layer(ConfigProvider.fromEnv({ env: {} })),
            ),
          ),
        );

      expect(yield* resolvePort({ port: Option.none() })).toBe(50_123);
      expect(selected).toEqual([3773]);
      expect(yield* resolvePort({ port: Option.some(8788) })).toBe(8788);
      expect(
        yield* resolvePort({ mode: Option.some("desktop" as const), port: Option.none() }),
      ).toBe(3773);
      expect(selected).toEqual([3773]);
    }),
  );

  it.effect(
    "resolves each signal's endpoint through ONICODE_OTLP_*_URL, an OTEL endpoint, the bootstrap envelope, and persisted Settings, in that order",
    () =>
      Effect.gen(function* () {
        const fs = yield* FileSystem.FileSystem;
        const path = yield* Path.Path;
        const baseDir = yield* fs.makeTempDirectoryScoped({
          prefix: "oni-cli-config-otel-precedence-",
        });
        const derivedPaths = yield* deriveExplicitServerPaths(baseDir, false);
        yield* fs.makeDirectory(path.dirname(derivedPaths.settingsPath), { recursive: true });
        yield* fs.writeFileString(
          derivedPaths.settingsPath,
          `${JSON.stringify({ observability: { otlpLogsUrl: "http://settings:4318/v1/logs" } })}\n`,
        );

        const fd = yield* openBootstrapFd(
          makeDesktopBootstrap({
            otlpMetricsUrl: "http://bootstrap:4318/v1/metrics",
            otlpLogsUrl: "",
          }),
        );

        const resolved = yield* resolveServerConfig(
          {
            ...minimalWebFlags(baseDir),
            mode: Option.some("desktop"),
            port: Option.some(4888),
            bootstrapFd: Option.some(fd),
          },
          Option.none(),
        ).pipe(
          Effect.provide(
            Layer.mergeAll(
              ConfigProvider.layer(
                ConfigProvider.fromEnv({
                  env: {
                    ONICODE_OTLP_TRACES_URL: "http://onicode:4318/v1/traces",
                    ONICODE_OTLP_HEADERS: "x-key=secret",
                    OTEL_EXPORTER_OTLP_TRACES_ENDPOINT: "http://otel-traces:4318/custom",
                    OTEL_EXPORTER_OTLP_METRICS_ENDPOINT: "http://otel-metrics:4318/custom",
                    OTEL_EXPORTER_OTLP_HEADERS: "x-key=otel",
                  },
                }),
              ),
              NetService.layer,
            ),
          ),
        );

        expect(resolved.otlpTracesUrl).toBe("http://onicode:4318/v1/traces");
        expect(resolved.otlpTracesExport.headers).toEqual({ "x-key": "secret" });
        expect(resolved.otlpMetricsUrl).toBe("http://otel-metrics:4318/custom");
        expect(resolved.otlpMetricsExport).toEqual({
          ...DEFAULT_SIGNAL_EXPORT,
          protocol: "http/protobuf",
          headers: { "x-key": "otel" },
        });
        expect(resolved.otlpLogsUrl).toBe("http://settings:4318/v1/logs");
        expect(resolved.otlpLogsExport.headers).toEqual({ "x-key": "secret" });
        expect(resolved).toMatchObject({
          otlpTracesVariable: "ONICODE_OTLP_TRACES_URL",
          otlpMetricsVariable: "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
        });
        expect(resolved.otlpLogsVariable).toBeUndefined();
      }),
  );

  it.effect(
    "exports nothing for a signal an OTEL endpoint claimed with a protocol or headers that do not read",
    () =>
      Effect.gen(function* () {
        const fs = yield* FileSystem.FileSystem;
        const path = yield* Path.Path;
        const baseDir = yield* fs.makeTempDirectoryScoped({
          prefix: "oni-cli-config-otel-off-",
        });
        const derivedPaths = yield* deriveExplicitServerPaths(baseDir, false);
        yield* fs.makeDirectory(path.dirname(derivedPaths.settingsPath), { recursive: true });
        yield* fs.writeFileString(
          derivedPaths.settingsPath,
          `${JSON.stringify({ observability: { otlpLogsUrl: "http://settings:4318/v1/logs" } })}\n`,
        );

        const fd = yield* openBootstrapFd(
          makeDesktopBootstrap({ otlpMetricsUrl: "http://bootstrap:4318/v1/metrics" }),
        );

        const resolved = yield* resolveServerConfig(
          {
            ...minimalWebFlags(baseDir),
            mode: Option.some("desktop"),
            port: Option.some(4888),
            bootstrapFd: Option.some(fd),
          },
          Option.none(),
        ).pipe(
          Effect.provide(
            Layer.mergeAll(
              ConfigProvider.layer(
                ConfigProvider.fromEnv({
                  env: {
                    ONICODE_OTLP_TRACES_URL: "http://onicode:4318/v1/traces",
                    OTEL_EXPORTER_OTLP_ENDPOINT: "http://otel:4318",
                    OTEL_EXPORTER_OTLP_HEADERS: "x-key=%zz",
                  },
                }),
              ),
              NetService.layer,
            ),
          ),
        );

        expect(resolved.otlpTracesUrl).toBe("http://onicode:4318/v1/traces");
        expect(resolved.otlpMetricsUrl).toBeUndefined();
        expect(resolved.otlpLogsUrl).toBeUndefined();
      }),
  );

  it.effect("listens on private networks per ONICODE_LAN, else under the service launcher", () =>
    Effect.gen(function* () {
      const { join } = yield* Path.Path;
      const baseDir = join(NodeOS.tmpdir(), "oni-cli-config-service-host");
      const resolveHost = (input: {
        readonly connected: boolean;
        readonly env?: Record<string, string>;
      }) =>
        resolveServerConfig(minimalWebFlags(baseDir), Option.none()).pipe(
          Effect.map(({ host, privateNetworkOnly }) => ({ host, privateNetworkOnly })),
          Effect.provide(
            Layer.mergeAll(
              ConfigProvider.layer(ConfigProvider.fromEnv({ env: input.env ?? {} })),
              NetService.layer,
            ),
          ),
          Effect.provideService(HostProcess.Environment, {
            [SERVICE_LAUNCHER_CONTEXT_ENV]: JSON.stringify({
              protocol: SERVICE_LAUNCHER_PROTOCOL,
              childVersion: packageJson.version,
            }),
          }),
          Effect.provideService(ServiceLauncherHostProcess, {
            connected: input.connected,
            send: () => false,
            on: () => undefined,
            off: () => undefined,
          }),
        );

      // A service installed before ONICODE_LAN existed.
      expect(yield* resolveHost({ connected: true })).toEqual({
        host: "0.0.0.0",
        privateNetworkOnly: true,
      });
      // `service.env` reaches the server as its environment.
      expect(yield* resolveHost({ connected: true, env: { ONICODE_HOST: "127.0.0.1" } })).toEqual({
        host: "127.0.0.1",
        privateNetworkOnly: undefined,
      });
      expect(yield* resolveHost({ connected: true, env: { ONICODE_LAN: "false" } })).toEqual({
        host: undefined,
        privateNetworkOnly: undefined,
      });
      expect(yield* resolveHost({ connected: false, env: { ONICODE_LAN: "true" } })).toEqual({
        host: "0.0.0.0",
        privateNetworkOnly: true,
      });
      // Without the launcher's IPC channel the context was inherited, e.g. by
      // `onicode serve` started from an agent inside the service.
      expect(yield* resolveHost({ connected: false })).toEqual({
        host: undefined,
        privateNetworkOnly: undefined,
      });
    }),
  );
});
