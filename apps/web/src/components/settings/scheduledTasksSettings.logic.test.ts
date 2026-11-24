import {
  EnvironmentId,
  ProjectId,
  DEFAULT_SERVER_SETTINGS,
  type ServerConfig,
  ProviderInstanceId,
  ScheduledTaskId,
  type ModelSelection,
  type ScheduledTask,
  ThreadId,
} from "@onicode/contracts";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";

import type {
  SidebarProjectGroupMember,
  SidebarProjectSnapshot,
} from "../../sidebarProjectGrouping";
import { resolveSettingsScope, type SettingsScopeSearch } from "./settingsScope";

import { deriveProviderInstanceEntries } from "../../providerInstances";
import {
  scheduledTaskDefaultModel,
  defaultScheduledTaskProjectId,
  hasDraftChanges,
  modelSelectionKey,
  editorLeaveDecision,
  matchesScheduledTaskScope,
  scheduleFromDraft,
  scheduleLabel,
  rebaseDraft,
  taskToDraft,
  unbindDraftFromThread,
} from "./scheduledTasksSettings.logic";

const laptopId = EnvironmentId.make("laptop");
const serverId = EnvironmentId.make("server");
const environments = [
  { environmentId: laptopId, label: "Laptop" },
  { environmentId: serverId, label: "Server" },
];

function member(id: string, environmentId: EnvironmentId): SidebarProjectGroupMember {
  return {
    id: ProjectId.make(id),
    environmentId,
    title: "Onicode",
    workspaceRoot: `/repos/${id}`,
    physicalProjectKey: `${environmentId}:/repos/${id}`,
    environmentLabel:
      environments.find((environment) => environment.environmentId === environmentId)?.label ??
      null,
    defaultModelSelection: null,
    scripts: [],
    createdAt: "2026-09-07T00:00:00.000Z",
    updatedAt: "2026-09-07T00:00:00.000Z",
  };
}

const first = member("first", laptopId);
const second = member("second", laptopId);
const third = member("third", serverId);
const other = member("other", serverId);

function group(
  projectKey: string,
  members: readonly SidebarProjectGroupMember[],
): SidebarProjectSnapshot {
  return {
    ...members[0]!,
    projectKey,
    displayName: projectKey,
    memberProjects: members,
    memberProjectRefs: members.map((project) => ({
      environmentId: project.environmentId,
      projectId: project.id,
    })),
    groupedProjectCount: members.length,
    environmentPresence: "mixed",
    allRemoteMembersAreDesktopLocal: false,
    allRemoteMembersAreWsl: false,
    remoteEnvironmentLabels: [],
    pinned: false,
    archived: false,
  };
}

const sameIdElsewhere = member("first", serverId);
const groups = [group("onicode", [first, second, third]), group("other", [other, sameIdElsewhere])];
const tasks = [first, second, third, other, sameIdElsewhere].map((project, index) => ({
  id: `task-${index}`,
  environmentId: project.environmentId,
  projectId: project.id,
}));

describe("scheduled task settings scope", () => {
  it.each<{ search: SettingsScopeSearch; expected: string[] }>([
    { search: {}, expected: ["task-0", "task-1", "task-2", "task-3", "task-4"] },
    { search: { machine: laptopId }, expected: ["task-0", "task-1"] },
    { search: { project: "onicode" }, expected: ["task-0", "task-1", "task-2"] },
    { search: { project: "onicode", machine: serverId }, expected: ["task-2"] },
    { search: { project: "onicode", checkout: second.physicalProjectKey }, expected: ["task-1"] },
    { search: { project: "missing" }, expected: [] },
    { search: { machine: "removed" }, expected: [] },
    { search: { project: "onicode", checkout: "removed" }, expected: [] },
    { search: { project: "other", machine: laptopId }, expected: [] },
  ])("lists only matching tasks for $search", ({ search, expected }) => {
    const scope = resolveSettingsScope(search, groups, environments);
    expect(
      tasks
        .filter((task) => matchesScheduledTaskScope(scope, task.environmentId, task.projectId))
        .map((task) => task.id),
    ).toEqual(expected);
  });

  it("defaults a new task to a pinned open project, and to an archived one only when scoped to it", () => {
    const at = "2026-01-01T00:00:00.000Z";
    const project = (id: string, pinnedAt: string | null, archivedAt: string | null) => ({
      id: ProjectId.make(id),
      pinnedAt,
      archivedAt,
    });
    const all = resolveSettingsScope({}, groups, environments);
    const scoped = resolveSettingsScope({ project: "onicode" }, groups, environments);
    expect(
      defaultScheduledTaskProjectId([project("a", null, null), project("b", at, null)], all),
    ).toBe("b");
    expect(defaultScheduledTaskProjectId([project("x", null, at)], all)).toBe("");
    expect(defaultScheduledTaskProjectId([project("x", null, at)], scoped)).toBe("x");
  });

  it("keeps tasks with removed projects manageable at environment scope", () => {
    const removedProject = ProjectId.make("removed");
    expect(
      matchesScheduledTaskScope(
        resolveSettingsScope({}, groups, environments),
        laptopId,
        removedProject,
      ),
    ).toBe(true);
    expect(
      matchesScheduledTaskScope(
        resolveSettingsScope({ project: "onicode" }, groups, environments),
        laptopId,
        removedProject,
      ),
    ).toBe(false);
  });
});

