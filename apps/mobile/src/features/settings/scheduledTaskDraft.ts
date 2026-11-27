import { rebaseScheduledTaskDestination } from "@onicode/client-runtime/scheduled-task-draft";
import { isWebhookDedupeTemplate, scheduledTaskThreadMode } from "@onicode/contracts";
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
  parseWebhookFilterDrafts,
  webhookFilterDrafts,
  webhookFilterDraftsChanged,
  type WebhookFilterDraft,
} from "@onicode/client-runtime/scheduled-task-webhook";
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
  readonly mode: "fixed_time" | "once" | "interval" | "webhook";
  readonly timeOfDay: string;
  /** ISO time for a one-time run. Empty until the user picks the "Once" mode. */
  readonly runAt: string;
  readonly weekdays: ReadonlyArray<number>;
  readonly intervalMinutes: string;
  readonly signatureEnabled: boolean;
  readonly signatureHeader: string;
  readonly signatureEncoding: "hex" | "base64";
  readonly signaturePrefix: string;
  /** Write-only: empty keeps the secret already stored on the server. */
  readonly signatureSecret: string;
  readonly replayKey: string;
  readonly coalesceKey: string;
  readonly coalesceWindowSeconds: string;
  readonly filters: ReadonlyArray<WebhookFilterDraft>;
  /** The filter rows the form opened with. */
  readonly savedFilters: ReadonlyArray<WebhookFilterDraft>;
};

export const DEFAULT_SCHEDULE: ScheduleDraft = {
  mode: "fixed_time",
  timeOfDay: "09:00",
  runAt: "",
  weekdays: [1, 2, 3, 4, 5],
  intervalMinutes: "15",
  signatureEnabled: false,
  signatureHeader: "x-hub-signature-256",
  signatureEncoding: "hex",
  signaturePrefix: "sha256=",
  signatureSecret: "",
  replayKey: "",
  coalesceKey: "",
  coalesceWindowSeconds: "",
  filters: [],
  savedFilters: [],
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
    case "webhook": {
      const { signature, dedupe, filters } = task.schedule;
      const drafts = webhookFilterDrafts(filters);
      return {
        ...DEFAULT_SCHEDULE,
        mode: "webhook",
        ...(signature === null
          ? {}
          : {
              signatureEnabled: true,
              signatureHeader: signature.header,
              signatureEncoding: signature.encoding,
              signaturePrefix: signature.prefix,
            }),
        replayKey: dedupe?.replayKey ?? "",
        coalesceKey: dedupe?.coalesceKey ?? "",
        coalesceWindowSeconds:
          dedupe?.coalesceWindowSeconds === undefined ? "" : String(dedupe.coalesceWindowSeconds),
        filters: drafts,
        savedFilters: drafts,
      };
    }
  }
}

export function parseIntervalMinutes(value: string): number {
  return Number(value.trim().replace(",", "."));
}

/** Whole seconds from 10 to 3600, or undefined for blank; null when invalid. */
function parseCoalesceWindow(value: string): number | undefined | null {
  const trimmed = value.trim();
  if (!trimmed) return undefined;
  const seconds = Number(trimmed);
  return Number.isInteger(seconds) && seconds >= 10 && seconds <= 3600 ? seconds : null;
}

/** Which of the webhook settings would stop the save, so the form can say which. */
export function webhookScheduleProblems(draft: ScheduleDraft) {
  const invalidKey = (key: string) => key.trim() !== "" && !isWebhookDedupeTemplate(key.trim());
  return {
    replayKey: invalidKey(draft.replayKey),
    coalesceKey: invalidKey(draft.coalesceKey),
    coalesceWindow: parseCoalesceWindow(draft.coalesceWindowSeconds) === null,
    signatureHeader: draft.signatureEnabled && !draft.signatureHeader.trim(),
  };
}

