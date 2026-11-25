import {
  PROJECT_TRACKED_FOLDERS_MAX,
  type ProjectTrackedFolder,
  type WorkspaceScannedFolder,
} from "@onicode/contracts";
import { splitsSurrogatePair } from "@onicode/shared/assistantCitations";

const PROJECT_TRACKED_FOLDER_NAME_MAX = 128;

/** Truncates a folder label to the project schema limit without splitting a surrogate pair. */
export function normalizeTrackedFolderName(name: string): string {
  const trimmed = name.trimStart();
  return trimmed
    .slice(
      0,
      splitsSurrogatePair(trimmed, PROJECT_TRACKED_FOLDER_NAME_MAX)
        ? PROJECT_TRACKED_FOLDER_NAME_MAX - 1
        : PROJECT_TRACKED_FOLDER_NAME_MAX,
    )
    .trimEnd();
}

/**
 * Folders a project's `.code-workspace` file lists that the project does not cover yet. The
 * server finds repositories inside the project folder itself; a listed folder outside it is only
 * offered, since adding it lets agents work somewhere new.
 */
export function suggestedTrackedFolders(
  tracked: ReadonlyArray<ProjectTrackedFolder>,
  scanned: ReadonlyArray<WorkspaceScannedFolder>,
): ReadonlyArray<ProjectTrackedFolder> {
  const seen = new Set(tracked.map((folder) => folder.path));
  return scanned.flatMap((folder) => {
    if (folder.source !== "code-workspace" || seen.has(folder.path)) return [];
    seen.add(folder.path);
    return [
      {
        path: folder.path,
        name: normalizeTrackedFolderName(folder.name) || normalizeTrackedFolderName(folder.path),
      },
    ];
  });
}

/**
 * The list to send as a project's `trackedFolders` after adding folders. Tracked folders keep
 * their place and name; `null` means the result would pass the server's limit.
 */
export function appendTrackedFolders(
  tracked: ReadonlyArray<ProjectTrackedFolder>,
  added: ReadonlyArray<ProjectTrackedFolder>,
): ReadonlyArray<ProjectTrackedFolder> | null {
  const seen = new Set(tracked.map((folder) => folder.path));
  const next = [...tracked];
  for (const folder of added) {
    if (seen.has(folder.path)) continue;
    seen.add(folder.path);
    next.push(folder);
  }
  return next.length > PROJECT_TRACKED_FOLDERS_MAX ? null : next;
}
