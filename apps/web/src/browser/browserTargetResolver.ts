import type {
  BrowserNavigationTarget,
  EnvironmentId,
  PreviewUrlResolution,
} from "@onicode/contracts";
import { isLoopbackHost, normalizePreviewUrl } from "@onicode/shared/preview";
import {
  isLocalLoopbackHost,
  isPrivateNetworkHost,
  normalizeHostname,
} from "@onicode/shared/hostClassification";

import type { ConnectionTarget } from "@onicode/client-runtime/connection";

import { isDesktopLocalConnectionTarget } from "~/connection/desktopLocal";
import { readPreparedConnection } from "~/state/session";

export { normalizeHostname, isLocalLoopbackHost } from "@onicode/shared/hostClassification";

const resolveEnvironmentPortTarget = (
  environmentId: EnvironmentId,
  target: Extract<BrowserNavigationTarget, { readonly kind: "environment-port" }>,
  environmentUrl: URL,
  requestedUrl?: string,
  sourceUrl?: URL,
): PreviewUrlResolution => {
  if (!isPrivateNetworkHost(environmentUrl.hostname)) {
    throw new Error(
      "This environment port needs the planned authenticated preview gateway; its server address is not directly private-network reachable.",
    );
  }
  const protocol = target.protocol ?? "http";
  const path = target.path?.startsWith("/") ? target.path : `/${target.path ?? ""}`;
  const normalizedEnvironmentHost = environmentUrl.hostname.replace(/^\[|\]$/g, "");
  // localhost lets Chromium reach loopback servers bound to either IPv4 or IPv6.
  const resolvedHost = isLocalLoopbackHost(normalizedEnvironmentHost)
    ? "localhost"
    : normalizedEnvironmentHost.includes(":")
      ? `[${normalizedEnvironmentHost}]`
      : normalizedEnvironmentHost;
  const resolved = sourceUrl
    ? new URL(sourceUrl)
    : new URL(path, `${protocol}://${resolvedHost}:${target.port}`);
  if (sourceUrl) {
    resolved.hostname = resolvedHost;
    resolved.port = String(target.port);
  }
  return {
    requestedUrl: requestedUrl ?? `${protocol}://localhost:${target.port}${path}`,
    resolvedUrl: resolved.toString(),
    resolutionKind: isLocalLoopbackHost(normalizedEnvironmentHost)
      ? "direct"
      : "direct-private-network",
    environmentId,
  };
};

/** Every name the desktop's preview tunnel treats as the environment's loopback. */
export const namesLoopback = (host: string): boolean =>
  isLoopbackHost(host) ||
  isLocalLoopbackHost(host) ||
  normalizeHostname(host).endsWith(".localhost");

// Loopback servers on the desktop's own primary and WSL backends answer on this machine's localhost.
const reachesEnvironmentLoopback = (target: ConnectionTarget): boolean =>
  target._tag === "PrimaryConnectionTarget" || isDesktopLocalConnectionTarget(target);

/**
 * Desktop carries every other environment's loopback traffic through the preview tunnel. Undefined
 * until the catalog knows the environment's target.
 */
export const tunnelsEnvironmentLoopback = (
  target: ConnectionTarget | undefined,
): boolean | undefined => (target === undefined ? undefined : !reachesEnvironmentLoopback(target));

/** The URL an in-app tab loads. A desktop webview keeps localhost; its partition reaches the environment. */
export const resolveDiscoveredServerUrl = (rawUrl: string): string => {
  try {
    return normalizePreviewUrl(rawUrl);
  } catch {
    return rawUrl;
  }
};

/**
 * The URL for the system browser, which has no tunnel: null when it has no route to the
 * environment's loopback, since localhost there would be a different server on this computer.
 */
export const resolveExternalServerUrl = (
  environmentId: EnvironmentId,
  rawUrl: string,
): string | null => {
  let parsed: URL;
  try {
    parsed = new URL(normalizePreviewUrl(rawUrl));
  } catch {
    return rawUrl;
  }
  if (!namesLoopback(parsed.hostname)) return parsed.toString();
  const connection = readPreparedConnection(environmentId);
  if (!connection) return null;
  if (reachesEnvironmentLoopback(connection.target)) return parsed.toString();
  // An SSH forward's loopback address is this computer, not the environment.
  if (connection.target._tag === "SshConnectionTarget") return null;
  try {
    return resolveEnvironmentPortTarget(
      environmentId,
      {
        kind: "environment-port",
        port: Number(parsed.port || (parsed.protocol === "https:" ? 443 : 80)),
        protocol: parsed.protocol === "https:" ? "https" : "http",
        path: `${parsed.pathname}${parsed.search}${parsed.hash}`,
      },
      new URL(connection.httpBaseUrl),
      rawUrl,
      parsed,
    ).resolvedUrl;
  } catch {
    return null;
  }
};
