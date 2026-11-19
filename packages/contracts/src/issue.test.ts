import { describe, expect, it } from "@effect/vitest";

import { findThreadIssueLink, IssueTrackerKind } from "./issue.ts";

describe("findThreadIssueLink", () => {
  const eng7 = { tracker: IssueTrackerKind.make("linear"), host: "linear.app", scope: "ENG" };
  const link = (key: string, workspace?: string) => ({
    ...eng7,
    key,
    ...(workspace === undefined ? {} : { workspace }),
  });

  it("finds a link saved under the workspace's old key, but not another workspace's", () => {
    const renamed = link("ENG-7", "acme-old");
    // The issue now reads as `acme`; the tab that opened it still names `acme-old`.
    expect(findThreadIssueLink([renamed], link("ENG-7", "acme"), "acme-old")).toBe(renamed);
    expect(findThreadIssueLink([renamed], link("ENG-7", "acme"))).toBeUndefined();
    const elsewhere = link("ENG-7", "othercorp");
    const own = link("eng-7", "acme");
    expect(findThreadIssueLink([elsewhere, own], link("ENG-7", "Acme"), "acme-old")).toBe(own);
    // A link saved before workspaces were kept matches any.
    const legacy = link("ENG-7");
    expect(findThreadIssueLink([legacy], link("ENG-7", "acme"))).toBe(legacy);
  });
});
