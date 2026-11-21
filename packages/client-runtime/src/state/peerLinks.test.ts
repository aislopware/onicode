import { EnvironmentId, type PeerLinkSummary, type ServerConfig } from "@onicode/contracts";
import * as DateTime from "effect/DateTime";
import { describe, expect, it } from "vite-plus/test";

import { linkThroughRoutes } from "./peerLinks.ts";

const now = DateTime.makeUnsafe("2026-11-20T00:00:00.000Z");
const id = (value: string) => EnvironmentId.make(value);

const link = (target: string, overrides: Partial<PeerLinkSummary> = {}): PeerLinkSummary => ({
  environmentId: id(target),
  label: target,
  urls: [`https://${target}.example`],
  access: "full-access",
  status: "reachable",
  linkedAt: now,
  lastReachedAt: now,
  lastError: null,
  expiresAt: DateTime.makeUnsafe("2026-12-20T00:00:00.000Z"),
  ...overrides,
});

const config = (linkPairingForLink: boolean) =>
  ({
    environment: { capabilities: { peerLinks: true, linkPairingForLink } },
    settings: { linkedEnvironmentsEnabled: true },
  }) as unknown as ServerConfig;

describe("linkThroughRoutes", () => {
  it("offers only machines another environment reaches over a full-access link that answers", () => {
    const routes = linkThroughRoutes({
      sourceEnvironmentId: id("source"),
      hosts: [
        { environmentId: id("source"), config: config(true), links: [link("own")] },
        // A version that cannot get a code for a link leaves the route to one that can.
        { environmentId: id("old"), config: config(false), links: [link("open"), link("legacy")] },
        {
          environmentId: id("via"),
          config: config(true),
          links: [
            link("open"),
            link("source"),
            link("supervised", { access: "approval-required" }),
            link("down", { status: "unreachable" }),
            link("lapsed", { expiresAt: now }),
          ],
        },
        { environmentId: id("later"), config: config(true), links: [link("open")] },
      ],
      now,
    });
    expect([...routes.values()]).toEqual([
      {
        viaEnvironmentId: id("via"),
        environmentId: id("open"),
        label: "open",
        urls: ["https://open.example"],
      },
    ]);
  });
});
