import { afterEach, expect, it, vi } from "vite-plus/test";

// Keep config parsing independent of local env files and build credentials.
vi.mock("../../scripts/lib/public-config.ts", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../scripts/lib/public-config.ts")>()),
  loadRepoEnv: () => ({
    APP_VARIANT: process.env.APP_VARIANT,
    ONICODE_SENTRY_MOBILE_DSN: process.env.ONICODE_SENTRY_MOBILE_DSN,
    SENTRY_AUTH_TOKEN: process.env.SENTRY_AUTH_TOKEN,
    SENTRY_URL: process.env.SENTRY_URL,
    SENTRY_ORG: process.env.SENTRY_ORG,
    ONICODE_SENTRY_DEV: process.env.ONICODE_SENTRY_DEV,
  }),
}));

afterEach(() => {
  vi.unstubAllEnvs();
});

async function loadConfig(variant: string | undefined, env: Record<string, string> = {}) {
  vi.resetModules();
  vi.stubEnv("APP_VARIANT", variant);
  vi.stubEnv("ONICODE_SENTRY_MOBILE_DSN", env.ONICODE_SENTRY_MOBILE_DSN);
  vi.stubEnv("SENTRY_AUTH_TOKEN", env.SENTRY_AUTH_TOKEN);
  vi.stubEnv("SENTRY_URL", env.SENTRY_URL);
  vi.stubEnv("SENTRY_ORG", env.SENTRY_ORG);
  vi.stubEnv("ONICODE_SENTRY_DEV", env.ONICODE_SENTRY_DEV);
  return (await import("./app.config.ts")).default;
}

it.each(["prod", "preview", ""])(
  "rejects APP_VARIANT=%j before selecting production IDs",
  async (value) => {
    await expect(loadConfig(value)).rejects.toThrow(
      "APP_VARIANT must be development or production.",
    );
  },
);

it.each([
  [undefined, "dev.aislopware.onicode"],
  ["development", "dev.aislopware.onicode.dev"],
  ["production", "dev.aislopware.onicode"],
])("keeps the iOS and Android identifiers for APP_VARIANT=%j", async (value, identifier) => {
  const config = await loadConfig(value);
  expect(config.ios?.bundleIdentifier).toBe(identifier);
  expect(config.android?.package).toBe(identifier);
});

const DSN = "https://key@sentry.example/1";

it.each([
  ["production", {}, null, undefined],
  ["production", { ONICODE_SENTRY_MOBILE_DSN: " " }, null, undefined],
  ["production", { ONICODE_SENTRY_MOBILE_DSN: DSN }, DSN, true],
  ["production", { ONICODE_SENTRY_MOBILE_DSN: DSN, SENTRY_AUTH_TOKEN: "token" }, DSN, true],
  [
    "production",
    {
      ONICODE_SENTRY_MOBILE_DSN: DSN,
      SENTRY_AUTH_TOKEN: "token",
      SENTRY_URL: "https://sentry.example",
      SENTRY_ORG: "example",
    },
    DSN,
    false,
  ],
  ["development", { ONICODE_SENTRY_MOBILE_DSN: DSN }, null, undefined],
  ["development", { ONICODE_SENTRY_MOBILE_DSN: DSN, ONICODE_SENTRY_DEV: "1" }, DSN, true],
])(
  "configures Sentry for a %s build only with a DSN (%j)",
  async (variant, env, dsn, disableAutoUpload) => {
    const config = await loadConfig(variant, env);
    const plugin = config.plugins?.find(
      (entry) => Array.isArray(entry) && entry[0] === "@sentry/react-native/expo",
    );
    expect(config.extra?.sentryDsn).toBe(dsn);
    expect(plugin === undefined ? undefined : plugin[1]?.disableAutoUpload).toBe(disableAutoUpload);
  },
);
