import { decodeString } from "micromark-util-decode-string";
import remarkParse from "remark-parse";
import { unified } from "unified";

import {
  FRONTMATTER_MAX_LINES,
  leadingFrontmatterLength,
  maskMarkdownFrontmatter,
} from "./markdownFrontmatter.ts";

// About one screen of rendered Markdown; parsing costs up to ~40 ms per KB with tables and math.
const CHUNK_TARGET_CHARS = 4 * 1024;
// A chunk past this (one huge fence or HTML block) is too slow to parse at once; show source.
export const LARGE_MARKDOWN_CHUNK_MAX_CHARS = 128 * 1024;
// A backtick fence's info string cannot contain a backtick, so "```a``` is inline" is a code span.
const FENCE_OPEN = /^ {0,3}(`{3,}(?![^`\n]*`)|~{3,})/;
const ATX_HEADING = /^ {0,3}#{1,6}(?:[ \t]|\r?$)/;
// A thematic break, or a "---" setext underline; no paragraph stays open after either. A lone
// "===" is paragraph text unless it underlines one, so it is left out.
const THEMATIC_BREAK = /^ {0,3}(?:(?:-[ \t]*){3,}|(?:\*[ \t]*){3,}|(?:_[ \t]*){3,})\r?$/;
// A column-0 paragraph, quote or table after a blank line closes any open list, quote or table.
const BLOCK_START = /^[\p{L}\p{N}`[!|>]/u;
const ORDERED_ITEM = /^\d{1,9}[.)](?:[ \t]|\r?$)/;
const DISPLAY_MATH_OPEN = /^ {0,3}\$\$/;
// CommonMark HTML blocks of types 1-5 and display math run past blank lines until their closer.
const SPANNING_BLOCKS: ReadonlyArray<readonly [open: RegExp, close: RegExp]> = [
  [/^ {0,3}<(?:script|pre|style|textarea)(?=[\s>]|$)/i, /<\/(?:script|pre|style|textarea)>/i],
  // "<!-->" and "<!--->" close themselves, so the closer is searched from just after "<!".
  [/^ {0,3}<!(?=--)/, /-->/],
  [/^ {0,3}<\?/, /\?>/],
  [/^ {0,3}<!\[CDATA\[/, /\]\]>/],
  [/^ {0,3}<![A-Za-z]/, />/],
  [DISPLAY_MATH_OPEN, /\$\$/],
];

// CommonMark type-6 block tags start an HTML block even with trailing text or across lines.
const HTML_BLOCK_TAG =
  /^ {0,3}<\/?(?:address|article|aside|base|basefont|blockquote|body|caption|center|col|colgroup|dd|details|dialog|dir|div|dl|dt|fieldset|figcaption|figure|footer|form|frame|frameset|h[1-6]|head|header|hr|html|iframe|legend|li|link|main|menu|menuitem|nav|noframes|ol|optgroup|option|p|param|search|section|summary|table|tbody|td|tfoot|th|thead|title|tr|track|ul)(?=[ \t>]|\/>|\r?$)/i;
