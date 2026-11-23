import { describe, expect, it } from "vite-plus/test";

import type { EnvironmentProject } from "@onicode/client-runtime/state/models";
import { EnvironmentId, ProjectId, SourceControlProviderKind } from "@onicode/contracts";

import {
  issueFilterProjects,
  issueListTargets,
  issueProjectKeys,
  issueRowHidden,
  issueSurfaceTargetFromSearch,
  issuesSearchFromUrl,
  selectedIssueHost,
} from "./issuesSearch.logic";

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
    expect(
      issuesSearchFromUrl({ hideProjects: [["env", "web"], ["env"], "api", ["env", 3]] })
        .hideProjects,
    ).toEqual([
      ["env", "web"],
      ["env", "3"],
    ]);
  });
});

describe("issueListTargets", () => {
  it("leaves hidden projects out and skips an environment with every project hidden", () => {
    const laptop = EnvironmentId.make("laptop");
    const vps = EnvironmentId.make("vps");
    const projects = [
      { environmentId: laptop, id: ProjectId.make("web") },
      { environmentId: laptop, id: ProjectId.make("api") },
      { environmentId: vps, id: ProjectId.make("docs") },
    ];
    const input = { state: "open" } as const;
    const hidden = [
      [laptop, ProjectId.make("api")],
      [laptop, ProjectId.make("removed")],
      [vps, ProjectId.make("docs")],
    ] as const;
    expect(issueListTargets([laptop, vps], projects, hidden, input)).toEqual([
      { environmentId: laptop, input: { state: "open", excludeProjectIds: ["api"] } },
    ]);
    // Before the project list loads, the saved choice still leaves its projects out.
    expect(issueListTargets([laptop, vps], [], hidden, input)).toEqual([
      { environmentId: laptop, input: { state: "open", excludeProjectIds: ["api", "removed"] } },
      { environmentId: vps, input: { state: "open", excludeProjectIds: ["docs"] } },
    ]);
    expect(issueListTargets([laptop, vps], projects, undefined, input)).toEqual([
      { environmentId: laptop, input },
      { environmentId: vps, input },
    ]);
  });

  it("keeps ids and the selected issue whole, however long", () => {
    const long = (prefix: string) => `${prefix}-${"x".repeat(220)}`;
    const raw = {
      hideProjects: [[long("server"), long("project")]],
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
    expect(issuesSearchFromUrl({ hideProjects: [["server", 42]] }).hideProjects).toEqual([
      ["server", "42"],
    ]);
    expect(issuesSearchFromUrl({ q: long("query") }).q).toHaveLength(200);
  });

  it("reopens the selected issue with its link, so a tab Back recreates reads its workspace", () => {
    const url = "https://linear.app/acme/issue/ENG-7/a-long-title";
    const search = issuesSearchFromUrl({
      selectedEnvironmentId: "server",
      selectedProjectId: "project",
      selectedHost: "linear.app",
      tracker: "linear",
      scope: "ENG",
      key: "ENG-7",
      selectedIssueUrl: url,
    });
    expect(search.selectedIssueUrl).toBe(url);
    expect(issueSurfaceTargetFromSearch(search)).toEqual({
      environmentId: "server",
      projectId: "project",
      host: "linear.app",
      tracker: "linear",
      scope: "ENG",
      key: "ENG-7",
      url,
    });
    expect(issueSurfaceTargetFromSearch({ ...search, key: undefined })).toBeNull();
  });

  it("names the project's host on a link that left it out, and waits for the project", () => {
    const search = issuesSearchFromUrl({
      selectedEnvironmentId: "server",
      selectedProjectId: "web",
      tracker: "github",
      scope: "acme/web",
      key: "7",
    });
    const web = {
      id: ProjectId.make("web"),
      environmentId: EnvironmentId.make("server"),
      repositoryIdentity: {
        canonicalKey: "code.acme.test/acme/web",
        provider: SourceControlProviderKind.make("github"),
        displayName: "acme/web",
        owner: "acme",
        name: "web",
      },
    } as unknown as EnvironmentProject;
    expect(selectedIssueHost(search, [web])).toBe("code.acme.test");
    expect(selectedIssueHost(search, [])).toBeNull();
  });
});

describe("issueFilterProjects", () => {
  it("keeps every project, scratch ones too, as its own choice", () => {
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
      ["scratch", "onicode · laptop · /work/scratch"],
      ["worktree", "onicode · laptop · /work/worktree"],
    ]);
  });
});

describe("issueRowHidden", () => {
  it("keeps a scope's rows while a shown project reads that scope", () => {
    const environmentId = EnvironmentId.make("env");
    const project = (id: string, repository: string, origin?: string) =>
      ({
        id: ProjectId.make(id),
        environmentId,
        repositoryIdentity: {
          canonicalKey: `github.com/${repository}`,
          provider: SourceControlProviderKind.make("github"),
          displayName: repository,
          owner: repository.split("/")[0],
          name: repository.split("/")[1],
          ...(origin
            ? { origin: { canonicalKey: `github.com/${origin}`, displayName: origin } }
            : {}),
        },
      }) as unknown as EnvironmentProject;
    const projects = [
      project("main", "acme/api"),
      project("worktree", "acme/api"),
      project("fork", "acme/api", "me/api"),
      project("fork-clone", "me/api"),
    ];
    const hidden = (...ids: string[]) =>
      issueRowHidden(
        issueProjectKeys(ids.map((id) => [environmentId, ProjectId.make(id)] as const)),
        projects,
      );
    const row = (projectId: string, scope?: string, host = "github.com") => ({
      projectId: ProjectId.make(projectId),
      ...(scope ? { host, scope } : {}),
    });

    expect(hidden("main")(environmentId, row("main", "acme/api"))).toBe(false);
    expect(hidden("main", "worktree")(environmentId, row("main", "acme/api"))).toBe(false);
    expect(hidden("fork")(environmentId, row("fork", "me/api"))).toBe(false);
    // The fork's clone shares its repository but reads only the fork.
    expect(hidden("main", "worktree", "fork")(environmentId, row("fork", "acme/api"))).toBe(true);
    // Settings-bound trackers and errors name no repository scope.
    expect(hidden("main")(environmentId, row("main", "ENG", "linear.app"))).toBe(false);
    expect(hidden("main")(environmentId, row("main"))).toBe(false);
    expect(
      hidden("fork", "fork-clone")(environmentId, row("fork-clone", "ENG", "linear.app")),
    ).toBe(true);
  });
});
