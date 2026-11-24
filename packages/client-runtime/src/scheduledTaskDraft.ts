import {
  ScheduledTaskError,
  scheduledTaskThreadMode,
  type ProjectId,
  type ScheduledTask,
  type ScheduledTaskExpected,
  type ScheduledTaskThreadMode,
  type ThreadId,
} from "@onicode/contracts";
import * as Schema from "effect/Schema";

const isScheduledTaskError = Schema.is(ScheduledTaskError);

/** What an editor loaded, sent so a save is refused once another client changed it. */
export function scheduledTaskExpected(task: ScheduledTask): ScheduledTaskExpected {
  return {
    title: task.title,
    prompt: task.prompt,
    enabled: task.enabled,
    schedule: task.schedule,
    projectId: task.projectId,
    threadId: task.threadId,
    threadMode: scheduledTaskThreadMode(task),
    workspaceStrategy: task.workspaceStrategy,
    modelSelection: task.modelSelection,
    runtimeMode: task.runtimeMode,
    interactionMode: task.interactionMode,
  };
}

/** The stored task a refused save carries; the live list can still hold the one before it. */
export function refusedScheduledTask(error: unknown): ScheduledTask | undefined {
  return isScheduledTaskError(error) ? error.current : undefined;
}

/**
 * Where a draft runs after another client's change refused its save. The user's choices stay and
 * the rest takes the stored values, but a thread never mixes with another project or the
 * new-thread mode.
 */
export function rebaseScheduledTaskDestination<
  TProject extends string | null,
  TThread extends string,
>(
  draft: {
    readonly projectId: TProject;
    readonly threadId: TThread | null;
    readonly threadMode: ScheduledTaskThreadMode;
  },
  loaded: ScheduledTask,
  current: ScheduledTask,
): {
  readonly projectId: TProject | ProjectId;
  readonly threadId: TThread | ThreadId | null;
  readonly threadMode: ScheduledTaskThreadMode;
} {
  const loadedProjectId: string = loaded.projectId;
  const loadedThreadId: string | null = loaded.threadId;
  const projectEdited = draft.projectId !== loadedProjectId;
  const threadEdited = draft.threadId !== loadedThreadId;
  const pickedThread = threadEdited && draft.threadId !== null;
  const currentMode = scheduledTaskThreadMode(current);
  const threadMode =
    draft.threadMode !== scheduledTaskThreadMode(loaded) ||
    (pickedThread && currentMode === "new_thread")
      ? draft.threadMode
      : currentMode;
  return {
    projectId: projectEdited || pickedThread ? draft.projectId : current.projectId,
    threadId:
      threadMode === "new_thread"
        ? null
        : threadEdited || projectEdited
          ? draft.threadId
          : current.threadId,
    threadMode,
  };
}
