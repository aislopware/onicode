// @effect-diagnostics nodeBuiltinImport:off - Tests exercise root env file precedence directly.
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";
import { afterEach, describe, expect, it } from "vite-plus/test";

import { loadRepoEnv } from "./public-config.ts";

const temporaryDirectories: string[] = [];

afterEach(() => {
  for (const directory of temporaryDirectories.splice(0)) {
    NodeFS.rmSync(directory, { recursive: true, force: true });
  }
});

describe("loadRepoEnv", () => {
  it("applies process, root local, and root precedence in that order", () => {
    const repoRoot = makeTemporaryDirectory();
    NodeFS.writeFileSync(
      NodePath.join(repoRoot, ".env"),
      "ONICODE_SHARED_VALUE=root\nONICODE_ONLY_ROOT=root\n",
    );
    NodeFS.writeFileSync(NodePath.join(repoRoot, ".env.local"), "ONICODE_SHARED_VALUE=local\n");

    expect(loadRepoEnv({ baseEnv: {}, repoRoot })).toMatchObject({
      ONICODE_SHARED_VALUE: "local",
      ONICODE_ONLY_ROOT: "root",
    });
    expect(
      loadRepoEnv({ baseEnv: { ONICODE_SHARED_VALUE: "process" }, repoRoot }).ONICODE_SHARED_VALUE,
    ).toBe("process");
  });
});

function makeTemporaryDirectory() {
  const directory = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "onicode-public-config-"));
  temporaryDirectories.push(directory);
  return directory;
}
