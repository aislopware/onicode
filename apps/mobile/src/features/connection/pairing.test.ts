import { describe, expect, it } from "vite-plus/test";

import {
  buildPairingUrl,
  extractPairingUrlFromQrPayload,
  pairingFormLinkPath,
  parsePairingUrl,
} from "./pairing";

describe("buildPairingUrl", () => {
  it("uses HTTP for a schemeless IP address", () => {
    expect(buildPairingUrl("192.168.1.100:3773", "pairing-token")).toBe(
      "http://192.168.1.100:3773/#token=pairing-token",
    );
  });

  it("keeps HTTPS as the default for a schemeless hostname", () => {
    expect(buildPairingUrl("remote.example.com", "pairing-token")).toBe(
      "https://remote.example.com/#token=pairing-token",
    );
  });

  it("preserves an explicit scheme for an IP address", () => {
    expect(buildPairingUrl("https://192.168.1.100:3773", "pairing-token")).toBe(
      "https://192.168.1.100:3773/#token=pairing-token",
    );
  });
});

describe("extractPairingUrlFromQrPayload", () => {
  it("unwraps mobile deep links that carry an encoded pairing url", () => {
    expect(
      extractPairingUrlFromQrPayload(
        "onicode://pair?pairingUrl=https%3A%2F%2Fremote.example.com%2Fpair%23token%3Dpairing-token",
      ),
    ).toBe("https://remote.example.com/pair#token=pairing-token");
  });
});

describe("pairingFormLinkPath", () => {
  it.each([
    [
      "pair?pairingUrl=http%3A%2F%2F100.64.0.2%3A3773",
      "connections/new?pairingUrl=http%3A%2F%2F100.64.0.2%3A3773",
    ],
    ["/pair/?pairingUrl=x", "connections/new?pairingUrl=x"],
    ["pairing?x=1", "pairing?x=1"],
    ["settings/pair", "settings/pair"],
  ])("maps %s to %s", (path, expected) => {
    expect(pairingFormLinkPath(path)).toBe(expected);
  });
});

describe("parsePairingUrl", () => {
  it("reads a server pairing link into backend host fields", () => {
    expect(parsePairingUrl("https://desktop.tailnet.ts.net/pair#token=pairing-token")).toEqual({
      host: "https://desktop.tailnet.ts.net",
      code: "pairing-token",
    });
  });
});
