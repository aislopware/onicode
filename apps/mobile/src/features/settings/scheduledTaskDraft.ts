import { scheduledTaskThreadMode } from "@onicode/contracts";
import type {
  ModelSelection,
  ServerConfig,
  ProjectId,
  RuntimeMode,
  ScheduledTask,
  ScheduledTaskThreadMode,
  ScheduledTaskUpsertSchedule,
  ThreadId,
} from "@onicode/contracts";

import { DEFAULT_SERVER_SETTINGS } from "@onicode/contracts";
import {
  resolveProjectSettings,
  type LegacyProjectSettingsFields,
} from "@onicode/shared/projectSettings";
import {
  buildModelOptions,
  resolveDefaultableModelSelection,
  resolveNewTaskModelSelection,
} from "../../lib/modelOptions";

export function scheduledTaskDefaultModel(
  config: ServerConfig | null,
  project: (LegacyProjectSettingsFields & { readonly id: ProjectId }) | null,
): ModelSelection | null {
  const settings = config?.settings ?? DEFAULT_SERVER_SETTINGS;
  const configured = resolveProjectSettings(settings, project?.id ?? null, project).settings
    .defaultModelSelection;
  const projectDefaultSelection =
    resolveDefaultableModelSelection(config, configured, project?.id ?? null) ??
    resolveDefaultableModelSelection(config, settings.defaultModelSelection, project?.id ?? null);
  return resolveNewTaskModelSelection({
    draftSelection: null,
    projectDefaultSelection,
    stickySelection: null,
    modelOptions: buildModelOptions(
      config,
      projectDefaultSelection,
      undefined,
      project?.id ?? null,
    ),
  });
}

export type ScheduleDraft = {
  readonly mode: "fixed_time" | "once" | "interval";
  readonly timeOfDay: string;
  /** ISO time for a one-time run. Empty until the user picks the "Once" mode. */
  readonly runAt: string;
  readonly weekdays: ReadonlyArray<number>;
  readonly intervalMinutes: string;
};

export const DEFAULT_SCHEDULE: ScheduleDraft = {
  mode: "fixed_time",
  timeOfDay: "09:00",
  runAt: "",
  weekdays: [1, 2, 3, 4, 5],
  intervalMinutes: "15",
};

export function scheduleDraftForTask(task: Pick<ScheduledTask, "schedule">): ScheduleDraft {
  switch (task.schedule.type) {
    case "fixed_time":
      return {
        ...DEFAULT_SCHEDULE,
        timeOfDay: task.schedule.timeOfDay,
        weekdays: task.schedule.weekdays?.length
          ? [...new Set(task.schedule.weekdays)].sort((a, b) => a - b)
          : [0, 1, 2, 3, 4, 5, 6],
      };
    case "once":
      return { ...DEFAULT_SCHEDULE, mode: "once", runAt: task.schedule.runAt };
    case "interval":
      return {
        ...DEFAULT_SCHEDULE,
        mode: "interval",
        intervalMinutes: String(Math.max(1, task.schedule.everyMs / 60_000)),
      };
  }
}

export function parseIntervalMinutes(value: string): number {
  return Number(value.trim().replace(",", "."));
}

export function scheduleFromDraft(draft: ScheduleDraft): ScheduledTaskUpsertSchedule | null {
  if (draft.mode === "once") {
    const runAt = new Date(draft.runAt);
    return Number.isNaN(runAt.getTime()) ? null : { type: "once", runAt: runAt.toISOString() };
  }
  if (draft.mode === "interval") {
    const minutes = parseIntervalMinutes(draft.intervalMinutes);
    const everyMs = Math.round(minutes * 60_000);
    return minutes >= 1 && Number.isSafeInteger(everyMs) ? { type: "interval", everyMs } : null;
  }
  const weekdays = [...new Set(draft.weekdays)].sort((a, b) => a - b);
  if (
    !/^([01]?\d|2[0-3]):[0-5]\d$/.test(draft.timeOfDay) ||
    weekdays.length === 0 ||
    weekdays.some((day) => !Number.isInteger(day) || day < 0 || day > 6)
  ) {
    return null;
  }
  return {
    type: "fixed_time",
    timeOfDay: draft.timeOfDay,
    ...(weekdays.length === 7 ? {} : { weekdays }),
  };
}

type Workspace = "worktree" | "root" | "existing_worktree";
export type ScheduledTaskDraft = {
  readonly task: ScheduledTask | null;
  readonly title: string;
  readonly prompt: string;
  readonly projectId: ProjectId | null;
  readonly threadId: ThreadId | null;
  readonly modelSelection: ModelSelection | null;
  readonly modelSelectionIsExplicit: boolean;
  readonly schedule: ScheduleDraft;
  readonly threadMode: ScheduledTaskThreadMode;
  readonly workspace: Workspace;
  readonly baseRef: string;
  readonly checkoutPath: string;
  readonly enabled: boolean;
  readonly startFromOrigin: boolean;
  readonly runtimeMode: RuntimeMode;
};

