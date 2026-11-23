import { DEFAULT_TOKENIZE_MAX_LENGTH } from "@pierre/diffs";

import { FULL_TEXT_MAX_BYTES, RENDERED_MAX_LINES } from "./largeTextProtocol";
import { hasLineLongerThan, MAX_SHOWN_LINE_CHARS } from "./shownText";

// Pierre keeps every token of a highlighted file in memory: a 320k-line log took about 280 MB.
export const HIGHLIGHT_MAX_LINES = DEFAULT_TOKENIZE_MAX_LENGTH;

export function countNewlines(text: string): number {
  let lines = 0;
  for (let index = text.indexOf("\n"); index !== -1; index = text.indexOf("\n", index + 1)) {
    lines += 1;
  }
  return lines;
}

/** Estimates the whole file's lines from the first megabyte's density. */
export function estimatedLines(preview: string, sizeBytes: number): number {
  const lines = countNewlines(preview) + 1;
  return (lines / Math.max(1, new TextEncoder().encode(preview).length)) * sizeBytes;
}

/** Whether the worker-backed plain viewer shows the file up front instead of its full text. */
export function largeFileStartsPlain(file: {
  readonly preview: string;
  readonly sizeBytes: number;
  readonly estimatedLines: number;
  readonly table: boolean;
  readonly markdown: boolean;
}): boolean {
  return (
    file.table ||
    file.sizeBytes > FULL_TEXT_MAX_BYTES ||
    // The full text would end in the plain viewer anyway, after laying out its first part.
    file.estimatedLines > RENDERED_MAX_LINES ||
    // The source viewer lays out a whole line; the plain viewer cuts it like a source line.
    (!file.markdown && hasLineLongerThan(file.preview, MAX_SHOWN_LINE_CHARS))
  );
}

/** The text shown so far; `lines` is one more than its newlines, null while the preview shows. */
export interface ShownFileText {
  readonly text: string;
  readonly done: boolean;
  readonly lines: number | null;
  /** Whether a line is too long to lay out, known once the text is done. */
  readonly longLine: boolean;
}

/**
 * A line too long to lay out, or more lines than the main thread can lay out, can sit past the
 * preview; the finished text then goes plain.
 */
export function largeFileEndsPlain(file: ShownFileText, markdown: boolean): boolean {
  if (!file.done) return false;
  return (!markdown && file.longLine) || (file.lines ?? 0) > RENDERED_MAX_LINES;
}

/**
 * Up front the estimate decides, so the ordinary file is not highlighted and then dropped; the
 * real count overrides it as it grows past the limit, and decides alone once the file is done.
 */
export function highlightsLargeFile(estimatedLines: number, file: ShownFileText): boolean {
  if (file.lines === null) return estimatedLines <= HIGHLIGHT_MAX_LINES;
  if (file.done) return file.lines <= HIGHLIGHT_MAX_LINES;
  return estimatedLines <= HIGHLIGHT_MAX_LINES && file.lines <= HIGHLIGHT_MAX_LINES;
}

/** Whether a line link can be revealed now, rather than clamped to the last line shown so far. */
export function largeFileRevealReady(revealLine: number | null, file: ShownFileText): boolean {
  if (revealLine === null || file.done) return true;
  // The preview's last line can be cut short; a streaming swap ends at a newline.
  const wholeLines = file.lines === null ? countNewlines(file.text) : file.lines - 1;
  return revealLine <= wholeLines;
}
