import {
  EnvironmentId,
  IssueTrackerAccountId,
  IssueTrackerKind,
  ProjectId,
  SourceControlProviderKind,
} from "@onicode/contracts";
import { describe, expect, it, vi } from "vite-plus/test";

import {
  createIssueLinkResolver,
  issueLinkWorkspace,
  mayContainStandaloneIssueLink,
  resolveIssueLinkTarget,
  withProjectIssueHost,
} from "./issueLinks.ts";
import { sourceControlClients } from "./sourceControlClients.ts";

const project = (id: string, repository: string, origin?: string) =>
  ({
    id: ProjectId.make(id),
    environmentId: EnvironmentId.make("env"),
    repositoryIdentity: {
      canonicalKey: `github.com/${repository}`,
      provider: SourceControlProviderKind.make("github"),
      displayName: repository,
      owner: repository.split("/")[0],
      name: repository.split("/")[1],
      ...(origin ? { origin: { canonicalKey: `github.com/${origin}`, displayName: origin } } : {}),
    },
  }) as never;

describe("resolveIssueLinkTarget", () => {
  const projects = [project("p1", "acme/api"), project("p2", "acme/web")];

  it.each([
    {
      name: "reads an issue through the project bound to its repository",
      url: "https://github.com/acme/web/issues/7",
      expected: { projectId: "p2", scope: "acme/web", key: "7" },
    },
    {
      name: "lends a project on the same host to a repository no project is bound to",
      url: "https://github.com/other/repo/issues/9",
      expected: { projectId: "p1", scope: "other/repo", key: "9" },
    },
  ])("$name", ({ url, expected }) => {
    const target = resolveIssueLinkTarget({ url, projects });
    expect(target?.ref).toMatchObject({ tracker: "github", host: "github.com", ...expected });
  });

  it("reads a fork's own issue through the fork, not as a borrowed repository", () => {
    const target = resolveIssueLinkTarget({
      url: "https://github.com/me/web/issues/3",
      projects: [project("p1", "acme/api"), project("fork", "acme/web", "me/web")],
    });
    expect(target?.ref).toMatchObject({ projectId: "fork", scope: "me/web", key: "3" });
  });

  it("uses the current repository identity after a project changes repositories", () => {
    const url = "https://github.com/acme/api/issues/7";
    const original = project("p1", "acme/api");
    const replacement = project("p1", "acme/mobile");
    const lender = project("p3", "acme/web");
    expect(resolveIssueLinkTarget({ url, projects: [lender, original] })?.ref.projectId).toBe("p1");
    expect(resolveIssueLinkTarget({ url, projects: [lender, replacement] })?.ref.projectId).toBe(
      "p3",
    );
  });

  it("prefers the project bound to the link's own port over one on the bare hostname", () => {
    const enterprise = (id: string, host: string) =>
      ({
        id: ProjectId.make(id),
        environmentId: EnvironmentId.make("env"),
        repositoryIdentity: {
          canonicalKey: `${host}/acme/web`,
          provider: SourceControlProviderKind.make("github"),
          displayName: "acme/web",
          owner: "acme",
          name: "web",
        },
      }) as never;
    const target = resolveIssueLinkTarget({
      url: "https://github.example.com:8443/acme/web/issues/7",
      projects: [
        enterprise("bare", "github.example.com"),
        enterprise("ported", "github.example.com:8443"),
      ],
    });
    expect(target?.ref.projectId).toBe("ported");

    // An Enterprise Server's hostname need not say "github"; its project vouches for it.
    const custom = resolveIssueLinkTarget({
      url: "https://code.acme.test/acme/web/issues/7",
      projects: [enterprise("custom", "code.acme.test")],
    });
    expect(custom?.ref).toMatchObject({ projectId: "custom", host: "code.acme.test", key: "7" });
  });

  it("reads a Linear link through the project bound to its team, else any bound to Linear", () => {
    const trackers = {
      trackers: [
        {
          tracker: IssueTrackerKind.make("linear"),
          displayName: "Linear",
          host: "linear.app",
          scopeNoun: "team",
          credential: { label: "Personal API key", helpUrl: null },
          connected: true,
          account: "julius",
          detail: null,
          workspace: "acme",
          scopes: [],
        },
      ],
      bindings: {
        [ProjectId.make("p1")]: { tracker: IssueTrackerKind.make("linear"), scope: "API" },
        [ProjectId.make("p2")]: {
          tracker: IssueTrackerKind.make("linear"),
          scope: "WEB",
          additionalScopes: ["DOCS"],
        },
      },
    };
    const resolve = (url: string) => resolveIssueLinkTarget({ url, projects, trackers });
    expect(resolve("https://linear.app/acme/issue/WEB-17/fix-it")?.ref).toEqual({
      projectId: "p2",
      tracker: "linear",
      host: "linear.app",
      scope: "WEB",
      key: "WEB-17",
      // The server refuses the key in any other workspace, should the credential move to one.
      workspace: "acme",
    });
    expect(resolve("https://linear.app/acme/issue/DOCS-4")?.ref).toMatchObject({
      projectId: "p2",
      scope: "DOCS",
    });
    expect(resolve("https://linear.app/acme/issue/OPS-3")?.ref).toMatchObject({
      projectId: "p1",
      scope: "OPS",
      key: "OPS-3",
    });
    expect(resolve("https://linear.app.evil.example/acme/issue/WEB-17")).toBeNull();
    // Team keys repeat across workspaces; only the connected one's links are its issues.
    expect(resolve("https://linear.app/othercorp/issue/WEB-17")).toBeNull();
    // One spelling, so a link and a stored URL that differ in case read the same cached issue.
    expect(resolve("https://linear.app/Acme/issue/WEB-17")?.ref.workspace).toBe("acme");
    // The panel opens an issue at its top, so a link to one of its comments is the browser's.
    expect(resolve("https://linear.app/acme/issue/WEB-17/fix-it")?.namesPlace).toBe(false);
    expect(resolve("https://linear.app/acme/issue/WEB-17/fix-it#comment-cmt_1")).toMatchObject({
      ref: { key: "WEB-17" },
      namesPlace: true,
    });
    expect(
      resolveIssueLinkTarget({ url: "https://linear.app/acme/issue/WEB-17", projects }),
    ).toBeNull();

    // A second account reaches its own workspace, through only the projects bound with it.
    const accountId = IssueTrackerAccountId.make("second");
    const [p1, p2] = [ProjectId.make("p1"), ProjectId.make("p2")];
    const both = (url: string) =>
      resolveIssueLinkTarget({
        url,
        projects,
        trackers: {
          ...trackers,
          accounts: [{ ...trackers.trackers[0]!, accountId, workspace: "othercorp" }],
          bindings: { [p1]: trackers.bindings[p1]! },
          accountBindings: { [p2]: { ...trackers.bindings[p2]!, accountId } },
        },
      })?.ref.projectId;
    expect([
      both("https://linear.app/othercorp/issue/WEB-17"),
      both("https://linear.app/acme/issue/WEB-17"),
    ]).toEqual(["p2", "p1"]);
  });

  it("reads a Jira link through the account connected to the site it names", () => {
    const trackers = {
      trackers: [
        {
          tracker: IssueTrackerKind.make("jira"),
          displayName: "Jira",
          host: "atlassian.net",
          scopeNoun: "project",
          credential: { label: "API token", helpUrl: null },
          connected: true,
          account: "julius",
          detail: null,
          workspace: "acme.atlassian.net",
          scopes: [],
        },
      ],
      bindings: {
        [ProjectId.make("p1")]: { tracker: IssueTrackerKind.make("jira"), scope: "ENG" },
      },
    };
    const resolve = (url: string) => resolveIssueLinkTarget({ url, projects, trackers });
    expect(
      resolve("https://acme.atlassian.net/jira/software/c/projects/ENG/issues/ENG-7")?.ref,
    ).toEqual({
      projectId: "p1",
      tracker: "jira",
      host: "atlassian.net",
      scope: "ENG",
      key: "ENG-7",
      workspace: "acme.atlassian.net",
    });
    expect(issueLinkWorkspace("https://Acme.atlassian.net/browse/ENG-7")).toBe(
      "acme.atlassian.net",
    );
    expect(resolve("https://other.atlassian.net/browse/ENG-7")).toBeNull();
    // Jira's comment permalinks name the comment in the query, not the hash.
    expect(resolve("https://acme.atlassian.net/browse/ENG-7")?.namesPlace).toBe(false);
    expect(resolve("https://acme.atlassian.net/browse/ENG-7?focusedCommentId=c9")?.namesPlace).toBe(
      true,
    );
  });

  it.each([
    "https://github.com.evil.example/acme/web/issues/7",
    "https://github.com/acme/web/pull/7",
  ])("leaves %s an ordinary link", (url) => {
    expect(resolveIssueLinkTarget({ url, projects })).toBeNull();
  });

  it("marks a link to a comment, which may still name a pull request GitHub numbers as an issue", () => {
    expect(
      resolveIssueLinkTarget({
        url: "https://github.com/acme/web/issues/7#issuecomment-123",
        projects,
      }),
    ).toMatchObject({ ref: { scope: "acme/web", key: "7" }, namesPlace: true });
  });
});

