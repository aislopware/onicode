import { describe, expect, it } from "vite-plus/test";

import { allowsSinglePullRequestAutoMergeCancel } from "./pullRequestStackActions.ts";

describe("single-PR auto-merge cancel beside a stack", () => {
  // A stack's auto-merge cancelled as one pull request's skips the stack and its refresh.
  it.each([
    [false, true, true],
    [false, false, true],
    [true, true, true],
    [true, false, false],
  ] as const)(
    "stackAutoMerge=%s singleMerge=%s permits=%s",
    (stackAutoMerge, canMergeSinglePullRequest, allowed) => {
      expect(
        allowsSinglePullRequestAutoMergeCancel({ stackAutoMerge, canMergeSinglePullRequest }),
      ).toBe(allowed);
    },
  );
});
