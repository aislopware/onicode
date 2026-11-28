import { describe, expect, it } from "vite-plus/test";
import {
  MessageId,
  NodeId,
  PlanId,
  ProviderDriverKind,
  ProviderInstanceId,
  RunId,
  ThreadId,
  TurnItemId,
  type OrchestrationV2Subagent,
  type OrchestrationV2SubagentWorkflow,
  type OrchestrationV2TurnItem,
} from "@onicode/contracts";
import * as DateTime from "effect/DateTime";

import {
  countSubagentsAwaitedFromEarlierRuns,
  countSubagentsBlockingRun,
  deriveWorkflowGroups,
  isWorkflowScriptMessage,
  projectedSubagentsToRuntime,
  resolveWorkflowOpening,
  workflowMemberModelSelection,
} from "./subagentRuntime.ts";
import { resolveSubagentMetadata } from "./subagentDisplay.ts";
import { formatSubagentEffort } from "./threadExecution.ts";

const at = (iso: string) => DateTime.makeUnsafe(iso);

type ProjectedSubagent = Parameters<typeof projectedSubagentsToRuntime>[0][number];

function subagent(overrides: Partial<ProjectedSubagent> = {}): ProjectedSubagent {
  return {
    id: "coordinator",
    title: "probe",
    prompt: "export const meta = …",
    model: "claude-opus-5[1m]",
    status: "running" satisfies OrchestrationV2Subagent["status"],
    result: null,
    startedAt: at("2026-08-01T10:00:00.000Z"),
    completedAt: null,
    updatedAt: at("2026-08-01T10:00:05.000Z"),
    ...overrides,
  };
}

const member = (
  overrides: Partial<OrchestrationV2SubagentWorkflow["agents"][number]> = {},
): OrchestrationV2SubagentWorkflow["agents"][number] => ({
  index: 1,
  label: "alpha:one",
  state: "running",
  phaseIndex: 1,
  phaseTitle: "Alpha",
  ...overrides,
});

const groupsOf = (subagents: ReadonlyArray<ProjectedSubagent>) =>
  deriveWorkflowGroups(projectedSubagentsToRuntime(subagents));

describe("projectedSubagentsToRuntime workflow expansion", () => {
  it("uses a settled member's wall-clock completion separately from its execution duration", () => {
    const [, settled] = projectedSubagentsToRuntime([
      subagent({
        status: "completed",
        completedAt: at("2026-08-01T10:00:09.000Z"),
        workflow: {
          phases: [],
          agents: [
            member({
              state: "completed",
              model: "claude-opus-5[1m]",
              effort: "xhigh",
              speed: "standard",
              startedAt: Date.UTC(2026, 7, 1, 10, 0, 0),
              durationMs: 1500,
              completedAt: Date.UTC(2026, 7, 1, 10, 0, 4, 432),
              totalTokens: 71_141,
              toolCalls: 3,
              prompt: "Reply with exactly: A1",
              result: "A1",
            }),
          ],
        },
      }),
    ]);
    expect(settled?.status).toBe("completed");
    expect(settled?.completedAt).toBe("2026-08-01T10:00:04.432Z");
    expect(settled?.result).toBe("A1");
    expect(settled?.model).toBe("claude-opus-5[1m]");
    expect(settled?.effort).toBe("xhigh");
    expect(settled?.speed).toBe("standard");
    expect(settled?.progress).toBe("Reply with exactly: A1");
    expect(settled?.usage).toEqual({ totalTokens: 71_141, toolUses: 3, durationMs: 1500 });
    const instanceId = ProviderInstanceId.make("claudeAgent");
    expect(workflowMemberModelSelection(settled!, instanceId)).toEqual({
      instanceId,
      model: "claude-opus-5[1m]",
      options: [
        { id: "effort", value: "xhigh" },
        { id: "fastMode", value: false },
      ],
    });
    expect(workflowMemberModelSelection({ model: null, effort: "xhigh" }, instanceId)).toBe(
      undefined,
    );
    const provider = {
      driver: ProviderDriverKind.make("claudeAgent"),
      models: [
        { slug: "claude-opus-5", name: "Claude Opus 5", isCustom: false, capabilities: null },
      ],
    };
    const modelSelection = workflowMemberModelSelection(settled!, instanceId);
    const { modelSlug } = resolveSubagentMetadata({
      model: settled!.model,
      modelSelection,
      provider,
    });
    expect(formatSubagentEffort({ modelSlug, modelSelection, provider })).toBe("xhigh");
  });

  it("names a workflow by the model it runs on when the provider reports none", () => {
    const [workflow] = projectedSubagentsToRuntime([subagent({ model: null })]);
    expect(
      resolveSubagentMetadata({
        model: workflow!.model,
        modelSelection: { model: "claude-opus-5-5" },
      }).modelLabel,
    ).toBe("Claude Opus 5.5");
    expect(resolveSubagentMetadata({ model: workflow!.model }).modelLabel).toBe("Not reported");
  });

  it.each(["completed", "cancelled", "failed"] as const)(
    "settles unfinished members when the coordinator is %s",
    (status) => {
      const groups = groupsOf([
        subagent({
          status,
          completedAt: at("2026-08-01T10:00:09.000Z"),
          workflow: {
            phases: [],
            agents: [
              member(),
              member({ index: 2, state: "queued" }),
              member({
                index: 3,
                state: "completed",
                startedAt: Date.UTC(2026, 7, 1, 10, 0, 0),
                durationMs: 1500,
              }),
            ],
          },
        }),
      ]);
      expect(groups[0]?.workflow.status).toBe(status);
      const members = groups[0]?.phases.flatMap((phase) => phase.members);
      expect(members?.map((agent) => agent.status)).toEqual([status, status, "completed"]);
      expect(members?.map((agent) => agent.completedAt)).toEqual([
        "2026-08-01T10:00:09.000Z",
        "2026-08-01T10:00:09.000Z",
        "2026-08-01T10:00:01.500Z",
      ]);
      expect(members?.[0]?.model).toBeNull();
      expect(members?.[0]?.effort).toBeNull();
      expect(members?.[0]?.speed).toBeUndefined();
    },
  );

  it("titles an untitled subagent from its prompt's first line without splitting an emoji", () => {
    const [untitled, multiline] = projectedSubagentsToRuntime([
      subagent({ title: null, prompt: `${"a".repeat(76)}😀 and more work.` }),
      subagent({ id: "multiline", title: null, prompt: "\n  Fix the build\r\nThen run tests" }),
    ]);
    expect(untitled?.title).toBe(`${"a".repeat(76)}...`);
    expect(multiline?.title).toBe("Fix the build");
  });
});

