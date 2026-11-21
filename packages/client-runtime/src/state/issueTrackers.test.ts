import { IssueTrackerKind, MAX_ISSUE_TRACKER_ADDITIONAL_SCOPES } from "@onicode/contracts";
import { describe, expect, it } from "vite-plus/test";

import { issueTrackerBindingFor, issueTrackerScopeOptions } from "./issueTrackers.ts";

describe("issueTrackerScopeOptions", () => {
  it("offers no new team once the binding is full, and keeps bound ones removable", () => {
    const tracker = {
      tracker: IssueTrackerKind.make("linear"),
      displayName: "Linear",
      host: "linear.app",
      scopeNoun: "team",
      credential: { label: "Personal API key", helpUrl: null },
      connected: true,
      account: "demo",
      detail: null,
      scopes: [
        { key: "ENG", name: "Engineering" },
        { key: "OPS", name: "Operations" },
      ],
    };
    const full = [
      "ENG",
      ...Array.from({ length: MAX_ISSUE_TRACKER_ADDITIONAL_SCOPES }, (_, i) => `T${i}`),
    ];
    const options = issueTrackerScopeOptions(tracker, full);
    expect(options.find((scope) => scope.key === "OPS")?.addable).toBe(false);
    expect(options.find((scope) => scope.key === "ENG")?.addable).toBe(true);
    expect(options.find((scope) => scope.key === "T0")).toEqual({
      key: "T0",
      name: "T0",
      addable: true,
    });
    expect(issueTrackerScopeOptions(tracker, ["ENG"]).every((scope) => scope.addable)).toBe(true);
  });
});

describe("issueTrackerBindingFor", () => {
  it("sends an empty team list, since the server keeps stored teams when the list is absent", () => {
    const linear = IssueTrackerKind.make("linear");
    expect(issueTrackerBindingFor(linear, ["ENG", "OPS"], ["ENG"])).toEqual({
      tracker: linear,
      scope: "ENG",
      additionalScopes: [],
    });
  });
});
