import { useAtomValue } from "@effect/atom-react";
import {
  AuthTerminalOperateScope,
  type EnvironmentId,
  type PreviewRuntime,
  type PreviewSessionSnapshot,
} from "@onicode/contracts";
import { isLocalLoopbackHost, normalizeHostname } from "@onicode/shared/hostClassification";
import { normalizePreviewUrl } from "@onicode/shared/preview";

import { tunnelsEnvironmentLoopback } from "~/browser/browserTargetResolver";
import { isPreviewSupportedInRuntime } from "~/previewStateStore";
import { appAtomRegistry } from "~/rpc/atomRegistry";
import { primaryEnvironmentIdAtom } from "~/state/primaryEnvironment";
import {
  readEnvironmentSupportsPreviewTunnel,
  readEnvironmentSupportsServerBrowser,
  useEnvironmentSupportsServerBrowser,
} from "~/state/entities";
import { readEnvironmentScope, readPreparedConnection } from "~/state/session";

// What a desktop partition sends through the tunnel; it redirects unspecified addresses to localhost.
const isTunnelledHost = (hostname: string): boolean => {
  const host = normalizeHostname(hostname);
  return (
    isLocalLoopbackHost(host) || host.endsWith(".localhost") || host === "0.0.0.0" || host === "::"
  );
};

const urlPort = (url: URL) => Number(url.port || (url.protocol === "https:" ? 443 : 80));

/** Over SSH the environment's own origin is a local forward, which the desktop dials directly. */
const needsTunnel = (environmentId: EnvironmentId, rawUrl: string): boolean => {
  try {
    const url = new URL(normalizePreviewUrl(rawUrl));
    if (!isTunnelledHost(url.hostname)) return false;
    const origin = readPreparedConnection(environmentId)?.httpBaseUrl;
    if (origin === undefined) return true;
    const environment = new URL(origin);
    return !(isLocalLoopbackHost(environment.hostname) && urlPort(environment) === urlPort(url));
  } catch {
    return true;
  }
};

/**
 * Desktop opens another environment's tabs itself unless they need a tunnel it can't open: hosts
 * from before the tunnel, and sessions without terminal:operate, stream those from the host.
 */
export function previewRuntimeFor(
  environmentId: EnvironmentId,
  url: string | undefined,
): PreviewRuntime | undefined {
  if (!readEnvironmentSupportsServerBrowser(environmentId)) return undefined;
  if (!isPreviewSupportedInRuntime()) return "server";
  if (environmentId === appAtomRegistry.get(primaryEnvironmentIdAtom)) return "server";
  // A blank tab counts as loopback: on another environment that is what one usually opens.
  if (url !== undefined && !needsTunnel(environmentId, url)) return undefined;
  if (tunnelsEnvironmentLoopback(readPreparedConnection(environmentId)?.target) === false) {
    return undefined;
  }
  return readEnvironmentSupportsPreviewTunnel(environmentId) &&
    readEnvironmentScope(environmentId, AuthTerminalOperateScope)
    ? undefined
    : "server";
}

export function alternatePreviewRuntime(
  environmentId: EnvironmentId,
  primaryEnvironmentId: EnvironmentId | null,
  serverBrowser: boolean,
  snapshot: Pick<PreviewSessionSnapshot, "runtime" | "navStatus"> | null | undefined,
): PreviewRuntime | null {
  if (!snapshot || !serverBrowser || !isPreviewSupportedInRuntime()) return null;
  if (environmentId === primaryEnvironmentId) return null;
  if (snapshot.runtime !== "server") return "server";
  const url = snapshot.navStatus._tag === "Idle" ? undefined : snapshot.navStatus.url;
  return previewRuntimeFor(environmentId, url) === undefined ? "desktop" : null;
}

export function isPreviewAvailableFor(environmentId: EnvironmentId): boolean {
  return isPreviewSupportedInRuntime() || readEnvironmentSupportsServerBrowser(environmentId);
}

export function usePreviewAvailable(environmentId: EnvironmentId | null): boolean {
  const serverBrowser = useEnvironmentSupportsServerBrowser(environmentId);
  return isPreviewSupportedInRuntime() || serverBrowser;
}

export function rendersServerTabNatively(
  environmentId: EnvironmentId,
  primaryEnvironmentId: EnvironmentId | null,
  snapshot: Pick<PreviewSessionSnapshot, "runtime" | "headless"> | null | undefined,
): boolean {
  return (
    snapshot?.runtime === "server" &&
    !snapshot.headless &&
    primaryEnvironmentId !== null &&
    environmentId === primaryEnvironmentId
  );
}

export function useRendersServerTabNatively(
  environmentId: EnvironmentId,
  snapshot: Pick<PreviewSessionSnapshot, "runtime" | "headless"> | null | undefined,
): boolean {
  return rendersServerTabNatively(environmentId, useAtomValue(primaryEnvironmentIdAtom), snapshot);
}
