import {
  isManagedSettingPath,
  isNullableProjectSettingsOverride,
  PROJECT_SCOPED_SERVER_SETTING_KEYS,
  type EnvironmentId,
  type ProjectId,
  type ProjectScopedServerSettingKey,
  type ProjectSettingsOverrides,
  type ServerSettings,
  type ServerSettingsPatch,
} from "@onicode/contracts";
import {
  clearProjectSettingsOverrides,
  type ProjectOverrideWrite,
  projectOverridesPatch,
  resolveProjectSettings,
} from "@onicode/shared/projectSettings";

import { msg } from "gt-react-native";

import { m, type MessageResolver } from "../../i18n/messages";
import type { SettingsTarget } from "./settings-environment-filter";

export interface ScopedMobileSettingsTarget {
  readonly environment: SettingsTarget;
  readonly projectId: ProjectId | null;
  readonly settings: ServerSettings;
  readonly sources: ReturnType<typeof resolveProjectSettings>["sources"];
}

/**
 * "Managed by <organization>." when any target's policy locks `path`: the server refuses the
 * whole write then, so the control is managed for the selection.
 */
export function managedSettingReason(
  targets: readonly ScopedMobileSettingsTarget[],
  paths: readonly string[],
  resolve: MessageResolver = m,
): string | undefined {
  const policy = targets
    .map((target) => target.environment.serverConfig.managedPolicy)
    .find((candidate) => paths.some((path) => isManagedSettingPath(candidate, path)));
  return policy === undefined
    ? undefined
    : resolve(msg("Managed by {organization}.", { organization: policy.organization }));
}

export function resolveMobileSettingsTargets(
  environments: readonly SettingsTarget[],
  members: readonly { readonly environmentId: EnvironmentId; readonly id: ProjectId }[] | null,
): readonly ScopedMobileSettingsTarget[] {
  if (members === null) {
    return environments.map((environment) => ({
      environment,
      projectId: null,
      ...resolveProjectSettings(environment.serverConfig.settings, null),
    }));
  }
  const byId = new Map(environments.map((environment) => [environment.environmentId, environment]));
  return members.flatMap((member) => {
    const environment = byId.get(member.environmentId);
    if (!environment) return [];
    return [
      {
        environment,
        projectId: member.id,
        ...resolveProjectSettings(environment.serverConfig.settings, member.id),
      },
    ];
  });
}

export function planMobileScopedSettingsPatch(
  targets: readonly ScopedMobileSettingsTarget[],
  projectSelected: boolean,
  patch: ServerSettingsPatch,
) {
  if (!projectSelected) {
    return targets.map((target) => ({ environmentId: target.environment.environmentId, patch }));
  }
  const keys = Object.keys(patch);
  if (
    keys.some(
      (key) => !PROJECT_SCOPED_SERVER_SETTING_KEYS.includes(key as ProjectScopedServerSettingKey),
    )
  )
    return [];
  return projectOverrideWrites(targets, keys, (settings, projectId) => {
    const next: Record<string, unknown> = { ...settings.projectSettingsOverrides[projectId] };
    for (const [key, value] of Object.entries(patch)) {
      if (
        value === null &&
        !isNullableProjectSettingsOverride(key as ProjectScopedServerSettingKey)
      ) {
        delete next[key];
      } else {
        next[key] = value;
      }
    }
    return next as ProjectSettingsOverrides;
  });
}

export function uniformMobileSetting<K extends keyof ServerSettings>(
  targets: readonly Pick<ScopedMobileSettingsTarget, "settings">[],
  key: K,
): ServerSettings[K] | null {
  const reference = targets[0];
  if (!reference) return null;
  const value = reference.settings[key];
  return targets.every((target) => target.settings[key] === value) ? value : null;
}

export function planMobileScopedSettingsClear(
  targets: readonly ScopedMobileSettingsTarget[],
  keys: readonly ProjectScopedServerSettingKey[],
) {
  return projectOverrideWrites(targets, keys, (settings, projectId) =>
    clearProjectSettingsOverrides(settings, projectId, keys),
  );
}

function projectOverrideWrites(
  targets: readonly ScopedMobileSettingsTarget[],
  keys: readonly string[],
  next: (settings: ServerSettings, projectId: ProjectId) => ProjectSettingsOverrides | null,
) {
  const writes = new Map<
    EnvironmentId,
    { readonly perKey: boolean; readonly entries: ProjectOverrideWrite[] }
  >();
  for (const { environment, projectId } of targets) {
    if (projectId === null) continue;
    const pending = writes.get(environment.environmentId) ?? {
      perKey:
        environment.serverConfig.environment.capabilities.projectSettingsOverrideKeys === true,
      entries: [],
    };
    pending.entries.push({
      projectId,
      keys,
      next: next(environment.serverConfig.settings, projectId),
    });
    writes.set(environment.environmentId, pending);
  }
  return [...writes].map(([environmentId, { perKey, entries }]) => ({
    environmentId,
    patch: projectOverridesPatch(entries, perKey),
  }));
}
