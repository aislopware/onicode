import { decodeString } from "micromark-util-decode-string";
import remarkGfm from "remark-gfm";
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
const LIST_ITEM = /^( {0,3}(?:[-*+]|\d{1,9}[.)]))([ \t]+|(?=\r?$))/;
// Further markers on an item's first line open nested items that start on the same line.
const NESTED_LIST_MARKERS = /^(?:(?:[-*+]|\d{1,9}[.)])(?:[ \t]+|(?=\r?$)))+/;
const BLOCK_QUOTE = /^ {0,3}>/;
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
const flowParser = unified().use(remarkParse).use(remarkGfm).freeze();
const HTML_FLOW_MAX_CHARS = 8 * 1024;
// Parsing is far slower than the line scan, so a whole document shares one budget.
const HTML_PARSE_BUDGET_CHARS = 16 * 1024;

function indentationColumn(whitespace: string, column = 0): number {
  for (const character of whitespace) {
    column += character === "\t" ? 4 - (column % 4) : 1;
  }
  return column;
}
// A streamed reply rescans its last piece on every append; only the growing flow is new text.
const htmlFlowStarts = new Map<string, readonly number[]>();
const HTML_FLOW_CACHE_ENTRIES = 32;

/** Lines, from 0, where `flowText` starts an HTML node. */
function flowHtmlStarts(flowText: string): readonly number[] {
  const cached = htmlFlowStarts.get(flowText);
  if (cached) return cached;
  const starts: number[] = [];
  for (const node of flowParser.parse(flowText).children) {
    if (node.type === "html" && node.position) starts.push(node.position.start.line - 1);
  }
  if (htmlFlowStarts.size >= HTML_FLOW_CACHE_ENTRIES) {
    htmlFlowStarts.delete(htmlFlowStarts.keys().next().value!);
  }
  htmlFlowStarts.set(flowText, starts);
  return starts;
}

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

/** Whether `text` opens a block that ends a paragraph and is not one itself. */
function opensNonParagraph(text: string): boolean {
  return (
    ATX_HEADING.test(text) ||
    THEMATIC_BREAK.test(text) ||
    BLOCK_QUOTE.test(text) ||
    FENCE_OPEN.test(text) ||
    HTML_BLOCK_TAG.test(text) ||
    SPANNING_BLOCKS.some(([open]) => open.test(text))
  );
}

/** Null means the block ends on this line. Undefined means no block opens. */
function blockCloser(line: string): ((line: string) => boolean) | null | undefined {
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
  return HTML_BLOCK_TAG.test(line) ? closesAtBlankLine : undefined;
}

export interface MarkdownChunk {
  readonly text: string;
  readonly lines: number;
  /** The line break after this piece, when there is one. */
  readonly separator?: "\n" | "\r";
  /** Footnote labels this piece references, defined or not, when footnotes are shared. */
  readonly footnoteRefs?: readonly string[];
}

