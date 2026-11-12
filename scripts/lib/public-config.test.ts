// @effect-diagnostics nodeBuiltinImport:off - Tests exercise root env file precedence directly.
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";
import { afterEach, describe, expect, it } from "vite-plus/test";

import { loadRepoEnv, resolveMobileIdentity } from "./public-config.ts";

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

describe("resolveMobileIdentity", () => {
  it("builds in no push gateway or Firebase project, even for the maintainers' app", () => {
    expect(resolveMobileIdentity({})).toEqual({
      own: false,
      appId: "dev.aislopware.onicode",
      appleTeamId: "UK58J62H8L",
      androidGoogleServicesFile: undefined,
      pushGatewayUrl: undefined,
    });
  });

  it("does not inherit the maintainers' Apple team under another app id", () => {
    expect(resolveMobileIdentity({ ONICODE_MOBILE_APP_ID: "com.example.onicode" })).toMatchObject({
      own: true,
      appId: "com.example.onicode",
      appleTeamId: undefined,
    });
    expect(
      resolveMobileIdentity({
        ONICODE_MOBILE_APP_ID: "com.example.onicode",
        ONICODE_APPLE_TEAM_ID: "ABCDE12345",
        ONICODE_ANDROID_GOOGLE_SERVICES_FILE: "/keys/google-services.json",
        ONICODE_PUSH_GATEWAY_URL: " https://push.example.com ",
      }),
    ).toEqual({
      own: true,
      appId: "com.example.onicode",
      appleTeamId: "ABCDE12345",
      androidGoogleServicesFile: "/keys/google-services.json",
      pushGatewayUrl: "https://push.example.com",
    });
  });

  it("refuses an id that is not valid on both platforms", () => {
    expect(() => resolveMobileIdentity({ ONICODE_MOBILE_APP_ID: "com.example.my-app" })).toThrow(
      "ONICODE_MOBILE_APP_ID",
    );
  });
});

function makeTemporaryDirectory() {
  const directory = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "onicode-public-config-"));
  temporaryDirectories.push(directory);
  return directory;
}
