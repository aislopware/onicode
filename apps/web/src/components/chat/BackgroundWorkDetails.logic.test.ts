import type { ThreadStructure } from "@onicode/client-runtime/state/shell";
import { describe, expect, it } from "vite-plus/test";

import { backgroundTaskCommand } from "./BackgroundWorkDetails.logic";

const script = `python3 - <<'EOF'\n${"print('step')\n".repeat(2_000)}EOF`;
const preview = "python3 - <<'EOF'…";

const item = (type: "command_execution" | "dynamic_tool", input: unknown) =>
  ({ type, input }) as ThreadStructure["turnItems"][number];

describe("backgroundTaskCommand", () => {
  it("shows the launch item's full command over the task's preview", () => {
    expect(backgroundTaskCommand({ command: preview }, item("command_execution", script))).toBe(
      script,
    );
    expect(
      backgroundTaskCommand({ command: preview }, item("dynamic_tool", { command: script })),
    ).toBe(script);
    expect(backgroundTaskCommand({ command: preview }, undefined)).toBe(preview);
  });
});
