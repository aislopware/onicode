import { describe, expect, it } from "vite-plus/test";
import {
  DEFAULT_SERVER_SETTINGS,
  type ServerConfig,
  ProviderDriverKind,
  ProviderInstanceId,
  ProjectId,
  ScheduledTaskId,
  ThreadId,
  type ScheduledTask,
} from "@onicode/contracts";
import {
  createDraft,
  editDraft,
  hasScheduledTaskDraftChanges,
  moveDraftToThread,
  rebaseScheduledTaskDraft,
  scheduledTaskDefaultModel,
  DEFAULT_SCHEDULE,
  scheduleDraftForTask,
  scheduleFromDraft,
} from "./scheduledTaskDraft";

describe("scheduleDraftForTask", () => {
  it("opens legacy sub-minute schedules at the writable minimum", () => {
    const draft = scheduleDraftForTask({ schedule: { type: "interval", everyMs: 30_000 } });
    expect(draft.intervalMinutes).toBe("1");
    expect(scheduleFromDraft(draft)).toEqual({ type: "interval", everyMs: 60_000 });
  });

  it("round-trips dedupe settings and rejects invalid keys and windows", () => {
    const dedupe = {
      replayKey: "{{headers.delivery-id}}",
      coalesceKey: "{{body.id}}",
      coalesceWindowSeconds: 3600,
    };
    const draft = scheduleDraftForTask({ schedule: { type: "webhook", signature: null, dedupe } });
    expect(scheduleFromDraft(draft)).toMatchObject({ dedupe });
    expect(
      scheduleFromDraft({
        ...draft,
        replayKey: `  ${dedupe.replayKey}  `,
        coalesceKey: `\t${dedupe.coalesceKey}\n`,
      }),
    ).toMatchObject({ dedupe });
    // Blank keys clear what is stored.
    expect(
      scheduleFromDraft({
        ...draft,
        replayKey: " \n ",
        coalesceKey: "",
        coalesceWindowSeconds: "",
      }),
    ).toMatchObject({ dedupe: {} });
    for (const coalesceWindowSeconds of ["9", "3601", "1.5", "abc"]) {
      expect(scheduleFromDraft({ ...draft, coalesceWindowSeconds })).toBeNull();
    }
    expect(scheduleFromDraft({ ...draft, replayKey: "literal" })).toBeNull();
    expect(scheduleFromDraft({ ...draft, coalesceKey: "{{body.bad" })).toBeNull();
  });

  it("sends a webhook secret only when typed, and clears it with the signature", () => {
    const signature = {
      header: "x-hub-signature-256",
      encoding: "hex" as const,
      prefix: "sha256=",
    };
    const draft = scheduleDraftForTask({ schedule: { type: "webhook", signature } });
    expect(scheduleFromDraft(draft)).toEqual({ type: "webhook", signature, dedupe: {} });
    expect(scheduleFromDraft({ ...draft, signatureSecret: " s3cret " })).toMatchObject({
      signature: { ...signature, secret: "s3cret" },
    });
    expect(scheduleFromDraft({ ...draft, signatureEnabled: false })).toMatchObject({
      signature: null,
    });
    expect(scheduleFromDraft({ ...draft, signatureHeader: " " })).toBeNull();
  });

  it("round-trips webhook filters and refuses an invalid one", () => {
    const filters = [
      { path: "headers.x-github-event", op: "equals", value: "pull_request" },
      { path: "body.label.name", op: "in", value: ["needs review", "agent:fix"] },
    ] as const;
    const draft = scheduleDraftForTask({ schedule: { type: "webhook", signature: null, filters } });
    // Untouched rows are left out, so the server keeps whatever is stored now.
    expect(scheduleFromDraft(draft)).not.toHaveProperty("filters");
    expect(scheduleFromDraft({ ...draft, filters: [] })).toMatchObject({ filters: [] });
    expect(scheduleFromDraft({ ...draft, filters: draft.filters.slice(1) })).toMatchObject({
      filters: filters.slice(1),
    });
    expect(
      scheduleFromDraft({ ...draft, filters: [{ id: "f1", path: "", op: "exists", value: "" }] }),
    ).toBeNull();
  });
});