// CommonMark type-7 HTML blocks require a complete standalone open or closing tag on the line.
const HTML_STANDALONE_TAG =
  /^ {0,3}(?:<[A-Za-z][A-Za-z0-9-]*(?:[ \t]+[A-Za-z_:][A-Za-z0-9_.:-]*(?:[ \t]*=[ \t]*(?:[^ \t\r"'=<>`]+|'[^']*'|"[^"]*"))?)*[ \t]*\/?>|<\/[A-Za-z][A-Za-z0-9-]*[ \t]*>)[ \t]*\r?$/;
const BLANK_LINE = /^[ \t]*\r?$/;
const closesAtBlankLine = (next: string) => BLANK_LINE.test(next);

// A <details> or <table> wraps the Markdown between its tags, across the blank lines that end
// each HTML block, so a cut waits until the ones an HTML block opened are closed.
const WRAPPER_TAG = /<(\/?)(?:details|table)(?=[\s/>]|$)/gi;
// An end tag that never comes must not make one huge piece.
const WRAPPED_MAX_CHARS = 32 * 1024;
// A wrapper may start a piece of its own, so back-to-back wrappers stay apart.
const WRAPPER_START = /^<(?:details|table)(?=[\s>]|$)/i;

/** The line outside comments, and whether a comment is still open at its end. */
function withoutComments(line: string, inComment: boolean): [kept: string, inComment: boolean] {
  let kept = "";
  let at = 0;
  if (inComment) {
    const close = line.indexOf("-->");
    if (close === -1) return ["", true];
    at = close + 3;
  }
  for (let open = line.indexOf("<!--", at); open !== -1; open = line.indexOf("<!--", at)) {
    kept += line.slice(at, open);
    // `<!-->` and `<!--->` are whole comments.
    const short = /^<!---?>/.exec(line.slice(open, open + 6))?.[0].length;
    if (short !== undefined) {
      at = open + short;
      continue;
    }
    const close = line.indexOf("-->", open + 4);
    if (close === -1) return [kept, true];
    at = close + 3;
  }
  return [kept + line.slice(at), false];
}

/** The wrapper depth after `text`, and whether its last open wrapper closed on the way. */
function wrapperDepth(
  text: string,
  closersOnly: boolean,
  depth: number,
): { readonly depth: number; readonly closed: boolean } {
  let closed = false;
  for (const [, closing] of text.matchAll(WRAPPER_TAG)) {
    if (!closing) {
      if (!closersOnly) depth += 1;
    } else if (depth > 0) {
      depth -= 1;
      closed ||= depth === 0;
    }
  }
  return { depth, closed };
}

const COMMENT_START = /^ {0,3}<!--/;
const INDENTED_CODE = /^(?: {4}|\t)/;
// A backtick run is whole, so a span never starts inside a longer run.
const INLINE_CODE = /(?<!`)(`+)(?:[^`]|[^`][\s\S]*?[^`])\1(?!`)/g;

/** Returns how to recognize the end of a block that `line` opens, or null for none. */
function blockCloser(line: string): ((line: string) => boolean) | null {
  const fence = FENCE_OPEN.exec(line)?.[1];
  if (fence) {
    return (next) => {
      const match = FENCE_OPEN.exec(next);
      const closer = match?.[1];
      return (
        match !== null &&
        closer !== undefined &&
        closer[0] === fence[0] &&
        closer.length >= fence.length &&
        BLANK_LINE.test(next.slice(match[0].length))
      );
    };
  }
  for (const [open, close] of SPANNING_BLOCKS) {
    const opener = open.exec(line)?.[0];
    if (opener === undefined) continue;
    const rest = line.slice(opener.length);
    return close.test(rest) ? null : (next) => close.test(next);
  }
  // Other HTML blocks (CommonMark types 6 and 7) run until a blank line.
  return HTML_BLOCK_TAG.test(line) || HTML_STANDALONE_TAG.test(line) ? closesAtBlankLine : null;
}

export interface MarkdownChunk {
  readonly text: string;
  readonly lines: number;
}

// Footnotes are left alone so each piece does not grow a footnote section.
const LINK_DEFINITION_START = /^ {0,3}\[(?!\^)/;
const LINK_LABEL_MAX_CHARS = 999;
const LINK_TITLE_START = /^[ \t]*(["'(])/;
// Definitions past this are not repeated into every piece; their parse cost would outweigh links.
const SHARED_DEFINITIONS_MAX_CHARS = 16 * 1024;
const definitionParser = unified().use(remarkParse);
// A one-line definition; its parse would re-run on every append while a definition run streams.
const ONE_LINE_DEFINITION =
  /^ {0,3}\[((?:[^\][\\]|\\.)+)\]:[ \t]*(?:<[^<>\n]*>|([^\s<]\S*))([ \t]+(?:"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'|\((?:[^()\\]|\\.)*\)))?[ \t]*\r?$/;

/** The definition `lines[index]` holds alone, when no later line could extend or undo it. */
function readOneLineDefinition(lines: readonly string[], index: number) {
  const line = lines[index]!;
  const match = ONE_LINE_DEFINITION.exec(line);
  if (!match) return;
  const [, label = "", bareDestination, title] = match;
  if (label.length > LINK_LABEL_MAX_CHARS || BLANK_LINE.test(label)) return;
  // Without a title, a next line that opens one may still belong to this definition.
  const next = lines[index + 1];
  if (title === undefined && (next === undefined || LINK_TITLE_START.test(next))) return;
  if (bareDestination !== undefined) {
    let depth = 0;
    for (let at = 0; at < bareDestination.length; at++) {
      const char = bareDestination[at];
      if (char === "\\") at++;
      else if (char === "(") depth++;
      else if (char === ")" && --depth < 0) return;
    }
    if (depth !== 0) return;
  }
  return {
    text: line.slice(line.indexOf("["), line.endsWith("\r") ? -1 : undefined),
    end: index,
    identifier: label
      .replace(/[\t\n\r ]+/g, " ")
      .replace(/^ | $/g, "")
      .toLowerCase()
      .toUpperCase()
      .toLowerCase(),
  };
}

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
  /** Labels whose first definition was too large to share, so a later one must not win. */
  readonly omittedDefinitions: ReadonlySet<string>;
}

function hasDefinitionLabel(lines: readonly string[], start: number): boolean {
  const opener = LINK_DEFINITION_START.exec(lines[start]!);
  if (!opener) return false;

  let column = opener[0].length;
  let labelChars = 0;
  for (let index = start; index < lines.length; index++) {
    const line = lines[index]!;
    if (BLANK_LINE.test(line)) return false;
    const end = line.endsWith("\r") ? line.length - 1 : line.length;
    for (; column < end && labelChars <= LINK_LABEL_MAX_CHARS; column++, labelChars++) {
      const char = line[column];
      if (char === "]") return line[column + 1] === ":";
      if (char === "[") return false;
      if (char === "\\" && /[\\[\]]/.test(line[column + 1] ?? "")) {
        column++;
        labelChars++;
      }
    }
    if (labelChars > LINK_LABEL_MAX_CHARS) return false;
    column = 0;
  }
  return false;
}

function endsDefinitionRun(line: string) {
  // The preview treats display math as a block even when plain Remark would consume its opener.
  return (
    BLANK_LINE.test(line) ||
    ATX_HEADING.test(line) ||
    THEMATIC_BREAK.test(line) ||
    FENCE_OPEN.test(line) ||
    DISPLAY_MATH_OPEN.test(line)
  );
}

function findTitleEnd(lines: readonly string[], start: number, opener: RegExpExecArray) {
  const closer = opener[1] === "(" ? ")" : opener[1];
  let column = opener[0].length;
  for (let index = start; index < lines.length; index++) {
    const line = lines[index]!;
    if (endsDefinitionRun(line)) return;
    for (; column < line.length; column++) {
      const char = line[column];
      if (char === closer) return BLANK_LINE.test(line.slice(column + 1)) ? index : undefined;
      if (char === "\\" && (line[column + 1] === closer || line[column + 1] === "\\")) column++;
    }
    column = 0;
  }
}

function readLinkDefinitions(lines: readonly string[], start: number) {
  const definitions = new Map<number, { text?: string; end: number; identifier: string }>();
  let chars = lines[start]!.length;
  if (chars >= SHARED_DEFINITIONS_MAX_CHARS) return definitions;
  let end = start + 1;
  let capped = false;
  for (; end < lines.length; end++) {
    const line = lines[end]!;
    if (endsDefinitionRun(line)) break;
    chars += line.length + 1;
    if (chars >= SHARED_DEFINITIONS_MAX_CHARS) {
      capped = true;
      break;
    }
  }

  const text = lines.slice(start, end).join("\n");
  let partialTitleStart: number | undefined;
  for (const node of definitionParser.parse(text).children) {
    if (node.type !== "definition" || !node.position) break;
    const { start: from, end: to } = node.position;
    const firstLine = start + from.line - 1;
    if (!LINK_DEFINITION_START.test(lines[firstLine]!)) break;
    const lastLine = start + to.line - 1;
    definitions.set(firstLine, {
      text: text.slice(from.offset, to.offset),
      end: lastLine,
      identifier: node.identifier,
    });
    partialTitleStart =
      capped && node.title == null && LINK_TITLE_START.test(lines[lastLine + 1] ?? "")
        ? firstLine
        : undefined;
  }
  if (partialTitleStart !== undefined) {
    // Retry at the candidate's label before looking beyond the batch limit.
    if (partialTitleStart !== start) {
      definitions.delete(partialTitleStart);
    } else {
      const titleStart = definitions.get(start)!.end + 1;
      const opener = LINK_TITLE_START.exec(lines[titleStart]!)!;
      const titleEnd = findTitleEnd(lines, titleStart, opener);
      if (titleEnd !== undefined && titleEnd >= end) {
        const full = definitionParser.parse(lines.slice(start, titleEnd + 1).join("\n"))
          .children[0];
        if (full?.type === "definition" && full.title != null) {
          definitions.set(start, { end: titleEnd, identifier: full.identifier });
        }
      }
    }
  }
  return definitions;
}

export interface SplitMarkdown {
  readonly chunks: readonly MarkdownChunk[];
  readonly definitions: string;
  /** Absent while the text is one piece. */
  readonly resume?: MarkdownSplitResume;
}

/**
 * Cuts Markdown into pieces of about `targetChars` at column-0 block starts; frontmatter stays in
 * the first piece. Unless the text is `complete`, its last line may still grow, so it is never a
 * cut. Link definitions are shared across pieces. Footnotes resolve only within a piece, so a file
 * under the large-chunk cap that defines one stays whole. Duplicate heading slugs restart per piece.
 */
export function splitMarkdown(
  text: string,
  targetChars = CHUNK_TARGET_CHARS,
  complete = false,
): SplitMarkdown {
  if (keepsWhole(text)) return wholeMarkdown(text);
  return scanMarkdown(text, targetChars, complete, [], "", {
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
    omittedDefinitions: new Set(),
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
  complete = false,
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
    return splitMarkdown(text, targetChars, complete);
  }
  // Frontmatter is settled once its closer or line cap lies before the last cut.
  const frontmatter =
    resume.frontmatter > 0 || resume.linesBefore > FRONTMATTER_MAX_LINES
      ? resume.frontmatter
      : leadingFrontmatterLength(text);
  if (frontmatter !== resume.frontmatter) return splitMarkdown(text, targetChars, complete);
  return scanMarkdown(
    text,
    targetChars,
    complete,
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
  complete: boolean,
  chunks: MarkdownChunk[],
  definitionsBefore: string,
  from: MarkdownSplitResume,
): SplitMarkdown {
  const { frontmatter } = from;
  const definitions = definitionsBefore ? [definitionsBefore] : [];
  let definitionsLength = definitionsBefore.length;
  let { afterBlankLine, afterHeading, definitionChars } = from;
  const omittedDefinitions = new Set(from.omittedDefinitions);
  // The local index of the last line of the latest definition; -1 makes line 0 follow one.
  let definitionEnd = from.afterDefinition ? -1 : -2;
  let pendingDefinitions: ReturnType<typeof readLinkDefinitions> | undefined;
  let resume = from.start > 0 ? from : undefined;
  let lines: string[] = [];
  let chars = 0;
  let closes: ((line: string) => boolean) | null = null;
  let openedFence = false;
  // A cut needs no open wrapper, so a resumed scan starts with none.
  let openWrappers = 0;
  let wrapperStart = 0;
  // The last cut an open wrapper held back, which a piece too large to render falls back to.
  let held: {
    line: number;
    index: number;
    definitionCount: number;
    resume: MarkdownSplitResume;
  } | null = null;
  // The kind of line whose comment is still open; it does not continue into another kind.
  let commentIn: "html" | "text" | null = null;
  let inCommentBlock = false;
  let offset = from.start;
  let linesBefore = from.linesBefore;
  const textLines = (from.start > 0 ? text.slice(from.start) : text).split("\n");
  // Lines from this index on may still grow, so they are not cuts.
  const wholeLines = complete ? textLines.length : textLines.length - 1;
  for (let index = 0; index < textLines.length; index++) {
    const line = textLines[index] ?? "";
    const inFrontmatter = offset < frontmatter;
    const blockBefore = closes;
    if (inFrontmatter) {
      // Masked to blank lines, so nothing inside is a cut, a block or a definition.
      openedFence = false;
    } else if (closes !== null) {
      if (closes(line)) closes = null;
    } else if (index > definitionEnd) {
      // The last line may still be growing; "1" can become "1. item" and "#" "#tag".
      if (openWrappers > 0 && chars - wrapperStart >= WRAPPED_MAX_CHARS) openWrappers = 0;
      const cutPoint =
        lines.length > 0 &&
        index < wholeLines &&
        chars >= targetChars &&
        ((line.startsWith("#") && ATX_HEADING.test(line)) ||
          (afterBlankLine &&
            ((BLOCK_START.test(line) && !ORDERED_ITEM.test(line)) || WRAPPER_START.test(line))));
      if (cutPoint) {
        const at: MarkdownSplitResume = {
          targetChars,
          textLength: 0,
          start: offset,
          linesBefore,
          frontmatter,
          definitions: definitionsLength,
          definitionChars,
          afterBlankLine,
          afterDefinition: index === definitionEnd + 1,
          afterHeading,
          omittedDefinitions: new Set(omittedDefinitions),
        };
        if (openWrappers === 0) {
          chunks.push({ text: lines.join("\n"), lines: lines.length });
          resume = at;
          lines = [];
          chars = 0;
          held = null;
        } else {
          held = { line: lines.length, index, definitionCount: definitions.length, resume: at };
        }
      }
      closes = blockCloser(line);
      // Two or more spaces may put a fence in a list item, which this does not track.
      openedFence = /^ ?[`~]/.test(line) && FENCE_OPEN.test(line);
      // A definition cannot interrupt a paragraph, so it follows a blank line, a heading,
      // a closing code fence or another definition.
      if (offset === 0 || afterBlankLine || index === definitionEnd + 1 || afterHeading) {
        let definition = pendingDefinitions?.get(index);
        if (!definition && definitionChars < SHARED_DEFINITIONS_MAX_CHARS) {
          definition = readOneLineDefinition(textLines, index);
          if (!definition && hasDefinitionLabel(textLines, index)) {
            pendingDefinitions = readLinkDefinitions(textLines, index);
            definition = pendingDefinitions.get(index);
          }
        }
        if (definition) {
          definitionEnd = definition.end;
          if (!omittedDefinitions.has(definition.identifier)) {
            if (
              definition.text !== undefined &&
              definitionChars + definition.text.length < SHARED_DEFINITIONS_MAX_CHARS
            ) {
              definitions.push(definition.text);
              definitionsLength += (definitionsLength > 0 ? 1 : 0) + definition.text.length;
              definitionChars += definition.text.length + 1;
            } else {
              omittedDefinitions.add(definition.identifier);
            }
          }
        }
      }
    }
    // Tags outside comments open and close wrappers on HTML block and comment block lines; a
    // closer may also end a paragraph line, outside code spans. Fences, indented code and other
    // spanning blocks, such as a one-line <pre>, count nothing.
    if (!inFrontmatter) {
      const opensComment = blockBefore === null && COMMENT_START.test(line);
      const htmlLine = closes === closesAtBlankLine || inCommentBlock || opensComment;
      const textLine =
        !htmlLine &&
        blockBefore === null &&
        closes === null &&
        !SPANNING_BLOCKS.some(([open]) => open.test(line)) &&
        !INDENTED_CODE.test(line);
      const kind = htmlLine ? "html" : textLine && !ATX_HEADING.test(line) ? "text" : null;
      if (commentIn !== kind) commentIn = null;
      if (htmlLine || textLine) {
        const [kept, stillInComment] = withoutComments(
          htmlLine ? line : line.replace(INLINE_CODE, ""),
          commentIn !== null,
        );
        const wrappers = wrapperDepth(kept, !htmlLine, openWrappers);
        // Only a wrapper open before this line can hold a cut; one opened and closed on it cannot.
        const closedOpen = openWrappers > 0 && wrappers.closed;
        if ((openWrappers === 0 || closedOpen) && wrappers.depth > 0) wrapperStart = chars;
        openWrappers = wrappers.depth;
        // A cut held inside a wrapper that has since closed would split it.
        if (closedOpen) held = null;
        commentIn = stillInComment ? kind : null;
      }
      inCommentBlock = (inCommentBlock || opensComment) && closes !== null;
      // A comment left open runs to the end of its paragraph or HTML block.
      if (BLANK_LINE.test(line) && !inCommentBlock) commentIn = null;
    }
    lines.push(line);
    chars += line.length + 1;
    // Cuts at the held cut and rescans from it in place, as extendMarkdownSplit would resume
    // there. A growing last line may still become a cut itself, so it waits for its line break.
    if (held !== null && index < wholeLines && chars - 1 > LARGE_MARKDOWN_CHUNK_MAX_CHARS) {
      chunks.push({ text: lines.slice(0, held.line).join("\n"), lines: held.line });
      ({ afterBlankLine, afterHeading, definitionChars } = held.resume);
      definitionEnd = held.resume.afterDefinition ? held.index - 1 : -2;
      omittedDefinitions.clear();
      for (const label of held.resume.omittedDefinitions) omittedDefinitions.add(label);
      resume = held.resume;
      offset = held.resume.start;
      linesBefore = held.resume.linesBefore;
      definitions.length = held.definitionCount;
      definitionsLength = held.resume.definitions;
      lines = [];
      chars = 0;
      closes = null;
      openedFence = false;
      openWrappers = 0;
      wrapperStart = 0;
      commentIn = null;
      inCommentBlock = false;
      index = held.index - 1;
      held = null;
      continue;
    }
    afterBlankLine = inFrontmatter || BLANK_LINE.test(line);
    afterHeading =
      !inFrontmatter &&
      closes === null &&
      (openedFence || ATX_HEADING.test(line) || THEMATIC_BREAK.test(line));
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

/** Prepends shared definitions in document order; source offsets shift by the prefix length. */
export function withMarkdownDefinitions(text: string, definitions: string): string {
  if (!definitions) return text;
  const masked = maskMarkdownFrontmatter(text);
  return masked.startsWith("\uFEFF")
    ? `\uFEFF${definitions}\n\n${masked.slice(1)}`
    : `${definitions}\n\n${masked}`;
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