describe("withProjectIssueHost", () => {
  it("names the host a link would, so a host-less reference keys as the same issue", () => {
    const enterprise = {
      id: ProjectId.make("ghe"),
      environmentId: EnvironmentId.make("env"),
      repositoryIdentity: {
        canonicalKey: "code.acme.test/acme/web",
        provider: SourceControlProviderKind.make("github"),
        displayName: "acme/web",
        owner: "acme",
        name: "web",
      },
    } as never;
    const ref: { tracker: string; host?: string; scope: string; key: string } = {
      tracker: "github",
      scope: "acme/web",
      key: "7",
    };
    const linked = resolveIssueLinkTarget({
      url: "https://code.acme.test/acme/web/issues/7",
      projects: [enterprise],
    });
    expect(withProjectIssueHost(ref, enterprise).host).toBe(linked?.ref.host);
    expect(withProjectIssueHost(ref, project("p1", "acme/web"))).toEqual({
      ...ref,
      host: "github.com",
    });
    // A named host stays: a fork's upstream can be on another server.
    expect(withProjectIssueHost({ ...ref, host: "github.com" }, enterprise).host).toBe(
      "github.com",
    );
    expect(withProjectIssueHost({ ...ref, tracker: "linear" }, enterprise)).toEqual({
      ...ref,
      tracker: "linear",
    });
    expect(withProjectIssueHost(ref, null)).toEqual(ref);
  });
});

