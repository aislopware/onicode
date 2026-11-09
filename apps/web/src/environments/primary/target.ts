import { PRIMARY_LOCAL_ENVIRONMENT_ID } from "@onicode/contracts";
import * as Schema from "effect/Schema";

import { isLocalEnvironmentDisabled } from "../../localEnvironment";

export class DesktopEnvironmentBootstrapIncompleteError extends Schema.TaggedError<DesktopEnvironmentBootstrapIncompleteError>()(
  "DesktopEnvironmentBootstrapIncompleteError",
  {
    hasHttpBaseUrl: Schema.Boolean,
    hasWsBaseUrl: Schema.Boolean,
  },
) {
  override get message(): string {
    const missing = [
      ...(this.hasHttpBaseUrl ? [] : ["httpBaseUrl"]),
      ...(this.hasWsBaseUrl ? [] : ["wsBaseUrl"]),
    ];
    return `Desktop bootstrap is missing ${missing.join(" and ")} for the local environment.`;
  }
}

export class PrimaryEnvironmentDisabledError extends Schema.TaggedError<PrimaryEnvironmentDisabledError>()(
  "PrimaryEnvironmentDisabledError",
  {},
) {
  override get message(): string {
    return "The local environment is disabled.";
  }
}

export interface PrimaryEnvironmentTarget {
  readonly httpBaseUrl: string;
  readonly wsBaseUrl: string;
}

const LOOPBACK_HOSTNAMES = new Set(["127.0.0.1", "::1", "localhost"]);

export function isLoopbackHostname(hostname: string): boolean {
  return LOOPBACK_HOSTNAMES.has(
    hostname
      .trim()
      .toLowerCase()
      .replace(/^\[(.*)\]$/, "$1"),
  );
}

/** The desktop app's own server, or null while its local environment is off or not started. */
export function readPrimaryEnvironmentTarget(): PrimaryEnvironmentTarget | null {
  if (isLocalEnvironmentDisabled()) {
    return null;
  }
  const bootstrap = window.desktopBridge
    ?.getLocalEnvironmentBootstraps()
    .find((entry) => entry.id === PRIMARY_LOCAL_ENVIRONMENT_ID);
  if (!bootstrap || (!bootstrap.httpBaseUrl && !bootstrap.wsBaseUrl)) {
    return null;
  }
  if (!bootstrap.httpBaseUrl || !bootstrap.wsBaseUrl) {
    throw new DesktopEnvironmentBootstrapIncompleteError({
      hasHttpBaseUrl: Boolean(bootstrap.httpBaseUrl),
      hasWsBaseUrl: Boolean(bootstrap.wsBaseUrl),
    });
  }
  return {
    httpBaseUrl: new URL(bootstrap.httpBaseUrl).toString(),
    wsBaseUrl: new URL(bootstrap.wsBaseUrl).toString(),
  };
}

export function resolvePrimaryEnvironmentHttpUrl(
  pathname: string,
  searchParams?: Record<string, string>,
): string {
  const primaryTarget = readPrimaryEnvironmentTarget();
  if (!primaryTarget) {
    throw new PrimaryEnvironmentDisabledError();
  }
  const url = new URL(primaryTarget.httpBaseUrl);
  url.pathname = pathname;
  if (searchParams) {
    url.search = new URLSearchParams(searchParams).toString();
  }
  return url.toString();
}
