import type { OrchestrationV2PendingBackgroundTask } from "@onicode/contracts";
import type { ThreadStructure } from "@onicode/client-runtime/state/shell";

/** The task carries only a preview; a loaded launch item has the full command. */
export function backgroundTaskCommand(
  task: Pick<OrchestrationV2PendingBackgroundTask, "command">,
  item: ThreadStructure["turnItems"][number] | undefined,
): string | undefined {
  if (item?.type === "command_execution") return item.input;
  // Claude's Monitor launches as a dynamic tool whose input holds the command.
  if (item?.type === "dynamic_tool" && typeof item.input === "object" && item.input !== null) {
    const command = Reflect.get(item.input, "command");
    if (typeof command === "string") return command;
  }
  return task.command;
}
