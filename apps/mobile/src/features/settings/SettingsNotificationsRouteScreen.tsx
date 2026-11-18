import { useAtomSet, useAtomValue } from "@effect/atom-react";
import type { EnvironmentId } from "@onicode/contracts";
import * as Effect from "effect/Effect";
import { msg, T, useGT, useMessages } from "gt-react-native";
import { AsyncResult } from "effect/reactivity";
import * as Notifications from "expo-notifications";
import { Linking, Platform } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";

import type { AppSymbolName } from "../../components/ui/AppSymbol";
import { AppText as Text } from "../../components/ui/AppText";
import * as Dialog from "../../components/ui/Dialog";
import type { MessageResolver } from "../../i18n/messages";
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
    label: msg("Approval", { $context: "notification kind" }),
    subtitle: msg("An agent asks to run a command or edit."),
  },
  {
    key: "notifyOnInput",
    icon: "questionmark.bubble",
    label: msg("Input", { $context: "notification kind: agent needs user input" }),
    subtitle: msg("An agent asks you a question."),
  },
  {
    key: "notifyOnCompletion",
    icon: "checkmark.circle",
    label: msg("Completion", { $context: "notification kind" }),
    subtitle: msg("A turn finishes."),
  },
  {
    key: "notifyOnFailure",
    icon: "exclamationmark.triangle",
    label: msg("Failure", { $context: "notification kind" }),
    subtitle: msg("A turn fails."),
  },
];

const ACTIVITY_LABEL =
  Platform.OS === "ios"
    ? msg("Live Activity")
    : supportsAndroidLiveUpdateSettings()
      ? msg("Live Updates")
      : msg("Ongoing notification");

function environmentStatusText(
  status: AgentAwarenessEnvironmentStatus | undefined,
  resolve: MessageResolver,
): string {
  switch (status?._tag) {
    case undefined:
      return resolve(msg("Not registered"));
    case "pending":
      return resolve(msg("Registering…"));
    case "registered":
      return resolve(msg("Registered"));
    case "gateway-not-allowed":
      return resolve(msg("This environment uses another push gateway"));
    case "unreachable":
      return resolve(msg("Unreachable: {message}", { message: status.message }));
    case "failed":
      return resolve(msg("Failed: {message}", { message: status.message }));
  }
}

function permissionText(
  permission: NotificationPermission,
  resolve: MessageResolver,
): string | undefined {
  switch (permission) {
    case "denied":
      return resolve(
        msg("Notifications are off for {product} in system Settings.", { product: "Onicode" }),
      );
    case "ask":
      return resolve(msg("{product} will ask to send notifications.", { product: "Onicode" }));
    default:
      return undefined;
  }
}

function showTestResult(environmentId: EnvironmentId, resolve: MessageResolver) {
  void sendAgentNotificationTest(environmentId).then((result) => {
    switch (result.outcome) {
      case "delivered":
        Dialog.alert(resolve(msg("Test sent")), resolve(msg("A test notification is on its way.")));
        return;
      case "unregistered":
        Dialog.alert(
          resolve(msg("Not registered")),
          resolve(msg("This environment has no registration for this phone.")),
        );
        return;
      case "failed":
        Dialog.alert(
          resolve(msg("Test failed")),
          result.detail ?? resolve(msg("The environment could not deliver it.")),
        );
    }
  });
}

export function SettingsNotificationsRouteScreen() {
  const gt = useGT();
  const m = useMessages();
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
        gt("Notifications are off"),
        gt("Allow notifications for {product} in system Settings, then turn this on again.", {
          product: "Onicode",
        }),
        [
          { text: gt("Cancel"), style: "cancel" },
          { text: gt("Open Settings"), onPress: () => void Linking.openSettings() },
        ],
      );
    });
  };

  return (
    <SettingsScreen title={gt("Notifications")}>
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
        <SettingsSection title={gt("This phone")}>
          <SettingsSwitchRow
            icon="bell.badge"
            label={gt("Notifications")}
            subtitle={enabled ? permissionText(permission, m) : undefined}
            disabled={!available || !ready || permission === "checking"}
            value={enabled}
            onValueChange={handleEnabledChange}
          />
          <SettingsSwitchRow
            icon="bolt.circle"
            label={m(ACTIVITY_LABEL)}
            subtitle={gt("Shows running agents on the Lock Screen.")}
            disabled={!available || !ready}
            value={preferences.activity}
            onValueChange={(activity) => save({ activity })}
          />
          {supportsAndroidLiveUpdateSettings() ? (
            <SettingsRow
              icon="gearshape"
              label={gt("Live Update Settings")}
              onPress={() => void openAndroidLiveUpdateSettings().catch(() => undefined)}
            />
          ) : null}
        </SettingsSection>
        <SettingsSection title={gt("Notify me about")}>
          {KINDS.map((kind) => (
            <SettingsSwitchRow
              key={kind.key}
              icon={kind.icon}
              label={m(kind.label)}
              subtitle={m(kind.subtitle)}
              disabled={!available || !ready}
              value={preferences[kind.key]}
              onValueChange={(value) => save({ [kind.key]: value })}
            />
          ))}
        </SettingsSection>
        {enabled && environments.length > 0 ? (
          <SettingsSection title={gt("Environments")}>
            {environments.map((environment) => {
              const environmentStatus = status.environments.get(environment.environmentId);
              return (
                <SettingsRow
                  key={environment.environmentId}
                  icon="desktopcomputer"
                  label={environment.label}
                  value={environmentStatusText(environmentStatus, m)}
                  valuePosition="below"
                  disabled={environmentStatus?._tag !== "registered"}
                  onPress={() => showTestResult(environment.environmentId, m)}
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
            <T>Tap a registered environment to send a test notification.</T>
          </Text>
        ) : null}
      </ScrollView>
    </SettingsScreen>
  );
}
