// The device suite's app command: an Onicode server on a throwaway home with a scratch
// project, then Metro. The runner waits on Metro's /status and stops this process on exit.
import * as NodeChildProcess from "node:child_process";
import * as NodeFS from "node:fs";
import * as NodeNet from "node:net";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";

import { METRO_PORT, STACK_STATE_FILE, type StackState } from "./stackState.ts";

const mobileDir = NodePath.resolve(import.meta.dirname, "..");
const repoRoot = NodePath.resolve(mobileDir, "../..");
const root = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "onicode-e2e-"));
const baseDir = NodePath.join(root, "home");
const projectDir = NodePath.join(root, "e2e-scratch");
const children: Array<NodeChildProcess.ChildProcess> = [];
let stopping = false;

const stop = () => {
  stopping = true;
  for (const child of children) child.kill("SIGTERM");
  NodeFS.rmSync(root, { recursive: true, force: true });
  NodeFS.rmSync(STACK_STATE_FILE, { force: true });
};
process.on("exit", stop);
for (const signal of ["SIGINT", "SIGTERM"] as const) process.on(signal, () => process.exit(0));

const freePort = () =>
  new Promise<number>((resolve, reject) => {
    const server = NodeNet.createServer();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      server.close(() =>
        typeof address === "object" && address !== null
          ? resolve(address.port)
          : reject(new Error("No free port.")),
      );
    });
  });

const start = (
  command: string,
  args: ReadonlyArray<string>,
  options: { cwd: string; env?: Record<string, string> },
) => {
  const child = NodeChildProcess.spawn(command, args, {
    cwd: options.cwd,
    env: { ...process.env, ...options.env },
    stdio: "inherit",
  });
  child.once("exit", (code) => {
    // Either process dying takes the stack down, so the run fails instead of hanging.
    if (!stopping) process.exit(code ?? 1);
  });
  children.push(child);
  return child;
};

const waitForEnvironment = async (serverPort: number) => {
  const deadline = Date.now() + 60_000;
  while (Date.now() < deadline) {
    const response = await fetch(
      `http://127.0.0.1:${serverPort}/.well-known/onicode/environment`,
    ).catch(() => null);
    if (response?.ok) {
      const body = (await response.json()) as { readonly environmentId?: unknown };
      if (typeof body.environmentId === "string") return body.environmentId;
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  throw new Error("The Onicode server did not start within 60 seconds.");
};

NodeFS.mkdirSync(projectDir, { recursive: true });
NodeFS.writeFileSync(NodePath.join(projectDir, "CLAUDE.md"), "# E2E scratch\nReply briefly.\n");
for (const args of [
  ["init", "-q"],
  [
    "-c",
    "user.name=e2e",
    "-c",
    "user.email=e2e@onicode.invalid",
    "commit",
    "-q",
    "--allow-empty",
    "-m",
    "init",
  ],
]) {
  NodeChildProcess.spawnSync("git", args, { cwd: projectDir, stdio: "inherit" });
}

const serverPort = await freePort();
start(
  process.execPath,
  [
    NodePath.join(repoRoot, "apps/server/src/bin.ts"),
    "--dev",
    "--base-dir",
    baseDir,
    "--port",
    String(serverPort),
    "--log-level",
    "warn",
    projectDir,
  ],
  { cwd: repoRoot },
);
const environmentId = await waitForEnvironment(serverPort);
NodeFS.mkdirSync(NodePath.dirname(STACK_STATE_FILE), { recursive: true });
NodeFS.writeFileSync(
  STACK_STATE_FILE,
  JSON.stringify({ baseDir, projectDir, serverPort, environmentId } satisfies StackState),
);

start(
  NodePath.join(mobileDir, "node_modules/.bin/expo"),
  ["start", "--dev-client", "--scheme", "onicode-dev", "--port", String(METRO_PORT)],
  { cwd: mobileDir, env: { APP_VARIANT: "development", CI: "1", EXPO_NO_TELEMETRY: "1" } },
);
