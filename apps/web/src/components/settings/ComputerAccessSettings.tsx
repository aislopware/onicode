import {
  type AgentComputerPermissionMode,
  type ComputerAccessStatus,
  DEFAULT_SERVER_SETTINGS,
} from "@onicode/contracts";
import { useEffect, useState } from "react";

import {
  brandProviderInstanceEntries,
  deriveProviderInstanceEntries,
  shouldShowInstanceBadge,
} from "~/providerInstances";
import { deviceEnvironment } from "~/state/device";
import { useEnvironmentProviders, usePrimaryEnvironmentId } from "~/state/environments";
import { useEnvironmentQuery } from "~/state/query";

import { ProviderInstanceIcon } from "../chat/ProviderInstanceIcon";
import { Button } from "../ui/button";
import { Select, SelectItem, SelectPopup, SelectTrigger, SelectValue } from "../ui/select";
import { Switch } from "../ui/switch";
import { ScopedSwitch } from "./ScopedSwitch";
import { useSettingsScope } from "./SettingsScopeContext";
import { SettingResetButton, SettingsRow, SettingsSection } from "./settingsLayout";
import { searchableSetting } from "./settingsSearch";
import { useScopedSettings, useUpdateScopedSettings } from "./useScopedSettings";

const PERMISSION_MODES: Readonly<
  Record<AgentComputerPermissionMode, { readonly label: string; readonly description: string }>
> = {
  standard: {
    label: "Standard",
    description:
      "The driver refuses to act on system security prompts and asks before it attaches to a signed-in browser profile.",
  },
  unrestricted: {
    label: "Unrestricted",
    description:
      "The driver runs every action without its own checks, including its guard against instructions hidden in what an app shows.",
  },
};

const megabytes = (bytes: number) => Math.round(bytes / 1_000_000);

function computerDriverStatusLine(status: ComputerAccessStatus | null): string | null {
  if (status === null) return null;
  switch (status.install.state) {
    case "unsupported":
      return "cua-driver has no build for this machine.";
    case "installing":
      return `Downloading cua-driver ${status.driverVersion}: ${megabytes(status.install.downloadedBytes)} of ${megabytes(status.install.totalBytes)} MB.`;
    case "failed":
      return `${status.install.message} Starting an agent session tries again.`;
    case "missing":
      return null;
    case "installed":
      return status.daemon.state === "failed"
        ? status.daemon.message
        : `cua-driver ${status.driverVersion} is installed.`;
  }
}

/** Keyed by the saved settings, so each change asks the server for the driver's new state. */
function DriverStatusRefresh({ refresh }: { readonly refresh: () => void }) {
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- the driver's state follows saved settings and OS permissions, which change on the server and in System Settings
  useEffect(() => {
    refresh();
    window.addEventListener("focus", refresh);
    return () => window.removeEventListener("focus", refresh);
  }, [refresh]);
  return null;
}

