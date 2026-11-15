// Scenarios: e2e tests that run against the real dev stack. See docs/operations/scenarios.md.
import * as NodeServices from "@effect/platform-node/NodeServices";
import { mobile } from "@e2e-dev/mobile";
import { web, type BrowserProvider } from "@e2e-dev/web";
import * as HostProcess from "@onicode/shared/HostProcess";
import type { E2EConfig } from "e2e";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Path from "effect/Path";
import * as Schema from "effect/Schema";

import { DesktopStack, evaluateInMain, RESET_WINDOW } from "./scenarios/desktop-main.ts";

// e2e loads this module for its values, so the paths and the host are resolved once, here.
const resolved = await Effect.gen(function* () {
  const path = yield* Path.Path;
  const env = yield* HostProcess.Environment;
  const scripts = yield* path.fromFileUrl(new URL(".", import.meta.url));
  const home = path.resolve(scripts, "../.onicode/scenarios");
  return {
    node: yield* HostProcess.ExecutablePath,
    scripts,
    /** The checkout whose app runs. `prove-scenario.ts` points it at a base or head worktree. */
    appDir: path.resolve(scripts, env.ONICODE_SCENARIO_APP_DIR ?? ".."),
    home,
    startApp: path.join(scripts, "scenarios/start-app.ts"),
    // Writing a scenario: the launcher URL of a desktop stack left running, which skips startup.
    runningApp: env.ONICODE_SCENARIO_APP_URL,
    // e2e gives the app only PATH, HOME and the temp directory. A scenario that needs more names
    // it here: exact names or prefixes ending in `*`, separated by commas.
    inheritEnv: (env.ONICODE_SCENARIO_INHERIT_ENV ?? "")
      .split(",")
      .map((name) => name.trim())
      .filter((name) => name !== ""),
    mobile: {
      // The simulator shares this machine's network.
      host: "127.0.0.1",
      // Names the simulator when several are booted: E2E_IOS_DEVICE="iPhone 18 Pro".
      device: env.E2E_IOS_DEVICE,
      // A mobile target has no `app.url`, so e2e cannot pick these ports.
      serverPort: Number(env.ONICODE_SCENARIO_SERVER_PORT ?? 13_990),
      metroPort: Number(env.ONICODE_SCENARIO_METRO_PORT ?? 8_190),
    },
  };
}).pipe(Effect.provide(NodeServices.layer), Effect.runPromise);

export const APP_DIR = resolved.appDir;
/** Onicode homes only scenarios use, recreated on every run. `.onicode` is gitignored. */
export const ONICODE_HOME = resolved.home;
export const MOBILE_ONI_HOME = `${ONICODE_HOME}-mobile`;
export const MOBILE = resolved.mobile;
const startApp = resolved.startApp;
/**
 * The one address e2e opens the app at and the start script serves its launcher on. Never
 * `localhost`: the runner and the app can resolve that name to different addresses.
 */
const LOOPBACK = "127.0.0.1";

export const DESKTOP_STACK_FILE = `${ONICODE_HOME}/desktop.json`;
const decodeDesktopStack = Schema.decodeUnknownEffect(Schema.fromJsonString(DesktopStack));
export const readDesktopStack = Effect.gen(function* () {
  const fs = yield* FileSystem.FileSystem;
  return yield* decodeDesktopStack(yield* fs.readFileString(DESKTOP_STACK_FILE));
});

/**
 * e2e cannot launch Electron or open a second context in it, so the "browser" of an attempt is
 * the dev window the app command already started. `scope: "attempt"` keeps e2e from expecting
 * a fresh browser per attempt: every scenario reloads the same window.
 */
// The dev runner builds the desktop shell and opens a window, which takes the developer's
// toolchain and display. e2e passes only PATH, HOME and the temp directory by itself.
const DESKTOP_ENV = [
  "CARGO_HOME",
  "CI",
  "DBUS_*",
  "DISPLAY",
  "LANG",
  "MISE_*",
  "RUSTUP_HOME",
  "SHELL",
  "USER",
  "WAYLAND_DISPLAY",
  "XAUTHORITY",
  "XDG_*",
];

const devWindow: BrowserProvider = {
  name: "dev desktop window",
  scope: "attempt",
  acquire: () =>
    readDesktopStack.pipe(
      Effect.tap(({ inspectPort }) =>
        Effect.promise(() => evaluateInMain(inspectPort, RESET_WINDOW)),
      ),
      Effect.map(({ cdpPort }) => ({
        id: "dev-window",
        cdpEndpoint: `http://${LOOPBACK}:${cdpPort}`,
      })),
      Effect.provide(NodeServices.layer),
      Effect.runPromise,
    ),
  release: () => Promise.resolve(),
};

export default {
  tests: "scenarios/**/*.e2e.ts",
  targets: [
    {
      name: "desktop",
      // Platform "web": the renderer is a web page, which is what `WEB` scenarios select.
      engine: web({ browser: devWindow, viewport: null }),
      app:
        resolved.runningApp === undefined
          ? {
              // The start script serves a launcher here: a page that hands the window to the
              // app's own origin, and a proxy to the app's server for what a scenario fetches.
              url: `http://${LOOPBACK}:0`,
              readyUrl: `http://${LOOPBACK}:{port}/.well-known/onicode/environment`,
              command: {
                executable: resolved.node,
                args: [startApp, "desktop", ONICODE_HOME, LOOPBACK, "{port}"],
                cwd: APP_DIR,
                inheritEnv: [...DESKTOP_ENV, ...resolved.inheritEnv],
                startupTimeout: 300_000,
                log: ".e2e/app.log",
              },
            }
          : { url: resolved.runningApp },
    },
    {
      name: "ios",
      // A booted simulator on this machine, as `apps/mobile/e2e` uses.
      engine: mobile({ platform: "ios", device: MOBILE.device }),
      app: {
        bundleId: "dev.aislopware.onicode.dev",
        // The server starts after Metro has built the first bundle, so it answering means both.
        readyUrl: `http://127.0.0.1:${MOBILE.serverPort}/.well-known/onicode/environment`,
        command: {
          executable: resolved.node,
          args: [
            startApp,
            "mobile",
            MOBILE_ONI_HOME,
            String(MOBILE.serverPort),
            String(MOBILE.metroPort),
            MOBILE.host,
          ],
          cwd: APP_DIR,
          inheritEnv: resolved.inheritEnv,
          // A cold Metro cache makes the first bundle take four minutes on a hosted runner.
          startupTimeout: 600_000,
          log: ".e2e/mobile-app.log",
        },
      },
    },
  ],
  // One server holds every test's state, and a dev stack is heavy.
  workers: 1,
  timeout: 300_000,
  assertionTimeout: 15_000,
  // e2e retries once under CI unless told otherwise, and a retried pass would hide a race.
  retries: 0,
  video: "on",
  trace: "on",
  reporters: ["list", "markdown"],
} satisfies E2EConfig;
