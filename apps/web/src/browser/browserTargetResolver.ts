import type {
  BrowserNavigationTarget,
  EnvironmentId,
  PreviewUrlResolution,
} from "@onicode/contracts";
import { isLoopbackHost, normalizePreviewUrl } from "@onicode/shared/preview";
import { isLocalLoopbackHost, isPrivateNetworkHost } from "@onicode/shared/hostClassification";

import type { ConnectionTarget } from "@onicode/client-runtime/connection";

import { isDesktopLocalConnectionTarget } from "~/connection/desktopLocal";
import { readPreparedConnection } from "~/state/session";

export { normalizeHostname, isLocalLoopbackHost } from "@onicode/shared/hostClassification";

const readEnvironmentUrl = (environmentId: EnvironmentId): URL => {
  const connection = readPreparedConnection(environmentId);
  if (!connection) throw new Error(`Environment ${environmentId} is not connected.`);
  return new URL(connection.httpBaseUrl);
};

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

// Loopback servers on the desktop's own primary and WSL backends answer on this machine's localhost.
const reachesEnvironmentLoopback = (target: ConnectionTarget): boolean =>
  target._tag === "PrimaryConnectionTarget" || isDesktopLocalConnectionTarget(target);

/** Desktop carries every other environment's loopback traffic through the preview tunnel. */
export const tunnelsEnvironmentLoopback = (target: ConnectionTarget | undefined): boolean =>
  !(target !== undefined && reachesEnvironmentLoopback(target));

const resolveLoopbackUrl = (
  environmentId: EnvironmentId,
  rawUrl: string,
  tunnelled: boolean,
): string => {
  try {
    const normalizedUrl = normalizePreviewUrl(rawUrl);
    const parsed = new URL(normalizedUrl);
    if (tunnelled || !isLoopbackHost(parsed.hostname)) return normalizedUrl;
    const target = readPreparedConnection(environmentId)?.target;
    if (target && reachesEnvironmentLoopback(target)) return normalizedUrl;
    return resolveEnvironmentPortTarget(
      environmentId,
      {
        kind: "environment-port",
        port: Number(parsed.port || (parsed.protocol === "https:" ? 443 : 80)),
        protocol: parsed.protocol === "https:" ? "https" : "http",
        path: `${parsed.pathname}${parsed.search}${parsed.hash}`,
      },
      readEnvironmentUrl(environmentId),
      rawUrl,
      parsed,
    ).resolvedUrl;
  } catch {
    return rawUrl;
  }
};

/** The URL an in-app tab loads. A desktop webview keeps localhost; its partition reaches the environment. */
export const resolveDiscoveredServerUrl = (environmentId: EnvironmentId, rawUrl: string): string =>
  resolveLoopbackUrl(environmentId, rawUrl, true);

/** The URL for the system browser, which has no tunnel and needs an address it can reach itself. */
export const resolveExternalServerUrl = (environmentId: EnvironmentId, rawUrl: string): string =>
  resolveLoopbackUrl(environmentId, rawUrl, false);