// Footnotes are collected separately so body pieces do not each render their own footnote section.
const LINK_DEFINITION_START = /^ {0,3}\[(?!\^)/;
const LINK_LABEL_MAX_CHARS = 999;
const LINK_TITLE_START = /^[ \t]*(["'(])/;
// Definitions past this are not repeated into every piece; their parse cost would outweigh links.
const SHARED_DEFINITIONS_MAX_CHARS = 16 * 1024;
const SHARED_FOOTNOTES_MAX_CHARS = 64 * 1024;
const FOOTNOTE_DEFINITION_START = /^ {0,3}\[\^([^\s[\]]+)\]:/;
const FOOTNOTE_CONTINUATION_INDENT = /^(?: {4}|\t)/;
const LIST_ITEM_START = /^ {0,3}(?:[-*+]|\d{1,9}[.)])(?:[ \t]|\r?$)/;
const FOOTNOTE_BLOCK_INTERRUPT = /^ {0,3}(?:>|[-*+](?:[ \t]|\r?$)|\d{1,9}[.)](?:[ \t]|\r?$))/;
const definitionParser = unified().use(remarkParse);
// A one-line definition; its parse would re-run on every append while a definition run streams.
const ONE_LINE_DEFINITION =
  /^ {0,3}\[(?!\^)((?:[^\][\\]|\\.)+)\]:[ \t]*(?:<(?:[^<>\\\n]|\\.)*>|([^\s<\x00-\x1f\x7f][^\s\x00-\x1f\x7f]*))([ \t]+(?:"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'|\((?:[^()\\]|\\.)*\)))?[ \t]*\r?$/;

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

// With footnotes kept whole, a footnote resolves only inside its own piece, so a file that
// defines one stays whole.
const FOOTNOTE_DEFINITION = /^ {0,3}\[\^[^\]\n]+\]:/m;

/**
 * `"shared"` resolves footnotes across pieces through `SplitMarkdown.footnotes`; `"whole"` keeps a
 * file that defines one as one piece, for renderers without a shared footnotes section.
 */
export type MarkdownFootnoteMode = "shared" | "whole";

interface FootnoteDefinition {
  readonly label: string;
  readonly text: string;
  readonly offset: number;
}

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
  /** What is left of the document-wide budget for parsing ambiguous HTML flows. */
  readonly parseBudget: number;
  readonly footnoteMode: MarkdownFootnoteMode;
  /** How many footnote definitions were captured before `start`. */
  readonly footnoteDefinitions: number;
  readonly footnoteChars: number;
  readonly footnoteCapExceeded: boolean;
  readonly inFootnoteList: boolean;
  /** Every captured footnote definition; set on the returned value. */
  readonly capturedFootnotes?: readonly FootnoteDefinition[];
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

function isSpanningBlockOpen(line: string): boolean {
  const firstChar = line.trimStart()[0];
  if (firstChar !== "<" && firstChar !== "$") return false;
  return SPANNING_BLOCKS.some(([open]) => open.test(line));
}

function interruptsFootnoteDefinition(line: string): boolean {
  return (
    (line.includes("[^") && FOOTNOTE_DEFINITION_START.test(line)) ||
    ATX_HEADING.test(line) ||
    THEMATIC_BREAK.test(line) ||
    FENCE_OPEN.test(line) ||
    FOOTNOTE_BLOCK_INTERRUPT.test(line) ||
    HTML_BLOCK_TAG.test(line) ||
    HTML_STANDALONE_TAG.test(line) ||
    isSpanningBlockOpen(line)
  );
}

const FOOTNOTE_INDENT_AFTER_QUOTE = /^[ \t]? {0,3}$/;

/** End of a link destination's `)` from `start`, or -1 for titles and nested parentheses. */
function linkDestinationEnd(text: string, start: number): number {
  // Every scan stops at the next `(` or `<`, so the scans of one paragraph do not overlap.
  let j = start;
  if (text[j] === "<") {
    for (j++; j < text.length && text[j] !== ">"; j++) {
      if (text[j] === "<" || text[j] === "\n") return -1;
      if (text[j] === "\\") j++;
    }
    return text[j + 1] === ")" ? j + 1 : -1;
  }
  for (; j < text.length; j++) {
    const c = text[j]!;
    if (c === ")") return j;
    if (c === "(" || c === "<" || /\s/.test(c)) return -1;
    if (c === "\\") j++;
  }
  return -1;
}

/**
 * Finds footnote references in body prose, skipping escapes, code spans, inline math, HTML comments
 * and link destinations; null `definedKeys` keeps every label.
 */
function extractFootnoteRefs(text: string, definedKeys: ReadonlySet<string> | null): string[] {
  if (!text.includes("[^")) return [];
  const refs: string[] = [];
  // Scans that found no close mark where later ones would fail too, so no text is rescanned.
  let mathUnclosedUntil = -1;
  let commentUnclosed = false;
  // Unmatched `[`; a `](` without one is literal text, not the start of a destination.
  let openBrackets = 0;
  // The current line's leading `>` markers and indentation, tracked forward.
  let scanned = 0;
  let prefixEnd = 0;
  let quoteEnd = 0;
  const isAtLineStart = (i: number) => {
    for (; scanned < i; scanned++) {
      const c = text[scanned];
      if (c === "\n") prefixEnd = quoteEnd = scanned + 1;
      else if (prefixEnd === scanned && (c === " " || c === "\t" || c === ">")) {
        prefixEnd++;
        if (c === ">") quoteEnd = prefixEnd;
      }
    }
    return prefixEnd === i && FOOTNOTE_INDENT_AFTER_QUOTE.test(text.slice(quoteEnd, i));
  };
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (ch === "\\") {
      i++;
      continue;
    }
    if (ch === "`") {
      let ticks = 1;
      while (text[i + ticks] === "`") ticks++;
      let j = i + ticks;
      let closed = -1;
      while (j < text.length) {
        if (text[j] === "`") {
          let run = 1;
          while (text[j + run] === "`") run++;
          if (run === ticks) {
            closed = j + run - 1;
            break;
          }
          j += run;
          continue;
        }
        j++;
      }
      if (closed !== -1) i = closed;
      else i += ticks - 1;
      continue;
    }
    if (ch === "$" && text[i + 1] !== "$" && i >= mathUnclosedUntil) {
      const next = text[i + 1];
      if (next !== undefined && next !== " " && next !== "\t" && next !== "\n") {
        let j = i + 1;
        let closed = -1;
        while (j < text.length && text[j] !== "\n") {
          if (text[j] === "\\") {
            j += 2;
            continue;
          }
          if (
            text[j] === "$" &&
            text[j + 1] !== "$" &&
            text[j - 1] !== " " &&
            text[j - 1] !== "\t"
          ) {
            closed = j;
            break;
          }
          j++;
        }
        if (closed !== -1) {
          i = closed;
          continue;
        }
        mathUnclosedUntil = j;
      }
    }
    if (ch === "<" && !commentUnclosed && text.startsWith("<!--", i)) {
      const close = text.indexOf("-->", i + 4);
      if (close !== -1) {
        i = close + 2;
        continue;
      }
      commentUnclosed = true;
    }
    if (ch === "[" && text[i + 1] !== "^") openBrackets++;
    if (ch === "]" && openBrackets > 0) {
      openBrackets--;
      const end = text[i + 1] === "(" ? linkDestinationEnd(text, i + 2) : -1;
      if (end !== -1) {
        i = end;
        continue;
      }
    }
    if (ch === "[" && text[i + 1] === "^") {
      let j = i + 2;
      while (j < text.length) {
        const c = text[j]!;
        if (c === "]" || c === "[" || /\s/.test(c)) break;
        j++;
      }
      if (j > i + 2 && text[j] === "]") {
        const isDefAtLineStart = text[j + 1] === ":" && isAtLineStart(i);
        if (!isDefAtLineStart) {
          const label = text.slice(i + 2, j);
          if (definedKeys === null || definedKeys.has(label.toLowerCase())) refs.push(label);
        }
        i = j;
      }
    }
  }
  return refs;
}

export interface MarkdownFootnotes {
  /** One-line stub definitions (`[^id]: .`) so body chunks resolve footnote references. */
  readonly stubs: string;
  /** Real footnote definitions in document order for the trailing footnotes section. */
  readonly definitions: string;
  /** Where each of `definitions` sits in the file, so edits made in the section reach the file. */
  readonly definitionSpans: readonly (readonly [offset: number, length: number])[];
  /** Referenced footnote labels in document order for each chunk (empty when a chunk has none). */
  readonly chunkRefs: readonly (readonly string[])[];
}

export interface SplitMarkdown {
  readonly chunks: readonly MarkdownChunk[];
  readonly definitions: string;
  readonly footnotes?: MarkdownFootnotes | undefined;
  /** Absent while the text is one piece. */
  readonly resume?: MarkdownSplitResume;
}

/**
 * Cuts Markdown into pieces of about `targetChars` at column-0 block starts; frontmatter stays in
 * the first piece. Unless the text is `complete`, its last line may still grow, so it is never a
 * cut. Link definitions are shared across pieces, and footnotes per `footnoteMode`. Duplicate
 * heading slugs restart per piece.
 */
export function splitMarkdown(
  text: string,
  targetChars = CHUNK_TARGET_CHARS,
  complete = false,
  footnoteMode: MarkdownFootnoteMode = "shared",
): SplitMarkdown {
  if (keepsWhole(text, footnoteMode)) return wholeMarkdown(text);
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
    parseBudget: HTML_PARSE_BUDGET_CHARS,
    footnoteMode,
    footnoteDefinitions: 0,
    footnoteChars: 0,
    footnoteCapExceeded: false,
    inFootnoteList: false,
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
  footnoteMode: MarkdownFootnoteMode = "shared",
): SplitMarkdown {
  const resume = previous.resume;
  // Callers only append; these checks catch a shorter text, another piece size, or a lost resume.
  if (
    resume === undefined ||
    resume.targetChars !== targetChars ||
    resume.footnoteMode !== footnoteMode ||
    text.length < resume.textLength ||
    !endsLineBreak(text, resume.start) ||
    keepsWhole(text, footnoteMode)
  ) {
    return splitMarkdown(text, targetChars, complete, footnoteMode);
  }
  // Frontmatter is settled once its closer or line cap lies before the last cut.
  const frontmatter =
    resume.frontmatter > 0 || resume.linesBefore > FRONTMATTER_MAX_LINES
      ? resume.frontmatter
      : leadingFrontmatterLength(text);
  if (frontmatter !== resume.frontmatter) {
    return splitMarkdown(text, targetChars, complete, footnoteMode);
  }
  return scanMarkdown(
    text,
    targetChars,
    complete,
    previous.chunks.slice(0, -1),
    previous.definitions.slice(0, resume.definitions),
    resume,
  );
}

/** Whether a line break ends right before `start`, and a CR there is not half of a CRLF. */
function endsLineBreak(text: string, start: number): boolean {
  const code = text.charCodeAt(start - 1);
  return code === 10 || (code === 13 && text.charCodeAt(start) !== 10);
}

function keepsWhole(text: string, footnoteMode: MarkdownFootnoteMode): boolean {
  return (
    footnoteMode === "whole" &&
    text.length <= LARGE_MARKDOWN_CHUNK_MAX_CHARS &&
    FOOTNOTE_DEFINITION.test(text)
  );
}

// A bare CR ends a line too; CRLF lines keep their `\r`, which the patterns here allow.
function nextLineBreak(text: string, from: number): number {
  for (let at = from; at < text.length; at++) {
    const code = text.charCodeAt(at);
    if (code === 10 || (code === 13 && text.charCodeAt(at + 1) !== 10)) return at;
  }
  return text.length;
}

/** The lines of `text` from `start`, each ending at a one-character line break. */
function splitLines(text: string, start: number): string[] {
  const body = start > 0 ? text.slice(start) : text;
  if (!body.includes("\r")) return body.split("\n");
  const lines: string[] = [];
  for (let at = 0; at <= body.length;) {
    const end = nextLineBreak(body, at);
    lines.push(body.slice(at, end));
    at = end + 1;
  }
  return lines;
}

/** The piece from `start` to the line break before `next`, which is the text's end past it. */
function markdownPiece(text: string, start: number, next: number, lines: number): MarkdownChunk {
  if (next > text.length) return { text: text.slice(start), lines };
  const separator = text[next - 1];
  const piece = { text: text.slice(start, next - 1), lines };
  return separator === "\n" || separator === "\r" ? { ...piece, separator } : piece;
}

/** The source of split pieces, with each piece's own line break between them. */
export function joinMarkdownChunks(chunks: readonly MarkdownChunk[]): string {
  return chunks
    .map((chunk, index) =>
      index + 1 < chunks.length ? `${chunk.text}${chunk.separator ?? "\n"}` : chunk.text,
    )
    .join("");
}

function wholeMarkdown(text: string): SplitMarkdown {
  return { chunks: [{ text, lines: splitLines(text, 0).length }], definitions: "" };
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
  // A cut line always resets the flow, so a resumed scan starts a fresh one.
  let ambiguousHtml = false;
  let flowStart = 0;
  let parsedThrough = 0;
  let htmlStarts: Set<number> | undefined;
  let parseBudget = from.parseBudget;
  const resetFlow = (start: number) => {
    flowStart = start;
    parsedThrough = start;
    htmlStarts = undefined;
  };
  let resume = from.start > 0 ? from : undefined;
  let pieceStart = from.start;
  let pieceLines = 0;
  let chars = 0;
  let closes: ((line: string) => boolean) | null = null;
  let openedFence = false;
  // A cut line is outside every list and paragraph, so a resumed scan starts with neither.
  // The outermost list item's content column; a line indented less is outside every item.
  let listContentIndent = Infinity;
  let listMarker = "";
  let emptyListItem = false;
  let paragraphOpen = false;
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
  const textLines = splitLines(text, from.start);
  // Lines from this index on may still grow, so they are not cuts.
  const wholeLines = complete ? textLines.length : textLines.length - 1;
  // A cut line ends any footnote definition and indented code, so a resumed scan is outside both.
  const sharedFootnotes = from.footnoteMode === "shared";
  const capturedFootnotes = from.capturedFootnotes?.slice(0, from.footnoteDefinitions) ?? [];
  const seenFootnoteKeys = new Set(capturedFootnotes.map(({ label }) => label.toLowerCase()));
  let { footnoteChars, footnoteCapExceeded, inFootnoteList } = from;
  let inIndentedCode = false;
  let inFootnoteDef = false;
  let activeFootnote: { label: string; offset: number; lines: string[] } | null = null;
  let pendingFootnoteBlanks: string[] = [];
  // Prose lines, whose `[^label]` are references; set per scanned line.
  const bodyLines = sharedFootnotes ? new Uint8Array(textLines.length) : null;
  let pieceStartIndex = 0;
  const finishFootnoteDef = () => {
    if (activeFootnote !== null && !footnoteCapExceeded) {
      const block = activeFootnote.lines.join("\n").trimEnd();
      if (footnoteChars + block.length < SHARED_FOOTNOTES_MAX_CHARS) {
        capturedFootnotes.push({
          label: activeFootnote.label,
          text: block,
          offset: activeFootnote.offset,
        });
        footnoteChars += block.length + 2;
      } else {
        footnoteCapExceeded = true;
      }
    }
    inFootnoteDef = false;
    activeFootnote = null;
    pendingFootnoteBlanks = [];
  };
  // A blank line or an indented one stays in an open footnote; another line ends it.
  const continueFootnote = (line: string): "kept" | "ended" | "interrupted" => {
    if (BLANK_LINE.test(line)) {
      pendingFootnoteBlanks.push(line);
      return "kept";
    }
    if (pendingFootnoteBlanks.length > 0) {
      if (!FOOTNOTE_CONTINUATION_INDENT.test(line)) {
        finishFootnoteDef();
        return "ended";
      }
      activeFootnote?.lines.push(...pendingFootnoteBlanks, line);
      pendingFootnoteBlanks = [];
      return "kept";
    }
    if (FOOTNOTE_CONTINUATION_INDENT.test(line) || !interruptsFootnoteDefinition(line)) {
      activeFootnote?.lines.push(line);
      return "kept";
    }
    finishFootnoteDef();
    return "interrupted";
  };
  const withFootnoteRefs = (piece: MarkdownChunk, start: number, end: number): MarkdownChunk => {
    if (bodyLines === null || !piece.text.includes("[^")) return piece;
    const refs: string[] = [];
    let block: string[] = [];
    for (let at = start; at <= end; at++) {
      if (at < end && bodyLines[at] === 1) {
        block.push(textLines[at]!);
      } else if (block.length > 0) {
        refs.push(...extractFootnoteRefs(block.join("\n"), null));
        block = [];
      }
    }
    return refs.length > 0 ? { ...piece, footnoteRefs: refs } : piece;
  };
  for (let index = 0; index < textLines.length; index++) {
    const line = textLines[index] ?? "";
    const inFrontmatter = offset < frontmatter;
    const blockBefore = closes;
    const blank = BLANK_LINE.test(line);
    const whitespace = /^[ \t]*/.exec(line)![0];
    const indent = indentationColumn(whitespace);
    if (emptyListItem && (blank || indent < listContentIndent)) listContentIndent = Infinity;
    emptyListItem = false;
    const dedented = indent < listContentIndent;
    let handledAsFootnote = false;
    let interruptedFootnote = false;
    if (bodyLines !== null) bodyLines[index] = 0;
    if (inFootnoteDef && !inFrontmatter && closes === null && index > definitionEnd) {
      const step = continueFootnote(line);
      handledAsFootnote = step === "kept";
      interruptedFootnote = step === "interrupted";
    }
    if (inFrontmatter) {
      // Masked to blank lines, so nothing inside is a cut, a block or a definition.
      openedFence = false;
    } else if (closes !== null) {
      if (closes(line)) {
        closes = null;
        resetFlow(index + 1);
      }
    } else if (index > definitionEnd && !handledAsFootnote) {
      // The last line may still be growing; "1" can become "1. item" and "#" "#tag".
      if (openWrappers > 0 && chars - wrapperStart >= WRAPPED_MAX_CHARS) openWrappers = 0;
      const cutPoint =
        !ambiguousHtml &&
        pieceLines > 0 &&
        index < wholeLines &&
        chars >= targetChars &&
        ((line.startsWith("#") && ATX_HEADING.test(line)) ||
          (afterBlankLine &&
            ((BLOCK_START.test(line) && !LIST_ITEM.test(line)) || WRAPPER_START.test(line))));
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
          parseBudget,
          footnoteMode: from.footnoteMode,
          footnoteDefinitions: capturedFootnotes.length,
          footnoteChars,
          footnoteCapExceeded,
          inFootnoteList,
        };
        if (openWrappers === 0) {
          chunks.push(
            withFootnoteRefs(
              markdownPiece(text, pieceStart, offset, pieceLines),
              pieceStartIndex,
              index,
            ),
          );
          resume = at;
          pieceStart = offset;
          pieceStartIndex = index;
          pieceLines = 0;
          chars = 0;
          held = null;
        } else {
          held = { line: pieceLines, index, definitionCount: definitions.length, resume: at };
        }
      }
      if (bodyLines !== null && !blank) {
        if (!FOOTNOTE_CONTINUATION_INDENT.test(line)) {
          inIndentedCode = false;
          inFootnoteList = LIST_ITEM_START.test(line) || (inFootnoteList && !afterBlankLine);
        } else if (!inFootnoteList && (offset === 0 || afterBlankLine || afterHeading)) {
          inIndentedCode = true;
        }
      }
      const footnoteStart =
        bodyLines !== null && line.includes("[^") ? FOOTNOTE_DEFINITION_START.exec(line) : null;
      if (footnoteStart) {
        const label = footnoteStart[1]!;
        const key = label.toLowerCase();
        inFootnoteDef = true;
        activeFootnote = null;
        if (!seenFootnoteKeys.has(key)) {
          seenFootnoteKeys.add(key);
          activeFootnote = { label, offset, lines: [line] };
        }
        pendingFootnoteBlanks = [];
        openedFence = false;
        handledAsFootnote = true;
      } else {
        const closer = blockCloser(line);
        closes = closer ?? null;
        if (closer === null || ATX_HEADING.test(line) || THEMATIC_BREAK.test(line)) {
          resetFlow(index + 1);
        } else if (closer === undefined && HTML_STANDALONE_TAG.test(line)) {
          // Type-7 HTML cannot interrupt prose. Keep ambiguous tags beyond the parse
          // budget with their following lines, as a cut could expose literal HTML as Markdown.
          if (htmlStarts === undefined && index > flowStart) {
            let end = flowStart;
            let flowChars = 0;
            const maxChars = Math.min(HTML_FLOW_MAX_CHARS, parseBudget);
            for (; end < textLines.length; end++) {
              const next = textLines[end]!;
              flowChars += next.length + 1;
              if (flowChars > maxChars || BLANK_LINE.test(next)) break;
              // Include the boundary line, which can end a lazy container continuation.
              if (
                ATX_HEADING.test(next) ||
                THEMATIC_BREAK.test(next) ||
                blockCloser(next) !== undefined
              ) {
                end++;
                break;
              }
            }
            parsedThrough = end;
            htmlStarts = new Set();
            if (index < end) {
              const flowText = textLines.slice(flowStart, end).join("\n");
              parseBudget -= flowText.length;
              for (const line of flowHtmlStarts(flowText)) htmlStarts.add(flowStart + line);
            }
          }
          if (index === flowStart || htmlStarts?.has(index)) closes = closesAtBlankLine;
          else if (index >= parsedThrough) ambiguousHtml = true;
        }
        const afterFence = afterHeading && openedFence && dedented;
        const fence = FENCE_OPEN.test(line);
        // Fence tracking ignores lists, so it finds the real end of a fence indented two or more
        // spaces only when that fence is outside every list item.
        openedFence = fence && (dedented || /^ ?[`~]/.test(line));
        const thematicBreak = THEMATIC_BREAK.test(line);
        const contentColumn = dedented ? 0 : listContentIndent;
        const canStartBlock = indent - contentColumn <= 3;
        const content = canStartBlock ? line.slice(whitespace.length) : "";
        const item = thematicBreak ? null : LIST_ITEM.exec(content);
        const marker = item?.[1];
        let itemContent = item ? content.slice(item[0].length) : "";
        const itemIsEmpty = BLANK_LINE.test(itemContent);
        const markerKind = marker?.at(-1);
        const continuesList =
          dedented && listContentIndent !== Infinity && markerKind === listMarker;
        const opensItem: boolean =
          item !== null &&
          (!paragraphOpen ||
            continuesList ||
            (!itemIsEmpty && (!/^\d/.test(marker!) || /^1[.)]$/.test(marker!))));
        let startsIndentedCode = false;
        if (item && opensItem) {
          const markerEnd = indent + item[1]!.length;
          const paddedColumn = indentationColumn(item[2]!, markerEnd);
          emptyListItem = dedented && itemIsEmpty;
          // Empty items and items starting with indented code use one column of padding.
          startsIndentedCode = paddedColumn - markerEnd > 4;
          if (dedented) {
            listContentIndent = itemIsEmpty || startsIndentedCode ? markerEnd + 1 : paddedColumn;
            listMarker = markerKind!;
          }
          itemContent = itemContent.replace(NESTED_LIST_MARKERS, "");
        } else if (
          dedented &&
          !blank &&
          (afterBlankLine || afterFence || opensNonParagraph(line))
        ) {
          // Dedented prose can still be a lazy continuation until a block boundary.
          listContentIndent = Infinity;
        }
        // A definition cannot interrupt a paragraph, so it follows a blank line, a heading,
        // a closing code fence or another definition.
        let startsDefinition = false;
        if (
          !ambiguousHtml &&
          (offset === 0 ||
            afterBlankLine ||
            index === definitionEnd + 1 ||
            afterHeading ||
            interruptedFootnote)
        ) {
          let definition = pendingDefinitions?.get(index);
          if (!definition && definitionChars < SHARED_DEFINITIONS_MAX_CHARS) {
            definition = readOneLineDefinition(textLines, index);
            if (!definition && hasDefinitionLabel(textLines, index)) {
              pendingDefinitions = readLinkDefinitions(textLines, index);
              definition = pendingDefinitions.get(index);
            }
          }
          if (definition) {
            startsDefinition = true;
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
        paragraphOpen =
          !blank &&
          !startsDefinition &&
          closes === null &&
          (canStartBlock || paragraphOpen) &&
          (item && opensItem
            ? !BLANK_LINE.test(itemContent) &&
              !startsIndentedCode &&
              !opensNonParagraph(itemContent)
            : !opensNonParagraph(line));
        if (
          bodyLines !== null &&
          closes === null &&
          !inIndentedCode &&
          !startsDefinition &&
          !blank &&
          !isSpanningBlockOpen(line)
        ) {
          bodyLines[index] = 1;
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
      if (blank && !inCommentBlock) commentIn = null;
    }
    pieceLines++;
    chars += line.length + 1;
    // Cuts at the held cut and rescans from it in place, as extendMarkdownSplit would resume
    // there. A growing last line may still become a cut itself, so it waits for its line break.
    if (held !== null && index < wholeLines && chars - 1 > LARGE_MARKDOWN_CHUNK_MAX_CHARS) {
      chunks.push(
        withFootnoteRefs(
          markdownPiece(text, pieceStart, held.resume.start, held.line),
          pieceStartIndex,
          held.index,
        ),
      );
      pieceStart = held.resume.start;
      pieceStartIndex = held.index;
      ({ afterBlankLine, afterHeading, definitionChars, parseBudget } = held.resume);
      ({ footnoteChars, footnoteCapExceeded, inFootnoteList } = held.resume);
      capturedFootnotes.length = held.resume.footnoteDefinitions;
      seenFootnoteKeys.clear();
      for (const { label } of capturedFootnotes) seenFootnoteKeys.add(label.toLowerCase());
      inIndentedCode = false;
      inFootnoteDef = false;
      activeFootnote = null;
      pendingFootnoteBlanks = [];
      ambiguousHtml = false;
      resetFlow(held.index);
      definitionEnd = held.resume.afterDefinition ? held.index - 1 : -2;
      omittedDefinitions.clear();
      for (const label of held.resume.omittedDefinitions) omittedDefinitions.add(label);
      resume = held.resume;
      offset = held.resume.start;
      linesBefore = held.resume.linesBefore;
      definitions.length = held.definitionCount;
      definitionsLength = held.resume.definitions;
      pieceLines = 0;
      chars = 0;
      closes = null;
      openedFence = false;
      listContentIndent = Infinity;
      listMarker = "";
      emptyListItem = false;
      paragraphOpen = false;
      openWrappers = 0;
      wrapperStart = 0;
      commentIn = null;
      inCommentBlock = false;
      index = held.index - 1;
      held = null;
      continue;
    }
    afterBlankLine = inFrontmatter || blank;
    if (afterBlankLine) {
      ambiguousHtml = false;
      resetFlow(index + 1);
    }
    afterHeading =
      !inFrontmatter &&
      !handledAsFootnote &&
      closes === null &&
      (openedFence || ATX_HEADING.test(line) || THEMATIC_BREAK.test(line));
    offset += line.length + 1;
    linesBefore++;
  }
  if (inFootnoteDef) finishFootnoteDef();
  chunks.push(
    withFootnoteRefs(
      markdownPiece(text, pieceStart, text.length + 1, pieceLines),
      pieceStartIndex,
      textLines.length,
    ),
  );
  const footnotes = sharedFootnotes
    ? collectFootnotes(chunks, capturedFootnotes, footnoteCapExceeded)
    : undefined;
  const split = {
    chunks,
    definitions: definitions.join("\n"),
    ...(footnotes ? { footnotes } : {}),
  };
  return resume === undefined
    ? split
    : { ...split, resume: { ...resume, textLength: text.length, capturedFootnotes } };
}

/** The footnotes a split shares, when it has more than one piece and one references a definition. */
function collectFootnotes(
  chunks: readonly MarkdownChunk[],
  captured: readonly FootnoteDefinition[],
  capExceeded: boolean,
): MarkdownFootnotes | undefined {
  if (capExceeded || chunks.length < 2 || captured.length === 0) return undefined;
  const keys = new Set(captured.map(({ label }) => label.toLowerCase()));
  const chunkRefs = chunks.map(
    (chunk) => chunk.footnoteRefs?.filter((label) => keys.has(label.toLowerCase())) ?? [],
  );
  if (chunkRefs.every((refs) => refs.length === 0)) return undefined;
  return {
    stubs: captured.map(({ label }) => `[^${label}]: .`).join("\n"),
    definitions: captured.map(({ text }) => text).join("\n\n"),
    definitionSpans: captured.map(({ offset, text }) => [offset, text.length] as const),
    chunkRefs,
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

/**
 * GitHub's heading anchor for a heading's rendered text, before duplicates are numbered.
 * GitHub keeps Ruby's \p{Word} (UTS #18 \w), spaces and hyphens.
 */
export function githubHeadingSlug(text: string): string {
  return text
    .trim()
    .toLowerCase()
    .replace(/[^\p{Alphabetic}\p{M}\p{Nd}\p{Pc}\p{Join_Control} -]/gu, "")
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
  for (const line of splitLines(text, 0)) {
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
    if (BLANK_LINE.test(line)) {
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
    closes = blockCloser(line) ?? (HTML_STANDALONE_TAG.test(line) ? closesAtBlankLine : null);
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

export interface PreparedMarkdownChunk {
  readonly text: string;
  /** What `text` puts before the chunk, for renderers that prepend it themselves. */
  readonly prefix: string;
  readonly offsetShift: number;
  readonly footnotesMode?: "omit" | "omit-seeded" | undefined;
}

/**
 * Prepares one chunk of a split Markdown document with shared link definitions and, when the
 * chunk references document footnotes, stub definitions plus prior references so footnote numbers
 * and backref IDs match the whole document.
 */
export function prepareMarkdownChunk(
  chunkText: string,
  chunkIndex: number,
  definitions: string,
  footnotes?: MarkdownFootnotes,
): PreparedMarkdownChunk {
  if (!footnotes) {
    return {
      text: withMarkdownDefinitions(chunkText, definitions),
      prefix: definitions,
      offsetShift: definitions ? definitions.length + 2 : 0,
    };
  }
  const refsInChunk = footnotes.chunkRefs[chunkIndex];
  if (!refsInChunk || refsInChunk.length === 0) {
    return {
      text: withMarkdownDefinitions(chunkText, definitions),
      prefix: definitions,
      offsetShift: definitions ? definitions.length + 2 : 0,
      footnotesMode: "omit",
    };
  }
  const priorRefs: string[] = [];
  for (let i = 0; i < chunkIndex; i++) {
    const refs = footnotes.chunkRefs[i];
    if (refs) priorRefs.push(...refs);
  }
  const prefixParts: string[] = [];
  if (definitions) prefixParts.push(definitions);
  prefixParts.push(footnotes.stubs);
  if (priorRefs.length > 0) {
    prefixParts.push(priorRefs.map((label) => `[^${label}]`).join(" "));
  } else {
    // An HTML comment closes the last stub footnote so a chunk starting with 4-space indented code
    // cannot be absorbed as a continuation of that stub.
    prefixParts.push("<!-- -->");
  }
  const prefix = prefixParts.join("\n\n");
  return {
    text: withMarkdownDefinitions(chunkText, prefix),
    prefix,
    offsetShift: prefix.length + 2,
    footnotesMode: priorRefs.length > 0 ? "omit-seeded" : "omit",
  };
}

export interface MarkdownFootnotesSection {
  readonly text: string;
  /** Maps an offset in `text` to the file, or null outside the footnote definitions. */
  readonly fileOffset: (offset: number) => number | null;
}

/** Builds the Markdown source for the single trailing footnotes section of a split document. */
export function buildMarkdownFootnotesSection(
  definitions: string,
  footnotes?: MarkdownFootnotes,
): MarkdownFootnotesSection | null {
  if (!footnotes || !footnotes.definitions) return null;
  const allRefs = footnotes.chunkRefs.flat();
  if (allRefs.length === 0) return null;
  const parts: string[] = [];
  if (definitions) parts.push(definitions);
  parts.push(allRefs.map((label) => `[^${label}]`).join(" "));
  parts.push(footnotes.definitions);
  const text = parts.join("\n\n");
  const { definitionSpans } = footnotes;
  return {
    text,
    fileOffset: (offset) => {
      let start = text.length - footnotes.definitions.length;
      for (const [fileStart, length] of definitionSpans) {
        if (offset < start) return null;
        if (offset < start + length) return fileStart + offset - start;
        start += length + 2;
      }
      return null;
    },
  };
}

/** Maps a footnote backref (`fnref-<fnNumber>` / `fnref-<fnNumber>-<occurrence>`) to its chunk. */
export function findFootnoteRefChunkIndex(
  footnotes: MarkdownFootnotes,
  fnNumber: number,
  occurrence: number,
): number | null {
  if (fnNumber < 1 || occurrence < 1) return null;
  const numberByKey = new Map<string, number>();
  const countByNumber = new Map<number, number>();
  let nextNumber = 1;
  for (let chunkIndex = 0; chunkIndex < footnotes.chunkRefs.length; chunkIndex++) {
    const refs = footnotes.chunkRefs[chunkIndex];
    if (!refs) continue;
    for (const label of refs) {
      const key = label.toLowerCase();
      let num = numberByKey.get(key);
      if (num === undefined) {
        num = nextNumber++;
        numberByKey.set(key, num);
      }
      const occ = (countByNumber.get(num) ?? 0) + 1;
      countByNumber.set(num, occ);
      if (num === fnNumber && occ === occurrence) {
        return chunkIndex;
      }
    }
  }
  return null;
}
