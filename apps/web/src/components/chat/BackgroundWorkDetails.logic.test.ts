import type { ThreadStructure } from "@onicode/client-runtime/state/shell";
import { TurnItemId } from "@onicode/contracts";
import { describe, expect, it } from "vite-plus/test";

import { backgroundTaskLaunch } from "./BackgroundWorkDetails.logic";

const script = `python3 - <<'EOF'\n${"print('step')\n".repeat(2_000)}EOF`;
const preview = "python3 - <<'EOF'…";

const item = (type: "command_execution" | "dynamic_tool", input: unknown, nativeId = "toolu_1") =>
  ({
    id: TurnItemId.make(`item-${nativeId}`),
    type,
    input,
    nativeItemRef: { nativeId },
  }) as ThreadStructure["turnItems"][number];

describe("backgroundTaskLaunch", () => {
  it("shows the launch item's full command over the task's preview", () => {
    const command = { taskId: "task-1", kind: "command", command: preview } as const;
    const monitor = { ...command, kind: "monitor" } as const;
    const launch = (
      task: Parameters<typeof backgroundTaskLaunch>[0],
      items: ReadonlyArray<ThreadStructure["turnItems"][number]>,
    ) => backgroundTaskLaunch(task, items).command;
    expect(
      launch({ ...command, itemId: TurnItemId.make("item-toolu_1") }, [
        item("command_execution", script),
      ]),
    ).toBe(script);
    // A task published before its launch was linked still names the tool call.
    expect(
      launch({ ...monitor, nativeItemId: "toolu_1" }, [item("dynamic_tool", { command: script })]),
    ).toBe(script);
    expect(launch(command, [])).toBe(preview);
    // Only a Monitor's dynamic tool input is a shell command.
    expect(
      launch({ taskId: "toolu_1", kind: "background_task" }, [
        item("dynamic_tool", { command: "workflow parameter" }),
      ]),
    ).toBeUndefined();
  });
});
