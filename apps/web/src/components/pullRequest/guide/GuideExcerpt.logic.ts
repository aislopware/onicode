import type { FileDiffContentsLoader, FileDiffMetadata, SelectedLineRange } from "@pierre/diffs";
import {
  excerptExpansionOldContents,
  type DiffRow,
} from "@onicode/client-runtime/review-guide-tour";
import type { PullRequestReviewGuideExcerpt, PullRequestReviewPosition } from "@onicode/contracts";

import {
  buildDiffReviewComment,
  diffReviewSelectionId,
  resolveDiffReviewAnchor,
  type ReviewCommentContext,
} from "~/reviewCommentContext";

export interface CommentDraft {
  readonly range: SelectedLineRange;
  readonly position: PullRequestReviewPosition;
  readonly startPosition?: PullRequestReviewPosition;
  /** What Ask agent sends, taken from the lines as they were drawn when selected. */
  readonly context: ReviewCommentContext;
}

// The review anchor comes from the pull request's diff. A selection can also reach unchanged
// lines the view loaded into `drawn`, which that diff lacks. The pull request's diff goes first
// because an expanded excerpt draws the changed lines around it as context.
export function commentDraftFor(input: {
  readonly path: string;
  readonly file: FileDiffMetadata;
  readonly drawn: FileDiffMetadata;
  readonly range: SelectedLineRange | null;
  readonly ranged: boolean;
}): CommentDraft | null {
  const { path, file, range } = input;
  if (range === null) return null;
  const anchor = resolveDiffReviewAnchor(file, range, input.ranged);
  if (anchor === null) return null;
  const contextFrom = (fileDiff: FileDiffMetadata) =>
    buildDiffReviewComment({
      id: `pull-request-guide:${diffReviewSelectionId(path, range)}`,
      sectionId: `pull-request-guide:${path}`,
      // oxlint-disable-next-line onicode/no-untranslated-copy -- context title sent to the agent
      sectionTitle: "Review guide",
      filePath: path,
      fileDiff,
      range,
      text: "",
    });
  const context = contextFrom(file) ?? contextFrom(input.drawn);
  return context === null ? null : { range, ...anchor, context };
}

// Other hunks shift old-side context; expand against an excerpt-matching old side.
// Loads with the source file's diff, which the loaded contents must reproduce.
export function excerptLoader(
  excerpt: PullRequestReviewGuideExcerpt,
  file: FileDiffMetadata,
  sliceRows: ReadonlyArray<DiffRow>,
  loadDiffFiles: FileDiffContentsLoader,
): FileDiffContentsLoader | null {
  // An added or deleted file has no unchanged context to expand.
  if (
    file.type === "new" ||
    file.type === "deleted" ||
    sliceRows.some((row) => row.hunk !== sliceRows[0]?.hunk)
  ) {
    return null;
  }
  return async () => {
    const loaded = await loadDiffFiles(file);
    const oldContents = excerptExpansionOldContents(loaded.newFile.contents, sliceRows);
    if (oldContents === null) return loaded;
    return {
      newFile: loaded.newFile,
      oldFile: {
        name: loaded.oldFile?.name ?? loaded.newFile.name,
        contents: oldContents,
        cacheKey: `${loaded.newFile.cacheKey ?? loaded.newFile.name}:excerpt:${excerpt.side}:${excerpt.start}-${excerpt.end}`,
      },
    };
  };
}
