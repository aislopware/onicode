// @effect-diagnostics nodeBuiltinImport:off -- Record files in a temporary directory; the helper fork is mocked.
import * as NodeCrypto from "node:crypto";
import * as NodeEvents from "node:events";
import * as NodeFSP from "node:fs/promises";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";
import * as NodeProcess from "node:process";

import { expect, it, vi } from "vite-plus/test";

const fork = vi.hoisted(() => vi.fn());
vi.mock("node:child_process", async (original) => ({
  ...(await original<typeof import("node:child_process")>()),
  fork,
}));

import * as DesktopBackendRecoveryProcess from "./DesktopBackendRecoveryProcess.ts";

it.each([
  { records: "none", forks: false },
  { records: "another home's", forks: false },
  { records: "this home's", forks: true },
] as const)(
  "starts the recovery helper with $records records: $forks",
  async ({ records, forks }) => {
    fork.mockReset();
    fork.mockImplementation(() => {
      const worker = Object.assign(new NodeEvents.EventEmitter(), { kill: vi.fn() });
      NodeProcess.nextTick(() => worker.emit("exit", 0));
      return worker;
    });
    const appDataDirectory = await NodeFSP.realpath(
      await NodeFSP.mkdtemp(NodePath.join(NodeOS.tmpdir(), "oni-recovery-process-")),
    );
    try {
      const stateDir = NodePath.join(appDataDirectory, "home");
      await NodeFSP.mkdir(stateDir);
      if (records !== "none") {
        const home =
          records === "this home's" ? stateDir : NodePath.join(appDataDirectory, "other");
        const prefix = NodeCrypto.createHash("sha256")
          .update(JSON.stringify([home, await NodeFSP.realpath(NodeProcess.execPath)]))
          .digest("hex");
        const directory = NodePath.join(appDataDirectory, "onicode-backend-recovery");
        await NodeFSP.mkdir(directory, { mode: 0o700 });
        await NodeFSP.writeFile(
          NodePath.join(directory, `${prefix}.${NodeCrypto.randomUUID()}.json`),
          "{}",
        );
      }
      const onError = vi.fn();
      await DesktopBackendRecoveryProcess.make({
        appDataDirectory,
        stateDir,
        onRecovery: () => {},
        onError,
      }).recover(new AbortController().signal);
      expect(fork).toHaveBeenCalledTimes(forks ? 1 : 0);
      expect(onError).not.toHaveBeenCalled();
    } finally {
      await NodeFSP.rm(appDataDirectory, { recursive: true, force: true });
    }
  },
);
