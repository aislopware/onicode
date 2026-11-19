import { describe, expect, it } from "vite-plus/test";

import { loopbackUrlForDevice, tunnelReaches } from "./browserTabs";

describe("loopbackUrlForDevice", () => {
  it("opens only the environment's loopback pages on this device", () => {
    expect(loopbackUrlForDevice("http://localhost:5173/app?x=1")).toBe(
      "http://localhost:5173/app?x=1",
    );
    expect(loopbackUrlForDevice("http://127.0.0.1:3000/")).toBe("http://127.0.0.1:3000/");
    expect(loopbackUrlForDevice("http://[::1]:3000/")).toBe("http://[::1]:3000/");
    // The unspecified address would name the phone's own interfaces.
    expect(loopbackUrlForDevice("http://0.0.0.0:3000/")).toBe("http://localhost:3000/");
    expect(loopbackUrlForDevice("http://[::]:3000/")).toBe("http://localhost:3000/");
    // The names the native tunnels carry, like the desktop app's.
    expect(loopbackUrlForDevice("http://app.localhost:5173/")).toBe("http://app.localhost:5173/");
    expect(loopbackUrlForDevice("http://localhost.:5173/")).toBe("http://localhost.:5173/");
    expect(loopbackUrlForDevice("http://127.0.0.2:3000/")).toBe("http://127.0.0.2:3000/");
    expect(loopbackUrlForDevice("http://localhost..:5173/")).toBeNull();
    expect(loopbackUrlForDevice("http://localhost.example.com/")).toBeNull();
    expect(loopbackUrlForDevice("http://notlocalhost/")).toBeNull();
    expect(loopbackUrlForDevice("https://example.com/")).toBeNull();
    expect(loopbackUrlForDevice("http://192.168.1.5:3000/")).toBeNull();
    expect(loopbackUrlForDevice("file:///tmp/a.html")).toBeNull();
    expect(loopbackUrlForDevice("")).toBeNull();
  });
});

describe("tunnelReaches", () => {
  it("reaches only the environment's localhost on iOS and any loopback address on Android", () => {
    for (const url of [
      "http://localhost:3000/",
      "http://app.localhost:3000/",
      "http://127.0.0.1:3000/",
      "http://0.0.0.0:3000/",
    ]) {
      expect(tunnelReaches(url, "ios")).toBe(true);
      expect(tunnelReaches(url, "android")).toBe(true);
    }
    // iOS dials the environment's localhost, which may be another server than this address.
    expect(tunnelReaches("http://127.0.0.2:3000/", "ios")).toBe(false);
    expect(tunnelReaches("http://127.0.0.2:3000/", "android")).toBe(true);
    expect(tunnelReaches("http://[::1]:3000/", "ios")).toBe(false);
    expect(tunnelReaches("http://[::1]:3000/", "android")).toBe(true);
    expect(tunnelReaches("https://example.com/", "android")).toBe(false);
  });
});
