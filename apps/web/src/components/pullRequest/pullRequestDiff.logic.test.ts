import type { FileDiffMetadata } from "@pierre/diffs";
import { describe, expect, it } from "vite-plus/test";

import { buildFileDiffContentVersion, getRenderablePatch } from "~/lib/diffRendering";
import {
  canAttachPullRequestDraft,
  carryDetachedPullRequestDraftText,
  getFileViewedRange,
  isLineInFileDiff,
} from "./pullRequestDiff.logic";

function fileWithHunks(
  hunks: ReadonlyArray<{
    deletionStart: number;
    deletionCount: number;
    additionStart: number;
    additionCount: number;
  }>,
): FileDiffMetadata {
  return { name: "src/app.ts", hunks } as unknown as FileDiffMetadata;
}

describe("isLineInFileDiff", () => {
  const file = fileWithHunks([
    { deletionStart: 10, deletionCount: 3, additionStart: 10, additionCount: 5 },
    { deletionStart: 40, deletionCount: 0, additionStart: 42, additionCount: 2 },
  ]);

  it("includes the first line of a hunk and excludes the one past its last", () => {
    expect(isLineInFileDiff(file, "right", 10)).toBe(true);
    expect(isLineInFileDiff(file, "right", 14)).toBe(true);
    expect(isLineInFileDiff(file, "right", 15)).toBe(false);
    expect(isLineInFileDiff(file, "left", 9)).toBe(false);
    expect(isLineInFileDiff(file, "left", 12)).toBe(true);
    expect(isLineInFileDiff(file, "left", 13)).toBe(false);
  });

  it("keeps the two sides apart, since one line number means two lines", () => {
    expect(isLineInFileDiff(file, "right", 43)).toBe(true);
    expect(isLineInFileDiff(file, "left", 40)).toBe(false);
  });
});

describe("getFileViewedRange", () => {
  const files = ["c.ts", "a.ts", "b.ts", "d.ts"].map((fileKey) => ({ fileKey }));

  it("includes both endpoints in displayed order in either direction", () => {
    expect(getFileViewedRange(files, "b.ts", "c.ts")).toEqual(files.slice(0, 3));
    expect(getFileViewedRange(files, "c.ts", "b.ts")).toEqual(files.slice(0, 3));
    expect(getFileViewedRange(files, "a.ts", "a.ts")).toEqual([files[1]]);
  });

  it("changes only the clicked file without a current anchor", () => {
    expect(getFileViewedRange(files, "a.ts", null)).toEqual([files[1]]);
    expect(getFileViewedRange(files, "a.ts", "removed.ts")).toEqual([files[1]]);
    expect(getFileViewedRange(files, "removed.ts", "a.ts")).toEqual([]);
  });
});

function parseDiffFiles(patch: string, scope: string): ReadonlyArray<FileDiffMetadata> {
  const parsed = getRenderablePatch(patch, scope);
  if (parsed?.kind !== "files") throw new Error("Expected a parsed patch.");
  return parsed.files;
}

describe("canAttachPullRequestDraft", () => {
  const basePatch = [
    "diff --git a/a.ts b/a.ts",
    "index 1111111..2222222 100644",
    "--- a/a.ts",
    "+++ b/a.ts",
    "@@ -1,4 +1,4 @@",
    "-old()",
    "+new()",
    " one()",
    " two()",
    " three()",
    "",
  ].join("\n");

  const original = parseDiffFiles(basePatch, "base")[0]!;
  const draft = {
    path: "a.ts",
    contentVersion: buildFileDiffContentVersion(original),
    side: "right" as const,
    line: 3,
  };

  it("attaches when another file changes on the page and the draft file content is unchanged", () => {
    const withSibling = parseDiffFiles(
      `${basePatch}${basePatch.replaceAll("a.ts", "other.ts")}`,
      "with-sibling",
    );
    expect(canAttachPullRequestDraft(draft, withSibling[0]!)).toBe(true);
    expect(canAttachPullRequestDraft(draft, withSibling[1]!)).toBe(false);
  });

  it("detaches when the draft file's diff changes, shifts lines, or drops the line from every hunk", () => {
    const shifted = parseDiffFiles(
      basePatch.replace("@@ -1,4 +1,4 @@", "@@ -1,4 +1,5 @@").replace("+new()", "+zero()\n+new()"),
      "shifted",
    )[0]!;
    const lineRemoved = parseDiffFiles(
      [
        "diff --git a/a.ts b/a.ts",
        "index 1111111..3333333 100644",
        "--- a/a.ts",
        "+++ b/a.ts",
        "@@ -20,2 +20,2 @@",
        "-oldTail()",
        "+newTail()",
        " tail()",
        "",
      ].join("\n"),
      "removed",
    )[0]!;
    expect(canAttachPullRequestDraft(draft, shifted)).toBe(false);
    expect(canAttachPullRequestDraft(draft, lineRemoved)).toBe(false);
    expect(canAttachPullRequestDraft({ ...draft, line: 99 }, original)).toBe(false);
    expect(canAttachPullRequestDraft(null, original)).toBe(false);
  });
});

describe("carryDetachedPullRequestDraftText", () => {
  it("carries typed text only when re-selecting a line in the same file after detaching", () => {
    expect(
      carryDetachedPullRequestDraftText({
        previousPath: "a.ts",
        previousAttached: false,
        previousText: "Unsent",
        nextPath: "a.ts",
      }),
    ).toBe("Unsent");
    expect(
      carryDetachedPullRequestDraftText({
        previousPath: "a.ts",
        previousAttached: true,
        previousText: "Unsent",
        nextPath: "a.ts",
      }),
    ).toBeUndefined();
    expect(
      carryDetachedPullRequestDraftText({
        previousPath: "a.ts",
        previousAttached: false,
        previousText: "Unsent",
        nextPath: "b.ts",
      }),
    ).toBeUndefined();
    expect(
      carryDetachedPullRequestDraftText({
        previousPath: "a.ts",
        previousAttached: false,
        previousText: "",
        nextPath: "a.ts",
      }),
    ).toBeUndefined();
  });
});
