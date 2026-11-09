import { useAtomValue } from "@effect/atom-react";
import { Link } from "@tanstack/react-router";
import {
  type EnvironmentId,
  managedCapabilityDenial,
  publicAccessFailureMessage,
  type PublicAccessStatus,
} from "@onicode/contracts";

import { Switch } from "../ui/switch";
import { ScopedSwitch } from "./ScopedSwitch";
import { SettingsPageContainer, SettingsRow, SettingsSection } from "./settingsLayout";
import { searchableSetting } from "./settingsSearch";
import { useScopedSettings, useUpdateScopedSettings } from "./useScopedSettings";
import { useOptionalSettingsScope, useSettingsScope } from "./SettingsScopeContext";
import { environmentAtomList, environmentIdsKey } from "~/state/environmentAtomList";
import { usePrimaryEnvironmentId } from "~/state/environments";
import { firstManagedDenial, usePrimaryManagedPolicy } from "~/state/managedPolicy";
import { serverEnvironment } from "~/state/server";

/** Features still settling in. */
export function LabsSettingsPanel() {
  return (
    <SettingsPageContainer>
      <SettingsSection id="beta" title="Beta">
        <BoringSidebarRow />
        <GuestLinksRow />
        <PublicAccessRow />
        <LinkedEnvironmentsRow />
        <PromptSuggestionsRow />
        <SimlockHostsRow />
        <FastWorktreeCheckoutRow />
      </SettingsSection>
    </SettingsPageContainer>
  );
}

function BoringSidebarRow() {
  const settings = useScopedSettings();
  const updateSettings = useUpdateScopedSettings();
  return (
    <SettingsRow
      {...searchableSetting("boring-sidebar")}
      description="Group sidebar threads into project folders. Pinned threads stay on top, and Snoozed and Settled stay below the folders."
      control={
        <Switch
          checked={settings.boringSidebarEnabled}
          onCheckedChange={(checked) => updateSettings({ boringSidebarEnabled: Boolean(checked) })}
          aria-label="Boring sidebar (group by project)"
        />
      }
    >
      {settings.boringSidebarEnabled ? (
        <div className="mt-2 mb-2 border-l-2 border-border/60 ps-3">
          <BoringSidebarMultilineRow />
        </div>
      ) : null}
    </SettingsRow>
  );
}

function BoringSidebarMultilineRow() {
  const settings = useScopedSettings();
  const updateSettings = useUpdateScopedSettings();
  return (
    <SettingsRow
      {...searchableSetting("boring-sidebar-multiline")}
      className="px-0 py-2 sm:px-0"
      description="Show folder threads on two lines: the title, then the branch, pull request, and agent."
      control={
        <Switch
          checked={settings.boringSidebarMultilineRows}
          onCheckedChange={(checked) =>
            updateSettings({ boringSidebarMultilineRows: Boolean(checked) })
          }
          aria-label="Two-line folder rows"
        />
      }
    />
  );
}

/** A policy that keeps clients on this machine turns guest links and public access off. */
function useRemoteClientsDenial() {
  const scope = useOptionalSettingsScope();
  const primaryPolicy = usePrimaryManagedPolicy();
  return firstManagedDenial(
    scope
      ? scope.connectedEnvironments.map((environment) => environment.serverConfig?.managedPolicy)
      : [primaryPolicy],
    (policy) => managedCapabilityDenial(policy, "remoteClients"),
  );
}

function GuestLinksRow() {
  const settings = useScopedSettings();
  const updateSettings = useUpdateScopedSettings();
  const remoteClientsDenial = useRemoteClientsDenial();
  return (
    <SettingsRow
      serverScoped
      {...searchableSetting("guest-links")}
      settingKeys={["guestSharingEnabled"]}
      managed={settings.guestSharingEnabled ? undefined : remoteClientsDenial}
      description="Share a thread with a guest who watches it live in a browser and sends messages you approve. Guests need public access. Turning it off stops every guest link."
      control={
        <ScopedSwitch
          settingKeys={["guestSharingEnabled"]}
          checked={settings.guestSharingEnabled}
          onCheckedChange={(checked) => updateSettings({ guestSharingEnabled: Boolean(checked) })}
          aria-label="Guest links"
        />
      }
    />
  );
}

const publicAccessStatusesAtom = environmentAtomList(
  "web-public-access-statuses",
  (environmentId) => serverEnvironment.publicAccessStatus({ environmentId, input: {} }),
);

function publicAccessStatusText(status: PublicAccessStatus): string | null {
  switch (status._tag) {
    case "off":
      return null;
    case "starting":
      return "Starting Tailscale Funnel…";
    case "on":
      return `Reachable at ${status.origin}`;
    case "failed":
      return publicAccessFailureMessage(status.reason);
  }
}