describe("deriveWorkflowGroups over a dynamic workflow", () => {
  it("groups members under their phases and counts the run", () => {
    const groups = groupsOf([
      subagent({
        workflow: {
          name: "probe-wf",
          phases: [
            { index: 1, title: "Alpha" },
            { index: 2, title: "Beta" },
          ],
          agents: [
            member({ state: "completed" }),
            member({ index: 2, label: "alpha:two", state: "failed" }),
            member({ index: 3, label: "beta:one", phaseIndex: 2, phaseTitle: "Beta" }),
            member({
              index: 4,
              label: "beta:two",
              phaseIndex: 2,
              phaseTitle: "Beta",
              state: "queued",
            }),
          ],
        },
      }),
    ]);
    const group = groups[0];
    expect(group?.phases.map((phase) => [phase.title, phase.state])).toEqual([
      ["Alpha", "done"],
      ["Beta", "running"],
    ]);
    expect(group?.phases.map((phase) => phase.settledCount)).toEqual([2, 0]);
    expect(group?.phases.flatMap((phase) => phase.members)).toHaveLength(4);
    expect(group?.unphasedMembers).toEqual([]);
  });

  it.each([false, true])("keeps a reported phase title, missing title first=%s", (missingFirst) => {
    const agents = [member({ phaseTitle: "Review" }), member({ index: 2, phaseTitle: undefined })];
    const groups = groupsOf([
      subagent({ workflow: { phases: [], agents: missingFirst ? agents.toReversed() : agents } }),
    ]);
    expect(groups[0]?.phases[0]?.title).toBe("Review");
  });

  it("keeps unphased members with their workflow and excludes unrelated agents", () => {
    const runtime = projectedSubagentsToRuntime([
      subagent({ id: "ordinary" }),
      subagent({
        id: "__proto__",
        workflow: {
          phases: [],
          agents: [member({ phaseIndex: undefined, phaseTitle: undefined })],
        },
      }),
      subagent({ id: "constructor", workflow: { phases: [], agents: [] } }),
      subagent({ id: "orphan", workflow: { phases: [], agents: [member()] } }),
    ]);
    const groups = deriveWorkflowGroups(runtime.filter((agent) => agent.id !== "orphan"));
    expect(groups.map((group) => group.workflow.id)).toEqual(["__proto__", "constructor"]);
    expect(groups[0]?.phases).toEqual([]);
    expect(groups[0]?.unphasedMembers.map((agent) => agent.title)).toEqual(["alpha:one"]);
    expect(groups[1]?.phases).toEqual([]);
    expect(groups[1]?.unphasedMembers).toEqual([]);
  });
});

