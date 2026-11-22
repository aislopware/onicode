import { useAtomValue } from "@effect/atom-react";
import type { EnvironmentId } from "@onicode/contracts";
import { useGT } from "gt-react";
import { useState } from "react";

import { serverEnvironment } from "../../state/server";
import { useAtomCommand } from "../../state/use-atom-command";
import { Switch } from "../ui/switch";
import { SettingsRow, SettingsSection } from "./settingsLayout";
import { useSearchableSetting } from "./settingsSearch";
import { useSettingsScope } from "./SettingsScopeContext";
import { SettingsScopeNotice } from "./SettingsScopeNotice";
import { useScopedSettings } from "./useScopedSettings";

/**
 * Consent to Sentry reports. This app's reports follow a desktop setting on this device; each
 * environment's server follows its own setting. Both apply on restart.
 */
export function ErrorReportingSettings() {
  const gt = useGT();
  const { environment, scope } = useSettingsScope();
  const environmentId = scope.kind === "environment" ? (environment?.environmentId ?? null) : null;
  return (
    <>
      <SettingsSection title={gt("Error reports")}>
        <AppErrorReportingRow />
        {environmentId === null ? null : <ServerErrorReportingRow environmentId={environmentId} />}
      </SettingsSection>
      {environmentId === null ? (
        <SettingsSection title={gt("Server error reports")} hideTitle>
          <SettingsScopeNotice target="environment">
            {gt("Choose one environment to change its server's error reports.")}
          </SettingsScopeNotice>
        </SettingsSection>
      ) : null}
    </>
  );
}

function AppErrorReportingRow() {
  const gt = useGT();
  const setting = useSearchableSetting();
  const bridge = window.desktopBridge;
  const [state, setState] = useState(() => bridge?.getErrorReporting?.() ?? null);
  const [saving, setSaving] = useState(false);
  if (!bridge) return null;
  return (
    <SettingsRow
      {...setting("error-reporting")}
      description={
        state
          ? gt(
              "Send this app's crashes, errors, sampled performance traces, and a screen replay around each error to the publisher of this Onicode build. Replays mask all text, media, and links.",
            )
          : gt("This build of Onicode does not send error reports.")
      }
      status={state && state.running !== state.saved ? gt("Restart Onicode to apply.") : undefined}
      control={
        <Switch
          aria-label={gt("Send error reports from this app")}
          checked={state?.saved === true}
          disabled={state === null || saving}
          onCheckedChange={async (checked) => {
            setSaving(true);
            await bridge.setErrorReportingEnabled?.(checked).catch(() => undefined);
            setState(bridge.getErrorReporting?.() ?? null);
            setSaving(false);
          }}
        />
      }
    />
  );
}

function ServerErrorReportingRow({ environmentId }: { readonly environmentId: EnvironmentId }) {
  const gt = useGT();
  const setting = useSearchableSetting();
  const { environment } = useSettingsScope();
  const saved = useScopedSettings((settings) => settings.observability.errorReporting);
  const [saving, setSaving] = useState(false);
  const canEdit = useAtomValue(serverEnvironment.updateSettings.permissionAtom(environmentId));
  const updateSettings = useAtomCommand(serverEnvironment.updateSettings, { reportFailure: true });
  const running = environment?.serverConfig?.observability.errorReporting;
  const available = running !== undefined;

  return (
    <SettingsRow
      {...setting("server-error-reporting")}
      description={
        available
          ? gt(
              "Send this server's crashes, errors, and sampled performance traces to the publisher of its Onicode build.",
            )
          : gt("This server's build of Onicode does not send error reports.")
      }
      status={available && running !== saved ? gt("Restart the server to apply.") : undefined}
      control={
        <Switch
          aria-label={gt("Send error reports from this server")}
          checked={available && saved}
          disabled={!available || !canEdit || saving}
          onCheckedChange={async (checked) => {
            setSaving(true);
            await updateSettings({
              environmentId,
              input: { patch: { observability: { errorReporting: checked } } },
            });
            setSaving(false);
          }}
        />
      }
    />
  );
}