/** Publishes only guest links and webhook addresses, through Tailscale Funnel. */
function PublicAccessRow() {
  const settings = useScopedSettings();
  const updateSettings = useUpdateScopedSettings();
  const remoteClientsDenial = useRemoteClientsDenial();
  const scope = useOptionalSettingsScope();
  const primaryEnvironmentId = usePrimaryEnvironmentId();
  const environmentIds: ReadonlyArray<EnvironmentId> = scope
    ? scope.connectedEnvironments.map((environment) => environment.environmentId)
    : primaryEnvironmentId === null
      ? []
      : [primaryEnvironmentId];
  const statuses = useAtomValue(publicAccessStatusesAtom(environmentIdsKey(environmentIds)));
  const statusLines = statuses.flatMap((result) => {
    const text = result._tag === "Success" ? publicAccessStatusText(result.value) : null;
    return text === null ? [] : [text];
  });
  return (
    <SettingsRow
      serverScoped
      {...searchableSetting("public-access")}
      settingKeys={["publicAccessEnabled"]}
      managed={settings.publicAccessEnabled ? undefined : remoteClientsDenial}
      description="Let guest links and webhook addresses reach this machine from the internet through Tailscale Funnel. Nothing else is published. The tailnet needs HTTPS certificates and the funnel node attribute."
      control={
        <ScopedSwitch
          settingKeys={["publicAccessEnabled"]}
          checked={settings.publicAccessEnabled}
          onCheckedChange={(checked) => updateSettings({ publicAccessEnabled: Boolean(checked) })}
          aria-label="Public access"
        />
      }
    >
      {statusLines.map((line) => (
        <p key={line} className="mt-1 text-muted-foreground text-xs">
          {line}
        </p>
      ))}
    </SettingsRow>
  );
}

function LinkedEnvironmentsRow() {
  const settings = useScopedSettings();
  const updateSettings = useUpdateScopedSettings();
  return (
    <SettingsRow
      serverScoped
      {...searchableSetting("linked-environments-beta")}
      settingKeys={["linkedEnvironmentsEnabled"]}
      description={
        <>
          Let agents here work on your other machines, and theirs here. Turn it on for each machine,
          then link them in{" "}
          <Link to="/settings/connections" className="underline underline-offset-2">
            Connections
          </Link>
          . While off, nothing new starts over a link; running tasks finish.
        </>
      }
      control={
        <ScopedSwitch
          settingKeys={["linkedEnvironmentsEnabled"]}
          checked={settings.linkedEnvironmentsEnabled}
          onCheckedChange={(checked) =>
            updateSettings({ linkedEnvironmentsEnabled: Boolean(checked) })
          }
          aria-label="Linked environments"
        />
      }
    />
  );
}

function PromptSuggestionsRow() {
  const settings = useScopedSettings();
  const updateSettings = useUpdateScopedSettings();
  return (
    <SettingsRow
      serverScoped
      {...searchableSetting("prompt-suggestions-beta")}
      settingKeys={["promptSuggestionsEnabled"]}
      description="Show a predicted next message in the empty composer after a Claude or Codex turn. Tab uses it. Each is an extra cached request on your plan. Needs Codex 0.165+. Claude threads update on restart."
      control={
        <ScopedSwitch
          settingKeys={["promptSuggestionsEnabled"]}
          checked={settings.promptSuggestionsEnabled}
          onCheckedChange={(checked) =>
            updateSettings({ promptSuggestionsEnabled: Boolean(checked) })
          }
          aria-label="Suggest next prompt"
        />
      }
    />
  );
}

function SimlockHostsRow() {
  const settings = useScopedSettings();
  const updateSettings = useUpdateScopedSettings();
  return (
    <SettingsRow
      serverScoped
      {...searchableSetting("simlock-hosts")}
      settingKeys={["simlockDeviceHostsEnabled"]}
      description={
        <>
          Let the Device panel lease simulators and emulators from your own{" "}
          <a
            href="https://incubator.callstack.com/simlock/"
            target="_blank"
            rel="noreferrer"
            className="underline underline-offset-2"
          >
            Simlock
          </a>{" "}
          pool, added in{" "}
          <Link to="/settings/devices" className="underline underline-offset-2">
            Devices
          </Link>
          . An alpha that may change or break. Onicode never installs Simlock.
        </>
      }
      control={
        <ScopedSwitch
          settingKeys={["simlockDeviceHostsEnabled"]}
          checked={settings.simlockDeviceHostsEnabled}
          onCheckedChange={(checked) =>
            updateSettings({ simlockDeviceHostsEnabled: Boolean(checked) })
          }
          aria-label="Simlock device hosts"
        />
      }
    />
  );
}

/** Shows only when every machine in scope is a Mac, since only macOS servers honor it. */
function FastWorktreeCheckoutRow() {
  const { scope, connectedEnvironments } = useSettingsScope();
  const settings = useScopedSettings();
  const updateSettings = useUpdateScopedSettings();
  if (
    scope.kind === "project" ||
    scope.kind === "checkout" ||
    connectedEnvironments.length === 0 ||
    connectedEnvironments.some(
      (environment) =>
        environment.serverConfig?.environment.capabilities.fastWorktreeCheckout !== true,
    )
  )
    return null;
  return (
    <SettingsRow
      serverScoped
      {...searchableSetting("worktree-fast-checkout")}
      settingKeys={["fastWorktreeCheckout"]}
      description="On a Mac, clone new worktrees from a checkout cached in the Onicode home folder instead of writing every file. Uses Git’s normal checkout when it can’t. Turning it off deletes the cache."
      control={
        <ScopedSwitch
          settingKeys={["fastWorktreeCheckout"]}
          checked={settings.fastWorktreeCheckout}
          onCheckedChange={(checked) => updateSettings({ fastWorktreeCheckout: Boolean(checked) })}
          aria-label="Fast worktree checkout"
        />
      }
    />
  );
}