describe("resolveWorkflowOpening", () => {
  const SCRIPT = '// probe\nexport const meta = { name: "probe-wf" };\nconst GROUPS = [];';
  const childThread = {
    id: ThreadId.make("child"),
    lineage: {
      parentThreadId: ThreadId.make("parent"),
      relationshipToParent: "subagent" as const,
      rootThreadId: ThreadId.make("parent"),
    },
    creationSource: "provider" as const,
  };
  const coordinator = (workflow?: OrchestrationV2SubagentWorkflow) => ({
    ...subagent({
      title: "Run the alpha workflow",
      childThreadId: "child",
      ...(workflow === undefined ? {} : { workflow }),
    }),
    driver: ProviderDriverKind.make("claudeAgent"),
    providerInstanceId: ProviderInstanceId.make("claude"),
  });
  const brief = (text: string, workflowScript?: boolean) => ({
    id: "message-1",
    role: "user",
    text,
    ...(workflowScript === undefined ? {} : { workflowScript }),
  });

  it("opens a marked thread with the coordinator's plan and members", () => {
    const opening = resolveWorkflowOpening({
      thread: childThread,
      messages: [brief(SCRIPT, true)],
      parentSubagents: [
        coordinator({
          name: "probe-wf",
          phases: [{ index: 1, title: "Alpha" }],
          agents: [member({ childThreadId: ThreadId.make("member-1") })],
        }),
      ],
    });
    expect(opening).toMatchObject({
      messageId: "message-1",
      script: SCRIPT,
      name: "probe-wf",
      description: "Run the alpha workflow",
    });
    expect(
      opening?.group?.phases.map((phase) => ({
        title: phase.title,
        members: phase.members.map((agent) => [agent.title, agent.status, agent.childThreadId]),
      })),
    ).toEqual([{ title: "Alpha", members: [["alpha:one", "running", "member-1"]] }]);
  });

  it("recognizes an unmarked script before the parent loads", () => {
    const opening = resolveWorkflowOpening({
      thread: childThread,
      messages: [brief(SCRIPT)],
      parentSubagents: null,
    });
    expect(opening).toMatchObject({ script: SCRIPT, name: null, group: null });
  });

  it("leaves an ordinary subagent thread and other threads alone", () => {
    expect(
      resolveWorkflowOpening({
        thread: childThread,
        messages: [brief("Audit the last five commits.")],
        parentSubagents: [coordinator()],
      }),
    ).toBeNull();
    expect(
      resolveWorkflowOpening({
        thread: {
          ...childThread,
          lineage: { ...childThread.lineage, relationshipToParent: null, parentThreadId: null },
        },
        messages: [brief(SCRIPT)],
        parentSubagents: null,
      }),
    ).toBeNull();
    expect(isWorkflowScriptMessage({ role: "user", text: "Explain `export const meta`." })).toBe(
      false,
    );
    expect(isWorkflowScriptMessage({ role: "assistant", text: SCRIPT })).toBe(false);
    expect(
      isWorkflowScriptMessage({ role: "user", text: `// a\r// b\u2028/* c\n * d */\r\n${SCRIPT}` }),
    ).toBe(true);
    expect(isWorkflowScriptMessage({ role: "user", text: "// export const meta = {}" })).toBe(
      false,
    );
    // Divider comments ahead of ordinary code must reject without catastrophic backtracking.
    const dividers = `${"/".repeat(40)}\n`.repeat(40);
    expect(isWorkflowScriptMessage({ role: "user", text: `${dividers}const x = 1;` })).toBe(false);
    expect(
      isWorkflowScriptMessage({ role: "user", text: `${"/**/".repeat(200)}const x = 1;` }),
    ).toBe(false);
  });
});

