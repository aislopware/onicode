import { useAtomValue } from "@effect/atom-react";
import type { DesktopBridge, RemoteDesktopHostCapabilities } from "@onicode/contracts";
import { useEffect, useState } from "react";

import { useClientSettings, useUpdatePrimarySettings } from "../../hooks/useSettings";
import { remoteDesktopHost, remoteDesktopHostSessionsAtom } from "../../state/remoteDesktop";
import { Button } from "../ui/button";
import { Switch } from "../ui/switch";
import { SettingResetButton, SettingsRow, SettingsSection } from "./settingsLayout";
import { searchableSetting } from "./settingsSearch";

function permissionLines(capabilities: RemoteDesktopHostCapabilities | null): string[] {
  if (capabilities === null) return ["Checking permissions…"];
  return [
    ...(capabilities.screenRecording === null
      ? []
      : [`Screen Recording: ${capabilities.screenRecording ? "allowed" : "not allowed"}`]),
    capabilities.inputControl === null
      ? "Control: view only on this operating system"
      : `Accessibility, for control: ${capabilities.inputControl ? "allowed" : "not allowed"}`,
    ...(capabilities.clipboardRead === false
      ? [
          "Clipboard: macOS asks before each read. To send what you copy here to viewers, allow Onicode under Privacy & Security → Paste from Other Apps.",
        ]
      : []),
  ];
}

/** This machine's side of remote desktop: whether it shares its screen, and who is watching. */
export function RemoteDesktopHostSettings(props: {
  readonly bridge: DesktopBridge["remoteDesktop"];
  /** Client names by auth session id, from this environment's access snapshot. */
  readonly clientLabels: ReadonlyMap<string, string>;
}) {
  const { bridge } = props;
  const enabled = useClientSettings((settings) => settings.remoteDesktopHostEnabled);
  const updateSettings = useUpdatePrimarySettings();
  const sessions = useAtomValue(remoteDesktopHostSessionsAtom);
  const [capabilities, setCapabilities] = useState<RemoteDesktopHostCapabilities | null>(null);
  const [requesting, setRequesting] = useState(false);

  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- reads OS permissions and displays, which change in System Settings outside React
  useEffect(() => {
    if (!enabled) return;
    let active = true;
    const refresh = () => {
      void bridge.capabilities().then((next) => {
        if (active) setCapabilities(next);
      });
    };
    refresh();
    window.addEventListener("focus", refresh);
    return () => {
      active = false;
      window.removeEventListener("focus", refresh);
    };
  }, [bridge, enabled]);

  const requestPermissions = async () => {
    setRequesting(true);
    const next = await bridge.requestPermissions().catch(() => null);
    setRequesting(false);
    if (next) setCapabilities(next);
  };
  const missingPermission =
    capabilities !== null &&
    (capabilities.screenRecording === false || capabilities.inputControl === false);

  return (
    <SettingsSection id="remote-desktop" title="Remote desktop">
      <SettingsRow
        {...searchableSetting("remote-desktop-host")}
        description="Clients that can already use this machine's terminal can see and control its screen. Keep the Onicode window open; while this is on, closing it on macOS only hides it."
        resetAction={
          enabled ? (
            <SettingResetButton
              label="screen sharing"
              onClick={() => updateSettings({ remoteDesktopHostEnabled: false })}
            />
          ) : null
        }
        control={
          <Switch
            checked={enabled}
            aria-label="Share this screen"
            onCheckedChange={(checked) => {
              updateSettings({ remoteDesktopHostEnabled: checked });
              if (checked) void requestPermissions();
            }}
          />
        }
      />
      {enabled ? (
        <>
          <SettingsRow
            title="Permissions"
            description={permissionLines(capabilities).map((line) => (
              <span key={line} className="block">
                {line}
              </span>
            ))}
            control={
              missingPermission ? (
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
          {sessions.length === 0 ? (
            <SettingsRow title="Viewers" description="No one is viewing this screen." />
          ) : (
            sessions.map((session) => (
              <SettingsRow
                key={session.id}
                title={props.clientLabels.get(session.viewer) ?? "Client"}
                description={
                  capabilities?.displays.find((display) => display.id === session.displayId)
                    ?.label ?? "Viewing this screen"
                }
                control={
                  <Button
                    size="xs"
                    variant="outline"
                    onClick={() => remoteDesktopHost()?.disconnect(session.id)}
                  >
                    Disconnect
                  </Button>
                }
              />
            ))
          )}
        </>
      ) : null}
    </SettingsSection>
  );
}
