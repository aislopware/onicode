import { assert, describe, it } from "@effect/vitest";

import {
  ONICODE_CODE_DEVICE_TOOL_INSTRUCTIONS,
  ONICODE_CODE_DEVICE_TOOL_INSTRUCTIONS_WITH_TOOL_SEARCH,
  ONICODE_CODE_ORCHESTRATION_INSTRUCTIONS,
  oniAcpPromptWithInstructions,
  oniOrchestrationPrompt,
  oniOrchestrationSystemPrompt,
} from "./orchestrationInstructions.ts";

describe("Onicode device instructions", () => {
  it("route device work through device_open and give Claude the line that loads the tools", () => {
    for (const intent of ["`device_list`", "`device_open`", "`agent-device`", "ssh", "`hostId`"]) {
      assert.include(ONICODE_CODE_DEVICE_TOOL_INSTRUCTIONS, intent);
    }
    assert.notInclude(ONICODE_CODE_DEVICE_TOOL_INSTRUCTIONS, "ToolSearch");
    const loadLine =
      " Load them with ToolSearch `select:mcp__onicode__device_list,mcp__onicode__device_open,mcp__onicode__device_screenshot,mcp__onicode__device_close`.";
    assert.include(ONICODE_CODE_DEVICE_TOOL_INSTRUCTIONS_WITH_TOOL_SEARCH, loadLine);
    assert.equal(
      ONICODE_CODE_DEVICE_TOOL_INSTRUCTIONS_WITH_TOOL_SEARCH.replace(loadLine, ""),
      ONICODE_CODE_DEVICE_TOOL_INSTRUCTIONS,
    );
  });

  it("open the app before the returned command's snapshots, screenshots and input", () => {
    assert.include(
      ONICODE_CODE_DEVICE_TOOL_INSTRUCTIONS,
      "its snapshots, screenshots and input need `open <app>` first,",
    );
  });

  it("keep the failure rule from taking over or closing a session another thread holds", () => {
    // agent-device's DEVICE_IN_USE advice is to rerun with the owner's --session or close it first.
    assert.include(
      ONICODE_CODE_DEVICE_TOOL_INSTRUCTIONS,
      "follow its advice once, but never reuse or close another session; if it still fails,",
    );
  });

  it("reach OpenCode and Cursor only when the session holds device access", () => {
    assert.include(oniOrchestrationSystemPrompt(true, true), ONICODE_CODE_DEVICE_TOOL_INSTRUCTIONS);
    assert.notInclude(oniOrchestrationSystemPrompt(true, false), "## Onicode devices");
    assert.isUndefined(oniOrchestrationSystemPrompt(false, true));

    const firstRun = (hasDeviceTools: boolean) =>
      oniOrchestrationPrompt({
        prompt: "Check the login screen.",
        hasOniMcp: true,
        hasDeviceTools,
        orchestrationDelivered: false,
        deviceInstructionsDelivered: false,
      });
    assert.include(firstRun(true), ONICODE_CODE_DEVICE_TOOL_INSTRUCTIONS);
    assert.notInclude(firstRun(false), "## Onicode devices");

    const laterRun = (deviceInstructionsDelivered: boolean) =>
      oniOrchestrationPrompt({
        prompt: "Check the login screen.",
        hasOniMcp: true,
        hasDeviceTools: true,
        orchestrationDelivered: true,
        deviceInstructionsDelivered,
      });
    assert.include(laterRun(false), ONICODE_CODE_DEVICE_TOOL_INSTRUCTIONS);
    assert.notInclude(laterRun(false), ONICODE_CODE_ORCHESTRATION_INSTRUCTIONS.trim());
    assert.equal(laterRun(true), "Check the login screen.");
  });

  it("reach ACP agents again when device access changes mid-thread", () => {
    const state = { interactionMode: "default", hasOniMcp: true, hasDeviceTools: false } as const;
    const withDevices = oniAcpPromptWithInstructions({
      prompt: "Check the login screen.",
      state: { ...state, hasDeviceTools: true },
      previousState: state,
    });
    assert.include(withDevices, ONICODE_CODE_DEVICE_TOOL_INSTRUCTIONS);
    assert.equal(
      oniAcpPromptWithInstructions({ prompt: "Again.", state, previousState: state }),
      "Again.",
    );
    assert.notInclude(oniAcpPromptWithInstructions({ prompt: "Hi.", state }), "## Onicode devices");
  });
});
