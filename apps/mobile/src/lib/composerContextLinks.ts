import {
  type ComposerContextReferenceOccurrence,
  collectComposerContextReferences,
  formatComposerContextHref,
} from "@onicode/shared/composerContextReferences";

/**
 * A message's context references as Markdown links the renderer shows as pills. An unescaped `!`
 * or backslash before one would turn the link into an image or plain text, so it becomes a
 * character reference; a backslash escape renders wrongly after a Windows path.
 */
export function composerContextLinkMarkdown(
  text: string,
  label: (reference: ComposerContextReferenceOccurrence) => string,
): string {
  let result = "";
  let cursor = 0;
  for (const reference of collectComposerContextReferences(text)) {
    result += text.slice(cursor, reference.start);
    if (/(?:^|[^\\])(?:\\\\)*[!\\]$/.test(result))
      result = `${result.slice(0, -1)}${result.endsWith("!") ? "&#33;" : "&#92;"}`;
    result += `[${label(reference)}](${formatComposerContextHref(reference.kind, reference.contextId)})`;
    cursor = reference.end;
  }
  return result + text.slice(cursor);
}
