import type { ScopedThreadRef } from "@onicode/contracts";

import ChatMarkdown from "~/components/ChatMarkdown";
import { resolvePathLinkTarget } from "@onicode/shared/fileLinks";
import { withMarkdownDefinitions } from "@onicode/shared/markdownChunks";
import { maskMarkdownFrontmatter } from "@onicode/shared/markdownFrontmatter";
import { lineCutMarkdown, useMoreCharactersLabel } from "./shownText";

export function FileMarkdownPreview(props: {
  readonly cwd: string;
  readonly relativePath: string;
  readonly text: string;
  readonly threadRef: ScopedThreadRef;
  readonly workspaceMutationId: string | null;
  readonly className?: string;
  /** Link definitions shared from elsewhere in the file; never laid out, so never cut. */
  readonly definitions?: string;
  readonly onTaskListChange?:
    | ((input: { readonly markerOffset: number; readonly checked: boolean }) => void)
    | undefined;
}) {
  const more = useMoreCharactersLabel();
  const lastSeparator = Math.max(
    props.relativePath.lastIndexOf("/"),
    props.relativePath.lastIndexOf("\\"),
  );
  const imageBaseDir =
    lastSeparator >= 0
      ? resolvePathLinkTarget(props.relativePath.slice(0, lastSeparator), props.cwd)
      : props.cwd;

  const masked = maskMarkdownFrontmatter(props.text);
  const text = lineCutMarkdown(masked, more);
  return (
    <ChatMarkdown
      text={props.definitions ? withMarkdownDefinitions(text, props.definitions) : text}
      cwd={props.cwd}
      imageBaseDir={imageBaseDir}
      threadRef={props.threadRef}
      workspaceMutationId={props.workspaceMutationId}
      className={props.className ?? "mx-auto max-w-4xl px-6 py-5"}
      // Task offsets point into the shown text, which no longer matches a file with a cut line.
      onTaskListChange={text === masked ? props.onTaskListChange : undefined}
    />
  );
}
