import { test as base, type Device } from "@e2e-dev/mobile";
import { expect, type TestFixtures } from "e2e";
import * as NodeChildProcess from "node:child_process";
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";
import * as NodeUtil from "node:util";

import { BUNDLE_ID, METRO_PORT, STACK_STATE_FILE, type StackState } from "./stackState.ts";

const run = NodeUtil.promisify(NodeChildProcess.execFile);
const repoRoot = NodePath.resolve(import.meta.dirname, "../../..");
const serverBin = NodePath.join(repoRoot, "apps/server/src/bin.ts");
// A fresh install bundles the whole app from Metro before its first screen.
const FIRST_BUNDLE_TIMEOUT = 240_000;

interface ClaudeThread {
  readonly environmentId: string;
  readonly threadId: string;
}

type Fixtures = TestFixtures & { readonly device: Device };

const onicodeCli = async (stack: StackState, args: ReadonlyArray<string>) => {
  const { stdout } = await run(
    process.execPath,
    [serverBin, ...args, "--base-dir", stack.baseDir],
    {
      cwd: repoRoot,
      env: { ...process.env, ONICODE_PORT: String(stack.serverPort) },
    },
  );
  return stdout;
};

// The cheapest real Claude turn: Claude's settings views read a live session, which only a turn starts.
const startClaudeThread = async (stack: StackState): Promise<ClaudeThread> => {
  const reply = JSON.parse(
    await onicodeCli(stack, [
      "thread",
      "start",
      "--provider",
      "claudeAgent",
      "--model",
      "claude-haiku-5-5",
      "--wait",
      "--json",
      stack.projectDir,
      "Reply with just: ok",
    ]),
  ) as { readonly threadId: string; readonly status: string };
  if (reply.status !== "completed") throw new Error(`The seed turn ended ${reply.status}.`);
  return { environmentId: stack.environmentId, threadId: reply.threadId };
};

const pairApp = async (
  stack: StackState,
  { app, device, platform, screen }: Pick<Fixtures, "app" | "device" | "platform" | "screen">,
) => {
  // The emulator reaches the host's loopback at 10.0.2.2; the simulator shares it.
  const host = platform === "android" ? "10.0.2.2" : "127.0.0.1";
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
  if (platform === "android") {
    // expo-dev-launcher on Android ignores --initialUrl, so the bundle loads from its link.
    // The flags match the iOS launch arguments: no dev menu, onboarding sheet, or floating button.
    const query = new URLSearchParams({
      url: `http://${host}:${METRO_PORT}`,
      disableOnboarding: "1",
      disableAutoLaunch: "1",
      disableFab: "1",
    });
    await device.openLink(`exp+onicode://expo-development-client/?${query}`);
  }
  // The simulator's keychain outlives clearState, so the app may still hold older pairings.
  await expect(screen.getByTestId("adaptive-workspace-layout")).toBeVisible({
    timeout: FIRST_BUNDLE_TIMEOUT,
  });
  const pairing = await onicodeCli(stack, [
    "auth",
    "pairing",
    "create",
    "--base-url",
    `http://${host}:${stack.serverPort}`,
    "--ttl",
    "15m",
    "--label",
    "e2e",
  ]);
  const pairingUrl = /^Pair URL: (.+)$/m.exec(pairing)?.[1];
  if (pairingUrl === undefined) throw new Error("The server printed no pairing URL.");
  const query = new URLSearchParams({ pairingUrl, autoConnect: "1" });
  await device.openLink(`onicode-dev://connections/new?${query}`);
};

// One seeded thread and one pairing per worker: the app keeps both between tests.
let prepared: Promise<ClaudeThread> | undefined;

/** A Claude thread with a live session, opened in a freshly paired app. */
export const test = base.extend<{ claudeThread: ClaudeThread }>({
  claudeThread: async ({ app, device, platform, screen }, use) => {
    prepared ??= (async () => {
      const stack = JSON.parse(NodeFS.readFileSync(STACK_STATE_FILE, "utf8")) as StackState;
      const [thread] = await Promise.all([
        startClaudeThread(stack),
        pairApp(stack, { app, device, platform, screen }),
      ]);
      return thread;
    })();
    const thread = await prepared;
    const link = `onicode-dev://threads/${thread.environmentId}/${encodeURIComponent(thread.threadId)}`;
    // Pairing finishes in the background; the thread opens once the app knows its environment.
    await expect
      .poll(
        async () => {
          await device.openLink(link);
          return screen.getByRole("textbox").isVisible();
        },
        { timeout: 30_000, message: "the paired app never opened the Claude thread" },
      )
      .toBe(true);
    await use(thread);
  },
});
