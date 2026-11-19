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
  const loadDiffFiles = createGitDiffFileContentsLoader(
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

  const files = await excerptLoader(excerpt, file, rows, loadDiffFiles)!(excerptDiff);
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
  });

  expect(draft?.position).toEqual({ kind: "added", newLine: 10 });
  expect(draft?.context.diff).toBe(
    ["@@ -7,4 +7,4 @@", " line 7", " line 8", " line 9", "-line 10", "+line 10 changed"].join("\n"),
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
