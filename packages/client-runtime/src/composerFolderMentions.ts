import type { FilesystemBrowseEntry } from "@onicode/contracts";

const COMPOSER_FOLDER_QUERY_MIN_LENGTH = 2;
const COMPOSER_FOLDER_QUERY_MAX_LENGTH = 256;
export const COMPOSER_FOLDER_SEARCH_LIMIT = 12;
const COMPOSER_FOLDER_RESULT_LIMIT = 6;

/** The host folder search query for an `@` mention, or null when it is too short to be useful. */
export function composerFolderSearchQuery(query: string | null): string | null {
  const trimmed = query?.trim() ?? "";
  if (trimmed.length < COMPOSER_FOLDER_QUERY_MIN_LENGTH) return null;
  return trimmed.slice(0, COMPOSER_FOLDER_QUERY_MAX_LENGTH).replace(/[\uD800-\uDBFF]$/, "");
}

function comparablePath(path: string): string {
  const normalized = path.replaceAll("\\", "/").replace(/\/+$/, "");
  return /^[A-Za-z]:/.test(normalized) ? normalized.toLowerCase() : normalized;
}

/** Drops the workspace and folders inside it, which the workspace file search already offers. */
export function composerFolderMatches(
  entries: ReadonlyArray<FilesystemBrowseEntry>,
  workspaceRoot: string | null,
): ReadonlyArray<FilesystemBrowseEntry> {
  const root = workspaceRoot === null ? null : comparablePath(workspaceRoot);
  const matches = entries.filter((entry) => {
    if (root === null) return true;
    const path = comparablePath(entry.fullPath);
    return path !== root && !path.startsWith(`${root}/`);
  });
  return matches.slice(0, COMPOSER_FOLDER_RESULT_LIMIT);
}

/** The folder's parent path, shown beside its name. */
export function composerFolderParentPath(fullPath: string): string {
  const separatorIndex = Math.max(fullPath.lastIndexOf("/"), fullPath.lastIndexOf("\\"));
  return separatorIndex > 0 ? fullPath.slice(0, separatorIndex) : "";
}
