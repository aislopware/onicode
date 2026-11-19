import {
  DEFAULT_SERVER_SETTINGS,
  DEFAULT_UNIFIED_SETTINGS,
  EnvironmentId,
  ProviderDriverKind,
  ProviderInstanceId,
  type ServerProvider,
} from "@onicode/contracts";
import { resolveProjectSettings } from "@onicode/shared/projectSettings";
import { describe, expect, it } from "vite-plus/test";

import { deriveProviderInstanceEntries } from "../../providerInstances";
import { createScopedModelDisabledReason } from "./scopedModelAvailability.logic";

const environmentId = EnvironmentId.make("laptop");

const claude = (input: {
  readonly models: ServerProvider["models"];
  readonly availability?: ServerProvider["availability"];
}): ServerProvider => ({
  instanceId: ProviderInstanceId.make("claudeAgent"),
  driver: ProviderDriverKind.make("claudeAgent"),
  enabled: true,
  installed: true,
  version: null,
  status: "ready",
  ...(input.availability ? { availability: input.availability } : {}),
  auth: { status: "authenticated" },
  checkedAt: "2026-01-01T00:00:00.000Z",
  models: input.models,
  slashCommands: [],
  skills: [],
});

const disabledReasonOn = (providers: ReadonlyArray<ServerProvider>) =>
  createScopedModelDisabledReason({
    settings: DEFAULT_UNIFIED_SETTINGS,
    entries: deriveProviderInstanceEntries(providers),
    targets: [
      {
        environmentId,
        label: "Laptop",
        projectId: null,
        settings: DEFAULT_SERVER_SETTINGS,
        sources: resolveProjectSettings(DEFAULT_SERVER_SETTINGS, null).sources,
      },
    ],
    environments: [{ environmentId, label: "Laptop", serverConfig: {} }],
    providersByEnvironment: new Map([[environmentId, providers]]),
    textGeneration: false,
    unavailableMessage: (environment) => `unavailable on ${environment}`,
  });

describe("createScopedModelDisabledReason", () => {
  it("reads each environment's catalog once, not once per model asked about", () => {
    const catalog = Array.from({ length: 50 }, (_, index) => ({
      slug: `model-${index}`,
      name: `Model ${index}`,
      isCustom: false,
      capabilities: {},
    }));
    let reads = 0;
    const models = new Proxy(catalog, {
      get(target, key, receiver) {
        if (typeof key === "string" && /^\d+$/.test(key)) reads += 1;
        return Reflect.get(target, key, receiver);
      },
    });
    const provider = claude({ models });
    const reason = disabledReasonOn([provider]);

    expect(reason(provider.instanceId, "model-0")).toBeNull();
    const readsForFirst = reads;
    for (const model of catalog) {
      expect(reason(provider.instanceId, model.slug)).toBeNull();
    }
    expect(reason(provider.instanceId, "missing")).toBe("unavailable on Laptop");
    expect(reads).toBe(readsForFirst);
  });

  it("judges a repeated instance id by its first snapshot", () => {
    const opus = { slug: "opus", name: "Opus", isCustom: false, capabilities: {} };
    const reason = disabledReasonOn([
      claude({ models: [opus], availability: "unavailable" }),
      claude({ models: [opus] }),
    ]);

    expect(reason(ProviderInstanceId.make("claudeAgent"), "opus")).toBe("unavailable on Laptop");
  });
});