describe("scheduleFromDraft", () => {
  it("rejects an empty day selection rather than silently scheduling every day", () => {
    expect(scheduleFromDraft({ ...DEFAULT_SCHEDULE, weekdays: [] })).toBeNull();
  });

  it.each([
    ["1.5", 90_000],
    ["2,5", 150_000],
    [" 2,5 ", 150_000],
    [String(60_001 / 60_000), 60_001],
  ])("preserves a valid %s minute interval when saving", (intervalMinutes, everyMs) => {
    expect(scheduleFromDraft({ ...DEFAULT_SCHEDULE, mode: "interval", intervalMinutes })).toEqual({
      type: "interval",
      everyMs,
    });
  });

  it.each(["0,5", "2,5,0", "Infinity", "150119987580", ""])(
    "rejects invalid interval minutes: %s",
    (intervalMinutes) => {
      expect(
        scheduleFromDraft({ ...DEFAULT_SCHEDULE, mode: "interval", intervalMinutes }),
      ).toBeNull();
    },
  );
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
  const resolve = scheduledTaskDefaultModel;
  it("prefers the project's configured model", () => {
    expect(resolve(config, { id: projectId, defaultModelSelection: projectSelection })).toEqual(
      projectSelection,
    );
    expect(
      resolve(
        {
          ...config,
          settings: {
            ...config.settings,
            projectSettingsOverrides: {
              [projectId]: { defaultModelSelection: projectSelection },
            },
          },
        },
        { id: projectId },
      ),
    ).toEqual(projectSelection);
  });
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
  it("does not choose an implicit model on a disabled provider", () => {
    expect(
      resolve(
        {
          ...config,
          settings: {
            ...config.settings,
            providerInstances: {
              [instanceId]: { driver: ProviderDriverKind.make("codex"), enabled: false },
            },
          },
          providers: config.providers.map((provider) => ({ ...provider, enabled: false })),
        },
        null,
      ),
    ).toBeNull();
  });
});

