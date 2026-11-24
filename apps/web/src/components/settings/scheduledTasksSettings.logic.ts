import { isListedThreadLineage } from "@onicode/contracts";
import {
  EnvironmentId,
  type ProjectId,
  ScheduledTaskId,
  type ScheduledTask,
  type ScheduledTaskSchedule,
  type ScheduledTaskThreadMode,
  type ScheduledTaskUpsertInput,
  type ScheduledTaskUpsertSchedule,
  scheduledTaskThreadMode,
  type ModelSelection,
  type RuntimeMode,
  type ProviderInteractionMode,
  type ServerSettings,
  type ThreadId,
} from "@onicode/contracts";
import type { EnvironmentThreadShell } from "@onicode/client-runtime/state/models";
import { scheduleTimeZoneLabel } from "@onicode/client-runtime/scheduled-task-time-zone";
import type { TimestampFormat } from "@onicode/contracts/settings";
import { msg } from "gt-react";

import {
  resolveProjectSettings,
  type LegacyProjectSettingsFields,
} from "@onicode/shared/projectSettings";
import { getActiveLocale, m, type MessageResolver } from "../../i18n/messages";
import type { ProviderInstanceEntry } from "../../providerInstances";
import { formatDateTimeTimestamp, formatTimeOfDay } from "../../timestampFormat";

import type { ResolvedSettingsScope } from "./settingsScope";

/** Abbreviated weekday name, 0 = Sunday. */
export function weekdayShortLabel(day: number, locale: string = getActiveLocale()): string {
  // 2024-01-07 was a Sunday.
  return new Intl.DateTimeFormat(locale, { weekday: "short", timeZone: "UTC" }).format(
    new Date(Date.UTC(2024, 0, 7 + day)),
  );
}

export function scheduleLabel(
  schedule: ScheduledTaskSchedule,
  timestampFormat: TimestampFormat,
  timeZone?: string,
  runAt?: string | null,
  resolve: MessageResolver = m,
  locale: string = getActiveLocale(),
): string {
  if (schedule.type === "once") {
    return resolve(
      msg("Once, {time}", { time: formatDateTimeTimestamp(schedule.runAt, timestampFormat) }),
    );
  }
  if (schedule.type === "interval") {
    const minutes = schedule.everyMs / 60_000;
    return Number.isInteger(minutes)
      ? resolve(msg("Every {minutes} min", { minutes }))
      : resolve(msg("Every {seconds} sec", { seconds: Math.round(schedule.everyMs / 1000) }));
  }
  const weekdays = schedule.weekdays ?? [];
  const time = formatTimeOfDay(schedule.timeOfDay, timestampFormat);
  const zone = scheduleTimeZoneLabel(timeZone, runAt);
  if (weekdays.length === 0) {
    return zone === undefined
      ? resolve(msg("Daily at {time}", { time }))
      : resolve(msg("Daily at {time} {zone}", { time, zone }));
  }
  if (weekdays.length === 5 && weekdays.every((day) => day >= 1 && day <= 5)) {
    return zone === undefined
      ? resolve(msg("Weekdays at {time}", { time }))
      : resolve(msg("Weekdays at {time} {zone}", { time, zone }));
  }
  const days = weekdays
    .map((day) => (day >= 0 && day <= 6 ? weekdayShortLabel(day, locale) : ""))
    .join(", ");
  return zone === undefined
    ? resolve(msg("{days} at {time}", { days, time }))
    : resolve(msg("{days} at {time} {zone}", { days, time, zone }));
}

export function matchesScheduledTaskScope(
  scope: ResolvedSettingsScope,
  environmentId: EnvironmentId,
  projectId: ProjectId,
): boolean {
  if (scope.kind === "unavailable" || !scope.environmentIds.includes(environmentId)) return false;
  if (scope.kind === "project" || scope.kind === "checkout") {
    return scope.members.some(
      (member) => member.environmentId === environmentId && member.id === projectId,
    );
  }
  return true;
}

export function validateScheduledTasksSearch(raw: Record<string, unknown>) {
  return {
    ...(typeof raw.environmentId === "string" && raw.environmentId.trim()
      ? { environmentId: EnvironmentId.make(raw.environmentId) }
      : {}),
    ...(typeof raw.taskId === "string" && raw.taskId.trim()
      ? { taskId: ScheduledTaskId.make(raw.taskId) }
      : {}),
  };
}

export type ScheduleMode = "fixed" | "once" | "interval";
export type WorkspaceMode = "root" | "worktree" | "existing_worktree";

