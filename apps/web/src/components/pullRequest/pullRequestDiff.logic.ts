import type { FileDiffMetadata } from "@pierre/diffs";
import type { PullRequestDiffSide } from "@onicode/contracts";

import { buildFileDiffContentVersion, resolveFileDiffPath } from "~/lib/diffRendering";

/** Only rendered hunks can anchor comments; list others off-diff. */
export function isLineInFileDiff(
  file: FileDiffMetadata,
  side: PullRequestDiffSide,
  line: number,
): boolean {
  return file.hunks.some((hunk) =>
    side === "left"
      ? line >= hunk.deletionStart && line < hunk.deletionStart + hunk.deletionCount
      : line >= hunk.additionStart && line < hunk.additionStart + hunk.additionCount,
  );
}

export interface PullRequestDraftAnchorTarget {
  readonly path: string;
  readonly contentVersion: number;
  readonly side: PullRequestDiffSide;
  readonly line: number;
}

/** Attach a draft only while its file diff is unchanged and its anchor line still sits in a hunk. */
export function canAttachPullRequestDraft(
  draft: PullRequestDraftAnchorTarget | null,
  fileDiff: FileDiffMetadata,
): boolean {
  if (draft === null) return false;
  if (resolveFileDiffPath(fileDiff) !== draft.path) return false;
  if (buildFileDiffContentVersion(fileDiff) !== draft.contentVersion) return false;
  return isLineInFileDiff(fileDiff, draft.side, draft.line);
}

/** Carry typed text onto a new line selection in the same file after a refresh detached the draft. */
export function carryDetachedPullRequestDraftText(input: {
  readonly previousPath: string | null;
  readonly previousAttached: boolean;
  readonly previousText: string | undefined;
  readonly nextPath: string;
}): string | undefined {
  if (input.previousAttached) return undefined;
  if (input.previousPath === null || input.previousPath !== input.nextPath) return undefined;
  return input.previousText && input.previousText.length > 0 ? input.previousText : undefined;
}

export function getFileViewedRange<T extends { readonly fileKey: string }>(
  files: ReadonlyArray<T>,
  fileKey: string,
  anchorFileKey: string | null,
): ReadonlyArray<T> {
  const index = files.findIndex((file) => file.fileKey === fileKey);
  if (index === -1) return [];
  const anchorIndex = files.findIndex((file) => file.fileKey === anchorFileKey);
  if (anchorIndex === -1) return files.slice(index, index + 1);
  return files.slice(Math.min(index, anchorIndex), Math.max(index, anchorIndex) + 1);
}