describe("scheduled task worktree base", () => {
  const rootTask = {
    id: ScheduledTaskId.make("task"),
    title: "Review issues",
    prompt: "Review open issues",
    enabled: true,
    schedule: { type: "interval", everyMs: 60_000 },
    projectId: ProjectId.make("project"),
    threadId: null,
    workspaceStrategy: { type: "root" },
    modelSelection: { instanceId: ProviderInstanceId.make("codex"), model: "gpt-5.4" },
    runtimeMode: "full-access",
    interactionMode: "default",
    createdBy: "user",
    creationSource: "mobile",
    createdAt: "2026-09-17T00:00:00.000Z",
    updatedAt: "2026-09-17T00:00:00.000Z",
    nextRunAt: null,
    lastRunAt: null,
    lastRunStatus: "never",
    lastRunError: null,
    runCount: 0,
  } satisfies ScheduledTask;

  it("keeps the user's edits over another client's change and takes it for fields left alone", () => {
    const edited = { ...editDraft(rootTask), prompt: "Review twice" };
    // An agent paused the task and slowed it while the editor was open.
    const current = {
      ...rootTask,
      enabled: false,
      schedule: { type: "interval", everyMs: 120_000 },
    } satisfies ScheduledTask;
    expect(rebaseScheduledTaskDraft(edited, current)).toMatchObject({
      prompt: "Review twice",
      enabled: false,
      schedule: { mode: "interval", intervalMinutes: "2" },
      task: current,
    });
    const repaused = rebaseScheduledTaskDraft(
      { ...editDraft(rootTask), enabled: false },
      {
        ...current,
        enabled: true,
      },
    );
    expect(repaused.enabled).toBe(false);
    // A first run bound a thread meanwhile; the next save must not unbind it.
    const bound = { ...current, threadId: ThreadId.make("thread:bound") } satisfies ScheduledTask;
    expect(rebaseScheduledTaskDraft(edited, bound)).toMatchObject({
      threadId: "thread:bound",
      threadMode: "continue",
    });
    // Nor may it undo a move, a workspace change or a model change.
    const moved = {
      ...current,
      projectId: ProjectId.make("moved"),
      workspaceStrategy: { type: "existing_worktree", worktreePath: "/checkout" },
      modelSelection: { instanceId: ProviderInstanceId.make("claude"), model: "opus" },
    } satisfies ScheduledTask;
    expect(rebaseScheduledTaskDraft(edited, moved)).toMatchObject({
      projectId: "moved",
      workspace: "existing_worktree",
      checkoutPath: "/checkout",
      modelSelection: moved.modelSelection,
    });
    // A thread the user picked stays, with its project and mode.
    const thread = { id: ThreadId.make("thread:picked"), projectId: rootTask.projectId };
    expect(rebaseScheduledTaskDraft(moveDraftToThread(edited, thread), moved)).toMatchObject({
      projectId: rootTask.projectId,
      threadId: "thread:picked",
      threadMode: "continue",
    });
    // Reading the task again gives its filter rows new ids; that alone is not an edit.
    const webhookTask = {
      ...rootTask,
      schedule: {
        type: "webhook",
        signature: null,
        filters: [{ path: "body.action", op: "equals", value: "opened" }],
      },
    } satisfies ScheduledTask;
    const signed = {
      ...webhookTask,
      schedule: {
        ...webhookTask.schedule,
        signature: { header: "x-signature", encoding: "base64", prefix: "" },
      },
    } satisfies ScheduledTask;
    expect(
      rebaseScheduledTaskDraft({ ...editDraft(webhookTask), prompt: "Review twice" }, signed),
    ).toMatchObject({ schedule: { signatureEnabled: true, signatureHeader: "x-signature" } });
    // Filters left alone take the stored ones even when another webhook setting was edited.
    const refiltered = {
      ...webhookTask,
      schedule: {
        ...webhookTask.schedule,
        filters: [{ path: "body.action", op: "equals", value: "closed" }],
      },
    } satisfies ScheduledTask;
    const replayEdited = editDraft(webhookTask);
    const rebasedFilters = rebaseScheduledTaskDraft(
      { ...replayEdited, schedule: { ...replayEdited.schedule, replayKey: "{{body.id}}" } },
      refiltered,
    ).schedule;
    expect(rebasedFilters).toMatchObject({ replayKey: "{{body.id}}" });
    expect(rebasedFilters.filters.map((filter) => filter.value)).toEqual(["closed"]);
    expect(rebasedFilters.savedFilters.map((filter) => filter.value)).toEqual(["closed"]);
    // Edited filters stay, and the other webhook settings take the stored ones.
    const filterEdited = editDraft(webhookTask);
    const rebasedSigned = rebaseScheduledTaskDraft(
      {
        ...filterEdited,
        schedule: {
          ...filterEdited.schedule,
          filters: filterEdited.schedule.filters.map((row) => ({ ...row, value: "closed" })),
        },
      },
      signed,
    ).schedule;
    expect(rebasedSigned).toMatchObject({ signatureEnabled: true, signatureHeader: "x-signature" });
    expect(rebasedSigned.filters.map((filter) => filter.value)).toEqual(["closed"]);
    // Filters left alone stay when an edited webhook meets a task made a timer elsewhere.
    const timer = {
      ...webhookTask,
      schedule: { type: "interval", everyMs: 3_600_000 },
    } satisfies ScheduledTask;
    const keptOnTimer = rebaseScheduledTaskDraft(
      { ...replayEdited, schedule: { ...replayEdited.schedule, replayKey: "{{body.id}}" } },
      timer,
    ).schedule;
    expect(keptOnTimer.mode).toBe("webhook");
    expect(keptOnTimer.filters.map((filter) => filter.value)).toEqual(["opened"]);
    expect(keptOnTimer.savedFilters).toEqual([]);
    // A value an "exists" filter ignores is no edit.
    const existsTask = {
      ...webhookTask,
      schedule: { ...webhookTask.schedule, filters: [{ path: "body.action", op: "exists" }] },
    } satisfies ScheduledTask;
    const existsDraft = editDraft(existsTask);
    expect(
      hasScheduledTaskDraftChanges(existsDraft, {
        ...existsDraft,
        schedule: {
          ...existsDraft.schedule,
          filters: existsDraft.schedule.filters.map((row) => ({ ...row, value: "opened" })),
        },
      }),
    ).toBe(false);
  });

  it("counts only edits the saved task would carry", () => {
    const draft = editDraft(rootTask);
    const changed = (next: Partial<typeof draft>) =>
      hasScheduledTaskDraftChanges(draft, { ...draft, ...next });
    expect(changed({ schedule: { ...draft.schedule, runAt: "2026-11-23T09:00:00.000Z" } })).toBe(
      false,
    );
    expect(changed({ baseRef: "release", checkoutPath: "/x" })).toBe(false);
    expect(changed({ schedule: { ...draft.schedule, intervalMinutes: "5" } })).toBe(true);
    expect(changed({ schedule: { ...draft.schedule, mode: "once" } })).toBe(true);
    expect(changed({ schedule: { ...draft.schedule, replayKey: "{{body.id}}" } })).toBe(false);
    const webhook = { ...draft, schedule: { ...draft.schedule, mode: "webhook" as const } };
    const webhookChanged = (schedule: Partial<typeof draft.schedule>) =>
      hasScheduledTaskDraftChanges(webhook, {
        ...webhook,
        schedule: { ...webhook.schedule, ...schedule },
      });
    expect(webhookChanged({ signatureHeader: "x-other" })).toBe(false);
    expect(webhookChanged({ signatureEnabled: true })).toBe(true);
    expect(webhookChanged({ coalesceWindowSeconds: "60" })).toBe(true);
    expect(
      webhookChanged({ filters: [{ id: "f1", path: "body.action", op: "exists", value: "" }] }),
    ).toBe(true);
  });

  it("leaves the base automatic for new tasks and tasks without a worktree", () => {
    expect(createDraft(ProjectId.make("project"), null).baseRef).toBe("");
    expect(editDraft(rootTask).baseRef).toBe("");
    expect(
      editDraft({ ...rootTask, workspaceStrategy: { type: "worktree", baseRef: "release" } })
        .baseRef,
    ).toBe("release");
  });
});
