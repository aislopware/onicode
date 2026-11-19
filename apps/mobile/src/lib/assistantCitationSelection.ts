import { markdownMathRanges } from "@onicode/shared/markdownMath";
import { createAssistantTextSelector } from "@onicode/shared/assistantCitations";
import {
  splitCodexArtifactTemplateMarkdown,
  renderCodexInlineDirectivesAsMarkdown,
} from "@onicode/shared/codexMarkdownDirectives";
import { splitMarkdownDetails } from "./markdownDetails";
import {
  markdownForNativeParsing,
  markdownReferenceDefinitions,
  parseMarkdownDocument,
  withReferenceDefinitions,
  type MarkdownNode,
} from "./markdownDocument";

function renderedText(
  node: MarkdownNode,
  restoreMath: (value: string) => string,
  attachment: (node: MarkdownNode) => string = () => "",
): string {
  if (node.type === "image" || node.type === "imageReference" || node.type === "thematicBreak")
    return attachment(node);
  if (
    node.type === "definition" ||
    node.type === "html" ||
    (node.type === "code" && node.lang === "mermaid")
  )
    return "";
  if ("value" in node) return restoreMath(node.value);
  if ("children" in node) {
    const inline =
      node.type === "paragraph" ||
      node.type === "heading" ||
      node.type === "link" ||
      node.type === "linkReference" ||
      node.type === "emphasis" ||
      node.type === "strong" ||
      node.type === "delete" ||
      node.type === "tableCell";
    return node.children
      .map((child) => renderedText(child, restoreMath, attachment))
      .filter((text) => text.length > 0)
      .join(inline ? "" : "\n");
  }
  return node.type === "break" ? "\n" : "";
}

// Native text views report a rendered inline formula, an inline image, or an iOS rule as this.
const ATTACHMENT = "\ufffc";
// Stands in for display math md4c moves to its own view, which no text selection can include.
const DETACHED = "\ue002";
const EXPANDED = /[\ufffc\ue002]/g;
// Marks an expansion whose preceding line break only the native view shows.
const NATIVE_BREAK = "\ue003";

// The web's text for a native one: attachments expand, and native-only breaks drop.
function expandAttachments(text: string, expansions: readonly string[], from = 0): string {
  let index = from;
  return text.replace(/\n?[\ufffc\ue002]/g, (match) => {
    const expansion = expansions[index++]!;
    return expansion.startsWith(NATIVE_BREAK)
      ? expansion.slice(NATIVE_BREAK.length)
      : match.slice(0, -1) + expansion;
  });
}

type NativePlatform = "ios" | "android";

// md4c has no footnotes and reads `||text||` as a spoiler.
function rendersDifferently(node: MarkdownNode): boolean {
  if (node.type === "footnoteReference") return true;
  if (node.type === "text") return node.value.includes("||");
  return "children" in node && node.children.some(rendersDifferently);
}

const BLOCK_START =
  /^(?:$|[-+*](?:[ \t]|$)|\d{1,9}[.)](?:[ \t]|$)|#{1,6}(?:[ \t]|$)|`{3,}|~{3,}|[-=]+[ \t]*$|<)/;
const quoteDepth = (line: string) => /^[ \t>]*/.exec(line)![0].split(">").length - 1;

// md4c reads blocks before math, so a formula line that would start a block splits the formula.
function splitsIntoBlocks(markdown: string, start: number, source: string): boolean {
  const depth = quoteDepth(markdown.slice(markdown.lastIndexOf("\n", start - 1) + 1, start));
  return source
    .split(/\r?\n/)
    .slice(1)
    .some(
      (line) =>
        quoteDepth(line) > depth || BLOCK_START.test(line.slice(/^[ \t>]*/.exec(line)![0].length)),
    );
}

