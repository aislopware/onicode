import {
  type AgentBrowserUseTarget,
  type BrowserUseStatus,
  DEFAULT_SERVER_SETTINGS,
} from "@onicode/contracts";
import { useState } from "react";

import { deviceEnvironment } from "~/state/device";
import { useEnvironmentQuery } from "~/state/query";

import { Input } from "../ui/input";
import { Select, SelectItem, SelectPopup, SelectTrigger, SelectValue } from "../ui/select";
import { ScopedSwitch } from "./ScopedSwitch";
import { useSettingsScope } from "./SettingsScopeContext";
import { SettingResetButton, SettingsRow } from "./settingsLayout";
import { searchableSetting } from "./settingsSearch";
import { useScopedSettings, useUpdateScopedSettings } from "./useScopedSettings";

const TARGETS: Readonly<
  Record<AgentBrowserUseTarget, { readonly label: string; readonly description: string }>
> = {
  panel: {
    label: "Browser panel",
    description:
      "Agents drive their tab in the Browser panel, where you watch and can take over. It needs agent browser access.",
  },
  isolated: {
    label: "Its own Chrome",
    description:
      "Agents get a separate Chrome with its own logins. It needs Chrome or another Chromium browser on the server's machine.",
  },
  personal: {
    label: "Your browser",
    description: "Agents work in your own browser, signed in as you, in a window of their own.",
  },
};

const megabytes = (bytes: number) => Math.round(bytes / 1_000_000);

function browserUseStatusLine(status: BrowserUseStatus | null): string | null {
  if (status === null) return null;
  switch (status.install.state) {
    case "unsupported":
      return "obu has no build for this machine.";
    case "installing":
      return `Downloading obu ${status.driverVersion}: ${megabytes(status.install.downloadedBytes)} of ${megabytes(status.install.totalBytes)} MB.`;
    case "failed":
      return `${status.install.message} Starting an agent session tries again.`;
    case "missing":
      return null;
    case "installed":
      return `obu ${status.driverVersion} is installed.`;
  }
}

const parsePort = (text: string): number | null | undefined => {
  if (text.trim() === "") return null;
  const port = Number(text);
  return Number.isInteger(port) && port >= 1 && port <= 65_535 ? port : undefined;
};

/** Keyed by the saved port, so a change made elsewhere replaces the draft. */
function PersonalBrowserPort({
  saved,
  onSave,
}: {
  readonly saved: number | null;
  readonly onSave: (port: number | null) => void;
}) {
  const [draft, setDraft] = useState(saved === null ? "" : String(saved));
  const port = parsePort(draft);
  const save = () => {
    if (port !== undefined && port !== saved) onSave(port);
  };
  return (
    <Input
      className="w-full sm:w-28"
      size="sm"
      type="number"
      inputMode="numeric"
      min={1}
      max={65_535}
      step={1}
      placeholder="Automatic"
      aria-label="Browser debug port"
      aria-invalid={port === undefined}
      value={draft}
      onChange={(event) => setDraft(event.target.value)}
      onBlur={save}
      onKeyDown={(event) => {
        if (event.key === "Enter") save();
      }}
    />
  );
}

/** Agents browsing with obu: the switch, which browser it drives, and that browser's port. */
export function BrowserUseSettings() {
  const { scope, environment: selected } = useSettingsScope();
  const settings = useScopedSettings();
  const updateSettings = useUpdateScopedSettings();
  const projectScope = scope.kind === "project" || scope.kind === "checkout";
  const connected = selected?.connection.phase === "connected" && selected.serverConfig !== null;
  const environmentId = connected ? selected.environmentId : null;
  const enabled = settings.enableAgentBrowserUse;
  const target = settings.agentBrowserUseTarget;
  const status = useEnvironmentQuery(
    environmentId && enabled
      ? deviceEnvironment.browserUseStatus({ environmentId, input: {} })
      : null,
  ).data;
  const statusLine = enabled ? browserUseStatusLine(status) : null;
  const panelNeedsAccess = enabled && target === "panel" && !settings.enableAgentBrowserAccess;

  return (
    <>
      <SettingsRow
        {...searchableSetting("agent-browser-use")}
        serverScoped
        settingKeys={["enableAgentBrowserUse"]}
        description={
          <>
            {projectScope
              ? "Agents in this project browse with obu, a driver built for agents. Applies when the agent session next starts."
              : "New agent sessions browse with obu, a driver built for agents that works on sites that block automation. Projects can override it."}
            {statusLine ? <span className="block">{statusLine}</span> : null}
            {panelNeedsAccess ? (
              <span className="block">
                Agent browser access is off, so there is no Browser panel to drive.
              </span>
            ) : null}
          </>
        }
        resetAction={
          enabled !== DEFAULT_SERVER_SETTINGS.enableAgentBrowserUse ? (
            <SettingResetButton
              label="browser use"
              onClick={() =>
                updateSettings({
                  enableAgentBrowserUse: DEFAULT_SERVER_SETTINGS.enableAgentBrowserUse,
                })
              }
            />
          ) : null
        }
        control={
          <ScopedSwitch
            settingKeys={["enableAgentBrowserUse"]}
            aria-label="Agents browse with obu"
            checked={enabled}
            onCheckedChange={(checked) => updateSettings({ enableAgentBrowserUse: checked })}
          />
        }
      />
      {enabled && !projectScope ? (
        <SettingsRow
          {...searchableSetting("agent-browser-use-target")}
          serverScoped
          settingKeys={["agentBrowserUseTarget"]}
          description={`${TARGETS[target].description} Open threads switch when their agent session restarts.`}
          resetAction={
            target !== DEFAULT_SERVER_SETTINGS.agentBrowserUseTarget ? (
              <SettingResetButton
                label="the browser obu drives"
                onClick={() =>
                  updateSettings({
                    agentBrowserUseTarget: DEFAULT_SERVER_SETTINGS.agentBrowserUseTarget,
                  })
                }
              />
            ) : null
          }
          control={
            <Select
              value={target}
              onValueChange={(value) => {
                if (value === "panel" || value === "isolated" || value === "personal") {
                  updateSettings({ agentBrowserUseTarget: value });
                }
              }}
            >
              <SelectTrigger size="sm" className="w-full sm:w-40" aria-label="Browser obu drives">
                <SelectValue>{TARGETS[target].label}</SelectValue>
              </SelectTrigger>
              <SelectPopup align="end" alignItemWithTrigger={false}>
                {(Object.keys(TARGETS) as ReadonlyArray<AgentBrowserUseTarget>).map((value) => (
                  <SelectItem hideIndicator key={value} value={value}>
                    {TARGETS[value].label}
                  </SelectItem>
                ))}
              </SelectPopup>
            </Select>
          }
        />
      ) : null}
      {enabled && !projectScope && target === "personal" ? (
        <SettingsRow
          {...searchableSetting("agent-browser-use-port")}
          serverScoped
          settingKeys={["agentBrowserUsePort"]}
          description="The port your browser was started with (--remote-debugging-port). Leave it empty for a browser that had remote debugging turned on in its settings."
          control={
            <PersonalBrowserPort
              key={settings.agentBrowserUsePort ?? "automatic"}
              saved={settings.agentBrowserUsePort}
              onSave={(port) => updateSettings({ agentBrowserUsePort: port })}
            />
          }
        />
      ) : null}
    </>
  );
}
