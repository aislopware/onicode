import { isTestChangedFile } from "@onicode/shared/changedFileKind";
import type { ReviewParsedDiff } from "../review/reviewModel";

export function filterPullRequestTestFiles(
  parsed: ReviewParsedDiff,
  hideTestFiles: boolean,
  includePath?: string,
): ReviewParsedDiff {
  if (!hideTestFiles || parsed.kind !== "files") return parsed;
  const files = parsed.files.filter(
    (file) => file.path === includePath || !isTestChangedFile(file.path),
  );
  if (files.length === parsed.files.length) return parsed;
  return {
    ...parsed,
    files,
    fileCount: files.length,
    additions: files.reduce((total, file) => total + file.additions, 0),
    deletions: files.reduce((total, file) => total + file.deletions, 0),
  };
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
