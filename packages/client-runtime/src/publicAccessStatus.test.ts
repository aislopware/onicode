import type { ManagedPolicySummary } from "@onicode/contracts";
import { expect, it } from "vite-plus/test";

import { publicAccessSwitchDenial } from "./publicAccessStatus.ts";

const policy = (summary: Partial<ManagedPolicySummary>): ManagedPolicySummary => ({
  organization: "Example Corp",
  lockedSettings: [],
  ...summary,
});

it("locks a public access switch either way, and lets a refused capability only turn it off", () => {
  const locked = policy({ lockedSettings: ["publicAgentsEnabled"] });
  for (const on of [true, false]) {
    expect(publicAccessSwitchDenial(locked, "publicAgentsEnabled", on)).toBe(
      "Managed by Example Corp.",
    );
    expect(publicAccessSwitchDenial(locked, "publicWebhooksEnabled", on)).toBeUndefined();
  }
  const noWebhooks = policy({ limits: { webhookTasks: false } });
  expect(publicAccessSwitchDenial(noWebhooks, "publicWebhooksEnabled", false)).toBe(
    "Example Corp's policy does not allow webhook tasks.",
  );
  expect(publicAccessSwitchDenial(noWebhooks, "publicWebhooksEnabled", true)).toBeUndefined();
  expect(publicAccessSwitchDenial(noWebhooks, "publicAgentsEnabled", false)).toBeUndefined();
  const localOnly = policy({ limits: { clients: ["local"] } });
  expect(publicAccessSwitchDenial(localOnly, "publicAgentsEnabled", false)).toBe(
    "Example Corp's policy does not allow connecting from other devices.",
  );
  expect(publicAccessSwitchDenial(undefined, "publicAgentsEnabled", false)).toBeUndefined();
});