describe("countSubagentsBlockingRun", () => {
  const runId = RunId.make("parent-run");
  const base = (id: string) => ({
    id: TurnItemId.make(id),
    threadId: ThreadId.make("parent-thread"),
    runId,
    nodeId: null,
    providerThreadId: null,
    providerTurnId: null,
    nativeItemRef: null,
    parentItemId: null,
    ordinal: 0,
    title: null,
    startedAt: at("2026-08-01T10:00:00.000Z"),
    completedAt: null,
    updatedAt: at("2026-08-01T10:00:00.000Z"),
  });
  const child = (
    id: string,
    status: "running" | "completed" = "running",
    background?: boolean,
  ): OrchestrationV2TurnItem => ({
    ...base(id),
    status,
    type: "subagent",
    subagentId: NodeId.make(id),
    origin: "provider_native",
    driver: ProviderDriverKind.make("claudeAgent"),
    providerInstanceId: ProviderInstanceId.make("claudeAgent"),
    childThreadId: null,
    prompt: id,
    result: null,
    ...(background === undefined ? {} : { background }),
  });
  const reply = (id: string): OrchestrationV2TurnItem => ({
    ...base(id),
    status: "completed",
    type: "assistant_message",
    messageId: MessageId.make(id),
    text: `Status ${id}`,
    streaming: false,
  });
  const todo: OrchestrationV2TurnItem = {
    ...base("todo"),
    status: "completed",
    type: "todo_list",
    planId: PlanId.make("plan"),
    steps: [{ id: "step-1", text: "Review", status: "running" }],
  };
  const delegate = (status: "running" | "completed" | "failed"): OrchestrationV2TurnItem => ({
    ...base(`delegate-${status}`),
    status,
    type: "dynamic_tool",
    toolName: "onicode.delegate_task",
    input: { task: "Review" },
    ...(status === "running" ? {} : { output: { taskId: "delegated" } }),
  });
  const count = (items: ReadonlyArray<OrchestrationV2TurnItem>, repliesShown = true) =>
    countSubagentsBlockingRun(items, runId, (item) => [item], repliesShown);

  it.each([
    ["a foreground child, or one from a server that sends no flag", [child("a")], 1],
    ["a background child", [child("a", "running", true), child("b", "running", false)], 1],
    // The parent wrote and launched more after A, so it was not parked on A.
    ["a reply between children", [child("a"), reply("m1"), child("b", "completed")], 0],
    // A child the parent's wait names holds it wherever it sits in the run.
    [
      "an awaited child behind later work",
      [child("a", "running", false), reply("m1"), child("b", "completed"), delegate("failed")],
      1,
    ],
    ["a trailing status reply", [child("a"), child("b"), reply("m2")], 2],
    ["the parent's task list and delegate calls", [child("a"), delegate("completed"), todo], 1],
    ["a running delegate call", [child("a"), delegate("running"), child("b")], 2],
    ["a failed delegate call", [child("a"), delegate("failed")], 0],
    ["another run's child", [{ ...child("a"), runId: RunId.make("other-run") }], 0],
  ] as const)("counts %s", (_name, items, expected) => {
    expect(count(items)).toBe(expected);
  });

  it("leaves the trailing status reply to the live row when replies are hidden", () => {
    expect(count([child("a"), reply("m2")], false)).toBe(0);
    expect(count([child("a")], false)).toBe(1);
    expect(count([child("a", "running", false), reply("m2")], false)).toBe(0);
  });

  it("adds the children of earlier runs that the run's wait holds it on", () => {
    const earlier = RunId.make("earlier-run");
    expect(
      countSubagentsAwaitedFromEarlierRuns(
        [
          { runId: earlier, status: "running", awaitedByRunId: runId },
          { runId: earlier, status: "completed", awaitedByRunId: runId },
          { runId: earlier, status: "running", awaitedByRunId: earlier },
          { runId: earlier, status: "running" },
          // Its own row in the run counts it.
          { runId, status: "running", awaitedByRunId: runId },
        ],
        runId,
      ),
    ).toBe(1);
    const withEarlier = (items: ReadonlyArray<OrchestrationV2TurnItem>, repliesShown: boolean) =>
      countSubagentsBlockingRun(items, runId, (item) => [item], repliesShown, 1);
    expect(withEarlier([reply("m1")], true)).toBe(1);
    expect(withEarlier([child("a", "running", false), reply("m1")], true)).toBe(2);
    expect(withEarlier([reply("m2")], false)).toBe(0);
  });

  it("stops at an entry that holds no turn item", () => {
    expect(
      countSubagentsBlockingRun([[child("a")], null, [child("b")]], runId, (items) => items, true),
    ).toBe(1);
    expect(
      countSubagentsBlockingRun(
        [[child("a", "running", false)], null, [child("b")]],
        runId,
        (items) => items,
        true,
      ),
    ).toBe(2);
  });
});
