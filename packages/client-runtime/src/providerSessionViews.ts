import type {
  ProviderContextUsage,
  ProviderContextUsageCategory,
  ProviderMcpServer,
  ProviderSessionInfoView,
  ServerProvider,
} from "@onicode/contracts";

export type ProviderSessionView =
  | "context"
  | "mcp"
  | "status"
  | "memory"
  | "agents"
  | "skills"
  | "hooks"
  | "permissions"
  | "plugin"
  | "output-style";

export const PROVIDER_SESSION_VIEW_COMMANDS = {
  context: { name: "context", description: "Show what fills the context window" },
  mcp: { name: "mcp", description: "Manage MCP servers" },
  status: { name: "status", description: "Show the session's version, account and model" },
  memory: { name: "memory", description: "Show memory files" },
  agents: { name: "agents", description: "List subagents" },
  skills: { name: "skills", description: "List skills" },
  hooks: { name: "hooks", description: "List hooks" },
  permissions: { name: "permissions", description: "Show allow, ask and deny rules" },
  plugin: { name: "plugin", description: "List plugins and marketplaces" },
  "output-style": { name: "output-style", description: "Show output styles" },
} as const satisfies Record<ProviderSessionView, { name: string; description: string }>;

const SETTINGS_VIEWS = [
  "status",
  "memory",
  "agents",
  "skills",
  "hooks",
  "permissions",
  "plugin",
  "output-style",
] as const satisfies ReadonlyArray<ProviderSessionView>;

/** The `providerSession.info` view behind each settings view. */
export const PROVIDER_SESSION_INFO_VIEWS = {
  status: "status",
  memory: "memory",
  agents: "agents",
  skills: "skills",
  hooks: "hooks",
  permissions: "permissions",
  plugin: "plugins",
  "output-style": "output-styles",
} as const satisfies Record<(typeof SETTINGS_VIEWS)[number], ProviderSessionInfoView>;

export type ProviderSessionSettingsView = (typeof SETTINGS_VIEWS)[number];

export const isProviderSessionView = (value: string): value is ProviderSessionView =>
  Object.hasOwn(PROVIDER_SESSION_VIEW_COMMANDS, value);

export const isProviderSessionSettingsView = (
  view: ProviderSessionView,
): view is ProviderSessionSettingsView => Object.hasOwn(PROVIDER_SESSION_INFO_VIEWS, view);

const viewsByFlags = new Map<string, ReadonlyArray<ProviderSessionView>>();

/** Shared arrays, so memoized readers see the same value across renders. */
export function providerSessionViews(
  provider:
    | Pick<ServerProvider, "reportsContextUsage" | "managesMcpServers" | "managesSessionSettings">
    | null
    | undefined,
): ReadonlyArray<ProviderSessionView> {
  const context = provider?.reportsContextUsage === true;
  const mcp = provider?.managesMcpServers === true;
  const settings = provider?.managesSessionSettings === true;
  const key = `${context}|${mcp}|${settings}`;
  let views = viewsByFlags.get(key);
  if (views === undefined) {
    views = [
      ...(context ? (["context"] as const) : []),
      ...(mcp ? (["mcp"] as const) : []),
      ...(settings ? SETTINGS_VIEWS : []),
    ];
    viewsByFlags.set(key, views);
  }
  return views;
}

/** A bare `/context`, `/mcp`, `/status`… opens the view; with arguments the provider runs the command. */
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

// Context chip kinds both clients theme; skills wear the skill chip's color. System tools have none,
// so they stay grey with the free space.
const CONTEXT_CATEGORY_ACCENTS = {
  messages: "file",
  system_prompt: "video",
  skills: "skill",
  memory_files: "element",
  mcp_tools: "environment",
  mcp_instructions: "thread",
  custom_agents: "review-comment",
} as const;
const OTHER_CONTEXT_ACCENTS = ["mention", "terminal"] as const;

export type ContextCategoryAccent =
  | (typeof CONTEXT_CATEGORY_ACCENTS)[keyof typeof CONTEXT_CATEGORY_ACCENTS]
  | (typeof OTHER_CONTEXT_ACCENTS)[number];

/** Each category with the chip accent its bar wears, in the provider's order; null stays grey. */
export function contextCategoryAccents(
  categories: ReadonlyArray<ProviderContextUsageCategory>,
): ReadonlyArray<{
  readonly category: ProviderContextUsageCategory;
  readonly accent: ContextCategoryAccent | null;
}> {
  let others = 0;
  return categories.map((category) => {
    if (category.kind !== "used" || category.id === "system_tools") {
      return { category, accent: null };
    }
    const known = Object.hasOwn(CONTEXT_CATEGORY_ACCENTS, category.id ?? "")
      ? CONTEXT_CATEGORY_ACCENTS[category.id as keyof typeof CONTEXT_CATEGORY_ACCENTS]
      : undefined;
    return {
      category,
      accent: known ?? OTHER_CONTEXT_ACCENTS[others++ % OTHER_CONTEXT_ACCENTS.length]!,
    };
  });
}

/** What fills the used context, largest first, with each part's accent. */
export function contextUsedCategories(usage: ProviderContextUsage) {
  return contextCategoryAccents(usage.categories)
    .filter(({ category }) => category.kind === "used" && category.tokens > 0)
    .sort((a, b) => b.category.tokens - a.category.tokens);
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