describe("createIssueLinkResolver", () => {
  it("reads each project's binding once, not once per link", () => {
    const projects = Array.from({ length: 100 }, (_, index) =>
      project(`p${index}`, `acme/r${index}`),
    );
    using issueBindings = vi.spyOn(sourceControlClients, "issueBindings");
    const resolve = createIssueLinkResolver(projects);
    // A message without links never pays for them.
    expect(issueBindings).not.toHaveBeenCalled();
    for (let index = 0; index < 50; index++) {
      expect(resolve(`https://docs.example.com/guide/page-${index}`)).toBeNull();
    }
    expect(resolve("https://github.com/acme/r7/issues/3")?.ref).toMatchObject({
      projectId: "p7",
      key: "3",
    });
    expect(issueBindings).toHaveBeenCalledTimes(projects.length);
  });
});

describe("mayContainStandaloneIssueLink", () => {
  it.each([
    ["[issue](https://linear.app/acme/issue/WEB-17)", true],
    ["[issue](https://LINEAR.APP/acme/issue/WEB-17)", true],
    ["[issue](https://acme.atlassian.net/browse/ENG-7)", true],
    ["[issue](https://linear.app:443/acme/issue/WEB-17)", true],
    ["[issue](https://acme.atlassian.net:443/browse/ENG-7)", true],
    ["[issue](https://github.com/acme/web/issues/7)", false],
  ])("classifies %s as %s", (text, expected) => {
    expect(mayContainStandaloneIssueLink(text)).toBe(expected);
  });
});