/** Agents driving this machine's desktop apps: the switch, the driver's approval mode, and who gets it. */
export function ComputerAccessSettings() {
  const { scope, environment: selected } = useSettingsScope();
  const settings = useScopedSettings();
  const updateSettings = useUpdateScopedSettings();
  const primaryEnvironmentId = usePrimaryEnvironmentId();
  const providersByEnvironment = useEnvironmentProviders();
  const projectScope = scope.kind === "project" || scope.kind === "checkout";
  const connected = selected?.connection.phase === "connected" && selected.serverConfig !== null;
  const environmentId = connected ? selected.environmentId : null;
  const enabled = settings.enableAgentComputerAccess;
  const mode = settings.agentComputerPermissionMode;
  const query = useEnvironmentQuery(
    environmentId && enabled
      ? deviceEnvironment.computerStatus({ environmentId, input: {} })
      : null,
  );
  const { refresh } = query;
  const status = query.data;
  const [requesting, setRequesting] = useState(false);

  const permissions = status?.daemon.state === "running" ? status.daemon.permissions : null;
  const missingPermission =
    permissions !== null && !(permissions.accessibility && permissions.screenRecording);
  // Only the desktop app on the machine that runs this server can ask macOS for its own grants.
  const bridge =
    environmentId !== null && environmentId === primaryEnvironmentId
      ? window.desktopBridge?.remoteDesktop
      : undefined;
  const requestPermissions = async () => {
    if (!bridge) return;
    setRequesting(true);
    await bridge.requestPermissions().catch(() => null);
    setRequesting(false);
    refresh();
  };
  // One row per provider instance: two accounts of one provider are switched separately.
  const instances = brandProviderInstanceEntries(
    deriveProviderInstanceEntries(
      environmentId ? (providersByEnvironment.get(environmentId) ?? []) : [],
    ),
    settings,
  ).filter((instance) => instance.enabled);

  return (
    <SettingsSection id="computer-access" title="Computer">
      <DriverStatusRefresh key={`${enabled}:${mode}`} refresh={refresh} />
      <SettingsRow
        {...searchableSetting("agent-computer-access")}
        serverScoped
        settingKeys={["enableAgentComputerAccess"]}
        description={
          <>
            {projectScope
              ? "Agents in this project can see and control this machine's desktop apps. Applies when the agent session next starts."
              : "New agent sessions can see and control this machine's desktop apps. Projects can override it."}
            {enabled && computerDriverStatusLine(status) ? (
              <span className="block">{computerDriverStatusLine(status)}</span>
            ) : null}
          </>
        }
        resetAction={
          enabled !== DEFAULT_SERVER_SETTINGS.enableAgentComputerAccess ? (
            <SettingResetButton
              label="computer access"
              onClick={() =>
                updateSettings({
                  enableAgentComputerAccess: DEFAULT_SERVER_SETTINGS.enableAgentComputerAccess,
                })
              }
            />
          ) : null
        }
        control={
          <ScopedSwitch
            settingKeys={["enableAgentComputerAccess"]}
            aria-label="Agent computer access"
            checked={enabled}
            onCheckedChange={(checked) => updateSettings({ enableAgentComputerAccess: checked })}
          />
        }
      />
      {enabled && permissions !== null ? (
        <SettingsRow
          title="Permissions"
          description={
            <>
              <span className="block">
                Accessibility: {permissions.accessibility ? "allowed" : "not allowed"}
              </span>
              <span className="block">
                Screen Recording: {permissions.screenRecording ? "allowed" : "not allowed"}
              </span>
              {missingPermission && !bridge ? (
                <span className="block">
                  Allow the app that runs this server under Privacy &amp; Security on that Mac.
                </span>
              ) : null}
            </>
          }
          control={
            missingPermission && bridge ? (
              <Button
                size="xs"
                variant="outline"
                disabled={requesting}
                onClick={() => void requestPermissions()}
              >
                Grant access
              </Button>
            ) : undefined
          }
        />
      ) : null}
      {enabled ? (
        <SettingsRow
          {...searchableSetting("agent-computer-permission-mode")}
          serverScoped
          settingKeys={["agentComputerPermissionMode"]}
          description={`${PERMISSION_MODES[mode].description} Changing it restarts the driver; a thread already open uses the new mode once its agent session restarts.`}
          resetAction={
            mode !== DEFAULT_SERVER_SETTINGS.agentComputerPermissionMode ? (
              <SettingResetButton
                label="driver approvals"
                onClick={() =>
                  updateSettings({
                    agentComputerPermissionMode:
                      DEFAULT_SERVER_SETTINGS.agentComputerPermissionMode,
                  })
                }
              />
            ) : null
          }
          control={
            <Select
              value={mode}
              onValueChange={(value) => {
                if (value === "standard" || value === "unrestricted") {
                  updateSettings({ agentComputerPermissionMode: value });
                }
              }}
            >
              <SelectTrigger size="sm" className="w-full sm:w-40" aria-label="Driver approvals">
                <SelectValue>{PERMISSION_MODES[mode].label}</SelectValue>
              </SelectTrigger>
              <SelectPopup align="end" alignItemWithTrigger={false}>
                {(Object.keys(PERMISSION_MODES) as ReadonlyArray<AgentComputerPermissionMode>).map(
                  (value) => (
                    <SelectItem hideIndicator key={value} value={value}>
                      {PERMISSION_MODES[value].label}
                    </SelectItem>
                  ),
                )}
              </SelectPopup>
            </Select>
          }
        />
      ) : null}
      {enabled && !projectScope
        ? instances.map((instance) => (
            <SettingsRow
              key={instance.instanceId}
              title={
                <span className="inline-flex items-center gap-2.5">
                  <ProviderInstanceIcon
                    driverKind={instance.driverKind}
                    displayName={instance.displayName}
                    accentColor={instance.accentColor}
                    acpRegistryAgentId={instance.acpRegistryAgentId}
                    acpRegistryIconUrl={instance.acpRegistryIconUrl}
                    showBadge={shouldShowInstanceBadge(instance, instances)}
                  />
                  {instance.displayName}
                </span>
              }
              serverScoped
              control={
                <Switch
                  aria-label={`Computer access for ${instance.displayName}`}
                  checked={settings.agentComputerAccessProviders[instance.instanceId] !== false}
                  onCheckedChange={(checked) =>
                    updateSettings({
                      agentComputerAccessProviders: { [instance.instanceId]: checked },
                    })
                  }
                />
              }
            />
          ))
        : null}
    </SettingsSection>
  );
}
