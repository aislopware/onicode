import { WS_METHODS } from "@onicode/contracts";
import type { Atom } from "effect/reactivity";

import type { EnvironmentRegistry } from "../connection/registry.ts";
import { createEnvironmentRpcCommand, createEnvironmentRpcQueryAtomFamily } from "./runtime.ts";

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
    }),
    mcpServerAction: createEnvironmentRpcCommand(runtime, {
      label: "environment-command:provider-session:mcp-server-action",
      tag: WS_METHODS.providerSessionMcpServerAction,
    }),
    mcpOAuthCallback: createEnvironmentRpcCommand(runtime, {
      label: "environment-command:provider-session:mcp-oauth-callback",
      tag: WS_METHODS.providerSessionMcpOAuthCallback,
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
