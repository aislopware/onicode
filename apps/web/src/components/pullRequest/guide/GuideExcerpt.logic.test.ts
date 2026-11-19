import { DiffHunksRenderer, hydratePartialDiff, type FileDiffMetadata } from "@pierre/diffs";
import { excerptRows, fileDiffRows, rowsToPatch } from "@onicode/client-runtime/review-guide-tour";
import { EnvironmentId } from "@onicode/contracts";
import { AsyncResult } from "effect/reactivity";
import { expect, it } from "vite-plus/test";

import { createGitDiffFileContentsLoader } from "~/lib/diffFileContents";
import { getRenderablePatch } from "~/lib/diffRendering";
import { commentDraftFor, excerptLoader } from "./GuideExcerpt.logic";

function parse(patch: string): FileDiffMetadata {
  const renderable = getRenderablePatch(patch, "guide-excerpt-test");
  if (renderable?.kind !== "files") throw new Error("Expected a parsed patch.");
  return renderable.files[0]!;
}

function loaderFor(original: ReadonlyArray<string>, changed: ReadonlyArray<string>) {
  return createGitDiffFileContentsLoader(
    async () =>
      AsyncResult.success({
        oldContents: `${original.join("\n")}\n`,
        newContents: `${changed.join("\n")}\n`,
      }),
    {
      environmentId: EnvironmentId.make("environment-guide-excerpt-test"),
      cwd: "/repo",
      sourceKind: "working-tree",
      baseRef: null,
      headRef: null,
      cacheKey: "diff-hash",
    },
  );
}

it("expands an excerpt whose file has another hunk that changes its length", async () => {
  const original = Array.from({ length: 100 }, (_, index) => `line ${index + 1}`);
  const changed = original.map((line, index) => (index === 29 ? "line 30 changed" : line));
  changed.splice(80, 0, "later insertion");
  const file = parse(
    [
      "diff --git a/file.ts b/file.ts",
      "--- a/file.ts",
      "+++ b/file.ts",
      "@@ -27,7 +27,7 @@",
      " line 27",
      " line 28",
      " line 29",
      "-line 30",
      "+line 30 changed",
      " line 31",
      " line 32",
      " line 33",
      "@@ -78,6 +78,7 @@",
      " line 78",
      " line 79",
      " line 80",
      "+later insertion",
      " line 81",
      " line 82",
      " line 83",
      "",
    ].join("\n"),
  );
  const excerpt = { path: "file.ts", side: "new", start: 30, end: 30, caption: "" } as const;
  const rows = excerptRows(fileDiffRows(file), excerpt);
  const excerptDiff = parse(rowsToPatch("file.ts", "file.ts", rows));
  const files = await excerptLoader(excerpt, file, rows, loaderFor(original, changed))!(
    excerptDiff,
  );
  const hydrated = hydratePartialDiff("clone", excerptDiff, files);

  await expect(
    new DiffHunksRenderer({ expandUnchanged: true }).asyncRender(hydrated),
  ).resolves.toBeDefined();
});

it("keeps the lines a draft selected from expanded context for Ask agent", () => {
  const original = Array.from({ length: 12 }, (_, index) => `line ${index + 1}`);
  const changed = original.map((line, index) => (index === 9 ? "line 10 changed" : line));
  const file = parse(
    [
      "diff --git a/file.ts b/file.ts",
      "--- a/file.ts",
      "+++ b/file.ts",
      "@@ -9,3 +9,3 @@",
      " line 9",
      "-line 10",
      "+line 10 changed",
      " line 11",
      "",
    ].join("\n"),
  );
  // The view loads unchanged lines into its own copy of the diff.
  const drawn = hydratePartialDiff("clone", file, {
    oldFile: { name: "file.ts", contents: `${original.join("\n")}\n` },
    newFile: { name: "file.ts", contents: `${changed.join("\n")}\n` },
  });

  const draft = commentDraftFor({
    path: "file.ts",
    file,
    drawn,
    range: { start: 7, side: "additions", end: 10, endSide: "additions" },
    ranged: true,
    canComment: true,
  });

  expect(draft?.position).toEqual({ kind: "added", newLine: 10 });
  expect(draft?.context?.diff).toBe(
    ["@@ -7,4 +7,4 @@", " line 7", " line 8", " line 9", "-line 10", "+line 10 changed"].join("\n"),
  );
  // A diff replaced before its lines loaded still opens the review comment, without Ask agent.
  const unloaded = commentDraftFor({
    path: "file.ts",
    file,
    drawn: file,
    range: { start: 7, side: "additions", end: 10, endSide: "additions" },
    ranged: true,
    canComment: true,
  });
  expect(unloaded?.position).toEqual({ kind: "added", newLine: 10 });
  expect(unloaded?.context).toBeNull();
});

