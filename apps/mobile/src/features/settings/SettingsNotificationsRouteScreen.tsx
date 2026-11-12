import { useAtomSet, useAtomValue } from "@effect/atom-react";
import type { EnvironmentId } from "@onicode/contracts";
import * as Effect from "effect/Effect";
import { AsyncResult } from "effect/reactivity";
import * as Notifications from "expo-notifications";
import { Linking, Platform } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";

import type { AppSymbolName } from "../../components/ui/AppSymbol";
import { AppText as Text } from "../../components/ui/AppText";
import * as Dialog from "../../components/ui/Dialog";
import { ScreenScrollView as ScrollView } from "../../components/ui/ScreenScrollView";
import { makePermissionStatusAtom } from "../../lib/permissionStatusAtom";
import type { AgentNotificationPreferences } from "../../persistence/mobile-preferences";
import { useEnvironments } from "../../state/environments";
import { mobilePreferencesAtom, updateMobilePreferencesAtom } from "../../state/preferences";
import {
  agentAwarenessStatusAtom,
  DEFAULT_AGENT_NOTIFICATION_PREFERENCES,
  sendAgentNotificationTest,
  type AgentAwarenessEnvironmentStatus,
} from "../agent-awareness/agentAwareness";
import {
  openAndroidLiveUpdateSettings,
  supportsAndroidLiveUpdateSettings,
} from "../agent-awareness/androidNotifications";
import { requestAgentNotificationPermission } from "../agent-awareness/notificationPermissions";
import { SettingsRow } from "./components/SettingsRow";
import { SettingsScreen } from "./components/SettingsScreen";
import { SettingsSection } from "./components/SettingsSection";
import { SettingsSwitchRow } from "./components/SettingsSwitchRow";

type NotificationPermission = "checking" | "granted" | "ask" | "denied";

const notificationPermissionAtom = makePermissionStatusAtom<NotificationPermission>({
  label: "mobile:notification-permission",
  initial: "checking",
  read: () =>
    Notifications.getPermissionsAsync().then(
      (status): NotificationPermission =>
        status.granted ? "granted" : status.canAskAgain ? "ask" : "denied",
      (): NotificationPermission => "ask",
    ),
});

const KINDS: ReadonlyArray<{
  readonly key: "notifyOnApproval" | "notifyOnInput" | "notifyOnCompletion" | "notifyOnFailure";
  readonly icon: AppSymbolName;
  readonly label: string;
  readonly subtitle: string;
}> = [
  {
    key: "notifyOnApproval",
    icon: "exclamationmark.shield",
    label: "Approval",
    subtitle: "An agent asks to run a command or edit.",
  },
  {
    key: "notifyOnInput",
    icon: "questionmark.bubble",
    label: "Input",
    subtitle: "An agent asks you a question.",
  },
  {
    key: "notifyOnCompletion",
    icon: "checkmark.circle",
    label: "Completion",
    subtitle: "A turn finishes.",
  },
  {
    key: "notifyOnFailure",
    icon: "exclamationmark.triangle",
    label: "Failure",
    subtitle: "A turn fails.",
  },
];

const ACTIVITY_LABEL =
  Platform.OS === "ios"
    ? "Live Activity"
    : supportsAndroidLiveUpdateSettings()
      ? "Live Updates"
      : "Ongoing notification";

function environmentStatusText(status: AgentAwarenessEnvironmentStatus | undefined): string {
  switch (status?._tag) {
    case undefined:
      return "Not registered";
    case "pending":
      return "Registering…";
    case "registered":
      return "Registered";
    case "gateway-not-allowed":
      return "This environment uses another push gateway";
    case "unreachable":
      return `Unreachable: ${status.message}`;
    case "failed":
      return `Failed: ${status.message}`;
  }
}

function permissionText(permission: NotificationPermission): string | undefined {
  switch (permission) {
    case "denied":
      return "Notifications are off for Onicode in system Settings.";
    case "ask":
      return "Onicode will ask to send notifications.";
    default:
      return undefined;
  }
}

function showTestResult(environmentId: EnvironmentId) {
  void sendAgentNotificationTest(environmentId).then((result) => {
    switch (result.outcome) {
      case "delivered":
        Dialog.alert("Test sent", "A test notification is on its way.");
        return;
      case "unregistered":
        Dialog.alert("Not registered", "This environment has no registration for this phone.");
        return;
      case "failed":
        Dialog.alert("Test failed", result.detail ?? "The environment could not deliver it.");
    }
  });
}

