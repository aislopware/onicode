import type { RemoteDesktopViewerStats } from "@onicode/client-runtime/remote-desktop/viewer";
import type { EnvironmentId, RemoteDesktopHost, RemoteDesktopState } from "@onicode/contracts";

interface RemoteDesktopEnvironment {
  readonly environmentId: EnvironmentId;
  readonly connection: { readonly phase: string };
  readonly serverConfig: { readonly remoteDesktop?: boolean } | null;
}

/**
 * Whether to offer "Open remote desktop" for an environment. The primary environment is this
 * app's own machine, which would show this window inside itself; dev builds allow it for testing.
 */
export function canOpenRemoteDesktop(
  environment: RemoteDesktopEnvironment,
  primaryEnvironmentId: EnvironmentId | null,
  allowPrimary: boolean,
): boolean {
  return (
    environment.connection.phase === "connected" &&
    environment.serverConfig?.remoteDesktop === true &&
    (allowPrimary || environment.environmentId !== primaryEnvironmentId)
  );
}

export type RemoteDesktopPageState =
  | { readonly kind: "unknown-environment" }
  | { readonly kind: "disconnected" }
  | { readonly kind: "loading" }
  | { readonly kind: "unsupported" }
  | { readonly kind: "forbidden" }
  | { readonly kind: "error"; readonly message: string }
  | { readonly kind: "no-host" }
  | { readonly kind: "host"; readonly host: RemoteDesktopHost };

export function resolveRemoteDesktopPageState(input: {
  readonly environment: RemoteDesktopEnvironment | null;
  /** Null until the environment's session is known. */
  readonly canConnect: boolean | null;
  readonly state: RemoteDesktopState | null;
  readonly error: string | null;
}): RemoteDesktopPageState {
  const { environment } = input;
  if (environment === null) return { kind: "unknown-environment" };
  if (environment.connection.phase !== "connected") return { kind: "disconnected" };
  if (environment.serverConfig === null) return { kind: "loading" };
  if (environment.serverConfig.remoteDesktop !== true) return { kind: "unsupported" };
  if (input.canConnect === null) return { kind: "loading" };
  if (!input.canConnect) return { kind: "forbidden" };
  if (input.state === null) {
    return input.error === null ? { kind: "loading" } : { kind: "error", message: input.error };
  }
  return input.state.host === null ? { kind: "no-host" } : { kind: "host", host: input.state.host };
}

export function formatRemoteDesktopStats(stats: RemoteDesktopViewerStats): string {
  return [
    `${stats.width}×${stats.height}`,
    `${stats.framesPerSecond} fps`,
    `${(stats.kilobitsPerSecond / 1000).toFixed(1)} Mbps`,
    stats.roundTripMs === null ? null : `${stats.roundTripMs} ms`,
    stats.codec,
  ]
    .filter((part) => part !== null)
    .join(" · ");
}
