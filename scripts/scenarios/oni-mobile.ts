// `test` for iOS scenarios: e2e's mobile test plus `onicode`, which seeds state like the web one and
// opens the dev client paired with the scenario's server.
import { test as base } from "@e2e-dev/mobile";
import * as HostProcess from "@onicode/shared/HostProcess";
import { expect } from "e2e";
import * as Effect from "effect/Effect";
import * as Path from "effect/Path";
import * as Schema from "effect/Schema";

import { APP_DIR, MOBILE, MOBILE_ONI_HOME } from "../e2e.config.ts";
import { run, ScenarioError } from "./shell.ts";
import { makeState, runPromise, type OniState } from "./onicode.ts";

const APP_ID = "dev.aislopware.onicode.dev";
const SCHEME = "onicode-dev";

export interface MobileOni extends OniState {
  /** Installs the dev client if the device lacks it, loads it from Metro, and pairs it. */
  readonly open: () => Promise<void>;
}

const decodePrepared = Schema.decodeUnknownEffect(
  Schema.fromJsonString(Schema.Struct({ fingerprint: Schema.String })),
);

/** The dev client built for this checkout's native code, from the shared cache. */
const devClientPath = Effect.gen(function* () {
  const path = yield* Path.Path;
  const prepared = yield* run(
    yield* HostProcess.ExecutablePath,
    ["scripts/mobile-native-client.ts", "ensure", "ios", "--prepare"],
    { cwd: APP_DIR },
  );
  const { fingerprint } = yield* decodePrepared(prepared.stdout.trim().split("\n").at(-1) ?? "");
  return path.join(
    yield* HostProcess.HomeDirectory,
    ".cache/onicode/native-clients/artifacts/ios/development",
    fingerprint,
    "OnicodeDev.app",
  );
});

// The dev launcher gives Metro 10 seconds for the manifest and tries a loopback address once.
// Metro starts a process per manifest to resolve the runtime version, which a hosted runner
// still busy starting the server does not always finish in time. The launcher then stays on
// its error screen, so the load is tried again.
const LOAD_ATTEMPTS = 3;

/** Waits, within limits, until Metro answers for the manifest well inside the launcher's 10 s. */
const metroAnswersInTime = async (origin: string) => {
  for (let attempt = 0; attempt < 10; attempt += 1) {
    // @effect-diagnostics-next-line globalFetch:off - Metro's own HTTP endpoint, asked as the dev launcher asks it
    const answered = await fetch(origin, {
      headers: { "expo-platform": "ios", accept: "application/expo+json,application/json" },
      signal: AbortSignal.timeout(4_000),
    }).then(
      (response) => response.ok,
      () => false,
    );
    if (answered) return;
  }
};

// The simulator keeps an app's data between runs, and an earlier run's server is gone. The first
// install of a run removes the app first, so one install and pairing last the run.
let installed = false;
let paired = false;

export const test = base.extend<{ onicode: MobileOni }>({
  onicode: async ({ device, screen }, use) => {
    const { state, cli, cleanup } = makeState(MOBILE_ONI_HOME, {
      ONICODE_PORT: String(MOBILE.serverPort),
    });
    const pairingUrl = Effect.gen(function* () {
      const origin = `http://${MOBILE.host}:${MOBILE.serverPort}`;
      const pairing = ["auth", "pairing", "create", "--ttl", "15m", "--label", "scenario"];
      const created = yield* cli([...pairing, "--base-url", origin]);
      const url = /Pair URL:\s*(\S+)/.exec(`${created.stdout}\n${created.stderr}`)?.[1];
      return url ?? (yield* new ScenarioError({ message: "The server printed no pairing URL." }));
    });
    await use({
      ...state,
      open: async () => {
        if (!installed) {
          await device.installApp(await runPromise(devClientPath), { reinstall: true });
          installed = true;
        }
        // iOS asks before a link opens another app.
        const openLink = async (url: string) => {
          await device.openLink(url, { app: APP_ID });
          await device.alert("accept").catch(() => {});
        };
        const metroOrigin = `http://${MOBILE.host}:${MOBILE.metroPort}`;
        const load = async () => {
          await metroAnswersInTime(metroOrigin);
          await openLink(
            `${SCHEME}://expo-development-client/?url=${encodeURIComponent(metroOrigin)}&disableOnboarding=1&disableFab=1&disableAutoLaunch=1`,
          );
        };
        await load();
        // The first load bundles the app; the pairing link only works once it is on screen.
        if (paired) return;
        // Not the empty state's "Add environment": the simulator's keychain outlives the app, so
        // environments an earlier run saved can still be listed.
        const workspace = screen.getByTestId("adaptive-workspace-layout");
        const loadFailed = screen.getByText("Failed to load app", { exact: false });
        for (let attempt = 1; attempt < LOAD_ATTEMPTS; attempt += 1) {
          await expect(workspace.or(loadFailed)).toBeVisible({ timeout: 240_000 });
          if (await workspace.isVisible()) break;
          await load();
        }
        await expect(workspace).toBeVisible({ timeout: 240_000 });
        const query = new URLSearchParams({
          pairingUrl: await runPromise(pairingUrl),
          autoConnect: "1",
        });
        await openLink(`${SCHEME}://connections/new?${query.toString()}`);
        paired = true;
      },
    });
    await runPromise(cleanup);
  },
});

/** Test option for an iOS scenario: the desktop target skips it. */
export const IOS = { platforms: ["ios"] } as const;