it("keeps the lines drawn after a hunk that changes the line count for Ask agent", async () => {
  const original = Array.from({ length: 40 }, (_, index) => `line ${index + 1}`);
  const changed = original.map((line, index) => (index === 19 ? "line 20 changed" : line));
  changed.splice(5, 0, ...Array.from({ length: 10 }, (_, index) => `inserted ${index + 1}`));
  const file = parse(
    [
      "diff --git a/file.ts b/file.ts",
      "--- a/file.ts",
      "+++ b/file.ts",
      "@@ -3,6 +3,16 @@",
      " line 3",
      " line 4",
      " line 5",
      ...Array.from({ length: 10 }, (_, index) => `+inserted ${index + 1}`),
      " line 6",
      " line 7",
      " line 8",
      "@@ -17,7 +27,7 @@",
      " line 17",
      " line 18",
      " line 19",
      "-line 20",
      "+line 20 changed",
      " line 21",
      " line 22",
      " line 23",
      "",
    ].join("\n"),
  );
  const excerpt = { path: "file.ts", side: "new", start: 30, end: 30, caption: "" } as const;
  const rows = excerptRows(fileDiffRows(file), excerpt);
  const excerptDiff = parse(rowsToPatch("file.ts", "file.ts", rows));
  const files = await excerptLoader(excerpt, file, rows, loaderFor(original, changed))!(
    excerptDiff,
  );
  const drawn = hydratePartialDiff("clone", excerptDiff, files);

  // New line 22 is also an old line number inside the second hunk.
  const draft = commentDraftFor({
    path: "file.ts",
    file,
    drawn,
    range: { start: 22, side: "additions", end: 30, endSide: "additions" },
    ranged: true,
    canComment: true,
  });

  expect(draft?.position).toEqual({ kind: "added", newLine: 30 });
  expect(draft?.context?.diff).toBe(
    [
      "@@ -12,9 +22,9 @@",
      ...Array.from({ length: 8 }, (_, index) => ` line ${index + 12}`),
      "-line 20",
      "+line 20 changed",
    ].join("\n"),
  );

  // Across hunks, the pull request's diff lacks the unchanged lines loaded between them.
  const loaded = hydratePartialDiff("clone", file, await loaderFor(original, changed)(file));
  const acrossHunks = commentDraftFor({
    path: "file.ts",
    file,
    drawn: loaded,
    range: { start: 17, side: "additions", end: 30, endSide: "additions" },
    ranged: true,
    canComment: true,
  });
  expect(acrossHunks?.context?.diff).toBe(
    [
      "@@ -7,14 +17,14 @@",
      ...Array.from({ length: 13 }, (_, index) => ` line ${index + 7}`),
      "-line 20",
      "+line 20 changed",
    ].join("\n"),
  );
});

it("leaves a deleted file's excerpt without context expansion", () => {
  const file = parse(
    [
      "diff --git a/file.ts b/file.ts",
      "deleted file mode 100644",
      "--- a/file.ts",
      "+++ /dev/null",
      "@@ -1,3 +0,0 @@",
      "-a",
      "-b",
      "-c",
      "",
    ].join("\n"),
  );
  const excerpt = { path: "file.ts", side: "old", start: 2, end: 2, caption: "" } as const;
  const rows = excerptRows(fileDiffRows(file), excerpt);

  expect(excerptLoader(excerpt, file, rows, async () => Promise.reject(new Error("unused")))).toBe(
    null,
  );
});
