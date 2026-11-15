#!/usr/bin/env node
// The app command of e2e.config.ts: starts the dev stack of the checkout it runs in on a fresh
// Onicode home. e2e stops it with a signal, which ends the scope and the processes it started.
// @effect-diagnostics-next-line nodeBuiltinImport:off - the launcher pipes requests to the app's server
import * as NodeHttp from "node:http";

import * as NodeRuntime from "@effect/platform-node/NodeRuntime";
import * as NodeServices from "@effect/platform-node/NodeServices";
import * as HostProcess from "@onicode/shared/HostProcess";
import * as NetService from "@onicode/shared/Net";
import * as Console from "effect/Console";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Path from "effect/Path";
import * as Schedule from "effect/Schedule";
import * as Schema from "effect/Schema";
import * as Stream from "effect/Stream";
import { Argument, Command } from "effect/cli";
import { ChildProcess, ChildProcessSpawner } from "effect/process";

import { CAPTURE_NATIVE_UI, DesktopStack, evaluateInMain } from "./desktop-main.ts";
import { ScenarioError } from "./shell.ts";

/** The renderer's real origin. e2e only opens http(s), so the launcher page redirects here. */
const APP_ORIGIN = "onicode-dev://app";
// What a scenario fetches from `app.baseUrl` goes to the app's server; every other path is a
// route of the app, which the launcher page opens in the window.
const SERVER_PATHS = /^\/(?:\.well-known|api|mcp|attachments)(?:\/|$)/;
// The replay agent `vp run dev` registers: free, recorded turns through the real adapter.
const REPLAY = { instanceId: "devReplay", model: "showcase" };
// Generated text (titles) cannot go to the replay agent: the server swaps an ACP agent for the
// first enabled real provider, else its default, Codex. So every provider it could fall back to
// is off and Codex is a command that only fails: no run reaches a paid model.
// Mirror of TEXT_GENERATION_FALLBACK_DRIVERS in shared/serverSettings.
const REAL_PROVIDERS = [
  "codex",
  "claudeAgent",
  "cursor",
  "grok",
  "muse",
  "pi",
  "opencode",
  "antigravity",
];

/**
 * The name every scenario server takes. Left to itself a server reports the host's name, which
 * would put the machine that ran a scenario into its captures.
 */
export const ENVIRONMENT_LABEL = "Scenario";

const encodeSettings = Schema.encodeEffect(Schema.fromJsonString(Schema.Unknown));
const encodeDesktopStack = Schema.encodeEffect(Schema.fromJsonString(DesktopStack));

export const prepareHome = Effect.fn("startApp.prepareHome")(function* (home: string) {
  const fs = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  // This deletes `home`, and the developer's real one is a typo away from a scenario home.
  if (!path.basename(home).startsWith("scenarios")) {
    return yield* new ScenarioError({
      message: `${home} is not a scenario home: its last segment must start with "scenarios".`,
    });
  }
  yield* fs.remove(home, { recursive: true, force: true });
  yield* fs.makeDirectory(path.join(home, "userdata"), { recursive: true });
  yield* fs.makeDirectory(path.join(home, "bin"));
  const noModel = path.join(home, "bin/no-model");
  yield* fs.writeFileString(noModel, "#!/bin/sh\necho 'scenarios run no model' >&2\nexit 1\n");
  yield* fs.chmod(noModel, 0o755);
  yield* fs.writeFileString(
    path.join(home, "userdata/settings.json"),
    yield* encodeSettings({
      defaultModelSelection: REPLAY,
      providerInstances: Object.fromEntries(
        REAL_PROVIDERS.map((driver) => [
          driver,
          {
            driver,
            enabled: false,
            ...(driver === "codex" ? { config: { binaryPath: noModel } } : {}),
          },
        ]),
      ),
    }),
  );
});

/** Starts a process in the caller's scope and hands each line it prints to `onLine`. */
export const start = Effect.fn("startApp.start")(function* (input: {
  readonly cwd: string;
  readonly command: string;
  readonly args: ReadonlyArray<string>;
  readonly env: NodeJS.ProcessEnv;
  readonly onLine: (line: string) => Effect.Effect<void, never, FileSystem.FileSystem>;
}) {
  const spawner = yield* ChildProcessSpawner.ChildProcessSpawner;
  const child = yield* spawner.spawn(
    ChildProcess.make(input.command, [...input.args], {
      cwd: input.cwd,
      env: input.env,
      extendEnv: false,
      stdin: "ignore",
      stdout: "pipe",
      stderr: "inherit",
      forceKillAfter: "5 seconds",
    }),
  );
  yield* child.stdout.pipe(
    Stream.decodeText(),
    Stream.splitLines,
    Stream.runForEach(input.onLine),
    Effect.ignore,
    Effect.forkScoped,
  );
  return child;
});

