import { decodeString } from "micromark-util-decode-string";
import { UNFINISHED_DRIVE_PATH } from "./markdownLinks.ts";

export interface PastedTextRun {
  readonly text: string;
  /** The `href` of the anchor this text is the label of, as the clipboard wrote it. */
  readonly href?: string | undefined;
}

// Prose that would read as inline Markdown once pasted back: a backslash before punctuation,
// backticks, brackets, and emphasis or strikethrough runs (user remark-gfm sets singleTilde: false).
const INLINE_MARKDOWN = /\\(?=[!-/:-@[-`{-~\n]|$)|[`[\]]|\*+|_+|~{2,}/g;

/** Escapes rendered prose so it parses back to the same text, leaving runs that cannot open or close. */
export function escapeMarkdownProse(text: string): string {
  return text.replace(/\S+/g, (word, wordOffset: number) => {
    // Check each word once; testing every escape's growing prefix would be quadratic.
    const drive = UNFINISHED_DRIVE_PATH.exec(word);
    const driveStart = drive ? drive.index + drive[0].search(/[A-Za-z]/) + 2 : Infinity;
    return word.replace(INLINE_MARKDOWN, (match: string, offset: number) => {
      if (offset >= driveStart) return match;
      const before = text[wordOffset + offset - 1];
      const after = text[wordOffset + offset + match.length];
      if (match === "\\" && after !== undefined && /[^\S\n]/.test(after)) return match;
      // A text node's edge may touch another node's markers, so it counts as neither.
      const between = (pattern: RegExp) =>
        before !== undefined && after !== undefined && pattern.test(before) && pattern.test(after);
      if (/[*_~]/.test(match[0]!) && between(/\s/)) return match;
      if (match[0] === "_" && between(/[\p{L}\p{N}]/u)) return match;
      return match.replace(/[\s\S]/g, "\\$&");
    });
  });
}

function linkDestination(href: string): string | undefined {
  const trimmed = href.trim();
  if (!/^(?:https?:\/\/|mailto:)/i.test(trimmed)) return undefined;
  try {
    // React Native's URL keeps the input as written, so whitespace that ends a destination stays.
    return new URL(trimmed).href.replace(/[\s\p{Cc}]/gu, encodeURIComponent);
  } catch {
    return undefined;
  }
}

/**
 * Restores link destinations only when the visible runs and the plain clipboard describe the
 * same text. The plain rendition owns line breaks and spacing.
 */
export function restoreMarkdownLinkRuns(plain: string, runs: Iterable<PastedTextRun>): string {
  const characters = plain.matchAll(/\S/g);
  const parts: string[] = [];
  let resultEnd = 0;
  for (const run of runs) {
    let start: number | undefined;
    let end = 0;
    for (const character of run.text.matchAll(/\S/g)) {
      const match = characters.next().value;
      if (match?.[0] !== character[0]) return plain;
      start ??= match.index;
      end = match.index + 1;
    }
    if (run.href === undefined || start === undefined) continue;
    const href = linkDestination(run.href);
    if (href === undefined) continue;
    const label = plain.slice(start, end);
    if (label === href) continue;
    const prefix = plain.slice(resultEnd, start).replace(/(?<!\\)((?:\\\\)*)([!\\])$/, "$1\\$2");
    const destination = href.replaceAll("&", "&amp;").replace(/[\\()]/g, "\\$&");
    parts.push(
      prefix,
      label.replace(/\S(?:[^\n]*\S)?/g, (line) => `[${escapeMarkdownProse(line)}](${destination})`),
    );
    resultEnd = end;
  }
  if (!characters.next().done || parts.length === 0) return plain;
  return parts.join("") + plain.slice(resultEnd);
}

const VOID_ELEMENTS = new Set([
  "area",
  "base",
  "br",
  "col",
  "embed",
  "hr",
  "img",
  "input",
  "link",
  "meta",
  "param",
  "source",
  "track",
  "wbr",
]);
const TOKEN =
  /<!--[\s\S]*?(?:-->|$)|<[!?][^>]*>?|<(\/?)([A-Za-z][^\s/>]*)((?:[^>"']|"[^"]*"|'[^']*')*)>|[^<]+|</gy;
const ATTRIBUTE = /([^\s"'>/=]+)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+)))?/g;

interface OpenElement {
  readonly name: string;
  readonly removed: boolean;
  readonly invisible: boolean;
  readonly code: boolean;
  anchor?: { readonly href: string; text: string };
}

// micromark decodes character references; doubling backslashes keeps its escapes from applying.
const decodeHtml = (text: string) => decodeString(text.replaceAll("\\", "\\\\"));

function inlineStyle(style: string | undefined) {
  let display: string | undefined;
  let visibility: string | undefined;
  for (const declaration of style?.split(";") ?? []) {
    const colon = declaration.indexOf(":");
    if (colon < 0) continue;
    const property = declaration.slice(0, colon).trim().toLowerCase();
    const value = declaration
      .slice(colon + 1)
      .replace(/!\s*important\s*$/i, "")
      .trim()
      .toLowerCase();
    if (property === "display") display = value;
    else if (property === "visibility") visibility = value;
  }
  return { display, visibility };
}

/**
 * The visible text of clipboard HTML in document order, each link's label as one run. Clipboard
 * HTML is serialized by a browser, so a tag stack is enough without a full HTML parser.
 */
function pastedHtmlTextRuns(html: string): PastedTextRun[] {
  const runs: PastedTextRun[] = [];
  const stack: OpenElement[] = [];
  const close = (index: number) => {
    for (const element of stack.splice(index).reverse()) {
      if (element.anchor) runs.push(element.anchor);
    }
  };
  TOKEN.lastIndex = 0;
  for (let token = TOKEN.exec(html); token; token = TOKEN.exec(html)) {
    const [raw, closing, rawName, rawAttributes = ""] = token;
    const parent = stack.at(-1);
    if (rawName === undefined) {
      if (raw.startsWith("<!") || raw.startsWith("<?") || parent?.removed || parent?.invisible)
        continue;
      const text = decodeHtml(raw);
      const anchor = stack.findLast((element) => element.anchor)?.anchor;
      if (anchor) anchor.text += text;
      else runs.push({ text });
      continue;
    }
    const name = rawName.toLowerCase();
    if (closing) {
      const index = stack.findLastIndex((element) => element.name === name);
      if (index >= 0) close(index);
      continue;
    }
    if (name === "script" || name === "style") {
      const end = html.slice(TOKEN.lastIndex).search(new RegExp(`</${name}[\\s/>]`, "i"));
      TOKEN.lastIndex = end < 0 ? html.length : TOKEN.lastIndex + end;
      continue;
    }
    if (VOID_ELEMENTS.has(name)) continue;
    const attributes = new Map<string, string>();
    for (const [, key, double, single, unquoted] of rawAttributes.matchAll(ATTRIBUTE)) {
      const attribute = key!.toLowerCase();
      if (!attributes.has(attribute))
        attributes.set(attribute, decodeHtml(double ?? single ?? unquoted ?? ""));
    }
    if (name === "a") {
      // An anchor cannot nest; the parser closes the open one first.
      const open = stack.findLastIndex((element) => element.name === "a");
      if (open >= 0) close(open);
    }
    const { display, visibility } = inlineStyle(attributes.get("style"));
    // Visibility inherits, but a visible descendant can override a hidden ancestor.
    const inheritsHidden = parent?.invisible ?? false;
    const element: OpenElement = {
      name,
      removed:
        (parent?.removed ?? false) ||
        name === "template" ||
        attributes.has("hidden") ||
        display === "none",
      invisible:
        visibility === "hidden" ||
        visibility === "collapse" ||
        (inheritsHidden && visibility !== "visible" && visibility !== "initial"),
      code: (parent?.code ?? false) || name === "pre" || name === "code",
    };
    const href = attributes.get("href");
    // Linked identifiers in API docs code samples must stay literal code.
    if (name === "a" && href !== undefined && !element.code && !element.removed)
      element.anchor = { href, text: "" };
    stack.push(element);
  }
  close(0);
  return runs;
}

/** Restores pasted link destinations from clipboard HTML without a DOM, for clients that have none. */
export function restorePastedHtmlLinks(plain: string, html: string): string {
  if (!plain || !/<a[\s>]/i.test(html)) return plain;
  return restoreMarkdownLinkRuns(plain, pastedHtmlTextRuns(html));
}
