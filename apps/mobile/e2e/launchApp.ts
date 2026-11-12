import type { Device } from "@e2e-dev/mobile";
import { expect, type TestFixtures } from "e2e";
import * as NodeChildProcess from "node:child_process";
import * as NodeUtil from "node:util";

import { BUNDLE_ID, METRO_PORT } from "./stackState.ts";

const run = NodeUtil.promisify(NodeChildProcess.execFile);
// A fresh install bundles the whole app from Metro before its first screen.
const FIRST_BUNDLE_TIMEOUT = 240_000;

export type LaunchFixtures = Pick<TestFixtures, "app" | "platform" | "screen"> & {
  readonly device: Device;
};

/** The emulator reaches the host's loopback at 10.0.2.2; the simulator shares it. */
export const hostAddress = (platform: LaunchFixtures["platform"]) =>
  platform === "android" ? "10.0.2.2" : "127.0.0.1";

/** Waits for the workspace after `app.open()` or `app.restart()` launched the dev client. */
export const loadBundle = async ({ device, platform, screen }: LaunchFixtures) => {
  if (platform === "android") {
    // expo-dev-launcher on Android ignores --initialUrl, so the bundle loads from its link.
    // The flags match the iOS launch arguments: no dev menu, onboarding sheet, or floating button.
    const query = new URLSearchParams({
      url: `http://${hostAddress(platform)}:${METRO_PORT}`,
      disableOnboarding: "1",
      disableAutoLaunch: "1",
      disableFab: "1",
    });
    await device.openLink(`exp+onicode://expo-development-client/?${query}`);
  }
  await expect(screen.getByTestId("adaptive-workspace-layout")).toBeVisible({
    timeout: FIRST_BUNDLE_TIMEOUT,
  });
};

/** Launches the dev client from cleared state. */
export const launchApp = async (fixtures: LaunchFixtures) => {
  const { app, device, platform } = fixtures;
  await app.clearState();
  if (platform === "android") {
    // The dev launcher asks for local network access before it reaches Metro, and
    // e2e's permission names do not cover it.
    await run("adb", [
      "-s",
      device.unstable_raw.deviceId,
      "shell",
      "pm",
      "grant",
      BUNDLE_ID,
      "android.permission.ACCESS_LOCAL_NETWORK",
    ]);
  }
  await app.open();
  await loadBundle(fixtures);
};
