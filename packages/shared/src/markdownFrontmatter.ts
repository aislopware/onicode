const FRONTMATTER_OPEN = /\uFEFF?---[ \t]*(?=\r|\n|$)/y;
const FRONTMATTER_CLOSE = /(?:---|\.\.\.)[ \t]*(?=\r|\n|$)/y;
// An opening rule with no closer is a thematic break, not frontmatter; stop waiting for one.
export const FRONTMATTER_MAX_LINES = 1000;

/**
 * The length of a leading YAML frontmatter block through its closing fence, or 0 when there is
 * none. The Markdown splitter and the preview masks share it, so they agree on what is hidden.
 */
export function leadingFrontmatterLength(markdown: string): number {
  FRONTMATTER_OPEN.lastIndex = 0;
  if (!FRONTMATTER_OPEN.test(markdown)) return 0;
  const lineBreaks = /\r\n?|\n/g;
  lineBreaks.lastIndex = FRONTMATTER_OPEN.lastIndex;
  for (let line = 0; line < FRONTMATTER_MAX_LINES; line++) {
    const lineBreak = lineBreaks.exec(markdown);
    if (lineBreak === null) return 0;
    FRONTMATTER_CLOSE.lastIndex = lineBreak.index + lineBreak[0].length;
    if (FRONTMATTER_CLOSE.test(markdown)) return FRONTMATTER_CLOSE.lastIndex;
  }
  return 0;
}

/**
 * Blanks a leading YAML frontmatter block so rendered previews skip it instead
 * of showing a rule and a setext heading. Masked characters become spaces and
 * line breaks stay, so source offsets (task checkbox edits) stay valid.
 */
export function maskMarkdownFrontmatter(markdown: string): string {
  const end = leadingFrontmatterLength(markdown);
  return end === 0
    ? markdown
    : markdown.slice(0, end).replace(/[^\r\n]/g, " ") + markdown.slice(end);
}
