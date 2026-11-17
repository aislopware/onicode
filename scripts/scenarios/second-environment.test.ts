// @effect-diagnostics nodeBuiltinImport:off - runs a failing server fixture in an isolated directory.
import * as NodeFSP from "node:fs/promises";
import * as NodePath from "node:path";
import { expect, it, vi } from "vite-plus/test";

const root = await vi.hoisted(async () => {
  const fs = await import("node:fs/promises");
  const os = await import("node:os");
  const path = await import("node:path");
  return fs.mkdtemp(path.join(os.tmpdir(), "second-environment-exit-"));
});

vi.mock("../e2e.config.ts", () => ({
  APP_DIR: root,
  ONICODE_HOME: NodePath.join(root, "home"),
}));

import { startSecondEnvironment } from "./second-environment.ts";

it("rejects readiness when the second server exits before printing a token", async () => {
  await NodeFSP.mkdir(NodePath.join(root, "apps/server/src"), { recursive: true });
  await NodeFSP.writeFile(NodePath.join(root, "apps/server/src/bin.ts"), "process.exit(1);\n");
  try {
    await expect(
      startSecondEnvironment(NodePath.join(root, "scenarios-second")),
    ).rejects.toMatchObject({
      message: "The second server exited with code 1 at startup.",
    });
  } finally {
    await NodeFSP.rm(root, { recursive: true, force: true });
  }
});