export function SettingsNotificationsRouteScreen() {
  const insets = useSafeAreaInsets();
  const preferencesResult = useAtomValue(mobilePreferencesAtom);
  const savePreferences = useAtomSet(updateMobilePreferencesAtom);
  const permission = useAtomValue(notificationPermissionAtom);
  const setPermission = useAtomSet(notificationPermissionAtom);
  const status = useAtomValue(agentAwarenessStatusAtom);
  const { environments } = useEnvironments();

  const ready = AsyncResult.isSuccess(preferencesResult);
  const preferences =
    (ready ? preferencesResult.value.agentNotifications : undefined) ??
    DEFAULT_AGENT_NOTIFICATION_PREFERENCES;
  const available = status.unavailable === null;
  const enabled = available && preferences.enabled;

  const save = (patch: Partial<AgentNotificationPreferences>) =>
    savePreferences({ agentNotifications: { ...preferences, ...patch } });

  const handleEnabledChange = (next: boolean) => {
    if (!next) {
      save({ enabled: false });
      return;
    }
    void Effect.runPromise(
      requestAgentNotificationPermission.pipe(
        Effect.orElseSucceed(() => ({ type: "denied", canAskAgain: true }) as const),
      ),
    ).then((result) => {
      if (result.type === "granted") {
        setPermission("granted");
        save({ enabled: true });
        return;
      }
      setPermission(result.type === "denied" && !result.canAskAgain ? "denied" : "ask");
      Dialog.alert(
        "Notifications are off",
        "Allow notifications for Onicode in system Settings, then turn this on again.",
        [
          { text: "Cancel", style: "cancel" },
          { text: "Open Settings", onPress: () => void Linking.openSettings() },
        ],
      );
    });
  };

  return (
    <SettingsScreen title="Notifications">
      <ScrollView
        contentInsetAdjustmentBehavior="automatic"
        showsVerticalScrollIndicator={false}
        className="flex-1"
        contentContainerClassName="gap-6 px-5 pt-4"
        contentContainerStyle={{ paddingBottom: Math.max(insets.bottom, 18) + 18 }}
      >
        {status.unavailable !== null ? (
          <Text className="px-2 text-sm text-foreground-muted">{status.unavailable}</Text>
        ) : null}
        <SettingsSection title="This phone">
          <SettingsSwitchRow
            icon="bell.badge"
            label="Notifications"
            subtitle={enabled ? permissionText(permission) : undefined}
            disabled={!available || !ready || permission === "checking"}
            value={enabled}
            onValueChange={handleEnabledChange}
          />
          <SettingsSwitchRow
            icon="bolt.circle"
            label={ACTIVITY_LABEL}
            subtitle="Shows running agents on the Lock Screen."
            disabled={!available || !ready}
            value={preferences.activity}
            onValueChange={(activity) => save({ activity })}
          />
          {supportsAndroidLiveUpdateSettings() ? (
            <SettingsRow
              icon="gearshape"
              label="Live Update Settings"
              onPress={() => void openAndroidLiveUpdateSettings().catch(() => undefined)}
            />
          ) : null}
        </SettingsSection>
        <SettingsSection title="Notify me about">
          {KINDS.map((kind) => (
            <SettingsSwitchRow
              key={kind.key}
              icon={kind.icon}
              label={kind.label}
              subtitle={kind.subtitle}
              disabled={!available || !ready}
              value={preferences[kind.key]}
              onValueChange={(value) => save({ [kind.key]: value })}
            />
          ))}
        </SettingsSection>
        {enabled && environments.length > 0 ? (
          <SettingsSection title="Environments">
            {environments.map((environment) => {
              const environmentStatus = status.environments.get(environment.environmentId);
              return (
                <SettingsRow
                  key={environment.environmentId}
                  icon="desktopcomputer"
                  label={environment.label}
                  value={environmentStatusText(environmentStatus)}
                  valuePosition="below"
                  disabled={environmentStatus?._tag !== "registered"}
                  onPress={() => showTestResult(environment.environmentId)}
                />
              );
            })}
          </SettingsSection>
        ) : null}
        {enabled && status.error !== null ? (
          <Text className="px-2 text-sm text-foreground-muted">{status.error}</Text>
        ) : null}
        {enabled ? (
          <Text className="px-2 text-sm text-foreground-muted">
            Tap a registered environment to send a test notification.
          </Text>
        ) : null}
      </ScrollView>
    </SettingsScreen>
  );
}
