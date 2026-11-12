import Constants from "expo-constants";
import { requireOptionalNativeModule } from "expo";
import { Linking, Platform } from "react-native";

interface AndroidAgentNotifications {
  configure(deviceId: string, scheme: string, ongoingEnabled: boolean): void;
  clear(): void;
  clearEnvironment(environmentId: string): void;
  setThreadOnScreen(path: string | null): void;
  openLiveUpdateSettings(): boolean;
}

const native =
  Platform.OS === "android"
    ? requireOptionalNativeModule<AndroidAgentNotifications>("OniAgentNotifications")
    : null;

export function supportsAndroidAgentNotifications(): boolean {
  return native !== null;
}

function appScheme(): string {
  const scheme = Constants.expoConfig?.scheme;
  return (Array.isArray(scheme) ? scheme[0] : scheme) ?? "onicode";
}

/** The messaging service drops pushes addressed to another device id. */
export function configureAndroidAgentNotifications(
  deviceId: string,
  ongoingEnabled: boolean,
): void {
  native?.configure(deviceId, appScheme(), ongoingEnabled);
}

export function clearAndroidAgentNotifications(): void {
  native?.clear();
}

export function clearAndroidEnvironmentNotifications(environmentId: string): void {
  native?.clearEnvironment(environmentId);
}

export function setAndroidThreadOnScreen(path: string | null): void {
  native?.setThreadOnScreen(path);
}

export function supportsAndroidLiveUpdateSettings(): boolean {
  return native !== null && Number(Platform.Version) >= 36;
}

export async function openAndroidLiveUpdateSettings(): Promise<void> {
  if (!native?.openLiveUpdateSettings()) {
    await Linking.openSettings();
  }
}
