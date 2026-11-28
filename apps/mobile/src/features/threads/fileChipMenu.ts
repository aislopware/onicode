import { formatFilePathForClipboard } from "@onicode/client-runtime/media-actions";
import { fileBasename } from "@onicode/shared/path";
import type { ThreadId } from "@onicode/contracts";
import { resolveMarkdownLinkPresentation } from "../../lib/markdownLinks";
import type { MarkdownFileContextMenu } from "../../native/SelectableMarkdownText.types";
import { hostPreviewMimeTypeFromExtension } from "@onicode/shared/filePreview";
import { msg } from "gt-react-native";

import { m, type MessageResolver } from "../../i18n/messages";

import {
  isAbsolutePath,
  resolveFileLinkPath,
  resolveWorkspaceFilePath,
  resolveWorkspaceRelativeFilePath,
} from "../files/filePath";

export interface FileChipTarget {
  readonly fullPath?: string;
  readonly relativePath?: string;
  /** The link ends in a path separator, so it names a folder. */
  readonly folder?: true;
}

export function fileChipClipboardPath(
  target: FileChipTarget,
  action: "copy-full-path" | "copy-relative-path",
): string | undefined {
  const path = action === "copy-full-path" ? target.fullPath : target.relativePath;
  return path === undefined ? undefined : formatFilePathForClipboard(path, target.fullPath);
}

export function resolveFileChipTarget(
  href: string,
  workspaceRoot: string | null | undefined,
): FileChipTarget | null {
  const presentation = resolveMarkdownLinkPresentation(href);
  if (presentation.kind !== "file") return null;
  const path = resolveFileLinkPath(workspaceRoot, presentation.path);
  if (path === null) return null;
  const relativePath = resolveWorkspaceRelativeFilePath(workspaceRoot, path);
  const fullPath = isAbsolutePath(path)
    ? path
    : workspaceRoot && relativePath
      ? resolveWorkspaceFilePath(workspaceRoot, relativePath)
      : undefined;
  if (!fullPath && !relativePath) return null;
  return {
    ...(fullPath ? { fullPath } : {}),
    ...(relativePath ? { relativePath } : {}),
    ...(presentation.folder ? { folder: true as const } : {}),
  };
}

/**
 * Any file downloads as an attachment, whatever its type; a folder does not. Use literal resolved
 * paths so encoded filename characters are not decoded twice.
 */
export function fileChipShareSource(target: FileChipTarget, threadId: ThreadId) {
  const path = target.fullPath ?? target.relativePath;
  if (!path || target.folder) return null;
  const name = fileBasename(path);
  const dot = name.lastIndexOf(".");
  return {
    name,
    mimeType:
      (dot < 0 ? null : hostPreviewMimeTypeFromExtension(name.slice(dot))) ??
      "application/octet-stream",
    resource: { _tag: "media-file" as const, threadId, path, disposition: "attachment" as const },
  };
}

export function fileChipMenu(
  target: FileChipTarget,
  resolve: MessageResolver = m,
): MarkdownFileContextMenu {
  return {
    title: target.fullPath ?? target.relativePath ?? "",
    actions: [
      ...(target.fullPath ? [{ id: "copy-full-path", title: resolve(msg("Copy full path")) }] : []),
      ...(target.relativePath
        ? [{ id: "copy-relative-path", title: resolve(msg("Copy relative path")) }]
        : []),
      { id: "open-file", title: resolve(msg("Open in file viewer")) },
      ...(target.folder ? [] : [{ id: "save", title: resolve(msg("Save or share")) }]),
    ],
  };
}