const legacyTask: ScheduledTask = {
  id: ScheduledTaskId.make("legacy-task"),
  title: "Review issues",
  prompt: "Review open issues",
  enabled: true,
  schedule: { type: "interval", everyMs: 60_000 },
  projectId: ProjectId.make("project"),
  threadId: null,
  workspaceStrategy: { type: "worktree", baseRef: "release" },
  modelSelection: { instanceId: ProviderInstanceId.make("codex"), model: "gpt-5.4" },
  runtimeMode: "full-access",
  interactionMode: "default",
  createdBy: "user",
  creationSource: "web",
  createdAt: "2026-09-17T00:00:00.000Z",
  updatedAt: "2026-09-17T00:00:00.000Z",
  nextRunAt: null,
  lastRunAt: null,
  lastRunStatus: "never",
  lastRunError: null,
  runCount: 0,
};

describe("editing scheduled task branch settings", () => {
  it("keeps an omitted origin flag on the local base branch", () => {
    const draft = taskToDraft(legacyTask);
    expect(draft.baseRef).toBe("release");
    expect(draft.startFromOrigin).toBe(false);
  });

  it("leaves the base automatic when a task without a worktree switches to one", () => {
    const draft = taskToDraft({ ...legacyTask, workspaceStrategy: { type: "root" } });
    expect(draft.baseRef).toBe("");
  });
});

