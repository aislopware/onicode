// @effect-diagnostics globalDate:off - The reporter runs in input handlers, outside an Effect runtime.
import {
  type EnvironmentId,
  type ScopedThreadRef,
  type ThreadId,
  WS_METHODS,
} from "@onicode/contracts";
import * as Clock from "effect/Clock";
import * as Effect from "effect/Effect";
import type { Atom, AtomRegistry } from "effect/reactivity";

import { toServerClock } from "../clock.ts";
import type { EnvironmentRegistry } from "../connection/registry.ts";
import { threadKey } from "./entities.ts";
import { createEnvironmentRpcCommand, createEnvironmentRpcQueryAtomFamily } from "./runtime.ts";
import { clearMcpSignedIn, markMcpSignedIn } from "./threadCommands.ts";

// Claude Code asks for the first key and then about every 10 seconds; silence means typing stopped.
export const DRAFT_ACTIVITY_REPORT_INTERVAL_MS = 10_000;

/** Reports at once and then stays quiet for the interval, per thread. It never reports later. */
export function createDraftActivityReporter(
  report: (ref: ScopedThreadRef) => void,
  now: () => number = Date.now,
) {
  const lastReportedAt = new Map<string, number>();
  return (ref: ScopedThreadRef) => {
    const key = threadKey(ref);
    const at = now();
    const last = lastReportedAt.get(key);
    if (last !== undefined && at - last < DRAFT_ACTIVITY_REPORT_INTERVAL_MS) return;
    lastReportedAt.set(key, at);
    report(ref);
  };
}

const markSignedInNow = (
  registry: AtomRegistry.AtomRegistry,
  environmentId: EnvironmentId,
  input: { readonly threadId: ThreadId; readonly server: string },
) =>
  Clock.currentTimeMillis.pipe(
    Effect.map((now) =>
      markMcpSignedIn(registry, environmentId, input, toServerClock(environmentId, now)),
    ),
  );

/** Views read the thread's live provider process, so every open asks again. */
export function createProviderSessionEnvironmentAtoms<R, E>(
  runtime: Atom.AtomRuntime<EnvironmentRegistry | R, E>,
) {
  return {
    contextUsage: createEnvironmentRpcQueryAtomFamily(runtime, {
      label: "environment-data:provider-session:context-usage",
      tag: WS_METHODS.providerSessionContextUsage,
      staleTimeMs: 0,
      idleTtlMs: 0,
    }),
    mcpServers: createEnvironmentRpcQueryAtomFamily(runtime, {
      label: "environment-data:provider-session:mcp-servers",
      tag: WS_METHODS.providerSessionMcpServers,
      staleTimeMs: 0,
      idleTtlMs: 0,
      // A sign-in finished in the browser reaches the provider's own callback, not Onicode,
      // and another client can clear one.
      onAnswer: ({ environmentId, input }, { servers }, registry) =>
        Effect.forEach(
          servers,
          (server) => {
            const target = { threadId: input.threadId, server: server.name };
            if (server.status === "connected") {
              return markSignedInNow(registry, environmentId, target);
            }
            if (server.status === "needs-auth") {
              return Effect.sync(() => clearMcpSignedIn(registry, environmentId, target));
            }
            return Effect.void;
          },
          { discard: true },
        ),
    }),
    // Signing in here also clears the thread's sign-in notice (threadCommands.mcpSignedInAtom).
    mcpServerAction: createEnvironmentRpcCommand(runtime, {
      label: "environment-command:provider-session:mcp-server-action",
      tag: WS_METHODS.providerSessionMcpServerAction,
      onSuccess: ({ environmentId, input }, registry, result) => {
        if (input.action === "clear-auth") {
          return Effect.sync(() => clearMcpSignedIn(registry, environmentId, input));
        }
        if (input.action !== "authenticate" || result.authUrl !== undefined) return Effect.void;
        return markSignedInNow(registry, environmentId, input);
      },
    }),
    mcpOAuthCallback: createEnvironmentRpcCommand(runtime, {
      label: "environment-command:provider-session:mcp-oauth-callback",
      tag: WS_METHODS.providerSessionMcpOAuthCallback,
      onSuccess: ({ environmentId, input }, registry) =>
        markSignedInNow(registry, environmentId, input),
    }),
    draftActivity: createEnvironmentRpcCommand(runtime, {
      label: "environment-command:provider-session:draft-activity",
      tag: WS_METHODS.providerSessionDraftActivity,
    }),
    info: createEnvironmentRpcQueryAtomFamily(runtime, {
      label: "environment-data:provider-session:info",
      tag: WS_METHODS.providerSessionInfo,
      staleTimeMs: 0,
      idleTtlMs: 0,
    }),
    memoryFile: createEnvironmentRpcQueryAtomFamily(runtime, {
      label: "environment-data:provider-session:memory-file",
      tag: WS_METHODS.providerSessionMemoryFile,
      staleTimeMs: 0,
      idleTtlMs: 0,
    }),
  };
}
