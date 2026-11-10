import { describe, expect, it } from "vite-plus/test";

import { providerSessionViewForPrompt } from "./providerSessionViews.ts";

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