describe("a scheduled task save refused by another client's change", () => {
  it("keeps the user's edits and takes the stored values of the fields left alone", () => {
    const loaded = { ...legacyTask, schedule: { type: "fixed_time", timeOfDay: "09:00" } } as const;
    const edited = { ...taskToDraft(loaded), prompt: "Review twice" };
    // An agent paused the task and moved its time while the form was open.
    const current = {
      ...loaded,
      enabled: false,
      schedule: { type: "fixed_time", timeOfDay: "10:30" },
    } as const;
    const rebased = rebaseDraft(edited, current);
    expect(rebased).toMatchObject({ prompt: "Review twice", enabled: false, timeOfDay: "10:30" });
    expect(rebased.saved).toMatchObject({ enabled: false, schedule: current.schedule });

    // A field the user changed keeps their value even when it also changed elsewhere.
    const retimed = rebaseDraft({ ...taskToDraft(loaded), timeOfDay: "08:00" }, current);
    expect(retimed.timeOfDay).toBe("08:00");

    // Edited filters keep a webhook task a webhook task when another client made it a timer.
    const hook = {
      ...loaded,
      schedule: {
        type: "webhook",
        signature: null,
        filters: [{ path: "body.action", op: "equals", value: "opened" }],
      },
    } as const;
    const hookDraft = taskToDraft(hook);
    const refiltered = rebaseDraft(
      { ...hookDraft, filters: hookDraft.filters.map((row) => ({ ...row, value: "closed" })) },
      { ...hook, schedule: { type: "interval", everyMs: 3_600_000 } },
    );
    expect(refiltered.scheduleMode).toBe("webhook");
    expect(refiltered.filters.map((row) => row.value)).toEqual(["closed"]);
    // Filters left behind by a trip through the webhook mode are no edit.
    const roundTrip = { ...taskToDraft(loaded), title: "Renamed", filters: hookDraft.filters };
    expect(rebaseDraft(roundTrip, current).timeOfDay).toBe("10:30");

    // Another client bound a thread and changed the mode; the next save must not undo either.
    const bound = {
      ...current,
      threadId: ThreadId.make("thread:bound"),
      threadMode: "fresh_session",
    } as const;
    expect(rebaseDraft(edited, bound)).toMatchObject({
      threadId: "thread:bound",
      savedThreadId: "thread:bound",
      threadMode: "fresh_session",
    });

    // Nor may it undo a move, a workspace change or a model change.
    const moved = {
      ...current,
      projectId: ProjectId.make("moved"),
      workspaceStrategy: { type: "existing_worktree", worktreePath: "/checkout" },
      modelSelection: { instanceId: ProviderInstanceId.make("claude"), model: "opus" },
    } as const;
    expect(rebaseDraft(edited, moved)).toMatchObject({
      projectId: "moved",
      workspaceMode: "existing_worktree",
      existingWorktreePath: "/checkout",
      modelSelection: moved.modelSelection,
    });
  });

  it("takes stored webhook filters the user left alone and still sends the ones they edited", () => {
    const loaded = {
      ...legacyTask,
      schedule: {
        type: "webhook",
        signature: null,
        filters: [{ path: "body.action", op: "equals", value: "opened" }],
      },
    } as const;
    const stored = [{ path: "headers.x-event", op: "exists" }] as const;
    const current = { ...loaded, schedule: { ...loaded.schedule, filters: stored } } as const;

    const untouched = rebaseDraft({ ...taskToDraft(loaded), title: "Renamed" }, current);
    expect(untouched.filters).toMatchObject(stored);
    expect(scheduleFromDraft(untouched)).not.toHaveProperty("filters");

    const rebased = rebaseDraft({ ...taskToDraft(loaded), filters: [] }, current);
    expect(scheduleFromDraft(rebased)).toMatchObject({ filters: [] });
  });
});

describe("fixed-time scheduled tasks", () => {
  it("pads a stored single-digit hour so the time input shows it", () => {
    const draft = taskToDraft({
      ...legacyTask,
      schedule: { type: "fixed_time", timeOfDay: "9:05" },
    });
    expect(draft.timeOfDay).toBe("09:05");
  });

  it("keeps a fixed run time and rejects a cleared one", () => {
    const draft = taskToDraft({
      ...legacyTask,
      schedule: { type: "fixed_time", timeOfDay: "14:30" },
    });
    expect(scheduleFromDraft(draft)).toEqual({ type: "fixed_time", timeOfDay: "14:30" });
    // A native time input reports "" when it is cleared or only partly filled.
    expect(scheduleFromDraft({ ...draft, timeOfDay: "" })).toBeNull();
  });
});

