import type { PreviewSessionSnapshot } from "@onicode/contracts";
import { isLocalLoopbackHost, normalizeHostname } from "@onicode/shared/hostClassification";
import { msg } from "gt-react-native";

import { m, type MessageResolver } from "../../i18n/messages";

export function latestBrowserTab(tabs: ReadonlyArray<PreviewSessionSnapshot>) {
  return tabs.reduce<PreviewSessionSnapshot | null>(
    (latest, tab) => (latest === null || tab.updatedAt > latest.updatedAt ? tab : latest),
    null,
  );
}

export function browserTabUrl(tab: PreviewSessionSnapshot) {
  return tab.navStatus._tag === "Idle" ? "" : tab.navStatus.url;
}

export function browserTabTitle(tab: PreviewSessionSnapshot, resolve: MessageResolver = m) {
  if (tab.navStatus._tag === "Idle") return resolve(msg("New tab"));
  if (tab.navStatus.title.trim()) return tab.navStatus.title;
  try {
    return new URL(tab.navStatus.url).host || tab.navStatus.url;
  } catch {
    return tab.navStatus.url;
  }
}

/** The tab's URL for this device's tunnelled webview, when it is one of the environment's loopback pages. */
export function loopbackUrlForDevice(value: string): string | null {
  try {
    const url = new URL(value);
    if (!/^https?:$/.test(url.protocol)) return null;
    // The unspecified address would name this device's own interfaces, so use localhost instead.
    if (url.hostname === "0.0.0.0" || url.hostname === "[::]") url.hostname = "localhost";
    // One trailing dot, as the native tunnels and the desktop app's read it.
    const loopback =
      !url.hostname.endsWith("..") &&
      (isLocalLoopbackHost(url.hostname) || normalizeHostname(url.hostname).endsWith(".localhost"));
    return loopback ? url.toString() : null;
  } catch {
    return null;
  }
}

/**
 * Whether this device's webview reaches the environment's server at `value` itself. iOS carries
 * only the environment's `localhost`, which the host dials at 127.0.0.1 and then ::1, so another
 * loopback address there, ::1 included, could reach a different server.
 */
export function tunnelReaches(value: string, platform: string): boolean {
  const url = loopbackUrlForDevice(value);
  if (url === null) return false;
  const host = normalizeHostname(new URL(url).hostname);
  const isAddress = /^[\d.]+$/.test(host) || host.includes(":");
  return platform !== "ios" || !isAddress || host === "127.0.0.1";
}
