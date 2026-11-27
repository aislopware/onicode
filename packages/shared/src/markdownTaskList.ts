import type { ListItem, Nodes } from "mdast";
import remarkGfm from "remark-gfm";
import remarkParse from "remark-parse";
import { unified } from "unified";

import { taskListMarkerOffset } from "./markdownTaskEdits.ts";

const parser = unified().use(remarkParse).use(remarkGfm);

/** Hard breaks as U+2028, as iOS draws them; soft breaks stay "\n". */
function plainText(node: Nodes): string {
  if (node.type === "text" || node.type === "inlineCode") return node.value;
  if (node.type === "break" || (node.type === "html" && /^<br\b/i.test(node.value))) {
    return "\u2028";
  }
  return "children" in node ? node.children.map(plainText).join("") : "";
}

const lettersAndDigits = (text: string) => text.replace(/[^\p{L}\p{N}]/gu, "");

/** iOS sends the first paragraph whole. Android cuts it at a hard break, or any break with hard breaks on. */
function renderedTexts(item: ListItem): string[] {
  const first = item.children[0];
  const text = first?.type === "paragraph" ? plainText(first) : "";
  return [text, text.replace(/\u2028[^]*$/, ""), text.replace(/[\u2028\n][^]*$/, "")].map(
    lettersAndDigits,
  );
}

/**
 * Marker offset of the task a native checkbox tap names. The index counts every task in the
 * document; null when that task has another text or state, so a stale tap changes nothing.
 */
export function markdownTaskMarkerOffset(
  markdown: string,
  index: number,
  renderedText: string,
  checked: boolean,
): number | null {
  // Native attachments replace their labels, so the remaining text cannot confirm the task.
  if (renderedText.includes("\uFFFC")) return null;
  const tasks: Array<{ readonly item: ListItem; readonly offset: number }> = [];
  const collect = (node: Nodes) => {
    if (node.type === "listItem" && typeof node.checked === "boolean") {
      // Native enrmrkd only recognizes task markers on the same line as their list bullet.
      const start = node.position?.start.offset;
      const offset = start === undefined ? null : taskListMarkerOffset(markdown, start);
      if (start !== undefined && offset !== null && !/[\r\n]/.test(markdown.slice(start, offset))) {
        tasks.push({ item: node, offset });
      }
    }
    if ("children" in node) node.children.forEach(collect);
  };
  collect(parser.parse(markdown));
  const task = tasks[index];
  if (
    !task ||
    task.item.checked !== !checked ||
    !renderedTexts(task.item).includes(lettersAndDigits(renderedText))
  )
    return null;
  return task.offset;
}