describe("unsaved scheduled task edits", () => {
  const draft = taskToDraft({
    ...legacyTask,
    schedule: { type: "fixed_time", timeOfDay: "09:00", weekdays: [1, 3] },
  });
  it.each([
    ["an untouched draft", draft, false],
    ["the same weekdays toggled off and back on", { ...draft, weekdays: new Set([3, 1]) }, false],
    ["an edited name", { ...draft, title: "Review stale issues" }, true],
    ["a weekday removed", { ...draft, weekdays: new Set([1]) }, true],
    ["other weekdays of the same count", { ...draft, weekdays: new Set([1, 4]) }, true],
    [
      "Once picked and switched back",
      { ...draft, runAt: "2026-11-23T09:00", savedRunAt: "2026-11-23T08:00:00.000Z" },
      false,
    ],
    ["an interval edited and switched back", { ...draft, intervalMinutes: "30" }, false],
    ["a different schedule mode", { ...draft, scheduleMode: "interval" as const }, true],
    [
      "a checkout path left by a checkout mode not chosen",
      { ...draft, existingWorktreePath: "/x" },
      false,
    ],
    [
      "webhook settings edited and switched back",
      { ...draft, replayKey: "{{body.id}}", signatureEnabled: true },
      false,
    ],
  ])("%s has changes: %s", (_label, current, expected) => {
    expect(hasDraftChanges(draft, current, "codex:gpt-5")).toBe(expected);
  });
  it("counts picking the model already shown as the default as no change", () => {
    const codex = ProviderInstanceId.make("codex");
    const shown = modelSelectionKey({ instanceId: codex, model: "gpt-5" });
    const blank = { ...draft, modelSelection: null };
    const pick = (model: string, options?: ModelSelection["options"]) => ({
      ...draft,
      modelSelection: { instanceId: codex, model, ...(options ? { options } : {}) },
    });
    expect(hasDraftChanges(blank, pick("gpt-5"), shown)).toBe(false);
    expect(hasDraftChanges(blank, pick("gpt-6"), shown)).toBe(true);
    expect(hasDraftChanges(blank, pick("gpt-5", [{ id: "effort", value: "high" }]), shown)).toBe(
      true,
    );
    const effortThenFast = pick("gpt-5", [
      { id: "effort", value: "high" },
      { id: "fastMode", value: true },
    ]);
    const fastThenEffort = pick("gpt-5", [
      { value: true, id: "fastMode" },
      { id: "effort", value: "high" },
    ]);
    expect(hasDraftChanges(effortThenFast, fastThenEffort, shown)).toBe(false);
  });
  it("compares webhook filter rows by content", () => {
    const webhook = taskToDraft({
      ...legacyTask,
      schedule: {
        type: "webhook",
        signature: null,
        filters: [{ path: "body.action", op: "equals", value: "opened" }],
      },
    });
    const readded = { ...webhook, filters: webhook.filters.map((filter) => ({ ...filter })) };
    expect(hasDraftChanges(webhook, readded, "codex:gpt-5")).toBe(false);
    const retargeted = { ...webhook, filters: [{ ...webhook.filters[0]!, value: "closed" }] };
    expect(hasDraftChanges(webhook, retargeted, "codex:gpt-5")).toBe(true);
  });
});

describe("leaving an open scheduled task editor", () => {
  const leaving = { closing: false, unmountsEditor: true, asking: false, saving: false };
  it.each([
    ["the editor closing itself", { ...leaving, closing: true }, "allow"],
    ["a navigation that keeps the editor mounted", { ...leaving, unmountsEditor: false }, "allow"],
    ["a navigation while the confirmation is open", { ...leaving, asking: true }, "block"],
    ["a navigation while the save is in flight", { ...leaving, saving: true }, "wait-for-save"],
    ["a navigation away from an idle editor", leaving, "ask"],
  ] as const)("%s: %s", (_label, input, expected) => {
    expect(editorLeaveDecision(input)).toBe(expected);
  });
});

describe("scheduled task destination", () => {
  it("unbinding leaves the shared checkout for a new worktree from the remote", () => {
    const rootBound = taskToDraft({
      ...legacyTask,
      threadId: ThreadId.make("thread:bound"),
      workspaceStrategy: { type: "root" },
    });
    expect(unbindDraftFromThread(rootBound)).toMatchObject({
      threadId: "",
      workspaceMode: "worktree",
      startFromOrigin: true,
    });
  });
});

describe("one-time scheduled tasks", () => {
  afterEach(() => vi.unstubAllEnvs());

  it("round-trips the run time through the local date-time input", () => {
    const runAt = "2026-11-03T17:00:42.000Z";
    const draft = taskToDraft({ ...legacyTask, schedule: { type: "once", runAt } });
    expect(draft.scheduleMode).toBe("once");
    expect(scheduleFromDraft(draft)).toEqual({ type: "once", runAt });
    const moved = scheduleFromDraft({ ...draft, runAt: "2026-11-04T09:30" });
    expect(moved).toEqual({ type: "once", runAt: new Date("2026-11-04T09:30").toISOString() });
    expect(scheduleFromDraft({ ...draft, runAt: "" })).toBeNull();
  });

  it("rejects a time skipped by daylight saving", () => {
    vi.stubEnv("TZ", "America/Los_Angeles");
    const draft = taskToDraft({
      ...legacyTask,
      schedule: { type: "once", runAt: "2027-03-14T18:00:00.000Z" },
    });
    expect(scheduleFromDraft({ ...draft, runAt: "2027-03-14T02:30" })).toBeNull();
  });

  it("keeps the saved offset when an edit stays in an hour that repeats", () => {
    vi.stubEnv("TZ", "America/Los_Angeles");
    // 01:30 PST, the second 01:30 on 2026-11-01.
    const draft = taskToDraft({
      ...legacyTask,
      schedule: { type: "once", runAt: "2026-11-01T09:30:00.000Z" },
    });
    expect(scheduleFromDraft({ ...draft, runAt: "2026-11-01T01:35" })).toEqual({
      type: "once",
      runAt: "2026-11-01T09:35:00.000Z",
    });
  });
});

