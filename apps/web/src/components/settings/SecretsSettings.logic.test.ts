import { ServerSettings } from "@onicode/contracts";
import * as Schema from "effect/Schema";
import { expect, it } from "vite-plus/test";

import { secretProviderUses, secretsSettingsState } from "./SecretsSettings.logic";

const personal = (reference: string) => ({ provider: "personal", reference });
const work = { provider: "work", reference: "op://Work/token" };

const settings = Schema.decodeSync(ServerSettings)({
  providerInstances: {
    antigravity_work: {
      driver: "antigravity",
      displayName: "Antigravity Work",
      environment: [
        { name: "API_TOKEN", value: "", secret: personal("op://a/env") },
        { name: "OTHER_TOKEN", value: "", secret: work },
      ],
    },
    codex_plain: {
      driver: "codex",
      environment: [{ name: "CODEX_TOKEN", value: "", secret: personal("op://c/env") }],
    },
  },
  sourceControlHosts: {
    github: { tokens: { "github.com": personal("op://gh"), "ghe.acme.test": work } },
  },
  usageLimitSources: {
    hub: { kind: "cliproxy", url: "https://hub.test", managementKey: personal("op://hub") },
  },
  easDeviceHosts: [
    { kind: "eas", id: "eas", label: "Expo Simulators", accessToken: personal("op://expo") },
  ],
  simlockDeviceHosts: [
    {
      kind: "simlock",
      id: "pool",
      label: "Simlock Pool",
      gatewayUrl: "https://simlock.test",
      agentToken: personal("op://simlock"),
    },
  ],
  cloudSpeechCredentials: { openai: personal("op://voice"), xai: "" },
});

it.each([
  [
    "personal",
    [
      "Antigravity Work · API_TOKEN",
      "Codex · CODEX_TOKEN",
      "GitHub · github.com",
      "https://hub.test · Management Key",
      "Expo Simulators · Access Token",
      "Simlock Pool · Agent Token",
      "Voice · OpenAI",
    ],
  ],
  ["work", ["Antigravity Work · OTHER_TOKEN", "GitHub · ghe.acme.test"]],
  ["unused", []],
])("lists what reads from secret provider %s", (providerId, uses) => {
  expect(secretProviderUses(settings, providerId).map((use) => use.label)).toEqual(uses);
});

it.each([
  ["has secret providers on", "connected", { secretProviders: true }, true, "ready"],
  ["has secret providers off", "connected", {}, true, "disabled"],
  ["is offline", "disconnected", { secretProviders: true }, true, "offline"],
  ["predates the setting", "connected", { secretProviders: true }, undefined, "outdated"],
  ["predates feature flags", "connected", undefined, undefined, "outdated"],
] as const)(
  "decides the Secrets page from the selected environment, which %s",
  (_, phase, featureFlags, secretProviderSettings, expected) => {
    expect(
      secretsSettingsState({
        connection: { phase },
        serverConfig: {
          ...(featureFlags === undefined ? {} : { featureFlags }),
          ...(secretProviderSettings === undefined ? {} : { secretProviderSettings }),
        },
      }),
    ).toBe(expected);
  },
);
