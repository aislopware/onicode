import { ProviderDriverKind, ProviderInstanceId, type ServerProvider } from "@onicode/contracts";
import { describe, expect, it } from "vite-plus/test";

import { deriveProviderInstanceEntries } from "../../providerInstances";
import {
  modelPickerOptionsForInstance,
  retainedModelWhileProviderLocked,
} from "./ProviderModelPicker.logic";

const claude = ProviderDriverKind.make("claudeAgent");
const opus = { slug: "claude-opus-5-5", name: "Claude Opus 5.5", shortName: "Opus 5.5" };
const sonnet = { slug: "claude-sonnet-5-5", name: "Claude Sonnet 5.5", shortName: "Sonnet 5.5" };
const retained = (enabled: boolean, isAvailable: boolean, models = [sonnet, opus]) =>
  retainedModelWhileProviderLocked({ driverKind: claude, enabled, isAvailable, models }, opus.slug);

describe("retainedModelWhileProviderLocked", () => {
  it("shows a hidden saved model while its provider cannot run", () => {
    expect(retained(false, true)).toBe(opus);
    expect(retained(true, false)).toBe(opus);
    expect(retained(false, true, [sonnet])).toEqual({ slug: opus.slug, name: opus.slug });
    expect(retained(true, true)).toBeUndefined();
    expect(retainedModelWhileProviderLocked(null, opus.slug)).toBeUndefined();
  });
});

describe("modelPickerOptionsForInstance", () => {
  it("reads a warning provider's catalog once, not once per model", () => {
    const backendModel = (slug: string, subProvider: string) => ({
      slug,
      name: slug,
      subProvider,
      isCustom: false,
      capabilities: {},
    });
    // The usable backend's models come last, so each readiness check scans the whole catalog.
    const catalog = [
      ...Array.from({ length: 50 }, (_, index) => backendModel(`codex/${index}`, "Codex")),
      backendModel("gateway/model", "Gateway"),
    ];
    let reads = 0;
    const models = new Proxy(catalog, {
      get(target, key, receiver) {
        if (typeof key === "string" && /^\d+$/.test(key)) reads += 1;
        return Reflect.get(target, key, receiver);
      },
    });
    const snapshot: ServerProvider = {
      instanceId: ProviderInstanceId.make("fx"),
      driver: ProviderDriverKind.make("fx"),
      enabled: true,
      installed: true,
      version: null,
      status: "warning",
      auth: { status: "authenticated" },
      checkedAt: "2026-01-01T00:00:00.000Z",
      models,
      subProviders: [{ name: "Gateway" }, { name: "Codex", unavailableReason: "Signed out." }],
      slashCommands: [],
      skills: [],
    };
    const [entry] = deriveProviderInstanceEntries([snapshot]);
    if (!entry) throw new Error("no entry");

    const options = modelPickerOptionsForInstance({
      entry,
      options: catalog,
      activeInstanceId: entry.instanceId,
      activeModel: "",
    });

    expect(options).toHaveLength(catalog.length);
    expect(reads).toBeLessThanOrEqual(catalog.length);
  });
});