const MATH_DELIMITER = /\$|\\[([]/;
const UNCERTAIN_PROSE = /\$|\\[([]|!`/;

// md4c reads `$` as a delimiter only at a line edge or beside whitespace or punctuation.
const FLANKED_BEFORE = /(?:^|[\s\p{P}\p{S}])$/u;
const FLANKED_AFTER = /^(?:$|[\s\p{P}\p{S}])/u;

// Neither parser reads math in a code block, and agent replies often hold shell `$` in them.
function withoutCodeBlocks(node: MarkdownNode, source: string): string {
  const kept: string[] = [];
  let cursor = 0;
  const visit = (child: MarkdownNode) => {
    const start = child.position?.start.offset;
    const end = child.position?.end.offset;
    if (child.type === "code" && start !== undefined && end !== undefined) {
      kept.push(source.slice(cursor, start));
      cursor = end;
    } else if ("children" in child) child.children.forEach(visit);
  };
  visit(node);
  kept.push(source.slice(cursor));
  return kept.join("");
}

// Mirrors md4c's latexMath flag: with it on, each formula is ATTACHMENT in `text`, and
// `expansions` holds what each ATTACHMENT reads as on the web (a formula's TeX, or itself).
// `uncertainAttachments` marks text where a native ATTACHMENT may not be a formula; that text
// keeps the web's formulas instead.
function citationText(
  markdown: string,
  mathRenderingEnabled: boolean,
  platform: NativePlatform,
): { text: string; expansions: string[]; uncertainAttachments: boolean } {
  const formulas = mathRenderingEnabled ? markdownMathRanges(markdown) : [];
  let marker = "\ue000t3math";
  while (markdown.includes(marker)) marker += "_";
  const tokens = formulas.map((_, index) => `${marker}${index}\ue001`);
  let cursor = 0;
  let prose = "";
  for (const [index, formula] of formulas.entries()) {
    prose += markdown.slice(cursor, formula.start) + tokens[index];
    cursor = formula.end;
  }
  prose += markdown.slice(cursor);
  const nativeProse = markdownForNativeParsing(prose);
  const tree = parseMarkdownDocument(prose);
  const tokenPattern = new RegExp(`${marker}(\\d+)\ue001`, "g");
  // md4c moves a top-level paragraph's own display math to its own view when a neighbour is the
  // paragraph edge, a line break, or whitespace-only text (`isIsolatedDisplayMath`).
  const detached = new Set<number>();
  // Text beside a formula separates when blank (md4c's blank is only space, tab, and line ends)
  // or broken at a line; no text means the neighbour is a formula, a sibling, or the edge.
  // md4c may read a bare URL as text, so blank text next to one stays attached to be safe.
  const bareLink = (node?: MarkdownNode) =>
    node?.type === "link" && !"[<".includes(nativeProse[node.position?.start.offset ?? 0] ?? "[");
  const separates = (text: string, pattern: RegExp, last: boolean, sibling?: MarkdownNode) =>
    text === ""
      ? last && (sibling === undefined || sibling.type === "break")
      : pattern.test(text) && (text.includes("\n") || !last || !bareLink(sibling));
  for (const block of tree.children) {
    if (block.type !== "paragraph") continue;
    for (const [childIndex, child] of block.children.entries()) {
      if (child.type !== "text") continue;
      const parts = child.value.split(tokenPattern);
      for (let part = 1; part < parts.length; part += 2) {
        if (
          formulas[Number(parts[part])]!.display &&
          (separates(
            parts[part - 1]!,
            /(?:^|\n)[ \t\r\n]*$/,
            part === 1,
            block.children[childIndex - 1],
          ) ||
            separates(
              parts[part + 1]!,
              /^[ \t\r\n]*(?:\n|$)/,
              part === parts.length - 2,
              block.children[childIndex + 1],
            ))
        )
          detached.add(Number(parts[part]));
      }
    }
  }
  // A math fence is its own native view, except in a list, which shows it as code text.
  const listed = new Set<number>();
  const visitLists = (node: MarkdownNode) => {
    if (node.type === "list")
      for (const match of renderedText(node, (value) => value).matchAll(tokenPattern))
        listed.add(Number(match[1]));
    else if ("children" in node) node.children.forEach(visitLists);
  };
  visitLists(tree);
  const fenced = formulas.map((formula) => /^[ \t]*(?:`{3,}|~{3,})/.test(formula.source));
  for (const [index, fence] of fenced.entries())
    if (fence && !listed.has(index)) detached.add(index);
  // md4c lifts a tight list item's own display math out of its text, and Android ends the text
  // run before it with a line break; true when no whitespace precedes, so the web has none.
  const breaksBefore = new Map<number, boolean>();
  // A paragraph holding only an image, outside lists, is its own native view.
  const ownViews = new Set<MarkdownNode>();
  const visitBlocks = (node: MarkdownNode, inList: boolean) => {
    if (node.type === "list" && !node.spread && node.children.every((item) => !item.spread))
      for (const item of node.children)
        for (const child of item.children) {
          if (child.type !== "paragraph") continue;
          let run = false;
          let spaced = false;
          const read = (text: string) => {
            if (/\S/.test(text)) run = true;
            if (text) spaced = /\s$/.test(text);
          };
          for (const inline of child.children) {
            if (inline.type !== "text") {
              run ||= inline.type !== "break";
              spaced = inline.type === "break";
              continue;
            }
            let cursor = 0;
            for (const match of inline.value.matchAll(tokenPattern)) {
              read(inline.value.slice(cursor, match.index));
              cursor = match.index + match[0].length;
              const index = Number(match[1]);
              if (!formulas[index]!.display) read("x");
              else {
                if (run) breaksBefore.set(index, !spaced);
                run = false;
              }
            }
            read(inline.value.slice(cursor));
          }
        }
    if (!inList && (node.type === "root" || node.type === "blockquote"))
      for (const child of node.children)
        if (
          child.type === "paragraph" &&
          child.children.length === 1 &&
          (child.children[0]!.type === "image" || child.children[0]!.type === "imageReference")
        )
          ownViews.add(child.children[0]!);
    if ("children" in node)
      for (const child of node.children) visitBlocks(child, inList || node.type === "list");
  };
  visitBlocks(tree, false);
  const expansions: string[] = [];
  let authoredDetached = false;
  const text = renderedText(
    tree,
    (value) =>
      value.replace(
        new RegExp(`${tokenPattern.source}|${EXPANDED.source}`, "g"),
        (char: string, index: string | undefined) => {
          if (index === undefined) {
            authoredDetached ||= char === DETACHED;
            expansions.push(char);
            return char;
          }
          const formula = formulas[Number(index)]!;
          const tex = formula.tex.trim();
          const broken = platform === "android" ? breaksBefore.get(Number(index)) : undefined;
          expansions.push(
            (broken ? NATIVE_BREAK : "") + (formula.display ? `\\[\n${tex}\n\\]` : `\\(${tex}\\)`),
          );
          if (detached.has(Number(index))) return DETACHED;
          return broken === undefined ? ATTACHMENT : `\n${ATTACHMENT}`;
        },
      ),
    // The web shows neither images nor rules as text.
    (node) => {
      if (node.type === "thematicBreak" ? platform === "android" : ownViews.has(node)) return "";
      expansions.push("");
      return ATTACHMENT;
    },
  );
  const unflanked = (index: number) => {
    const at = nativeProse.indexOf(tokens[index]!);
    const end = at + tokens[index]!.length;
    return (
      at === -1 ||
      !FLANKED_BEFORE.test(nativeProse.slice(Math.max(0, at - 2), at)) ||
      !FLANKED_AFTER.test(nativeProse.slice(end, end + 2))
    );
  };
  // Spoilers, footnotes, delimiters md4c may read differently, and `!` paths it shows as images.
  const uncertainAttachments =
    mathRenderingEnabled &&
    (rendersDifferently(tree) ||
      formulas.some(
        (formula, index) =>
          MATH_DELIMITER.test(formula.tex) ||
          (!fenced[index] && splitsIntoBlocks(markdown, formula.start, formula.source)) ||
          (fenced[index] && listed.has(index)) ||
          (formula.inTable &&
            formula.source.startsWith("$") &&
            /(?:^|[^\\])(?:\\\\)*\|/.test(formula.source)) ||
          (formula.source.startsWith("$") && unflanked(index)),
      ) ||
      authoredDetached ||
      UNCERTAIN_PROSE.test(withoutCodeBlocks(tree, nativeProse)));
  // Without a reliable native form, match the web's text as before formulas were selectable.
  if (uncertainAttachments) {
    const webText = expandAttachments(text, expansions);
    return {
      text: webText,
      expansions: [...webText.matchAll(EXPANDED)].map((match) => match[0]),
      uncertainAttachments,
    };
  }
  return { text, expansions, uncertainAttachments };
}

function citationSegments(
  markdown: string,
  mathRenderingEnabled: boolean,
  platform: NativePlatform,
  path = "",
  definitions = markdownReferenceDefinitions(markdown),
): Array<{ path: string; text: string; expansions: string[]; uncertainAttachments: boolean }> {
  return splitMarkdownDetails(markdown).flatMap((block, index) => {
    if (block.kind === "details")
      return citationSegments(
        block.body,
        mathRenderingEnabled,
        platform,
        `${path}${index}/`,
        definitions,
      );
    return splitCodexArtifactTemplateMarkdown(block.markdown).flatMap((segment, segmentIndex) =>
      segment.kind === "markdown"
        ? [
            {
              path: `${path}${index}:${segmentIndex}`,
              ...citationText(
                renderCodexInlineDirectivesAsMarkdown(
                  withReferenceDefinitions(segment.markdown, definitions),
                ),
                mathRenderingEnabled,
                platform,
              ),
            },
          ]
        : [],
    );
  });
}

function occurrences(text: string, quote: string): number[] {
  const result: number[] = [];
  for (let index = text.indexOf(quote); index !== -1; index = text.indexOf(quote, index + 1))
    result.push(index);
  return result;
}

export function collectMobileAssistantTextSelectors(
  markdown: string,
  path: string,
  text: string,
  mathRenderingEnabled: boolean,
  platform: NativePlatform,
) {
  if (!text.trim()) return [];
  const normalize = (value: string) => value.replace(/\s+/g, " ");
  const quote = normalize(text);
  const segments = citationSegments(markdown, mathRenderingEnabled, platform).filter(
    (segment) => segment.text.trim().length > 0,
  );
  const segmentIndex = segments.findIndex((segment) => segment.path === path);
  const segment = segments[segmentIndex];
  if (!segment) return [];
  if (quote.includes(ATTACHMENT) && segment.uncertainAttachments) return [];
  const matchText = normalize(segments.map((entry) => entry.text).join("\n"));
  // Selectors use the web's text, so expand each attachment and keep where match offsets land.
  const expansions = segments.flatMap((entry) => entry.expansions);
  const fullOffsets: number[] = [];
  let fullText = "";
  let expansionIndex = 0;
  let position = 0;
  for (const char of matchText) {
    for (let unit = 0; unit < char.length; unit += 1) fullOffsets.push(fullText.length);
    const expansion = expansions[expansionIndex];
    if (char === ATTACHMENT || char === DETACHED)
      fullText += normalize(expansions[expansionIndex++]!.replace(NATIVE_BREAK, ""));
    else if (
      !(
        char === " " &&
        matchText[position + 1] === ATTACHMENT &&
        expansion?.startsWith(NATIVE_BREAK)
      )
    )
      fullText += char;
    position += char.length;
  }
  fullOffsets.push(fullText.length);
  // A native selection never spans two segments.
  const segmentMatches = segments.map((entry, index) => {
    const through = segments
      .slice(0, index + 1)
      .map((previous) => previous.text)
      .join("\n");
    const offset = normalize(through).length - normalize(entry.text).length;
    return occurrences(normalize(entry.text), quote).map((local) => offset + local);
  });
  const matches = segmentMatches.flat();
  const localMatches = segmentMatches[segmentIndex]!;
  if (localMatches.length > 20) return [];
  const choices = localMatches.map((start) => {
    const selector = createAssistantTextSelector(
      fullText,
      fullOffsets[start]!,
      fullOffsets[start + quote.length]!,
    );
    if (!selector) return null;
    const quoted = expandAttachments(
      text,
      expansions,
      matchText.slice(0, start).match(EXPANDED)?.length ?? 0,
    );
    return {
      selector: { ...selector, text: quoted },
      index: matches.indexOf(start),
      count: matches.length,
    };
  });
  // A passage with no web text, such as an image alone, cannot say which choice was meant.
  return choices.every((choice) => choice !== null) ? choices : [];
}
