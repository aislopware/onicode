import { mobile } from "@e2e-dev/mobile";
import type { E2EConfig } from "e2e";

import { BUNDLE_ID, METRO_PORT } from "./e2e/stackState.ts";

// e2e/stack.ts starts an isolated Onicode server and Metro; tests pair the dev client with it.
// The dev client must already be installed: `vp run mobile:install <ios|android>`.
const app = {
  bundleId: BUNDLE_ID,
  command: {
    executable: process.execPath,
    args: ["e2e/stack.ts"],
    // Claude's credentials and the Android SDK come from the developer's environment.
    inheritEnv: ["CLAUDE_*", "ANTHROPIC_*", "ANDROID_*", "JAVA_HOME"],
    startupTimeout: 180_000,
    log: ".e2e/logs/stack.log",
  },
  readyUrl: `http://127.0.0.1:${METRO_PORT}/status`,
};

export default {
  tests: ["e2e/**/*.e2e.ts"],
  targets: [
    {
      name: "ios",
      // Name a device when several are booted: E2E_IOS_DEVICE="iPhone 17", E2E_ANDROID_DEVICE=emulator-5554.
      engine: mobile({ platform: "ios", device: process.env.E2E_IOS_DEVICE }),
      app: {
        ...app,
        launchArguments: [
          "--initialUrl",
          `http://127.0.0.1:${METRO_PORT}`,
          "-EXDevMenuShowsAtLaunch",
          "NO",
          "-EXDevMenuIsOnboardingFinished",
          "YES",
          "-EXDevMenuShowFloatingActionButton",
          "NO",
        ],
      },
    },
    {
      name: "android",
      engine: mobile({ platform: "android", device: process.env.E2E_ANDROID_DEVICE }),
      app,
    },
  ],
  workers: 1,
  // A flaky device test gets fixed or quarantined with an issue, never retried into green.
  retries: 0,
  // The first launch waits for Metro to bundle the app and the seed Claude turn.
  timeout: 300_000,
} satisfies E2EConfig;