describe("webhook scheduled tasks", () => {
  const signature = { header: "x-signature", encoding: "base64", prefix: "" } as const;
  const webhookTask: ScheduledTask = {
    ...legacyTask,
    schedule: { type: "webhook", signature },
    webhook: { path: "/api/hooks/legacy-task/token", url: null, hasSecret: true },
  };

  it("round-trips dedupe settings, clears them, and rejects invalid windows", () => {
    const dedupe = {
      replayKey: "{{headers.delivery-id}}",
      coalesceKey: "{{body.id}}",
      coalesceWindowSeconds: 10,
    };
    const draft = taskToDraft({
      ...webhookTask,
      schedule: { type: "webhook", signature: null, dedupe },
    });
    expect(scheduleFromDraft(draft)).toMatchObject({ dedupe });
    expect(
      scheduleFromDraft({
        ...draft,
        replayKey: " \n ",
        coalesceKey: " \t ",
        coalesceWindowSeconds: "",
      }),
    ).toMatchObject({ dedupe: {} });
    expect(
      scheduleFromDraft({
        ...draft,
        replayKey: `  ${dedupe.replayKey}  `,
        coalesceKey: `\t${dedupe.coalesceKey}\n`,
      }),
    ).toMatchObject({ dedupe });
    expect(scheduleFromDraft({ ...draft, coalesceWindowSeconds: "" })).toMatchObject({
      dedupe: { replayKey: dedupe.replayKey, coalesceKey: dedupe.coalesceKey },
    });
    for (const coalesceWindowSeconds of ["9", "3601", "1.5", "abc"]) {
      expect(scheduleFromDraft({ ...draft, coalesceWindowSeconds })).toBeNull();
    }
    expect(scheduleFromDraft({ ...draft, replayKey: "literal" })).toBeNull();
    expect(scheduleFromDraft({ ...draft, coalesceKey: "{{body.bad" })).toBeNull();
  });

  it("keeps a stored secret when the secret field is left blank, and clears the signature when turned off", () => {
    const draft = taskToDraft(webhookTask);
    expect(draft.scheduleMode).toBe("webhook");
    expect(draft.signatureSecret).toBe("");
    expect(scheduleFromDraft(draft)).toEqual({ type: "webhook", signature, dedupe: {} });
    expect(scheduleFromDraft({ ...draft, signatureSecret: " new " })).toEqual({
      type: "webhook",
      signature: { ...signature, secret: "new" },
      dedupe: {},
    });
    expect(scheduleFromDraft({ ...draft, signatureEnabled: false })).toEqual({
      type: "webhook",
      signature: null,
      dedupe: {},
    });
  });

  it("sends filters only when the form changed them", () => {
    const filters = [
      { path: "headers.x-github-event", op: "equals", value: "pull_request" },
      { path: "body.action", op: "in", value: ["opened", "reopened"] },
    ] as const;
    const draft = taskToDraft({
      ...webhookTask,
      schedule: { type: "webhook", signature, filters },
    });
    expect(draft.filters.map((filter) => filter.op)).toEqual(["equals", "in"]);
    // A title-only save leaves the key out, so the server keeps whatever is stored now.
    expect(scheduleFromDraft({ ...draft, title: "Renamed" })).not.toHaveProperty("filters");
    expect(scheduleFromDraft({ ...draft, filters: draft.filters.slice(0, 1) })).toMatchObject({
      filters: filters.slice(0, 1),
    });
    // Removing every row is a change, so it clears the stored filters.
    expect(scheduleFromDraft({ ...draft, filters: [] })).toMatchObject({ filters: [] });
    expect(
      scheduleFromDraft({
        ...draft,
        filters: [{ id: "f1", path: "body.action", op: "in", value: " , " }],
      }),
    ).toBeNull();
  });
});

