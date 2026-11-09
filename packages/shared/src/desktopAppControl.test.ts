import { describe, expect, it } from "vite-plus/test";

import { resolveDesktopAppControlAddress } from "./desktopAppControl.ts";

describe("resolveDesktopAppControlAddress", () => {
  it.each([
    ["linux", "/tmp", "/tmp/onicode-1000", 107],
    ["linux", "/var/tmp/custom", "/var/tmp/custom/onicode-1000", 107],
    ["linux", `/tmp/${"x".repeat(120)}`, "/tmp/onicode-1000", 107],
    ["darwin", `/tmp/${"é".repeat(30)}`, "/tmp/onicode-1000", 103],
    ["darwin", `/tmp/${"x".repeat(55)}`, `/tmp/${"x".repeat(55)}/onicode-1000`, 103],
  ] as const)("keeps %s sockets short under %s", (platform, tempDir, directory, maxBytes) => {
    const first = resolveDesktopAppControlAddress({
      stateDir: `/home/user/${"long/".repeat(40)}userdata`,
      platform,
      tempDir,
      userId: 1000,
      joinPath: (...segments) => segments.join("/"),
    });
    const second = resolveDesktopAppControlAddress({
      stateDir: "/home/user/.onicode/other/userdata",
      platform,
      tempDir,
      userId: 1000,
      joinPath: (...segments) => segments.join("/"),
    });

    expect(new TextEncoder().encode(first.address).byteLength).toBeLessThanOrEqual(maxBytes);
    expect(first.directory).toBe(directory);
    expect(first.address).not.toBe(second.address);
  });

  it("uses a Windows named pipe", () => {
    const result = resolveDesktopAppControlAddress({
      stateDir: "C:\\Users\\user\\.onicode\\userdata",
      platform: "win32",
      tempDir: "C:\\Temp",
      userId: undefined,
      joinPath: (...segments) => segments.join("\\"),
    });

    expect(result.directory).toBeNull();
    expect(result.address).toMatch(/^\\\\\.\\pipe\\onicode-app-[a-f0-9]{24}$/);
  });
});
