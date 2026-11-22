import {
  CLOUD_SPEECH_LABELS,
  CloudSpeechProvider,
  EasDeviceHostConfig,
  isSecretReference,
  secretReferencesInSettings,
  type ServerConfig,
  type ServerSettings,
  SimlockDeviceHostConfig,
  TextGeneratorConfig,
  UsageLimitSourceConfig,
} from "@onicode/contracts";
import { hasFeatureFlag } from "@onicode/client-runtime/state/server";
import { getProviderClient } from "@onicode/client-runtime/provider-clients";
import { sourceControlClients } from "@onicode/client-runtime/source-control-clients";
import { msg } from "gt-react";

import { m, type MessageResolver } from "../../i18n/messages";

import { deriveProviderSettingsFields, type SettingsFormDefinition } from "./ProviderSettingsForm";

type SecretUseSettings = Pick<
  ServerSettings,
  | "providerInstances"
  | "sourceControlHosts"
  | "usageLimitSources"
  | "textGenerators"
  | "easDeviceHosts"
  | "simlockDeviceHosts"
  | "cloudSpeechCredentials"
>;

interface SecretProviderUse {
  readonly id: string;
  readonly label: string;
}

/** What reads from secret provider `providerId`, one readable name per field, in settings order. */
export function secretProviderUses(
  settings: SecretUseSettings,
  providerId: string,
  resolve: MessageResolver = m,
): ReadonlyArray<SecretProviderUse> {
  const uses = (
    definition: SettingsFormDefinition,
    ownerId: string,
    owner: string,
    value: unknown,
  ): ReadonlyArray<SecretProviderUse> => {
    const labels = new Map(
      deriveProviderSettingsFields(definition, undefined, resolve).map(
        (field) => [field.key, field.label] as const,
      ),
    );
    return secretReferencesInSettings(definition.settingsSchema, value).flatMap(
      ({ field, key, reference }) =>
        reference.provider === providerId
          ? [
              {
                id: `${ownerId}:${field}:${key ?? ""}`,
                label: `${owner} · ${key ?? labels.get(field) ?? field}`,
              },
            ]
          : [],
    );
  };

  return [
    ...Object.entries(settings.providerInstances).flatMap(([instanceId, instance]) => {
      const client = getProviderClient(instance.driver);
      const owner = instance.displayName ?? client?.label ?? instanceId;
      return [
        ...(client ? uses(client, `provider:${instanceId}`, owner, instance.config) : []),
        ...(instance.environment ?? []).flatMap((variable) =>
          isSecretReference(variable.value) && variable.value.provider === providerId
            ? [
                {
                  id: `provider:${instanceId}:env:${variable.name}`,
                  label: `${owner} · ${variable.name}`,
                },
              ]
            : [],
        ),
      ];
    }),
    ...sourceControlClients.definitions.flatMap((definition) =>
      definition.settings
        ? uses(
            { settingsSchema: definition.settings },
            `source-control:${definition.kind}`,
            definition.label,
            settings.sourceControlHosts[definition.kind],
          )
        : [],
    ),
    ...Object.entries(settings.usageLimitSources).flatMap(([id, source]) =>
      uses(
        { settingsSchema: UsageLimitSourceConfig },
        `usage-limit:${id}`,
        source.label ?? source.url,
        source,
      ),
    ),
    ...Object.entries(settings.textGenerators).flatMap(([id, generator]) =>
      uses(
        { settingsSchema: TextGeneratorConfig },
        `text-generator:${id}`,
        generator.label ?? generator.baseUrl,
        generator,
      ),
    ),
    ...settings.easDeviceHosts.flatMap((host) =>
      uses({ settingsSchema: EasDeviceHostConfig }, `eas:${host.id}`, host.label, host),
    ),
    ...settings.simlockDeviceHosts.flatMap((host) =>
      uses({ settingsSchema: SimlockDeviceHostConfig }, `simlock:${host.id}`, host.label, host),
    ),
    ...CloudSpeechProvider.literals.flatMap((provider) => {
      const credential = settings.cloudSpeechCredentials[provider];
      return isSecretReference(credential) && credential.provider === providerId
        ? [
            {
              id: `voice:${provider}`,
              label: resolve(
                msg("Voice · {provider}", { provider: CLOUD_SPEECH_LABELS[provider] }),
              ),
            },
          ]
        : [];
    }),
  ];
}

/** What the Secrets page shows for the selected environment; flags are per environment. */
export function secretsSettingsState(
  environment:
    | {
        readonly connection: { readonly phase: string };
        readonly serverConfig: Pick<ServerConfig, "featureFlags" | "secretProviderSettings"> | null;
      }
    | null
    | undefined,
): "offline" | "disabled" | "outdated" | "ready" {
  if (environment?.connection.phase !== "connected") return "offline";
  const config = environment.serverConfig;
  if (config === null) return "ready";
  // A server from before feature flags sends none: it needs an update, not a flag.
  if (config.featureFlags === undefined) return "outdated";
  if (!hasFeatureFlag(config, "secretProviders")) return "disabled";
  return config.secretProviderSettings === true ? "ready" : "outdated";
}
