import { describe, expect, it } from "@effect/vitest";

import * as ClaudeSessionSettings from "./ClaudeSessionSettings.ts";

describe("ClaudeSessionSettings", () => {
  it("lists only the permission rules in effect, with Claude's description", () => {
    const info = ClaudeSessionSettings.claudePermissionsInfo({
      state: {
        rules: [
          {
            behavior: "allow",
            source: "userSettings",
            rule: "Bash(git:*)",
            description: { prefix: "Any Bash command starting with ", emphasis: "git" },
          },
          { behavior: "deny", source: "policySettings", rule: "Agent" },
          { behavior: "ask", source: "localSettings", rule: "Write", notInEffect: true },
        ],
      },
    });
    expect(info.rules).toEqual([
      {
        behavior: "allow",
        rule: "Bash(git:*)",
        description: "Any Bash command starting with git",
        source: "userSettings",
      },
      { behavior: "deny", rule: "Agent", description: "Agent", source: "policySettings" },
    ]);
  });
});
