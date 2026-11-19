import { describe, expect, it } from "vite-plus/test";

import type { EnvironmentProject } from "@onicode/client-runtime/state/models";
import { EnvironmentId, ProjectId } from "@onicode/contracts";

import { issueFilterProjects, issueProjectScope, issuesSearchFromUrl } from "./issuesSearch.logic";

describe("issuesSearchFromUrl", () => {
  it("replaces a malformed filter instead of letting the raw value through", () => {
    // The router lays the validated keys over the raw search, so a key left out keeps its raw value.
    const raw = { label: true, order: "oldest", assignee: "   ", host: true };
    const search = { ...raw, ...issuesSearchFromUrl(raw) };
    expect(search.host).toBeUndefined();
    expect(search.label).toBeUndefined();
    expect(search.assignee).toBeUndefined();
    expect(search.order).toBeUndefined();
    expect(issuesSearchFromUrl({ label: "bug" }).label).toEqual(["bug"]);
    expect(issuesSearchFromUrl({ label: ["bug", " ui "], order: "created" })).toMatchObject({
      label: ["bug", "ui"],
      order: "created",
    });
  });

  it("keeps ids and the selected issue whole, however long", () => {
    const long = (prefix: string) => `${prefix}-${"x".repeat(220)}`;
    const raw = {
      issueServer: long("server"),
      issueProject: long("project"),
      selectedEnvironmentId: ` ${long("selected-server")} `,
      selectedProjectId: long("selected-project"),
      selectedHost: long("host"),
      tracker: `tracker-${"x".repeat(50)}`,
      scope: long("group/subgroup"),
      key: long("key"),
    };
    expect(issuesSearchFromUrl(raw)).toMatchObject({
      ...raw,
      selectedEnvironmentId: raw.selectedEnvironmentId.trim(),
    });
    expect(issuesSearchFromUrl({ issueProject: 42 }).issueProject).toBe("42");
    expect(issuesSearchFromUrl({ q: long("query") }).q).toHaveLength(200);
  });
});

describe("issueFilterProjects", () => {
  it("keeps every project on one repository as its own choice", () => {
    const environmentId = EnvironmentId.make("env");
    const project = (id: string, isScratch = false) =>
      ({
        id: ProjectId.make(id),
        environmentId,
        title: "onicode",
        workspaceRoot: `/work/${id}`,
        repositoryIdentity: { canonicalKey: "github.com/aislopware/onicode" },
        isScratch,
      }) as unknown as EnvironmentProject;
    const choices = issueFilterProjects(
      [project("main"), project("worktree"), project("scratch", true)],
      new Map([[environmentId, "laptop"]]),
    );
    expect(choices.map((choice) => [choice.id, choice.title])).toEqual([
      ["main", "onicode · laptop · /work/main"],
      ["worktree", "onicode · laptop · /work/worktree"],
    ]);
  });
});

describe("issueProjectScope", () => {
  it("calls a project filter unavailable only once every environment's projects are known", () => {
    const listed = {
      environmentId: EnvironmentId.make("laptop"),
      projectId: ProjectId.make("app"),
    };
    const gone = { environmentId: EnvironmentId.make("removed"), projectId: ProjectId.make("app") };
    const projects = [{ id: listed.projectId, environmentId: listed.environmentId }];
    expect(issueProjectScope(undefined, projects, false)).toBe("all");
    expect(issueProjectScope(listed, projects, false)).toBe("listed");
    // A remote environment still connecting has not listed its projects yet.
    expect(issueProjectScope(gone, projects, false)).toBe("pending");
    expect(issueProjectScope(gone, projects, true)).toBe("unavailable");
  });
});
