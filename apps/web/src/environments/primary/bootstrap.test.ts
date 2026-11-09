import { afterEach, describe, expect, it, vi } from "vite-plus/test";

import { readPrimaryEnvironmentTarget, resolvePrimaryEnvironmentHttpUrl } from ".";

function installDesktopBridge(bootstrap: { httpBaseUrl?: string; wsBaseUrl?: string }) {
  vi.stubGlobal("window", {
    location: new URL("onicode://app/"),
    desktopBridge: {
      getLocalEnvironmentEnabled: () => true,
      getLocalEnvironmentBootstraps: () => [{ id: "primary", label: "Local", ...bootstrap }],
    },
  });
}

describe("primary environment target", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("resolves requests against the desktop-managed server", () => {
    installDesktopBridge({
      httpBaseUrl: "http://127.0.0.1:3773",
      wsBaseUrl: "ws://127.0.0.1:3773",
    });

    expect(resolvePrimaryEnvironmentHttpUrl("/.well-known/onicode/environment")).toBe(
      "http://127.0.0.1:3773/.well-known/onicode/environment",
    );
  });

  it("has no target before the desktop app has started its server", () => {
    installDesktopBridge({});

    expect(readPrimaryEnvironmentTarget()).toBeNull();
  });

  it("refuses a bootstrap that names only one of its urls", () => {
    installDesktopBridge({ httpBaseUrl: "http://127.0.0.1:3773" });

    expect(() => readPrimaryEnvironmentTarget()).toThrow(/missing wsBaseUrl/);
  });
});
