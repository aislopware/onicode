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
