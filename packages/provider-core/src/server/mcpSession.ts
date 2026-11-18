import type { EnvironmentId, ProviderInstanceId, ThreadId } from "@onicode/contracts";
import type * as Effect from "effect/Effect";

/**
 * Sent by MCP clients that cannot defer tools themselves. The Onicode MCP server then
 * lists only a core set plus `tool_search`, which lists the rest on demand.
 */
export const MCP_TOOL_EXPOSURE_HEADER = "oni-tool-exposure";

/** A stdio MCP server the provider launches itself, beside Onicode's HTTP one. */
export interface McpStdioServer {
  readonly command: string;
  readonly args: ReadonlyArray<string>;
  readonly env: Readonly<Record<string, string>>;
}

export const COMPUTER_MCP_SERVER_NAME = "computer";
export const BROWSER_MCP_SERVER_NAME = "browser";

export interface McpProviderSessionConfig {
  readonly environmentId: EnvironmentId;
  readonly threadId: ThreadId;
  readonly providerSessionId: string;
  readonly providerInstanceId: ProviderInstanceId;
  readonly endpoint: string;
  readonly authorizationHeader: string;
  /** Omit preview_* instructions when the user withholds agent browser access. */
  readonly browserToolsAvailable: boolean;
  readonly capabilities?: ReadonlySet<string>;
  readonly agentDeviceEnvironment?: Readonly<Record<string, string>>;
  /** cua-driver's MCP server, present when this session may drive the machine's desktop apps. */
  readonly computerMcpServer?: McpStdioServer;
  /** obu's MCP server, present when this session browses with obu. */
  readonly browserMcpServer?: McpStdioServer;
  /** True while a delegated task this thread started will wake it with its result. */
  readonly hasPendingWake?: Effect.Effect<boolean>;
}

/**
 * The stdio servers a provider launches for this session, each under the name its tools carry.
 * Turning one on is the user's consent, so providers run their tools without a prompt per call.
 */
export const stdioMcpServers = (
  config: Pick<McpProviderSessionConfig, "computerMcpServer" | "browserMcpServer"> | undefined,
): ReadonlyArray<readonly [name: string, server: McpStdioServer]> => [
  ...(config?.computerMcpServer
    ? [[COMPUTER_MCP_SERVER_NAME, config.computerMcpServer] as const]
    : []),
  ...(config?.browserMcpServer
    ? [[BROWSER_MCP_SERVER_NAME, config.browserMcpServer] as const]
    : []),
];

export function withAgentDeviceEnvironment(
  base: NodeJS.ProcessEnv,
  config: Pick<McpProviderSessionConfig, "agentDeviceEnvironment"> | undefined,
): NodeJS.ProcessEnv {
  const extra = config?.agentDeviceEnvironment;
  if (!extra) return base;
  const separator = extra.PATH_SEPARATOR ?? ":";
  const basePath = base.PATH ?? base.Path;
  const { PATH: shimDir, PATH_SEPARATOR: _separator, ...rest } = extra;
  return {
    ...base,
    ...rest,
    ...(shimDir ? { PATH: basePath ? `${shimDir}${separator}${basePath}` : shimDir } : {}),
  };
}