interface CdpTarget {
  readonly id: string;
  readonly url: string;
}

/**
 * Succeeds once the app's window is the only page. The dev window opens DevTools beside it,
 * which e2e would take for the app's first tab, so those targets are closed first.
 */
const closeDevTools = Effect.fn("startApp.closeDevTools")(function* (cdpPort: number) {
  const cdp = `http://127.0.0.1:${cdpPort}`;
  const targets = yield* Effect.tryPromise({
    // @effect-diagnostics-next-line globalFetchInEffect:off - the DevTools HTTP endpoint, polled while the window starts
    try: async () => (await (await fetch(`${cdp}/json/list`)).json()) as ReadonlyArray<CdpTarget>,
    catch: () => new ScenarioError({ message: "The dev window is not up yet." }),
  });
  const devtools = targets.filter((target) => target.url.startsWith("devtools://"));
  yield* Effect.forEach(devtools, (target) =>
    // @effect-diagnostics-next-line globalFetchInEffect:off - the DevTools HTTP endpoint
    Effect.promise(() => fetch(`${cdp}/json/close/${target.id}`)),
  );
  if (devtools.length > 0 || !targets.some((target) => target.url.startsWith(APP_ORIGIN))) {
    return yield* new ScenarioError({ message: "The dev window is not ready yet." });
  }
});

/**
 * The page e2e opens stands in for the web client's origin: a route such as `/settings/general`
 * redirects the window to that route of the app, and a server path is proxied to the server.
 */
const serveLauncher = Effect.fn("startApp.serveLauncher")(function* (input: {
  readonly host: string;
  readonly port: number;
  readonly serverPort: Deferred.Deferred<number>;
}) {
  const page = `<!doctype html><script>location.replace("${APP_ORIGIN}/#" + location.pathname + location.search)</script>`;
  let serverPort: number | undefined;
  yield* Deferred.await(input.serverPort).pipe(
    Effect.tap((port) => Effect.sync(() => (serverPort = port))),
    Effect.forkScoped,
  );
  yield* Effect.acquireRelease(
    Effect.callback<NodeHttp.Server>((resume) => {
      const server = NodeHttp.createServer((request, response) => {
        if (serverPort === undefined) {
          response.writeHead(503).end();
        } else if (SERVER_PATHS.test(request.url ?? "")) {
          const upstream = NodeHttp.request(
            {
              host: "127.0.0.1",
              port: serverPort,
              path: request.url,
              method: request.method,
              headers: { ...request.headers, host: `127.0.0.1:${serverPort}` },
            },
            (reply) => {
              response.writeHead(reply.statusCode ?? 502, reply.headers);
              reply.pipe(response);
            },
          );
          upstream.on("error", () => {
            if (response.headersSent) response.destroy();
            else response.writeHead(502).end();
          });
          request.pipe(upstream);
        } else {
          response.writeHead(200, { "content-type": "text/html", "cache-control": "no-store" });
          response.end(page);
        }
      });
      server.listen(input.port, input.host, () => resume(Effect.succeed(server)));
    }),
    (server) =>
      Effect.sync(() => {
        server.closeAllConnections();
        server.close();
      }),
  );
});

