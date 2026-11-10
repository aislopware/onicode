import type {
  ProviderContextUsage,
  ProviderContextUsageCategory,
  ProviderMcpServer,
  ServerProvider,
} from "@onicode/contracts";

export type ProviderSessionView = "context" | "mcp";

export const PROVIDER_SESSION_VIEW_COMMANDS = {
  context: { name: "context", description: "Show what fills the context window" },
  mcp: { name: "mcp", description: "Manage MCP servers" },
} as const satisfies Record<ProviderSessionView, { name: string; description: string }>;

const NO_VIEWS: ReadonlyArray<ProviderSessionView> = [];
const CONTEXT_VIEWS: ReadonlyArray<ProviderSessionView> = ["context"];
const MCP_VIEWS: ReadonlyArray<ProviderSessionView> = ["mcp"];
const ALL_VIEWS: ReadonlyArray<ProviderSessionView> = ["context", "mcp"];

/** Shared arrays, so memoized readers see the same value across renders. */
export function providerSessionViews(
  provider: Pick<ServerProvider, "reportsContextUsage" | "managesMcpServers"> | null | undefined,
): ReadonlyArray<ProviderSessionView> {
  const context = provider?.reportsContextUsage === true;
  const mcp = provider?.managesMcpServers === true;
  return context ? (mcp ? ALL_VIEWS : CONTEXT_VIEWS) : mcp ? MCP_VIEWS : NO_VIEWS;
}

/** A bare `/context` or `/mcp` opens the view; with arguments the provider runs the command. */
export function providerSessionViewForPrompt(
  prompt: string,
  views: ReadonlyArray<ProviderSessionView>,
): ProviderSessionView | null {
  const command = prompt.trim().toLowerCase();
  return views.find((view) => command === `/${view}`) ?? null;
}

/** What a breakdown row opens: tool and instruction rows lead to MCP servers. */
export function contextCategoryDetail(
  category: ProviderContextUsageCategory,
): "mcp" | "memory" | "skills" | "agents" | null {
  switch (category.id) {
    case "mcp_tools":
    case "mcp_tools_deferred":
    case "mcp_instructions":
      return "mcp";
    case "memory_files":
      return "memory";
    case "skills":
      return "skills";
    case "custom_agents":
      return "agents";
    default:
      return null;
  }
}

export function contextUsageMcpTokensByServer(
  usage: ProviderContextUsage,
): ReadonlyArray<{ readonly server: string; readonly tokens: number; readonly tools: number }> {
  const byServer = new Map<string, { server: string; tokens: number; tools: number }>();
  for (const tool of usage.mcpTools) {
    const entry = byServer.get(tool.server) ?? { server: tool.server, tokens: 0, tools: 0 };
    entry.tokens += tool.tokens;
    entry.tools += 1;
    byServer.set(tool.server, entry);
  }
  return [...byServer.values()].sort((a, b) => b.tokens - a.tokens);
}

/** Signing in only applies to remote servers that ask for it or already hold a login. */
export function mcpServerCanAuthenticate(server: ProviderMcpServer): boolean {
  return server.transport !== undefined && server.transport !== "stdio";
}
