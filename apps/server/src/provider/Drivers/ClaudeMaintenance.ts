import { ProviderDriverKind } from "@onicode/contracts";
import {
  makeCachedProviderMaintenanceResolution,
  makePackageManagedProviderMaintenanceResolver,
  normalizeCommandPath,
  resolveProviderMaintenanceCapabilitiesEffect,
  type ProviderMaintenanceCapabilities,
  type ProviderMaintenanceResolverServices,
} from "@onicode/provider-core/server/maintenanceResolver";
import * as HostProcess from "@onicode/shared/HostProcess";
import { fromLenientJson } from "@onicode/shared/schemaJson";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Path from "effect/Path";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";
import * as HttpClient from "effect/http/HttpClient";
import * as HttpClientResponse from "effect/http/HttpClientResponse";
import { resolveClaudeHomePath } from "./ClaudeHome.ts";

const NATIVE_RELEASES = "https://downloads.claude.ai/claude-code-releases";
const decodeUpdateChannelSettings = Schema.decodeUnknownEffect(
  fromLenientJson(
    Schema.Struct({ autoUpdatesChannel: Schema.optional(Schema.Literals(["latest", "stable"])) }),
  ),
);
const decodeAutoUpdaterSettings = Schema.decodeUnknownEffect(
  fromLenientJson(
    Schema.Struct({
      env: Schema.optional(Schema.Struct({ DISABLE_AUTOUPDATER: Schema.optional(Schema.Unknown) })),
    }),
  ),
);

// Claude Code's own truthiness test for environment switches.
function isClaudeEnvTruthy(value: unknown): boolean {
  if (typeof value === "boolean") return value;
  if (typeof value !== "string" && typeof value !== "number") return false;
  return ["1", "true", "yes", "on"].includes(String(value).trim().toLowerCase());
}
const NativeVersion = Schema.String.check(Schema.isPattern(/^\d+\.\d+\.\d+$/));

const update = makePackageManagedProviderMaintenanceResolver({
  provider: ProviderDriverKind.make("claudeAgent"),
  npmPackageName: "@anthropic-ai/claude-code",
  mise: { tool: "claude" },
  nativeUpdate: {
    args: ["update"],
    isCommandPath: (commandPath) => {
      const normalized = normalizeCommandPath(commandPath);
      return (
        normalized.endsWith("/.local/bin/claude") ||
        normalized.endsWith("/.local/bin/claude.exe") ||
        normalized.includes("/.local/share/claude/")
      );
    },
  },
});

const readUpdateChannel = Effect.fn("ClaudeMaintenance.readUpdateChannel")(function* () {
  const fs = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  const environment = yield* HostProcess.Environment;
  const platform = yield* HostProcess.Platform;
  const managedDirectory =
    platform === "darwin"
      ? "/Library/Application Support/ClaudeCode"
      : platform === "win32"
        ? path.join(environment.PROGRAMFILES || "C:\\Program Files", "ClaudeCode")
        : "/etc/claude-code";
  const dropInDirectory = path.join(managedDirectory, "managed-settings.d");
  const dropIns = yield* fs.readDirectory(dropInDirectory).pipe(Effect.orElseSucceed(() => []));
  // The updater inherits the server's config directory, not the instance's homePath.
  const configDirectory = yield* resolveClaudeHomePath({ homePath: "" }, environment);
  const settingsPaths = [
    path.join(configDirectory, "settings.json"),
    path.join(managedDirectory, "managed-settings.json"),
    ...dropIns
      .filter((name) => name.endsWith(".json") && !name.startsWith("."))
      .toSorted()
      .map((name) => path.join(dropInDirectory, name)),
    path.join(configDirectory, "remote-settings.json"),
  ];
  const settings = yield* Effect.forEach(settingsPaths, (settingsPath) =>
    fs.readFileString(settingsPath).pipe(
      Effect.flatMap((contents) =>
        Effect.all([
          decodeUpdateChannelSettings(contents).pipe(
            Effect.map((decoded) => decoded.autoUpdatesChannel),
            Effect.orElseSucceed(() => undefined),
          ),
          decodeAutoUpdaterSettings(contents).pipe(
            Effect.map((decoded) => decoded.env?.DISABLE_AUTOUPDATER),
            Effect.orElseSucceed(() => undefined),
          ),
        ]),
      ),
      Effect.orElseSucceed(() => [undefined, undefined] as const),
    ),
  );
  const autoUpdater = settings
    .map(([, disabled]) => disabled)
    .findLast((disabled) => disabled !== undefined);
  return {
    channel:
      settings.map(([channel]) => channel).findLast((channel) => channel !== undefined) ?? "latest",
    autoUpdatesDisabled: isClaudeEnvTruthy(autoUpdater ?? environment.DISABLE_AUTOUPDATER),
  };
});

const resolveClaudeMaintenance = Effect.fn("ClaudeMaintenance.resolve")(function* (
  capabilities: ProviderMaintenanceCapabilities,
) {
  const { channel, autoUpdatesDisabled } = yield* readUpdateChannel();
  // Whoever turned Claude's updater off owns its version, so no release feed is asked.
  if (autoUpdatesDisabled) {
    const { lookupLatestVersion: _, ...rest } = capabilities;
    return { ...rest, latestVersion: null };
  }
  if (capabilities.update?.lockKey !== "claudeAgent-native") return capabilities;
  const client = yield* HttpClient.HttpClient;
  const url = `${NATIVE_RELEASES}/${channel}`;
  return {
    ...capabilities,
    latestVersion: null,
    latestVersionCacheKey: url,
    lookupLatestVersion: client.get(url).pipe(
      Effect.flatMap(HttpClientResponse.filterStatusOk),
      Effect.flatMap((response) => response.text),
      Effect.map((text) => text.trim()),
      Effect.flatMap(Schema.decodeUnknownEffect(NativeVersion)),
      Effect.timeoutOption("4 seconds"),
      Effect.map(Option.getOrNull),
      Effect.orElseSucceed(() => null),
    ),
  };
});

export const makeClaudeMaintenanceResolution = Effect.fn("ClaudeMaintenance.make")(function* (
  options: Parameters<typeof resolveProviderMaintenanceCapabilitiesEffect>[1],
) {
  const services = yield* Effect.context<
    ProviderMaintenanceResolverServices | HttpClient.HttpClient
  >();
  const resolveInstallation = yield* makeCachedProviderMaintenanceResolution(
    resolveProviderMaintenanceCapabilitiesEffect(update, options).pipe(
      Effect.provideContext(services),
    ),
  );
  const resolve: typeof resolveInstallation = (refreshOptions) =>
    resolveInstallation(refreshOptions).pipe(
      Effect.flatMap(resolveClaudeMaintenance),
      Effect.provideContext(services),
    );
  return resolve;
});
