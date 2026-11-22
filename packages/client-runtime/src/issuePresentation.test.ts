import { IssueTrackerKind, type IssueDetail, type IssueState } from "@onicode/contracts";
import { describe, expect, it } from "vite-plus/test";

import { issueChangeRequestHost, issueWriteAccess } from "./issuePresentation.ts";

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

describe("issueChangeRequestHost", () => {
  // A reference opened from Activity may carry no host; the issue's resolved host still serves
  // a change request in another repository, which the server refuses without one.
  it.each([
    ["gitlab", "gitlab.acme.dev", "gitlab.acme.dev"],
    ["github", "github.com", "github.com"],
    ["linear", "linear.app", undefined],
  ])("reads a %s issue's change requests on %s", (tracker, host, expected) => {
    expect(issueChangeRequestHost({ tracker: IssueTrackerKind.make(tracker), host })).toBe(
      expected,
    );
  });
});