export function scheduleFromDraft(draft: ScheduleDraft): ScheduledTaskUpsertSchedule | null {
  if (draft.mode === "webhook") {
    if (Object.values(webhookScheduleProblems(draft)).some(Boolean)) return null;
    const parsedFilters = parseWebhookFilterDrafts(draft.filters);
    if (!("filters" in parsedFilters)) return null;
    const replayKey = draft.replayKey.trim();
    const coalesceKey = draft.coalesceKey.trim();
    const coalesceWindowSeconds = parseCoalesceWindow(draft.coalesceWindowSeconds) ?? undefined;
    const secret = draft.signatureSecret.trim();
    return {
      type: "webhook",
      // No secret is sent unless typed, so the server keeps the stored one.
      signature: draft.signatureEnabled
        ? {
            header: draft.signatureHeader.trim(),
            encoding: draft.signatureEncoding,
            prefix: draft.signaturePrefix,
            ...(secret ? { secret } : {}),
          }
        : null,
      dedupe: {
        ...(replayKey ? { replayKey } : {}),
        ...(coalesceKey ? { coalesceKey } : {}),
        ...(coalesceWindowSeconds === undefined ? {} : { coalesceWindowSeconds }),
      },
      // Only edited filters are sent; the server keeps the stored ones otherwise.
      ...(webhookFilterDraftsChanged(draft.savedFilters, draft.filters)
        ? { filters: parsedFilters.filters }
        : {}),
    };
  }
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
function scheduleSettingsSignature(schedule: ScheduleDraft) {
  const { mode } = schedule;
  const webhook = mode === "webhook";
  const signed = webhook && schedule.signatureEnabled;
  return [
    mode,
    mode === "fixed_time" ? schedule.timeOfDay : null,
    mode === "once" ? schedule.runAt : null,
    mode === "fixed_time" ? [...schedule.weekdays].sort((a, b) => a - b) : null,
    mode === "interval" ? schedule.intervalMinutes : null,
    webhook ? schedule.signatureEnabled : null,
    signed
      ? [
          schedule.signatureHeader,
          schedule.signatureEncoding,
          schedule.signaturePrefix,
          schedule.signatureSecret,
        ]
      : null,
    webhook ? [schedule.replayKey, schedule.coalesceKey, schedule.coalesceWindowSeconds] : null,
  ];
}

const filtersEdited = (schedule: ScheduleDraft) =>
  schedule.mode === "webhook" &&
  webhookFilterDraftsChanged(schedule.savedFilters, schedule.filters);

function scheduleSignature(schedule: ScheduleDraft) {
  return [
    scheduleSettingsSignature(schedule),
    schedule.mode === "webhook"
      ? schedule.filters.map(({ path, op, value }) => [path, op, op === "exists" ? "" : value])
      : null,
  ];
}

function draftSignature(draft: ScheduledTaskDraft): string {
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
    scheduleSignature(draft.schedule),
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
  return { ...unbindDraftFromThread(draft), projectId, baseRef: "", checkoutPath: "" };
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
  const opened = editDraft(loaded);
  const now = editDraft(current);
  const workspaceUntouched =
    draft.workspace === opened.workspace &&
    draft.baseRef === opened.baseRef &&
    draft.checkoutPath === opened.checkoutPath &&
    draft.startFromOrigin === opened.startFromOrigin;
  const modelUntouched =
    JSON.stringify(draft.modelSelection) === JSON.stringify(loaded.modelSelection);
  const nextSchedule = scheduleDraftForTask(current);
  const editedFilters = filtersEdited(draft.schedule);
  // Edited filters belong to a webhook trigger, so they keep the task one.
  const takeStoredSchedule =
    JSON.stringify(scheduleSettingsSignature(draft.schedule)) ===
      JSON.stringify(scheduleSettingsSignature(scheduleDraftForTask(loaded))) &&
    !(editedFilters && nextSchedule.mode !== "webhook");
  // A webhook the user kept over a stored timer keeps its filters too.
  const keepFilters =
    editedFilters ||
    (!takeStoredSchedule && draft.schedule.mode === "webhook" && nextSchedule.mode !== "webhook");
  return {
    ...draft,
    ...(draft.title === loaded.title ? { title: current.title } : {}),
    ...(draft.prompt === loaded.prompt ? { prompt: current.prompt } : {}),
    ...(draft.enabled === loaded.enabled ? { enabled: current.enabled } : {}),
    schedule: {
      ...(takeStoredSchedule ? nextSchedule : draft.schedule),
      // Edited filter rows stay; untouched ones take what is stored now.
      filters: keepFilters ? draft.schedule.filters : nextSchedule.filters,
      savedFilters: nextSchedule.savedFilters,
    },
    ...rebaseScheduledTaskDestination(draft, loaded, current),
    ...(workspaceUntouched
      ? {
          workspace: now.workspace,
          baseRef: now.baseRef,
          checkoutPath: now.checkoutPath,
          startFromOrigin: now.startFromOrigin,
        }
      : {}),
    ...(modelUntouched ? { modelSelection: now.modelSelection } : {}),
    ...(draft.runtimeMode === loaded.runtimeMode ? { runtimeMode: current.runtimeMode } : {}),
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