const startApp = Effect.fn("startApp")(function* (input: {
  readonly surface: "desktop" | "mobile";
  readonly home: string;
  readonly ports: ReadonlyArray<string>;
}) {
  const fs = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  const net = yield* NetService.NetService;
  const node = yield* HostProcess.ExecutablePath;
  const checkout = yield* HostProcess.WorkingDirectory;
  const hostEnvironment = yield* HostProcess.Environment;
  yield* prepareHome(input.home);
  const env = {
    ...hostEnvironment,
    PATH: `${path.join(input.home, "bin")}:${hostEnvironment.PATH ?? ""}`,
    ONICODE_AUTO_BOOTSTRAP_PROJECT_FROM_CWD: "0",
    ONICODE_ENVIRONMENT_LABEL: ENVIRONMENT_LABEL,
  };
  const devRunner = (mode: string, args: ReadonlyArray<string>) => [
    "scripts/dev-runner.ts",
    mode,
    "--home-dir",
    input.home,
    "--no-seed",
    ...args,
  ];

  if (input.surface === "desktop") {
    const [host = "", launcherPort = ""] = input.ports;
    const inspectPort = yield* net.reserveLoopbackPort();
    const ports = yield* Deferred.make<{ readonly serverPort: number; readonly cdpPort: number }>();
    // Set once the window is ready for a scenario: the launcher answers 503 until then, so e2e's
    // ready check waits for the app and not only for its server.
    const serverPort = yield* Deferred.make<number>();
    yield* serveLauncher({ host, port: Number(launcherPort), serverPort });
    const agentHome = (name: string) => path.join(input.home, "agents", name);
    const app = yield* start({
      cwd: checkout,
      command: node,
      args: devRunner("dev", []),
      env: {
        ...env,
        // The welcome page offers to import the developer's own agent history; these are empty.
        CLAUDE_CONFIG_DIR: agentHome("claude"),
        CODEX_HOME: agentHome("codex"),
        PI_CODING_AGENT_SESSION_DIR: agentHome("pi"),
        // Every dev app otherwise shares one Chromium profile, its drafts and its instance lock.
        ONICODE_DESKTOP_APP_DATA_DIR: path.join(input.home, "profile"),
        ONICODE_DESKTOP_INSPECT_PORT: String(inspectPort),
        ONICODE_DISABLE_AUTO_UPDATE: "1",
      },
      onLine: (line) => {
        const match = /\[dev-runner\].* serverPort=(\d+) .*cdpPort=(\d+)/.exec(line);
        return Console.log(line).pipe(
          Effect.andThen(
            match === null
              ? Effect.void
              : Deferred.succeed(ports, {
                  serverPort: Number(match[1]),
                  cdpPort: Number(match[2]),
                }),
          ),
        );
      },
    });
    const prepareWindow = Effect.gen(function* () {
      const { cdpPort, serverPort: port } = yield* Deferred.await(ports);
      yield* closeDevTools(cdpPort).pipe(
        Effect.retry({ schedule: Schedule.spaced("250 millis"), times: 1200 }),
      );
      yield* Effect.tryPromise({
        try: () => evaluateInMain(inspectPort, CAPTURE_NATIVE_UI),
        catch: (cause) => new ScenarioError({ message: `The main process: ${String(cause)}` }),
      });
      yield* fs.writeFileString(
        path.join(input.home, "desktop.json"),
        yield* encodeDesktopStack({ cdpPort, inspectPort }),
      );
      yield* Deferred.succeed(serverPort, port);
      return yield* Effect.never;
    });
    return yield* Effect.raceFirst(app.exitCode, prepareWindow);
  }

  // The device is another machine: the server listens on every interface, and Metro serves the
  // dev client its JavaScript. Tests mint their own pairing links, so the startup one is dropped.
  const [serverPort = "", metroPort = "", host = ""] = input.ports;
  const server = yield* start({
    cwd: checkout,
    command: node,
    args: devRunner("dev:server", ["--host", "0.0.0.0", "--port", serverPort]),
    env,
    onLine: (line) => (line.includes("pairingUrl:") ? Effect.void : Console.log(line)),
  });
  yield* net.hasListenerOnHost(Number(serverPort), "127.0.0.1").pipe(
    Effect.filterOrFail(
      (listening) => listening,
      () => new ScenarioError({ message: "The server is not listening yet." }),
    ),
    Effect.retry({ schedule: Schedule.spaced("500 millis"), times: 600 }),
  );
  const metro = yield* start({
    cwd: path.join(checkout, "apps/mobile"),
    command: "npx",
    args: [
      "expo",
      "start",
      "--dev-client",
      "--scheme",
      "onicode-dev",
      "--lan",
      "--port",
      metroPort,
    ],
    env: {
      ...env,
      APP_VARIANT: "development",
      CI: "1",
      // Lazily fetched chunks sometimes arrive broken on the device.
      EXPO_NO_METRO_LAZY: "1",
      REACT_NATIVE_PACKAGER_HOSTNAME: host,
    },
    onLine: Console.log,
  });
  return yield* Effect.raceFirst(server.exitCode, metro.exitCode);
});

const command = Command.make(
  "start-app",
  {
    surface: Argument.Literals("surface", ["desktop", "mobile"]).pipe(
      Argument.withDescription(
        "desktop: the dev desktop app. mobile: server on every interface and Metro.",
      ),
    ),
    home: Argument.String("oni-home").pipe(Argument.withDescription("Onicode home to recreate.")),
    ports: Argument.String("port").pipe(
      Argument.withDescription(
        "desktop: <host> <launcher-port>. mobile: <server-port> <metro-port> <host>.",
      ),
      Argument.variadic(),
    ),
  },
  (input) =>
    startApp(input).pipe(
      Effect.flatMap((exitCode) =>
        Effect.sync(() => {
          process.exitCode = Number(exitCode) || 1;
        }),
      ),
    ),
);

if (import.meta.main) {
  Command.run(command, { version: "0.0.0" }).pipe(
    Effect.scoped,
    Effect.provide([NodeServices.layer, NetService.layer]),
    NodeRuntime.runMain,
  );
}