export interface DraftState {
  readonly editingId: string | null;
  readonly title: string;
  readonly prompt: string;
  readonly enabled: boolean;
  readonly scheduleMode: ScheduleMode;
  readonly intervalMinutes: string;
  readonly timeOfDay: string;
  /** Local date and time as a `datetime-local` input value, such as 2026-11-02T15:00. */
  readonly runAt: string;
  /** The stored ISO run time, sent back unchanged when the field was not edited. */
  readonly savedRunAt: string;
  readonly weekdays: ReadonlySet<number>;
  readonly projectId: string;
  readonly threadId: string;
  /** The thread the form opened with, so a save can tell a cleared destination from an unbound one. */
  readonly savedThreadId: string;
  readonly threadMode: ScheduledTaskThreadMode;
  readonly workspaceMode: WorkspaceMode;
  readonly baseRef: string;
  readonly startFromOrigin: boolean;
  readonly existingWorktreePath: string;
  readonly modelKey: string;
  readonly runtimeMode: RuntimeMode;
  readonly interactionMode: ProviderInteractionMode;
  /** Retain the source selection's provider options when the picker leaves instanceId:model unchanged. */
  readonly baseModelSelection: ModelSelection | null;
  /** The task as the form loaded it, so a save is refused once another client changed it. */
  readonly saved: ScheduledTask | null;
}

export const savedTaskFields = (
  task: ScheduledTask,
): NonNullable<ScheduledTaskUpsertInput["expected"]> => ({
  title: task.title,
  prompt: task.prompt,
  enabled: task.enabled,
  schedule: task.schedule,
});

