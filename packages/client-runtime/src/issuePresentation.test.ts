import { IssueTrackerKind, type IssueDetail, type IssueState } from "@onicode/contracts";
import { describe, expect, it } from "vite-plus/test";

import {
  issueLinkHost,
  issueWriteAccess,
  linksChangeRequestsOnItsHost,
} from "./issuePresentation.ts";

const workflow: ReadonlyArray<IssueState> = [
  { category: "open", name: "Backlog" },
  { category: "open", name: "Todo" },
  { category: "open", name: "In Progress" },
  { category: "closed", name: "Done", closeReason: "completed" },
  { category: "closed", name: "Canceled", closeReason: "canceled" },
  { category: "closed", name: "Duplicate", closeReason: "duplicate" },
];

function issue(overrides: { writable?: boolean; setState?: boolean; states?: typeof workflow }) {
  return {
    writable: overrides.writable ?? true,
    state: { category: "open", name: "Todo" },
    states: overrides.states ?? workflow,
    capabilities: {},
    viewerPermissions: { setState: overrides.setState ?? true },
  } as unknown as IssueDetail;
}

describe("issueWriteAccess", () => {
  it("offers every other state of the issue's workflow, in the tracker's order", () => {
    expect(issueWriteAccess(issue({}), true).states.map((state) => state.name)).toEqual([
      "Backlog",
      "In Progress",
      "Done",
      "Canceled",
      "Duplicate",
    ]);
  });

  it("offers no state when the issue cannot be moved", () => {
    expect(issueWriteAccess(issue({ setState: false }), true).states).toEqual([]);
    expect(issueWriteAccess(issue({ writable: false }), true).states).toEqual([]);
    expect(issueWriteAccess(issue({}), false).states).toEqual([]);
    expect(issueWriteAccess(issue({ states: [] }), true).states).toEqual([]);
  });
});

describe("linksChangeRequestsOnItsHost", () => {
  it.each([
    ["gitlab", true],
    ["github", true],
    ["linear", false],
  ])("is %s for a %s issue", (tracker, expected) => {
    expect(linksChangeRequestsOnItsHost({ tracker: IssueTrackerKind.make(tracker) })).toBe(
      expected,
    );
  });
});

describe("issueLinkHost", () => {
  const issue = { host: "gitlab.acme.dev", scope: "team/web" };
  // An issue opened without a host is in the project's own repository, which needs none, and
  // naming one there would open a second view of the same item.
  it.each([
    [undefined, "team/web", undefined],
    [undefined, "Team/Web", undefined],
    [undefined, "team/api", "gitlab.acme.dev"],
    ["gitlab.acme.dev", "team/web", "gitlab.acme.dev"],
  ])("names %s for a link to %s", (openedOn, repository, expected) => {
    expect(issueLinkHost(issue, openedOn, repository)).toBe(expected);
  });
});
