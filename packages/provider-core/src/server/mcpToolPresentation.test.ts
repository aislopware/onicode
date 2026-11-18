import { describe, expect, it } from "@effect/vitest";
import { mcpToolPresentation } from "./mcpToolPresentation.ts";

describe("mcpToolPresentation", () => {
  it.each([
    "javascript:alert(1)",
    "file:///tmp/logo.png",
    "not a URL",
    "https://example.com/" + "x".repeat(4096),
  ])("ignores an invalid logo %s", (iconUrl) => {
    expect(
      mcpToolPresentation({
        serverName: "weather",
        toolName: "get_weather",
        iconUrl,
        title: "x".repeat(161),
      }),
    ).toEqual({
      title: "get weather",
      toolSource: { key: "mcp:weather", name: "weather", kind: "integration" },
    });
  });

  it.each([
    [
      "mcp__computer__get_window_state",
      "get window state",
      { key: "computer-use", name: "Computer Use", kind: "computer" },
    ],
    ["mcp__browser__see", "see", { key: "browser-use", name: "Browser", kind: "browser" }],
  ])(
    "shows a driver's tool %s under the driver, not as an integration",
    (toolName, title, toolSource) => {
      expect(mcpToolPresentation({ toolName })).toEqual({ title, toolSource });
    },
  );
});
