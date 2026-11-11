import { describe, expect, it } from "vite-plus/test";

import type { ProviderContextUsage } from "@onicode/contracts";

import { contextUsedCategories, providerSessionViewForPrompt } from "./providerSessionViews.ts";

describe("providerSessionViewForPrompt", () => {
  it.each([
    ["/context", ["context", "mcp"], "context"],
    ["  /MCP \n", ["context", "mcp"], "mcp"],
    ["/mcp reconnect linear", ["context", "mcp"], null],
    ["/context", ["mcp"], null],
    ["explain /context", ["context"], null],
  ] as const)("%j with %j opens %j", (prompt, views, expected) => {
    expect(providerSessionViewForPrompt(prompt, views)).toBe(expected);
  });
});

describe("contextUsedCategories", () => {
  it("orders used parts largest first and keeps colors on their categories", () => {
    const usage = {
      categories: [
        { id: "system_prompt", name: "System prompt", tokens: 5_000, kind: "used" },
        { id: "system_tools", name: "System tools", tokens: 8_000, kind: "used" },
        { name: "Plugins", tokens: 300, kind: "used" },
        {
          id: "mcp_tools_deferred",
          name: "MCP tools (deferred)",
          tokens: 90_000,
          kind: "deferred",
        },
        { id: "messages", name: "Messages", tokens: 19_000, kind: "used" },
        { name: "Hooks", tokens: 200, kind: "used" },
        { id: "free_space", name: "Free space", tokens: 300_000, kind: "free" },
      ],
    } as unknown as ProviderContextUsage;
    expect(
      contextUsedCategories(usage).map(({ category, accent }) => [category.name, accent]),
    ).toEqual([
      ["Messages", "file"],
      ["System tools", null],
      ["System prompt", "video"],
      ["Plugins", "mention"],
      ["Hooks", "terminal"],
    ]);
  });
});
