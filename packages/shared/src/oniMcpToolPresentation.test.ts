import { describe, expect, it } from "vite-plus/test";

import {
  ONICODE_MCP_TOOL_NAMES,
  resolveOniMcpSummaryAction,
  resolveOniMcpToolDefinition,
  resolveOniMcpToolPresentation,
} from "./oniMcpToolPresentation.ts";

describe("resolveOniMcpToolPresentation", () => {
  it("recognizes every Onicode tool across provider prefixes and completion suffixes", () => {
    for (const tool of ONICODE_MCP_TOOL_NAMES) {
      const presentation = resolveOniMcpToolPresentation(tool);
      for (const prefix of [
        "mcp__onicode__",
        "mcp__t3_code__",
        "mcp__onicode__",
        "Onicode-code.",
        "onicode.",
        "onicode-direct.",
        "mcp__t3_code_direct__",
        "oni_code/",
        "onicode:",
        "mcp_onicode_",
        "Onicode ",
        "onicode · ",
      ]) {
        expect(resolveOniMcpToolPresentation(`${prefix}${tool} completed`), tool).toEqual(
          presentation,
        );
      }
      expect(resolveOniMcpToolPresentation(`mcp__another-server__${tool}`), tool).toBeNull();
    }
  });

  it("matches OpenCode 2's per-thread server names, whose thread ids hold underscores", () => {
    expect(
      resolveOniMcpToolPresentation("onicode-thread_opencode2-adapter_delegate_task")?.displayName,
    ).toBe("Delegate a child task");
    expect(resolveOniMcpToolPresentation("onicode-thread_opencode2-adapter_not_a_tool")).toBeNull();
  });
});

describe("resolveOniMcpToolDefinition", () => {
  it("labels an action tool call as the action it ran", () => {
    expect(
      resolveOniMcpToolDefinition("mcp__onicode__preview_act", { action: "type" })?.labels,
    ).toEqual(resolveOniMcpToolDefinition("preview_type")?.labels);
    expect(resolveOniMcpToolDefinition("oni_thread_transfer", { action: "fork" })?.displayName).toBe(
      resolveOniMcpToolDefinition("oni_thread_fork")?.displayName,
    );
    expect(resolveOniMcpToolDefinition("preview_act", { action: "unknown" })?.labels[2]).toBe(
      "Used",
    );
  });
});

describe("resolveOniMcpSummaryAction", () => {
  it("summarizes an action tool by the action each call ran", () => {
    expect(resolveOniMcpSummaryAction("mcp__onicode__t3_queue", { action: "list" })).toBe(
      "queue-list",
    );
    expect(resolveOniMcpSummaryAction("oni_queue", { action: "edit" })).toBe("queue-edit");
    expect(resolveOniMcpSummaryAction("oni_thread_transfer", { action: "status" })).toBe(
      "thread-transfers",
    );
    expect(resolveOniMcpSummaryAction("oni_thread_configure", {})).toBe("thread-configuration");
    expect(resolveOniMcpSummaryAction("schedule_task", { scheduledTaskId: "task-1" })).toBe(
      "schedule-update",
    );
    expect(resolveOniMcpSummaryAction("delegate_task", { task: "x" })).toBe("delegate");
  });
});
