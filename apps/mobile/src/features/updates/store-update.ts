import Constants from "expo-constants";
import * as SecureStore from "expo-secure-store";
import * as Updates from "expo-updates";
import { msg } from "gt-react-native";
// Metro namespace imports evaluate unrelated React Native getters, including unavailable native modules.
import { AppState, Platform } from "react-native";

import * as Dialog from "../../components/ui/Dialog";
import { m } from "../../i18n/messages";
import { tryOpenExternalUrl } from "../../lib/openExternalUrl";
import { whenProtectedDataAvailable } from "../../lib/protectedData";
import { isAppUpdateCheckAvailable, NOTHING_IS_LOST, runAppUpdateCheck } from "./app-updates";
import {
  isStoreUpdateSnoozed,
  parseMinimumSupportedVersion,
  parseStoreBuildFeed,
  resolveStoreUpdateOffer,
  STORE_UPDATE_SNOOZE_MS,
  type StoreFeed,
  type StoreUpdateSnooze,
} from "./store-update.logic";

interface StoreUpdateConfig {
  readonly feedsUrl: string;
  readonly storeUrl: string;
  readonly storeName: string;
}

function readStoreUpdateConfig(): StoreUpdateConfig | null {
  const extra: unknown = Constants.expoConfig?.extra?.storeUpdate;
  if (typeof extra !== "object" || extra === null) return null;
  const { feedsUrl, iosUrl, androidUrl } = extra as Record<string, unknown>;
  const storeUrl = Platform.OS === "ios" ? iosUrl : androidUrl;
  if (typeof feedsUrl !== "string" || typeof storeUrl !== "string") return null;
  const storeName =
    Platform.OS === "android"
      ? "Google Play"
      : storeUrl.includes("beta.itunes.apple.com")
        ? "TestFlight"
        : "App Store";
  return { feedsUrl: feedsUrl.replace(/\/+$/u, ""), storeUrl, storeName };
}

const FEED_TIMEOUT_MS = 10_000;

/** Null for anything but a 200: a feed that cannot be read never blocks or nags. */
async function fetchFeedText(url: string): Promise<string | null> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), FEED_TIMEOUT_MS);
  try {
    const response = await fetch(url, { signal: controller.signal });
    return response.ok ? await response.text() : null;
  } catch {
    return null;
  } finally {
    clearTimeout(timer);
  }
}

async function fetchStoreFeed(feedsUrl: string): Promise<StoreFeed> {
  const [store, desktop] = await Promise.all([
    fetchFeedText(`${feedsUrl}/${Platform.OS}/store.json`),
    fetchFeedText(`${feedsUrl}/macos/latest-mac.yml`),
  ]);
  return {
    latest: parseStoreBuildFeed(store),
    minimumSupportedVersion: parseMinimumSupportedVersion(desktop),
  };
}

const SNOOZE_STORAGE_KEY = "onicode.store-update-snooze.v1";

async function loadSnooze(): Promise<StoreUpdateSnooze | null> {
  try {
    await whenProtectedDataAvailable();
    const raw = await SecureStore.getItemAsync(SNOOZE_STORAGE_KEY);
    const value: unknown = raw === null ? null : JSON.parse(raw);
    if (typeof value !== "object" || value === null) return null;
    const { version, until } = value as Record<string, unknown>;
    return typeof version === "string" && typeof until === "number" ? { version, until } : null;
  } catch {
    return null;
  }
}

const RECHECK_AFTER_MS = 15 * 60 * 1000;
const FOREGROUND_RECHECK_MS = 60 * 60 * 1000;
// How long the store takes to come forward after its link opens.
const STORE_HANDOFF_MS = 3_000;

/**
 * App-lifetime watch for a newer store binary: offers it at launch and on return to the app, and
 * keeps a required one on screen until the app is updated.
 */
export function installStoreUpdateChecks(): () => void {
  const config = readStoreUpdateConfig();
  const appVersion = Constants.expoConfig?.version;
  if (config === null || appVersion === undefined) return () => undefined;

  let disposed = false;
  let busy = false;
  let lastCheckedAt = 0;
  let required = false;
  let appliedOverTheAir = false;

  const openStore = () => {
    void tryOpenExternalUrl(config.storeUrl);
  };

  const check = async () => {
    if (disposed || busy) return;
    busy = true;
    lastCheckedAt = Date.now();
    const feed = await fetchStoreFeed(config.feedsUrl);
    const offer = resolveStoreUpdateOffer({
      appVersion,
      runtimeVersion: Updates.runtimeVersion,
      feed,
    });
    required = offer.kind === "required" && offer.via === "store";
    if (disposed || offer.kind === "none") {
      busy = false;
      return;
    }
    if (offer.kind === "required" && offer.via === "ota") {
      busy = false;
      // Once per launch: a feed that asks for an update nobody published must not loop.
      if (appliedOverTheAir || !isAppUpdateCheckAvailable()) return;
      appliedOverTheAir = true;
      await runAppUpdateCheck({ applyMode: "immediate" });
      return;
    }
    const { version } = offer;
    const values = { version, store: config.storeName };
    if (offer.kind === "required") {
      Dialog.alert(
        m(msg("Update required")),
        `${m(msg("This version of Onicode is no longer supported. Install {version} from {store} to keep working.", values))} ${m(NOTHING_IS_LOST)}`,
        [
          {
            text: m(msg("Update")),
            onPress: () => {
              busy = false;
              openStore();
              // Still here means the store did not open, so the requirement comes back.
              setTimeout(() => {
                if (AppState.currentState === "active") void check();
              }, STORE_HANDOFF_MS);
            },
          },
        ],
        { cancelable: false },
      );
      return;
    }
    if (isStoreUpdateSnoozed(await loadSnooze(), version, Date.now())) {
      busy = false;
      return;
    }
    const snooze = () => {
      busy = false;
      void SecureStore.setItemAsync(
        SNOOZE_STORAGE_KEY,
        JSON.stringify({ version, until: Date.now() + STORE_UPDATE_SNOOZE_MS }),
      ).catch(() => {});
    };
    Dialog.alert(
      m(msg("Onicode {version} is available", values)),
      `${m(msg("Update from {store} to get it.", values))} ${m(NOTHING_IS_LOST)}`,
      [
        { text: m(msg("Later")), style: "cancel", onPress: snooze },
        {
          text: m(msg("Update")),
          onPress: () => {
            snooze();
            openStore();
          },
        },
      ],
      { cancelable: true, onDismiss: snooze },
    );
  };

  void check();
  const subscription = AppState.addEventListener("change", (state) => {
    if (state !== "active") return;
    if (required || Date.now() - lastCheckedAt >= RECHECK_AFTER_MS) void check();
  });
  // A tablet left open never returns from the background.
  const timer = setInterval(() => {
    if (AppState.currentState === "active") void check();
  }, FOREGROUND_RECHECK_MS);
  return () => {
    disposed = true;
    subscription.remove();
    clearInterval(timer);
  };
}
