import type { OrchestrationV2PendingBackgroundTask } from "@onicode/contracts";
import type { ThreadStructure } from "@onicode/client-runtime/state/shell";

type TurnItem = ThreadStructure["turnItems"][number];

/** The task carries only a preview of its command; a loaded launch item has it in full. */
export function backgroundTaskLaunch(
  task: Pick<
    OrchestrationV2PendingBackgroundTask,
    "taskId" | "kind" | "command" | "itemId" | "nativeItemId"
  >,
  turnItems: ReadonlyArray<TurnItem>,
): { readonly item: TurnItem | undefined; readonly command: string | undefined } {
  const nativeId = task.nativeItemId ?? task.taskId;
  const item = turnItems.findLast((candidate) =>
    task.itemId === undefined
      ? (candidate.nativeItemRef?.nativeId || candidate.id) === nativeId
      : candidate.id === task.itemId,
  );
  if (item?.type === "command_execution") return { item, command: item.input };
  // Claude's Monitor launches as a dynamic tool whose input holds the command.
  if (
    task.kind === "monitor" &&
    item?.type === "dynamic_tool" &&
    typeof item.input === "object" &&
    item.input !== null
  ) {
    const command = Reflect.get(item.input, "command");
    if (typeof command === "string") return { item, command };
  }
  return { item, command: task.command };
}
