import { describe, expect, it } from "vite-plus/test";

import { deriveAuthClientMetadata } from "./utils.ts";

describe("deriveAuthClientMetadata", () => {
  it("reads the device and OS from a desktop user agent", () => {
    const metadata = deriveAuthClientMetadata({
      request: {
        headers: {
          "user-agent":
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) onicode/0.0.15 Chrome/136.0.7103.93 Electron/36.3.2 Safari/537.36",
        },
        source: {
          remoteAddress: "::ffff:127.0.0.1",
        },
      } as never,
    });

    expect(metadata).toMatchObject({
      deviceType: "desktop",
      ipAddress: "127.0.0.1",
      os: "macOS",
    });
  });

  it("records the forwarded client address of a request proxied by Tailscale Serve", () => {
    const metadata = deriveAuthClientMetadata({
      request: {
        headers: {
          "x-forwarded-for": "100.64.0.2",
          "x-forwarded-host": "host-a.tailnet-name.ts.net",
          "x-forwarded-proto": "https",
        },
        source: { remoteAddress: "127.0.0.1" },
      } as never,
    });

    expect(metadata.ipAddress).toBe("100.64.0.2");

    const direct = deriveAuthClientMetadata({
      request: {
        headers: { "x-forwarded-for": "100.64.0.2" },
        source: { remoteAddress: "192.168.1.20" },
      } as never,
    });
    expect(direct.ipAddress).toBe("192.168.1.20");
  });

  it("applies client-presented display identity without replacing transport metadata", () => {
    const metadata = deriveAuthClientMetadata({
      request: {
        headers: {
          "user-agent":
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 Chrome/136.0.7103.93 Electron/36.3.2 Safari/537.36",
        },
        source: {
          remoteAddress: "::ffff:192.168.213.72",
        },
      } as never,
      presented: {
        label: "Onicode Mobile",
        deviceType: "mobile",
        os: "iOS",
      },
    });

    expect(metadata).toMatchObject({
      label: "Onicode Mobile",
      deviceType: "mobile",
      ipAddress: "192.168.213.72",
      os: "iOS",
    });
    expect(metadata.userAgent).toContain("Electron/36.3.2");
  });
});