/** Formats a date as a `datetime-local` input value in the browser's time zone. */
export function toDateTimeLocalValue(date: Date): string {
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

export function scheduleDraftErrors(draft: DraftState, resolve: MessageResolver = m) {
  const invalidRunTime = (description: string) => [
    { title: resolve(msg("Invalid run time")), description },
  ];
  if (draft.scheduleMode === "fixed" && !draft.timeOfDay)
    return invalidRunTime(resolve(msg("Pick a time.")));
  if (draft.scheduleMode !== "once") return [];
  const runAt = new Date(draft.runAt);
  if (Number.isNaN(runAt.getTime())) return invalidRunTime(resolve(msg("Pick a date and time.")));
  // A time skipped by a daylight saving change parses to a different clock time.
  if (toDateTimeLocalValue(runAt) !== draft.runAt)
    return invalidRunTime(resolve(msg("That time does not exist in your time zone.")));
  return [];
}

export function scheduleFromDraft(draft: DraftState): ScheduledTaskUpsertSchedule | null {
  if (scheduleDraftErrors(draft).length > 0) return null;
  if (draft.scheduleMode === "once") {
    const saved = draft.savedRunAt ? new Date(draft.savedRunAt) : null;
    if (saved !== null && toDateTimeLocalValue(saved) === draft.runAt)
      return { type: "once", runAt: draft.savedRunAt };
    // A `datetime-local` value has no offset. In an hour that repeats when clocks go
    // back, keep the saved time's offset; otherwise Date reads it as local time.
    const withSavedOffset =
      saved === null
        ? null
        : new Date(Date.parse(`${draft.runAt}Z`) + saved.getTimezoneOffset() * 60_000);
    return {
      type: "once",
      runAt:
        withSavedOffset !== null && toDateTimeLocalValue(withSavedOffset) === draft.runAt
          ? withSavedOffset.toISOString()
          : new Date(draft.runAt).toISOString(),
    };
  }
  if (draft.scheduleMode === "interval") {
    const everyMs = Math.round(Number(draft.intervalMinutes) * 60_000);
    return { type: "interval", everyMs };
  }
  const selectedEveryDay = draft.weekdays.size === 0 || draft.weekdays.size === 7;
  return {
    type: "fixed_time",
    timeOfDay: draft.timeOfDay,
    ...(selectedEveryDay ? {} : { weekdays: [...draft.weekdays].toSorted() }),
  };
}

export function taskToDraft(task: ScheduledTask): DraftState {
  const schedule = task.schedule;
  const weekdays =
    schedule.type === "fixed_time" && schedule.weekdays && schedule.weekdays.length > 0
      ? new Set(schedule.weekdays)
      : new Set([0, 1, 2, 3, 4, 5, 6]);
  return {
    editingId: task.id,
    title: task.title,
    prompt: task.prompt,
    enabled: task.enabled,
    scheduleMode: schedule.type === "fixed_time" ? "fixed" : schedule.type,
    intervalMinutes:
      schedule.type === "interval" ? String(Math.max(1, schedule.everyMs / 60_000)) : "15",
    // A time input shows nothing for an unpadded hour such as 9:05, which agents may save.
    timeOfDay: schedule.type === "fixed_time" ? schedule.timeOfDay.padStart(5, "0") : "09:00",
    runAt: schedule.type === "once" ? toDateTimeLocalValue(new Date(schedule.runAt)) : "",
    savedRunAt: schedule.type === "once" ? schedule.runAt : "",
    weekdays,
    projectId: task.projectId,
    threadId: task.threadId ?? "",
    savedThreadId: task.threadId ?? "",
    threadMode: scheduledTaskThreadMode(task),
    workspaceMode: task.workspaceStrategy.type,
    baseRef: task.workspaceStrategy.type === "worktree" ? task.workspaceStrategy.baseRef : "",
    startFromOrigin:
      task.workspaceStrategy.type === "worktree"
        ? (task.workspaceStrategy.startFromOrigin ?? false)
        : true,
    existingWorktreePath:
      task.workspaceStrategy.type === "existing_worktree"
        ? task.workspaceStrategy.worktreePath
        : "",
    modelKey: `${task.modelSelection.instanceId}:${task.modelSelection.model}`,
    runtimeMode: task.runtimeMode,
    interactionMode: task.interactionMode,
    baseModelSelection: task.modelSelection,
    saved: task,
  };
}

const SCHEDULE_FIELDS = [
  "scheduleMode",
  "intervalMinutes",
  "timeOfDay",
  "runAt",
  "savedRunAt",
  "weekdays",
] as const;

const sameValue = (left: unknown, right: unknown) =>
  left instanceof Set && right instanceof Set
    ? left.size === right.size && [...left].every((value) => right.has(value))
    : left === right;

/**
 * After a save another client's change refused: fields the user left alone take the task's
 * current values, the user's edits stay, and the next save checks against what is stored now.
 */
export function rebaseDraft(draft: DraftState, current: ScheduledTask): DraftState {
  if (draft.saved === null) return draft;
  const loaded = taskToDraft(draft.saved);
  const now = taskToDraft(current);
  const untouched = (key: keyof DraftState) => sameValue(draft[key], loaded[key]);
  return {
    ...draft,
    ...(untouched("title") ? { title: now.title } : {}),
    ...(untouched("prompt") ? { prompt: now.prompt } : {}),
    ...(untouched("enabled") ? { enabled: now.enabled } : {}),
    ...(SCHEDULE_FIELDS.every(untouched)
      ? {
          scheduleMode: now.scheduleMode,
          intervalMinutes: now.intervalMinutes,
          timeOfDay: now.timeOfDay,
          runAt: now.runAt,
          savedRunAt: now.savedRunAt,
          weekdays: now.weekdays,
        }
      : {}),
    ...(untouched("threadId") ? { threadId: now.threadId } : {}),
    ...(untouched("threadMode") ? { threadMode: now.threadMode } : {}),
    savedThreadId: now.savedThreadId,
    saved: now.saved,
  };
}

// Fields a mode leaves behind when the user switches away and back are not edits.
function relevantDraftFields(draft: DraftState, defaultModelKey: string) {
  const { weekdays: _weekdays, saved: _saved, ...fields } = draft;
  return {
    ...fields,
    modelKey: draft.modelKey || defaultModelKey,
    intervalMinutes: draft.scheduleMode === "interval" ? draft.intervalMinutes : "",
    timeOfDay: draft.scheduleMode === "fixed" ? draft.timeOfDay : "",
    runAt: draft.scheduleMode === "once" ? draft.runAt : "",
    savedRunAt: draft.scheduleMode === "once" ? draft.savedRunAt : "",
    baseRef: draft.workspaceMode === "worktree" ? draft.baseRef : "",
    startFromOrigin: draft.workspaceMode === "worktree" ? draft.startFromOrigin : false,
    existingWorktreePath:
      draft.workspaceMode === "existing_worktree" ? draft.existingWorktreePath : "",
  };
}

export type EditorLeaveDecision = "allow" | "block" | "wait-for-save" | "ask";

/** What a navigation does to an open editor; one that keeps the editor mounted needs no answer. */
export function editorLeaveDecision(input: {
  readonly closing: boolean;
  readonly unmountsEditor: boolean;
  readonly asking: boolean;
  readonly saving: boolean;
}): EditorLeaveDecision {
  if (input.closing || !input.unmountsEditor) return "allow";
  if (input.asking) return "block";
  // Leaving cannot cancel an upsert already sent.
  if (input.saving) return "wait-for-save";
  return "ask";
}

/** `defaultModelKey` is the model the picker shows while the draft names none. */
export function hasDraftChanges(
  initial: DraftState,
  current: DraftState,
  defaultModelKey: string,
): boolean {
  const before = relevantDraftFields(initial, defaultModelKey);
  const after = relevantDraftFields(current, defaultModelKey);
  const weekdaysChanged =
    current.scheduleMode === "fixed" &&
    (initial.weekdays.size !== current.weekdays.size ||
      [...initial.weekdays].some((day) => !current.weekdays.has(day)));
  return (
    weekdaysChanged ||
    (Object.keys(before) as Array<keyof typeof before>).some((key) => before[key] !== after[key])
  );
}

export function moveDraftToThread(
  draft: DraftState,
  thread: { readonly id: ThreadId; readonly projectId: ProjectId },
): DraftState {
  return {
    ...draft,
    threadId: thread.id,
    projectId: thread.projectId,
    threadMode: draft.threadMode === "new_thread" ? "continue" : draft.threadMode,
  };
}

/** Leaves the thread but keeps the mode. Matches MCP: new threads stay out of the shared checkout. */
export function unbindDraftFromThread(draft: DraftState): DraftState {
  if (!draft.threadId) return draft;
  return {
    ...draft,
    threadId: "",
    ...(draft.workspaceMode === "root"
      ? { workspaceMode: "worktree" as const, startFromOrigin: true }
      : {}),
  };
}

export function changeDraftProject(draft: DraftState, projectId: string): DraftState {
  if (projectId === draft.projectId) return draft;
  return { ...unbindDraftFromThread(draft), projectId };
}

/** Threads a task can move to. The current one is left out; the picker always shows it. */
export function scheduledTaskDestinationThreads(
  threads: ReadonlyArray<EnvironmentThreadShell>,
  target: {
    readonly environmentId: EnvironmentId;
    readonly projectId: string;
    readonly currentThreadId: ThreadId | null;
    readonly query: string;
  },
): ReadonlyArray<EnvironmentThreadShell> {
  const search = target.query.trim().toLocaleLowerCase();
  return threads
    .filter(
      (thread) =>
        thread.environmentId === target.environmentId &&
        thread.projectId === target.projectId &&
        thread.id !== target.currentThreadId &&
        thread.archivedAt === null &&
        isListedThreadLineage(thread.lineage) &&
        thread.title.toLocaleLowerCase().includes(search),
    )
    .toSorted((left, right) => right.updatedAt.localeCompare(left.updatedAt));
}

export function scheduledTaskDefaultModel(
  settings: ServerSettings,
  project: (LegacyProjectSettingsFields & { readonly id: ProjectId }) | null,
  entries: readonly ProviderInstanceEntry[],
): ModelSelection | null {
  const available = entries.filter(
    (entry) =>
      entry.enabled &&
      entry.installed &&
      entry.isAvailable &&
      entry.snapshot.auth.status !== "unauthenticated",
  );
  const configured = resolveProjectSettings(settings, project?.id ?? null, project).settings
    .defaultModelSelection;
  for (const selection of [configured, settings.defaultModelSelection]) {
    if (
      selection &&
      available.some(
        (entry) =>
          entry.instanceId === selection.instanceId &&
          entry.models.find((model) => model.slug === selection.model)?.isLegacy !== true,
      )
    )
      return selection;
  }
  const models = available.flatMap((entry) =>
    entry.models
      .filter((model) => !model.isLegacy)
      .map((model) => ({ instanceId: entry.instanceId, model })),
  );
  const fallback = models.find(({ model }) => model.isDefault) ?? models[0];
  return fallback ? { instanceId: fallback.instanceId, model: fallback.model.slug } : null;
}

/**
 * A new task's project: a pinned open project, then any open one. An archived
 * project only when Settings is scoped to it, so that scope can still save.
 */
export function defaultScheduledTaskProjectId<
  TProject extends {
    readonly id: ProjectId;
    readonly pinnedAt?: string | null | undefined;
    readonly archivedAt?: string | null | undefined;
  },
>(scopedProjects: ReadonlyArray<TProject>, scope: ResolvedSettingsScope): ProjectId | "" {
  const open = scopedProjects.filter((project) => project.archivedAt == null);
  const scopedToProject = scope.kind === "project" || scope.kind === "checkout";
  return (
    (
      open.find((project) => project.pinnedAt != null) ??
      open[0] ??
      (scopedToProject ? scopedProjects[0] : undefined)
    )?.id ?? ""
  );
}