describe("scheduled task model defaults", () => {
  const instanceId = ProviderInstanceId.make("codex");
  const projectId = ProjectId.make("project");
  const environmentSelection = {
    instanceId,
    model: "environment-model",
    options: [{ id: "reasoning", value: "high" }],
  };
  const projectSelection = { instanceId, model: "project-model" };
  const config = {
    settings: { ...DEFAULT_SERVER_SETTINGS, defaultModelSelection: environmentSelection },
    providers: [
      {
        instanceId,
        driver: "codex",
        displayName: "Codex",
        enabled: true,
        installed: true,
        status: "ready",
        auth: { status: "authenticated" },
        models: [
          { slug: "first-model", name: "First", isCustom: false, capabilities: null },
          {
            slug: "catalog-default",
            name: "Default",
            isDefault: true,
            isCustom: false,
            capabilities: null,
          },
          { slug: "environment-model", name: "Environment", isCustom: false, capabilities: null },
          { slug: "project-model", name: "Project", isCustom: false, capabilities: null },
        ],
      },
    ],
  } as unknown as ServerConfig;
  const resolve = (
    value: ServerConfig,
    project: { id: typeof projectId; defaultModelSelection?: typeof projectSelection } | null,
  ) =>
    scheduledTaskDefaultModel(
      value.settings,
      project,
      deriveProviderInstanceEntries(value.providers),
    );
  it("uses the advertised default instead of catalog order when no default is configured", () => {
    expect(
      resolve({ ...config, settings: { ...config.settings, defaultModelSelection: null } }, null),
    ).toEqual({ instanceId, model: "catalog-default" });
  });
  it("falls back to the environment default when the project provider is unavailable", () => {
    expect(
      resolve(config, {
        id: projectId,
        defaultModelSelection: {
          instanceId: ProviderInstanceId.make("unavailable"),
          model: "missing",
        },
      }),
    ).toEqual(environmentSelection);
  });
});

// The host locale's clock for a wall time, so these pass in any locale (digits, day period, zero padding).
function clock(hour12: boolean, hour: number, minute: number) {
  return new Intl.DateTimeFormat(undefined, {
    hour: "numeric",
    minute: "2-digit",
    hour12,
    timeZone: "UTC",
  }).format(Date.UTC(2000, 0, 1, hour, minute));
}

it("appends the server zone only to fixed-time schedule labels", () => {
  expect(scheduleLabel({ type: "fixed_time", timeOfDay: "16:31" }, "24-hour", "Etc/UTC")).toBe(
    `Daily at ${clock(false, 16, 31)} UTC`,
  );
  expect(scheduleLabel({ type: "fixed_time", timeOfDay: "09:00" }, "24-hour")).toBe(
    `Daily at ${clock(false, 9, 0)}`,
  );
  expect(scheduleLabel({ type: "interval", everyMs: 60_000 }, "24-hour", "Etc/UTC")).toBe(
    "Every 1 min",
  );
  expect(scheduleLabel({ type: "webhook", signature: null }, "24-hour", "Etc/UTC")).toBe(
    "On webhook",
  );
});

it("shows schedule times in the Time format setting", () => {
  const daily = { type: "fixed_time", timeOfDay: "17:30" } as const;
  expect(scheduleLabel(daily, "24-hour")).toBe(`Daily at ${clock(false, 17, 30)}`);
  expect(scheduleLabel(daily, "12-hour")).toBe(`Daily at ${clock(true, 17, 30)}`);
  const midnight = scheduleLabel({ type: "fixed_time", timeOfDay: "0:05" }, "24-hour");
  expect(midnight).toBe(`Daily at ${clock(false, 0, 5)}`);
  // An h24 clock would print midnight as 24:05 in both the label and the helper.
  expect(midnight).not.toContain("24");

  // A local wall time, so the label reads 17:30 in any host zone.
  const runAt = new Date(2026, 10, 20, 17, 30);
  const once = { type: "once", runAt: runAt.toISOString() } as const;
  for (const hour12 of [false, true]) {
    const dateTime = new Intl.DateTimeFormat(undefined, {
      dateStyle: "medium",
      timeStyle: "short",
      hour12,
    }).format(runAt);
    expect(scheduleLabel(once, hour12 ? "12-hour" : "24-hour")).toBe(`Once, ${dateTime}`);
  }
});
