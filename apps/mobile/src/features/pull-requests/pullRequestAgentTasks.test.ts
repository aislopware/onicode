import { describe, expect, it } from "vite-plus/test";
import {
  appendPullRequestAgentPrompt,
  pullRequestAgentMenuAction,
  pullRequestAgentMenuItems,
  pullRequestAgentPrompt,
  pullRequestAgentThread,
  pullRequestAgentThreadActions,
} from "./pullRequestAgentTasks";
import { IsoDateTime, ProjectId, ThreadId } from "@onicode/contracts";
import { sourceControlClients } from "@onicode/client-runtime/source-control-clients";
import type { AppBarMenuItem } from "../../components/ui/AppBar.types";

const GITHUB = sourceControlClients.get("github").changeRequest;

describe("PR agent tasks", () => {
  it("continues a single linked thread, but keeps an explicit new-thread choice separate", () => {
    const thread = {
      id: ThreadId.make("thread-one"),
      projectId: ProjectId.make("project-one"),
      title: "Existing work",
      archivedAt: null,
    };
    expect(pullRequestAgentThread([thread])).toBe(thread);
    expect(pullRequestAgentThread([thread], "new")).toBeUndefined();
    expect(pullRequestAgentThread([thread], "missing")).toBeUndefined();
    expect(pullRequestAgentThread([])).toBeUndefined();
    const archived = {
      ...thread,
      id: ThreadId.make("thread-two"),
      archivedAt: IsoDateTime.make("2026-10-09T10:00:00.000Z"),
    };
    expect(pullRequestAgentThread([thread, archived])).toBeUndefined();
    expect(pullRequestAgentThread([archived])).toBeUndefined();
    const settled = { ...thread, settled: true };
    expect(pullRequestAgentThread([settled])).toBeUndefined();
    expect(pullRequestAgentThread([settled], settled.id)).toBe(settled);
    expect(pullRequestAgentThread([thread, { ...settled, id: archived.id }])).toBeUndefined();
    expect(pullRequestAgentThread([thread, archived], archived.id)).toBe(archived);
    const actions = pullRequestAgentThreadActions([settled, archived], "ask", true);
    expect(actions.map((action) => action.title)).toEqual([
      "Start a new thread",
      "Existing work",
      "Settled threads",
    ]);
    expect(actions[2]?.subactions).toMatchObject([
      { id: `ask:${settled.id}`, subtitle: "Settled thread" },
    ]);
    expect(pullRequestAgentThreadActions([thread], "ask", false)).toMatchObject([
      { id: `ask:${thread.id}`, subtitle: "Continue thread" },
    ]);
    expect(
      pullRequestAgentThreadActions([settled], "feedback", false)[0]?.subactions,
    ).toMatchObject([{ id: `feedback:${settled.id}`, attributes: { disabled: false } }]);
    expect(pullRequestAgentThreadActions([archived], "feedback", true)[1]).toMatchObject({
      attributes: { disabled: true },
    });
  });
  it("offers no AI helper thread choice while a helper is busy", () => {
    const thread = {
      id: ThreadId.make("thread-one"),
      projectId: ProjectId.make("project-one"),
      title: "Existing work",
      archivedAt: null,
    };
    const threads = [thread, { ...thread, id: ThreadId.make("thread-two") }];
    const enabledLeaves = (items: ReadonlyArray<AppBarMenuItem>): string[] =>
      items.flatMap((item) =>
        "items" in item ? enabledLeaves(item.items) : item.disabled ? [] : [item.id],
      );
    const items = (disabled: boolean) =>
      pullRequestAgentMenuItems({
        actions: [{ task: "explain", title: "Explain this PR", icon: "book", disabled: false }],
        chooseThread: true,
        disabled,
        threadActions: (task) => pullRequestAgentThreadActions(threads, task, true),
        start: () => {},
        select: () => {},
      });
    expect(enabledLeaves(items(false))).toEqual([
      "explain:new",
      "explain:thread-one",
      "explain:thread-two",
    ]);
    expect(enabledLeaves(items(true))).toEqual([]);
    const menuAction = (disabled: boolean, android = false) =>
      pullRequestAgentMenuAction({
        task: "conflicts",
        action: { title: "Resolve conflicts", icon: "arrow.triangle.branch", disabled: false },
        threadActions: pullRequestAgentThreadActions(threads, "conflicts", true),
        disabled,
        chooseThread: true,
        android,
      });
    expect(menuAction(false).subactions).toHaveLength(3);
    expect(menuAction(true)).toMatchObject({ attributes: { disabled: true } });
    expect(menuAction(true).subactions).toBeUndefined();
    // Android greys out the row and its submenu chevron instead.
    expect(menuAction(true, true).subactions).toHaveLength(3);
  });
  it("replaces the helper's own text when switching tasks, keeping user edits", () => {
    const first = appendPullRequestAgentPrompt("My question", "explain", GITHUB);
    const next = appendPullRequestAgentPrompt(
      first,
      "feedback",
      GITHUB,
      pullRequestAgentPrompt("explain", GITHUB),
    );
    expect(next).toBe(`My question\n\n${pullRequestAgentPrompt("feedback", GITHUB)}`);
    expect(
      appendPullRequestAgentPrompt(next, "ask", GITHUB, pullRequestAgentPrompt("feedback", GITHUB)),
    ).toBe("My question");
    const edited = `${first} with my own edit`;
    expect(
      appendPullRequestAgentPrompt(
        edited,
        "ask",
        GITHUB,
        pullRequestAgentPrompt("explain", GITHUB),
      ),
    ).toBe(edited);
  });
  it("names the change request with its host's term", () => {
    expect(pullRequestAgentPrompt("conflicts", GITHUB)).toContain(
      "Resolve this pull request's conflicts with its base branch. First confirm this thread is on the PR's branch; if it isn't, prepare the PR in its own worktree.",
    );
    expect(
      pullRequestAgentPrompt("conflicts", sourceControlClients.get("gitlab").changeRequest),
    ).toContain(
      "Resolve this merge request's conflicts with its base branch. First confirm this thread is on the MR's branch; if it isn't, prepare the MR in its own worktree.",
    );
  });
});
