import * as Notifications from "expo-notifications";
import { Platform } from "react-native";

import { subscribeAppActive } from "../../lib/appState";
import { setAndroidThreadOnScreen } from "./androidNotifications";
import { foregroundNotificationBehavior } from "./foregroundNotificationBehavior";
import {
  notificationIdsForThread,
  routeAgentNotificationResponseOnce,
  threadDeepLinkOnScreen,
} from "./notificationPayload";
import {
  consumeLastAgentNotificationResponse,
  NotificationNavigationError,
} from "./notificationResponseConsumer";

let deepLinkOnScreen: string | null = null;

/**
 * Someone looking at a thread has seen its news, so every alert it left behind goes, however they
 * got here. Android cancels its own alerts when it hears which thread is on screen.
 */
function dismissNotificationsOnScreen(): void {
  const deepLink = deepLinkOnScreen;
  setAndroidThreadOnScreen(deepLink);
  if (deepLink === null || Platform.OS !== "ios") return;
  void Notifications.getPresentedNotificationsAsync()
    .then((presented) =>
      Promise.all(
        notificationIdsForThread(presented, deepLink).map((identifier) =>
          Notifications.dismissNotificationAsync(identifier),
        ),
      ),
    )
    .catch((cause: unknown) => {
      console.error(new NotificationNavigationError({ operation: "dismiss", cause }));
    });
}

/** Called with the workspace pathname on every root navigation state change. */
export function setAgentNotificationPathname(pathname: string): void {
  const next = threadDeepLinkOnScreen(pathname);
  if (next === deepLinkOnScreen) return;
  deepLinkOnScreen = next;
  dismissNotificationsOnScreen();
}

/**
 * App-lifetime notification routing: foreground presentation suppresses the
 * thread already on screen, and taps (including the cold-start tap) open the
 * notification's deep link through `navigate`.
 */
export function installAgentNotificationNavigation(navigate: (href: string) => void): () => void {
  const handledResponseIds = new Set<string>();
  Notifications.setNotificationHandler({
    handleNotification: (notification) =>
      Promise.resolve(foregroundNotificationBehavior(notification, deepLinkOnScreen)),
  });

  const handleResponse = (response: Notifications.NotificationResponse): void => {
    routeAgentNotificationResponseOnce({ handledResponseIds, response, navigate });
  };
  const subscription = Notifications.addNotificationResponseReceivedListener(handleResponse);
  void consumeLastAgentNotificationResponse({
    getLastResponse: () => Notifications.getLastNotificationResponseAsync(),
    clearLastResponse: () => Notifications.clearLastNotificationResponseAsync(),
    handleResponse,
  });

  // Alerts for the open thread still arrive while the app is in the background.
  const stopWatchingReturn = subscribeAppActive(dismissNotificationsOnScreen);

  return () => {
    stopWatchingReturn();
    subscription.remove();
    Notifications.setNotificationHandler(null);
  };
}