// Fields a mode leaves behind when the user switches away and back are not edits.
function draftSignature(draft: ScheduledTaskDraft): string {
  const { mode } = draft.schedule;
  const worktree = draft.workspace === "worktree";
  return JSON.stringify([
    draft.title,
    draft.prompt,
    draft.projectId,
    draft.threadId,
    draft.modelSelection?.instanceId,
    draft.modelSelection?.model,
    [...(draft.modelSelection?.options ?? [])]
      .sort((a, b) => a.id.localeCompare(b.id))
      .map((option) => [option.id, option.value]),
    mode,
    mode === "fixed_time" ? draft.schedule.timeOfDay : null,
    mode === "once" ? draft.schedule.runAt : null,
    mode === "fixed_time" ? [...draft.schedule.weekdays].sort((a, b) => a - b) : null,
    mode === "interval" ? draft.schedule.intervalMinutes : null,
    draft.threadMode,
    draft.workspace,
    worktree ? draft.baseRef : null,
    draft.workspace === "existing_worktree" ? draft.checkoutPath : null,
    draft.enabled,
    worktree ? draft.startFromOrigin : null,
    draft.runtimeMode,
  ]);
}

export function hasScheduledTaskDraftChanges(
  initial: ScheduledTaskDraft,
  current: ScheduledTaskDraft,
): boolean {
  return draftSignature(initial) !== draftSignature(current);
}

export function moveDraftToThread(
  draft: ScheduledTaskDraft,
  thread: { readonly id: ThreadId; readonly projectId: ProjectId },
): ScheduledTaskDraft {
  return {
    ...draft,
    threadId: thread.id,
    projectId: thread.projectId,
    threadMode: draft.threadMode === "new_thread" ? "continue" : draft.threadMode,
  };
}

/** Leaves the thread but keeps the mode. Matches MCP: new threads stay out of the shared checkout. */
export function unbindDraftFromThread(draft: ScheduledTaskDraft): ScheduledTaskDraft {
  if (draft.threadId === null) return draft;
  return {
    ...draft,
    threadId: null,
    ...(draft.workspace === "root"
      ? { workspace: "worktree" as const, startFromOrigin: true }
      : {}),
  };
}

export function changeDraftProject(
  draft: ScheduledTaskDraft,
  projectId: ProjectId,
): ScheduledTaskDraft {
  if (projectId === draft.projectId) return draft;
  return { ...unbindDraftFromThread(draft), projectId };
}

export function createDraft(
  projectId: ProjectId | null,
  modelSelection: ModelSelection | null,
): ScheduledTaskDraft {
  return {
    task: null,
    title: "",
    prompt: "",
    projectId,
    threadId: null,
    modelSelection,
    modelSelectionIsExplicit: false,
    schedule: DEFAULT_SCHEDULE,
    threadMode: "new_thread",
    workspace: "worktree",
    baseRef: "",
    checkoutPath: "",
    enabled: true,
    startFromOrigin: true,
    runtimeMode: "full-access",
  };
}

/** What an edit loaded, sent so a save is refused once another client changed it. */
export function savedTaskFields(draft: ScheduledTaskDraft) {
  return draft.task === null
    ? undefined
    : {
        title: draft.task.title,
        prompt: draft.task.prompt,
        enabled: draft.task.enabled,
        schedule: draft.task.schedule,
      };
}

/**
 * After a save another client's change refused: fields the user left alone take the task's
 * current values, the user's edits stay, and the next save checks against what is stored now.
 */
export function rebaseScheduledTaskDraft(
  draft: ScheduledTaskDraft,
  current: ScheduledTask,
): ScheduledTaskDraft {
  const loaded = draft.task;
  if (loaded === null) return draft;
  const scheduleUntouched =
    JSON.stringify(draft.schedule) === JSON.stringify(scheduleDraftForTask(loaded));
  return {
    ...draft,
    ...(draft.title === loaded.title ? { title: current.title } : {}),
    ...(draft.prompt === loaded.prompt ? { prompt: current.prompt } : {}),
    ...(draft.enabled === loaded.enabled ? { enabled: current.enabled } : {}),
    ...(scheduleUntouched ? { schedule: scheduleDraftForTask(current) } : {}),
    ...(draft.threadId === loaded.threadId ? { threadId: current.threadId } : {}),
    ...(draft.threadMode === scheduledTaskThreadMode(loaded)
      ? { threadMode: scheduledTaskThreadMode(current) }
      : {}),
    task: current,
  };
}

export function editDraft(task: ScheduledTask): ScheduledTaskDraft {
  return {
    task,
    title: task.title,
    prompt: task.prompt,
    projectId: task.projectId,
    threadId: task.threadId,
    modelSelection: task.modelSelection,
    modelSelectionIsExplicit: true,
    schedule: scheduleDraftForTask(task),
    threadMode: scheduledTaskThreadMode(task),
    workspace: task.workspaceStrategy.type,
    baseRef: task.workspaceStrategy.type === "worktree" ? task.workspaceStrategy.baseRef : "",
    checkoutPath:
      task.workspaceStrategy.type === "existing_worktree"
        ? task.workspaceStrategy.worktreePath
        : "",
    enabled: task.enabled,
    startFromOrigin:
      task.workspaceStrategy.type === "worktree"
        ? (task.workspaceStrategy.startFromOrigin ?? false)
        : true,
    runtimeMode: task.runtimeMode,
  };
}
