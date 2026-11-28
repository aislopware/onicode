import { isTestChangedFile } from "@onicode/shared/changedFileKind";
import type { ReviewParsedDiff } from "../review/reviewModel";

function keepFiles(parsed: ReviewParsedDiff, keep: (path: string) => boolean): ReviewParsedDiff {
  if (parsed.kind !== "files") return parsed;
  const files = parsed.files.filter((file) => keep(file.path));
  if (files.length === parsed.files.length) return parsed;
  return {
    ...parsed,
    files,
    fileCount: files.length,
    additions: files.reduce((total, file) => total + file.additions, 0),
    deletions: files.reduce((total, file) => total + file.deletions, 0),
  };
}

export function filterPullRequestTestFiles(
  parsed: ReviewParsedDiff,
  hideTestFiles: boolean,
  includePath?: string,
): ReviewParsedDiff {
  if (!hideTestFiles) return parsed;
  return keepFiles(parsed, (path) => path === includePath || !isTestChangedFile(path));
}

/** Viewed files among `parsed`, and those the filter hides: every one not kept on screen. */
export function viewedFilePaths(
  parsed: ReviewParsedDiff,
  isViewed: (path: string) => boolean,
  isKept: (path: string) => boolean,
) {
  const viewed =
    parsed.kind === "files" ? parsed.files.map((file) => file.path).filter(isViewed) : [];
  return { viewed, hidden: viewed.filter((path) => !isKept(path)) };
}

export function filterPullRequestViewedFiles(
  parsed: ReviewParsedDiff,
  hiddenPaths: ReadonlySet<string>,
): ReviewParsedDiff {
  return keepFiles(parsed, (path) => !hiddenPaths.has(path));
}

/** Patches to load around `index` (or from the top): the next three files the review will show. */
export function shownPatchIndices(
  files: ReadonlyArray<{ readonly path: string }>,
  hideTestFiles: boolean,
  index: number | null,
): ReadonlyArray<number> {
  const isShown = (file: { readonly path: string } | undefined) =>
    file !== undefined && !(hideTestFiles && isTestChangedFile(file.path));
  if (index !== null && !isShown(files[index])) return [index];
  const shown: number[] = [];
  for (let position = index ?? 0; position < files.length && shown.length < 3; position++) {
    if (isShown(files[position])) shown.push(position);
  }
  return shown;
}
