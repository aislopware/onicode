// @effect-diagnostics nodeBuiltinImport:off - Effect has no synchronous digest.
import type {
  AgentBrowserUseTarget,
  BrowserUseStatus,
  ServerSettings as ServerSettingsSnapshot,
  ThreadId,
} from "@onicode/contracts";
import type { McpStdioServer } from "@onicode/provider-core/server/mcpSession";
import { writeFileStringAtomically } from "@onicode/shared/atomicWrite";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Path from "effect/Path";
import * as Schema from "effect/Schema";
import * as Stream from "effect/Stream";
import { ChildProcess, ChildProcessSpawner } from "effect/process";
import * as NodeCrypto from "node:crypto";

import * as ServerConfig from "../config.ts";
import type * as McpInvocationContext from "../mcp/McpInvocationContext.ts";
import * as PreviewCdpBridge from "../preview/PreviewCdpBridge.ts";
import * as ServerSettings from "../serverSettings.ts";
import * as BrowserUseDriver from "./BrowserUseDriver.ts";

export class BrowserUseService extends Context.Service<
  BrowserUseService,
  {
    /** Installs obu when it is missing. False when it cannot run on this machine. */
    readonly ready: Effect.Effect<boolean>;
    /** The `browser` MCP server for one thread's provider session. Call after `ready`. */
    readonly mcpServer: (input: {
      readonly scope: McpInvocationContext.McpThreadInvocationScope;
      readonly target: AgentBrowserUseTarget;
      /** The personal browser's fixed debug port, when it has one. */
      readonly port: number | null;
    }) => Effect.Effect<Option.Option<McpStdioServer>>;
    /** Closes the tabs the thread's driver opened in the user's browser. `forget` drops its record. */
    readonly endSession: (
      threadId: ThreadId,
      options: { readonly forget: boolean },
    ) => Effect.Effect<void>;
    readonly status: Effect.Effect<BrowserUseStatus>;
  }
>()("onicode/browserUse/BrowserUseService") {}

const SESSION_WAIT = "45 seconds";
const STOP_TIMEOUT = "20 seconds";
const SHARED_ENVIRONMENT = { OBU_NO_UPDATE_CHECK: "1" };
// obu tells the agent these in place of its own guess that an attached browser is the user's.
// The personal browser needs none: that guess is right there.
const PANEL_NOTE =
  "Use these tools for browser work: they drive your tab in Onicode's Browser panel, which the user watches.";
const ISOLATED_NOTE =
  "Use these tools for browser work: they drive a Chrome of your own, which the user cannot see.";

const RealBrowserConfig = Schema.fromJsonString(Schema.Record(Schema.String, Schema.Unknown));
const decodeRealBrowserConfig = Schema.decodeUnknownOption(RealBrowserConfig);
const encodeRealBrowserConfig = Schema.encodeSync(RealBrowserConfig);

/** obu allows 32 characters; the hash keeps a thread id out of file names. */
export const browserUseSessionName = (threadId: ThreadId) =>
  `oni-${NodeCrypto.createHash("sha256").update(threadId).digest("hex").slice(0, 16)}`;

/** True when the environment or any project turns browser use on. */
const browserUseEnabledAnywhere = (settings: ServerSettingsSnapshot) =>
  settings.enableAgentBrowserUse ||
  Object.values(settings.projectSettingsOverrides).some(
    (overrides) => overrides.enableAgentBrowserUse === true,
  );

