import { decodeString } from "micromark-util-decode-string";

import { FRONTMATTER_MAX_LINES, leadingFrontmatterLength } from "./markdownFrontmatter.ts";

// About one screen of rendered Markdown; parsing costs up to ~40 ms per KB with tables and math.
const CHUNK_TARGET_CHARS = 4 * 1024;
// A chunk past this (one huge fence or HTML block) is too slow to parse at once; show source.
export const LARGE_MARKDOWN_CHUNK_MAX_CHARS = 128 * 1024;
// A backtick fence's info string cannot contain a backtick, so "```a``` is inline" is a code span.
const FENCE_OPEN = /^ {0,3}(`{3,}(?![^`\n]*`)|~{3,})/;
const ATX_HEADING = /^#{1,6}(?:[ \t]|\r?$)/;
// A thematic break, or a "---" setext underline; no paragraph stays open after either. A lone
// "===" is paragraph text unless it underlines one, so it is left out.
const THEMATIC_BREAK = /^ {0,3}(?:(?:-[ \t]*){3,}|(?:\*[ \t]*){3,}|(?:_[ \t]*){3,})\r?$/;
// A column-0 paragraph, quote or table after a blank line closes any open list, quote or table.
const BLOCK_START = /^[\p{L}\p{N}`[!|>]/u;
const ORDERED_ITEM = /^\d{1,9}[.)](?:[ \t]|\r?$)/;
// CommonMark HTML blocks of types 1-5 and display math run past blank lines until their closer.
const SPANNING_BLOCKS: ReadonlyArray<readonly [open: RegExp, close: RegExp]> = [
  [/^ {0,3}<(?:script|pre|style|textarea)(?=[\s>]|$)/i, /<\/(?:script|pre|style|textarea)>/i],
  [/^ {0,3}<!--/, /-->/],
  [/^ {0,3}<\?/, /\?>/],
  [/^ {0,3}<!\[CDATA\[/, /\]\]>/],
  [/^ {0,3}<![A-Za-z]/, />/],
  [/^ {0,3}\$\$/, /\$\$/],
];

// A tag name, not an autolink such as <https://example.com>.
const HTML_BLOCK_START = /^ {0,3}<\/?[A-Za-z][A-Za-z0-9-]*(?=[\s/>]|$)/;
const closesAtBlankLine = (next: string) => next.trim() === "";

// A <details> or <table> wraps the Markdown between its tags, across the blank lines that end
// each HTML block, so a cut waits until the ones an HTML block opened are closed.
const WRAPPER_TAG = /<(\/?)(?:details|table)(?=[\s/>]|$)/gi;
// An end tag that never comes, or back-to-back wrappers, must not make one huge piece.
const WRAPPED_MAX_CHARS = 32 * 1024;

function withoutComments(line: string): string {
  let kept = "";
  let at = 0;
  for (let open = line.indexOf("<!--"); open !== -1; open = line.indexOf("<!--", at)) {
    const close = line.indexOf("-->", open + 4);
    if (close === -1) break;
    kept += line.slice(at, open);
    at = close + 3;
  }
  return kept + line.slice(at);
}

function openedWrappers(line: string): number {
  let opened = 0;
  for (const [, closing] of withoutComments(line).matchAll(WRAPPER_TAG)) {
    opened += closing ? -1 : 1;
  }
  return opened;
}

/** Returns how to recognize the end of a block that `line` opens, or null for none. */
function blockCloser(line: string): ((line: string) => boolean) | null {
  const fence = FENCE_OPEN.exec(line)?.[1];
  if (fence) {
    return (next) => {
      const closer = FENCE_OPEN.exec(next)?.[1];
      return (
        closer !== undefined &&
        closer[0] === fence[0] &&
        closer.length >= fence.length &&
        !next.trim().slice(closer.length)
      );
    };
  }
  for (const [open, close] of SPANNING_BLOCKS) {
    const opener = open.exec(line)?.[0];
    if (opener === undefined) continue;
    return close.test(line.slice(opener.length)) ? null : (next) => close.test(next);
  }
  // Other HTML blocks (CommonMark types 6 and 7) run until a blank line.
  return HTML_BLOCK_START.test(line) ? closesAtBlankLine : null;
}

export interface MarkdownChunk {
  readonly text: string;
  readonly lines: number;
}

