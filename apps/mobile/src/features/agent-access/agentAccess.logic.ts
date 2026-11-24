import {
  managedCapabilityDenial,
  managedRuntimeModeDenial,
} from "@onicode/client-runtime/contract-labels";
import {
  type AuthClientSession,
  type AuthEnvironmentScope,
  AuthMcpClientAccess,
  type ManagedPolicySummary,
  mcpClientScopes,
  type PublicAccessStatus,
  type SessionGrantInput,
  sessionGrantsScope,
} from "@onicode/contracts";
import { isLocalLoopbackHost } from "@onicode/shared/hostClassification";
import { msg } from "gt-react-native";

import { m, type MessageResolver } from "../../i18n/messages";

/** The level an approval starts at: Supervised, the one that asks before acting. */
export const DEFAULT_AGENT_ACCESS: AuthMcpClientAccess = "approval-required";

/**
 * Why this device cannot grant each level on an environment; absent when it can. The server
 * checks the same: a device never grants more than its own session holds.
 */
export function agentAccessDenials(
  input: {
    readonly session: SessionGrantInput | null;
    readonly canGrant: boolean;
    readonly policy: ManagedPolicySummary | undefined;
    readonly environmentLabel: string;
  },
  resolve: MessageResolver = m,
): ReadonlyMap<AuthMcpClientAccess, string> {
  const { session, canGrant, policy, environmentLabel } = input;
  const remoteDenial = managedCapabilityDenial(policy, "remoteClients", resolve);
  return new Map(
    AuthMcpClientAccess.literals.flatMap((level) => {
      const reason =
        remoteDenial ??
        (!canGrant
          ? resolve(
              msg("This device needs the Manage access permission on {environment}.", {
                environment: environmentLabel,
              }),
            )
          : session === null ||
              !mcpClientScopes(level).every((scope) => sessionGrantsScope(session, scope))
            ? resolve(
                msg("This device cannot grant this on {environment}.", {
                  environment: environmentLabel,
                }),
              )
            : level === "read-only"
              ? undefined
              : managedRuntimeModeDenial(policy, level, resolve));
      return reason === undefined ? [] : [[level, reason] as const];
    }),
  );
}

/** The grantable level closest below `wanted`, or null when none can be granted. */
export function closestGrantable(
  wanted: AuthMcpClientAccess,
  denied: ReadonlyMap<AuthMcpClientAccess, string>,
): AuthMcpClientAccess | null {
  const levels = AuthMcpClientAccess.literals;
  for (let index = levels.indexOf(wanted); index >= 0; index -= 1) {
    const level = levels[index]!;
    if (!denied.has(level)) return level;
  }
  return null;
}

function parseUrl(value: string | null): URL | null {
  if (value === null) return null;
  try {
    return new URL(value);
  } catch {
    return null;
  }
}

const mcpUrlFor = (base: URL) => `${base.origin}${base.pathname.replace(/\/+$/u, "")}/mcp`;

/**
 * The MCP address an agent on one of the user's devices can sign in at. MCP clients send OAuth
 * credentials only over https or to localhost, so a plain-http LAN or tailnet address finds the
 * server but never finishes signing in: an https address this device knows wins, then a
 * loopback one, which works for agents on the computer running Onicode.
 */
export function deviceMcpUrl(addresses: ReadonlyArray<string | null>): string | null {
  const urls = addresses.map(parseUrl).filter((url) => url !== null);
  const https = urls.find((url) => url.protocol === "https:");
  if (https !== undefined) return mcpUrlFor(https);
  const loopback = urls.find(
    (url) => url.protocol === "http:" && isLocalLoopbackHost(url.hostname),
  );
  return loopback === undefined ? null : mcpUrlFor(loopback);
}

/** A token needs no sign-in, so any http address this device reaches works for one. */
export function tokenMcpUrl(addresses: ReadonlyArray<string | null>): string | null {
  const plain = addresses
    .map(parseUrl)
    .find((url) => url !== null && (url.protocol === "http:" || url.protocol === "https:"));
  return (
    deviceMcpUrl(addresses) ?? (plain === undefined || plain === null ? null : mcpUrlFor(plain))
  );
}

/** Public MCP address: only while public agents is on and Funnel answers. */
export const publicMcpUrl = (agentsEnabled: boolean, status: PublicAccessStatus | null) =>
  agentsEnabled && status?._tag === "on" ? `${status.origin}/mcp` : null;

export const claudeCodeCommands = (url: string) => [
  `claude mcp add --transport http onicode ${url}`,
  "claude mcp login onicode",
];

export const codexCommands = (url: string) => [
  `codex mcp add onicode --url ${url}`,
  "codex mcp login onicode",
];

/** The subject the server gives sessions of outside agents that signed in or hold a token. */
const OUTSIDE_AGENT_SUBJECT = "outside-agent";

export const outsideAgentSessions = (sessions: ReadonlyArray<AuthClientSession>) =>
  sessions.filter((session) => session.subject === OUTSIDE_AGENT_SUBJECT);

/** The name the agent gave itself when it signed in, or the token's name. */
export const agentSessionName = (session: AuthClientSession): string | undefined =>
  // oxlint-disable-next-line onicode/no-rpc-permission-bypass -- session metadata, not an RPC client
  session.client.label;

/**
 * The level a session's scopes name, when they name one alone. Supervised and the auto levels
 * carry the same scopes (the server caps their runtime mode), so those read as null.
 */
export function agentSessionAccess(
  scopes: ReadonlyArray<AuthEnvironmentScope>,
): "read-only" | "full-access" | null {
  const granted = new Set(scopes);
  const matches = (level: AuthMcpClientAccess) => {
    const needed = mcpClientScopes(level);
    return needed.length === granted.size && needed.every((scope) => granted.has(scope));
  };
  return matches("read-only") ? "read-only" : matches("full-access") ? "full-access" : null;
}

/** `m:ss` for a countdown in whole seconds. */
export const formatCountdown = (seconds: number) =>
  `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