export const make = Effect.gen(function* () {
  const driver = yield* BrowserUseDriver.BrowserUseDriver;
  const bridge = yield* PreviewCdpBridge.PreviewCdpBridge;
  const settings = yield* ServerSettings.ServerSettingsService;
  const config = yield* ServerConfig.ServerConfig;
  const spawner = yield* ChildProcessSpawner.ChildProcessSpawner;
  const fs = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  const serviceScope = yield* Effect.scope;
  const fileServices = yield* Effect.context<FileSystem.FileSystem | Path.Path>();
  // One data dir for every thread: logins, site knowledge and the fingerprint are shared,
  // and `OBU_SESSION` keeps each thread's tabs apart inside it.
  const home = path.join(config.stateDir, "browser-use");
  const sessionEnvironment = (threadId: ThreadId) => ({
    ...SHARED_ENVIRONMENT,
    OBU_HOME: home,
    OBU_SESSION: browserUseSessionName(threadId),
  });

  // obu reads the port of a browser started with `--remote-debugging-port` from its config;
  // such a browser writes no `DevToolsActivePort` for discovery to find.
  const syncPersonalPort = Effect.fn("BrowserUseService.syncPersonalPort")(function* (
    port: number | null,
  ) {
    const file = path.join(home, "realbrowser.json");
    // A missing or unreadable file is obu's default config.
    const current: Readonly<Record<string, unknown>> = yield* fs.readFileString(file).pipe(
      Effect.map((text) => Option.getOrElse(decodeRealBrowserConfig(text), () => ({}))),
      Effect.orElseSucceed(() => ({})),
    );
    if ((current.port ?? null) === port) return;
    const { port: _port, ...kept } = current;
    yield* writeFileStringAtomically({
      filePath: file,
      contents: `${encodeRealBrowserConfig(port === null ? kept : { ...kept, port })}\n`,
    }).pipe(Effect.provideContext(fileServices));
  });

  // Sessions wait for one install. Once that fails or runs long, they start without obu
  // and the download is retried behind them, so a blocked network delays one session only.
  let waitForInstall = true;
  const ready = Effect.gen(function* () {
    const install = yield* driver.state;
    if (install.state === "unsupported") return false;
    if (install.state === "installed") return true;
    if (!waitForInstall) {
      yield* driver.executable.pipe(Effect.ignore, Effect.forkIn(serviceScope));
      return false;
    }
    const installed = yield* driver.executable.pipe(
      Effect.as(true),
      Effect.timeoutOrElse({ duration: SESSION_WAIT, orElse: () => Effect.succeed(false) }),
      Effect.catch((cause) =>
        Effect.logWarning("Browser use is unavailable for this session.", { cause }).pipe(
          Effect.as(false),
        ),
      ),
    );
    waitForInstall = installed;
    return installed;
  });

  const mcpServer: BrowserUseService["Service"]["mcpServer"] = Effect.fn(
    "BrowserUseService.mcpServer",
  )(
    function* ({ scope, target, port }) {
      const command = yield* driver.executable;
      const env = sessionEnvironment(scope.thread.threadId);
      switch (target) {
        case "panel": {
          const endpoint = yield* bridge.endpoint(scope);
          return Option.some<McpStdioServer>({
            command,
            args: ["mcp", "--host", endpoint.host, "--port", String(endpoint.port)],
            // The real lane sends a page no stealth patches: the panel's browser is Onicode's.
            env: {
              ...env,
              OBU_LANE: "real",
              OBU_ENDPOINT_PATH: endpoint.path,
              OBU_HOST_NOTE: PANEL_NOTE,
            },
          });
        }
        case "isolated":
          return Option.some<McpStdioServer>({
            command,
            args: ["mcp"],
            env: { ...env, OBU_LANE: "agent", OBU_HOST_NOTE: ISOLATED_NOTE },
          });
        case "personal":
          yield* syncPersonalPort(port);
          return Option.some<McpStdioServer>({
            command,
            args: ["mcp"],
            env: { ...env, OBU_LANE: "real" },
          });
      }
    },
    Effect.catch((cause) =>
      Effect.logWarning("Browser use is unavailable for this session.", { cause }).pipe(
        Effect.as(Option.none<McpStdioServer>()),
      ),
    ),
  );

  const endSession: BrowserUseService["Service"]["endSession"] = Effect.fn(
    "BrowserUseService.endSession",
  )(
    function* (threadId, options) {
      yield* bridge.release(threadId);
      const directory = path.join(home, "agent-sessions", browserUseSessionName(threadId));
      // Only a driver that attached to a browser leaves a record.
      if (!(yield* fs.exists(directory))) return;
      if ((yield* driver.state).state === "installed") {
        const child = yield* spawner.spawn(
          ChildProcess.make(yield* driver.executable, ["stop", "--close-tabs"], {
            shell: false,
            stdin: "ignore",
            stdout: "ignore",
            stderr: "ignore",
            env: sessionEnvironment(threadId),
            extendEnv: true,
            forceKillAfter: "3 seconds",
          }),
        );
        yield* child.exitCode.pipe(Effect.timeoutOption(STOP_TIMEOUT));
      }
      if (options.forget) yield* fs.remove(directory, { recursive: true, force: true });
    },
    Effect.scoped,
    Effect.catch((cause) => Effect.logWarning("Could not end a browser use session.", { cause })),
  );

  // Download ahead of the first session that needs obu.
  yield* Stream.concat(
    Stream.fromEffect(settings.getSettings).pipe(Stream.catch(() => Stream.empty)),
    settings.streamChanges,
  ).pipe(
    Stream.map(browserUseEnabledAnywhere),
    Stream.changes,
    Stream.runForEach((enabled) => (enabled ? Effect.ignore(driver.executable) : Effect.void)),
    Effect.forkScoped,
  );

  const status = Effect.gen(function* () {
    const enabled = yield* settings.getSettings.pipe(
      Effect.map(browserUseEnabledAnywhere),
      Effect.orElseSucceed(() => false),
    );
    let install = yield* driver.state;
    if (enabled && install.state === "missing") {
      // A client asks right after turning this on, before the settings stream sees the change.
      yield* driver.executable.pipe(
        Effect.ignore,
        Effect.forkIn(serviceScope, { startImmediately: true }),
      );
      install = yield* driver.state;
    }
    return { driverVersion: driver.version, install } satisfies BrowserUseStatus;
  }).pipe(Effect.withSpan("BrowserUseService.status"));

  return BrowserUseService.of({ ready, mcpServer, endSession, status });
});

export const layer = Layer.effect(BrowserUseService, make);