// A one-line link reference definition; footnotes are left alone so each piece does not grow
// a footnote section.
const LINK_DEFINITION =
  /^ {0,3}\[(?!\^)(?:[^\]\\]|\\.)+\]:[ \t]*(?:<[^<>\n]*>|[^\s<]\S*)(?:[ \t]+(?:"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'|\((?:[^()\\]|\\.)*\)))?[ \t]*\r?$/;
// Definitions past this are not repeated into every piece; their parse cost would outweigh links.
const SHARED_DEFINITIONS_MAX_CHARS = 16 * 1024;
// A footnote resolves only inside its own piece, so a file that defines one stays whole.
const FOOTNOTE_DEFINITION = /^ {0,3}\[\^[^\]\n]+\]:/m;

/** The scan state before the last chunk's first line, so a longer text rescans only from there. */
interface MarkdownSplitResume {
  readonly targetChars: number;
  /** Length of the text this split came from; set on the returned value. */
  readonly textLength: number;
  readonly start: number;
  readonly linesBefore: number;
  readonly frontmatter: number;
  /** Length of `definitions` collected before `start`. */
  readonly definitions: number;
  readonly definitionChars: number;
  readonly afterBlankLine: boolean;
  readonly afterDefinition: boolean;
  readonly afterHeading: boolean;
}

export interface SplitMarkdown {
  readonly chunks: readonly MarkdownChunk[];
  readonly definitions: string;
  /** Absent while the text is one piece. */
  readonly resume?: MarkdownSplitResume;
}

/**
 * Cuts Markdown into pieces of about `targetChars` at column-0 block starts, never at an
 * unfinished last line; frontmatter stays in the first piece. Link definitions are shared across
 * pieces. Footnotes resolve only within a piece, so a file under the large-chunk cap that defines
 * one stays whole. Duplicate heading slugs restart per piece.
 */
export function splitMarkdown(text: string, targetChars = CHUNK_TARGET_CHARS): SplitMarkdown {
  if (keepsWhole(text)) return wholeMarkdown(text);
  return scanMarkdown(text, targetChars, [], "", {
    targetChars,
    textLength: 0,
    start: 0,
    linesBefore: 0,
    frontmatter: leadingFrontmatterLength(text),
    definitions: 0,
    definitionChars: 0,
    afterBlankLine: false,
    afterDefinition: false,
    afterHeading: false,
  });
}

/**
 * `splitMarkdown(text, targetChars)` for a `text` that starts with the text `previous` was split
 * from. Rescans only from the start of `previous`'s last chunk and reuses every earlier chunk.
 */
export function extendMarkdownSplit(
  previous: SplitMarkdown,
  text: string,
  targetChars = CHUNK_TARGET_CHARS,
): SplitMarkdown {
  const resume = previous.resume;
  // Callers only append; these checks catch a shorter text, another piece size, or a lost resume.
  if (
    resume === undefined ||
    resume.targetChars !== targetChars ||
    text.length < resume.textLength ||
    text.charCodeAt(resume.start - 1) !== 10 ||
    keepsWhole(text)
  ) {
    return splitMarkdown(text, targetChars);
  }
  // Frontmatter is settled once its closer or line cap lies before the last cut.
  const frontmatter =
    resume.frontmatter > 0 || resume.linesBefore > FRONTMATTER_MAX_LINES
      ? resume.frontmatter
      : leadingFrontmatterLength(text);
  if (frontmatter !== resume.frontmatter) return splitMarkdown(text, targetChars);
  return scanMarkdown(
    text,
    targetChars,
    previous.chunks.slice(0, -1),
    previous.definitions.slice(0, resume.definitions),
    resume,
  );
}

function keepsWhole(text: string): boolean {
  return text.length <= LARGE_MARKDOWN_CHUNK_MAX_CHARS && FOOTNOTE_DEFINITION.test(text);
}

function wholeMarkdown(text: string): SplitMarkdown {
  let lines = 1;
  for (let at = text.indexOf("\n"); at >= 0; at = text.indexOf("\n", at + 1)) lines++;
  return { chunks: [{ text, lines }], definitions: "" };
}

function scanMarkdown(
  text: string,
  targetChars: number,
  chunks: MarkdownChunk[],
  definitionsBefore: string,
  from: MarkdownSplitResume,
): SplitMarkdown {
  const { frontmatter } = from;
  const definitions = definitionsBefore ? [definitionsBefore] : [];
  let definitionsLength = definitionsBefore.length;
  let { afterBlankLine, afterDefinition, afterHeading, definitionChars } = from;
  let resume = from.start > 0 ? from : undefined;
  let lines: string[] = [];
  let chars = 0;
  let closes: ((line: string) => boolean) | null = null;
  // A cut needs no open wrapper, so a resumed scan starts with none.
  let openWrappers = 0;
  let offset = from.start;
  let linesBefore = from.linesBefore;
  const textLines = (from.start > 0 ? text.slice(from.start) : text).split("\n");
  for (const [index, line] of textLines.entries()) {
    const inFrontmatter = offset < frontmatter;
    if (inFrontmatter) {
      // Masked to blank lines, so nothing inside is a cut, a block or a definition.
    } else if (closes !== null) {
      if (closes(line)) closes = null;
    } else {
      // The last line may still be growing; "1" can become "1. item" and "#" "#tag".
      if (chars >= WRAPPED_MAX_CHARS) openWrappers = 0;
      const cut =
        lines.length > 0 &&
        index < textLines.length - 1 &&
        chars >= targetChars &&
        openWrappers === 0 &&
        (ATX_HEADING.test(line) ||
          (afterBlankLine && BLOCK_START.test(line) && !ORDERED_ITEM.test(line)));
      if (cut) {
        chunks.push({ text: lines.join("\n"), lines: lines.length });
        resume = {
          targetChars,
          textLength: 0,
          start: offset,
          linesBefore,
          frontmatter,
          definitions: definitionsLength,
          definitionChars,
          afterBlankLine,
          afterDefinition,
          afterHeading,
        };
        lines = [];
        chars = 0;
      }
      closes = blockCloser(line);
      // A definition cannot interrupt a paragraph, so it follows a blank line, a heading or
      // another definition.
      const definition: boolean =
        (offset === 0 || afterBlankLine || afterDefinition || afterHeading) &&
        LINK_DEFINITION.test(line);
      afterDefinition = definition;
      if (definition && definitionChars + line.length < SHARED_DEFINITIONS_MAX_CHARS) {
        const trimmed = line.trim();
        definitions.push(trimmed);
        definitionsLength += (definitionsLength > 0 ? 1 : 0) + trimmed.length;
        definitionChars += line.length + 1;
      }
    }
    if (closes === closesAtBlankLine)
      openWrappers = Math.max(0, openWrappers + openedWrappers(line));
    lines.push(line);
    chars += line.length + 1;
    afterBlankLine = inFrontmatter || line.trim() === "";
    afterHeading = closes === null && (ATX_HEADING.test(line) || THEMATIC_BREAK.test(line));
    offset += line.length + 1;
    linesBefore++;
  }
  chunks.push({ text: lines.join("\n"), lines: lines.length });
  return resume === undefined
    ? { chunks, definitions: definitions.join("\n") }
    : {
        chunks,
        definitions: definitions.join("\n"),
        resume: { ...resume, textLength: text.length },
      };
}

/** Appends shared link definitions after a piece, leaving its own offsets unchanged. */
export function withMarkdownDefinitions(text: string, definitions: string): string {
  return definitions ? `${text}\n\n${definitions}` : text;
}

/**
 * Cuts source text into runs of whole lines of about `targetChars`, cutting a longer line too,
 * so no one text view lays out a huge run. The line break between two runs is dropped.
 */
export function cutSourceLines(text: string, targetChars = CHUNK_TARGET_CHARS): string[] {
  const runs: string[] = [];
  let start = 0;
  while (text.length - start > targetChars) {
    let end = start + targetChars;
    // Searches back only within this run; lastIndexOf would rescan a long line for every run.
    let newline = end;
    while (newline > start && text.charCodeAt(newline) !== 10) newline--;
    if (newline > start) {
      runs.push(text.slice(start, newline));
      start = newline + 1;
      continue;
    }
    // Never split a surrogate pair, unless the run would be empty.
    const code = text.charCodeAt(end - 1);
    if (end - 1 > start && code >= 0xd800 && code <= 0xdbff) end--;
    runs.push(text.slice(start, end));
    start = end;
  }
  runs.push(text.slice(start));
  return runs;
}

/** GitHub's heading anchor for a heading's rendered text, before duplicates are numbered. */
export function githubHeadingSlug(text: string): string {
  return text
    .trim()
    .toLowerCase()
    .replace(/[^\p{L}\p{M}\p{N}\p{Pc} -]/gu, "")
    .replace(/ /g, "-");
}

/** Numbers repeated slugs as GitHub does: `a`, `a-1`, `a-2`, skipping any id already taken. */
function headingIdNumberer(): (slug: string) => string {
  const taken = new Set<string>();
  const nextSuffix = new Map<string, number>();
  return (slug) => {
    let count = nextSuffix.get(slug) ?? 0;
    let id = slug;
    while (taken.has(id)) {
      count += 1;
      id = `${slug}-${count}`;
    }
    nextSuffix.set(slug, count);
    taken.add(id);
    return id;
  };
}

const ATX_HEADING_TEXT = /^ {0,3}#{1,6}(?:[ \t]+(.*?))?(?:[ \t]+#+)?[ \t]*\r?$/;
const SETEXT_UNDERLINE = /^ {0,3}(?:=+|-+)[ \t]*\r?$/;
// Lines that start a block other than a paragraph, which a setext underline cannot follow, and
// those of them that also end an open paragraph.
const NON_PARAGRAPH_LINE = /^(?: {4}|\t| {0,3}(?:[-*+][ \t]|\d{1,9}[.)][ \t]|>|\|))/;
const PARAGRAPH_INTERRUPT = /^ {0,3}(?:[-*+][ \t]|1[.)][ \t]|>)/;
const CODE_SPAN = /(`+)([^`]|[^`][\s\S]*?[^`])\1(?!`)/g;

const DECODED_UNDERSCORE = "\uE000";

/** The text a heading's inline Markdown renders as, outside code spans. */
function inlinePlainText(markdown: string): string {
  return (
    markdown
      .replace(/!?\[((?:[^\]\\]|\\.)*)\](?:\([^)]*\)|\[[^\]]*\])/g, "$1")
      .replace(/<((?:https?|mailto):[^<>\s]*)>/gi, "$1")
      .replace(/<[^<>]*>/g, "")
      // A decoded underscore is text, so it hides from the emphasis pass.
      .replace(/&(?:[a-z][a-z\d]*|#\d+|#x[\da-f]+);/gi, (reference) =>
        decodeString(reference).replaceAll("_", DECODED_UNDERSCORE),
      )
      // Emphasis underscores sit at word edges; snake_case ones are text.
      .replace(/(^|[^\p{L}\p{N}_])_+|_+(?=[^\p{L}\p{N}_]|$)/gu, "$1")
      .replaceAll(DECODED_UNDERSCORE, "_")
  );
}

function headingPlainText(markdown: string): string {
  let text = "";
  let last = 0;
  for (const match of markdown.matchAll(CODE_SPAN)) {
    text += inlinePlainText(markdown.slice(last, match.index)) + (match[2] ?? "");
    last = match.index + match[0].length;
  }
  return text + inlinePlainText(markdown.slice(last));
}

/**
 * The slug of each ATX and setext heading in `text`, in order, as its rendered text gives it and
 * before duplicates are numbered. Headings in fences, HTML blocks and frontmatter are left out.
 */
function headingSlugs(text: string): string[] {
  const slugs: string[] = [];
  const frontmatter = leadingFrontmatterLength(text);
  let offset = 0;
  let closes: ((line: string) => boolean) | null = null;
  // Lines of the open paragraph, or null inside another block until a blank line.
  let paragraph: string[] | null = [];
  const add = (heading: string) => {
    const slug = githubHeadingSlug(headingPlainText(heading));
    if (slug) slugs.push(slug);
  };
  for (const line of text.split("\n")) {
    const inFrontmatter = offset < frontmatter;
    offset += line.length + 1;
    if (inFrontmatter) continue;
    if (closes !== null) {
      if (closes(line)) {
        closes = null;
        paragraph = [];
      }
      continue;
    }
    if (line.trim() === "") {
      paragraph = [];
      continue;
    }
    const atx = ATX_HEADING_TEXT.exec(line);
    if (atx) {
      add(atx[1] ?? "");
      paragraph = [];
      continue;
    }
    if (paragraph !== null && paragraph.length > 0 && SETEXT_UNDERLINE.test(line)) {
      add(paragraph.join("\n"));
      paragraph = [];
      continue;
    }
    closes = blockCloser(line);
    if (closes !== null || THEMATIC_BREAK.test(line)) {
      paragraph = [];
    } else if (
      paragraph !== null &&
      !(paragraph.length > 0 ? PARAGRAPH_INTERRUPT : NON_PARAGRAPH_LINE).test(line)
    ) {
      paragraph.push(line.trim());
    } else {
      paragraph = null;
    }
  }
  return slugs;
}

/**
 * The piece holding the heading GitHub anchors as `id`, numbering duplicates across the whole
 * file, with the id that piece's own render gives it, as each piece numbers duplicates anew.
 */
export function markdownHeadingPiece(
  chunks: readonly MarkdownChunk[],
  id: string,
): { readonly index: number; readonly id: string } | null {
  const fileId = headingIdNumberer();
  for (const [index, chunk] of chunks.entries()) {
    const pieceId = headingIdNumberer();
    for (const slug of headingSlugs(chunk.text)) {
      const ownId = pieceId(slug);
      if (fileId(slug) === id) return { index, id: ownId };
    }
  }
  return null;
}
