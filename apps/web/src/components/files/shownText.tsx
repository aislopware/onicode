import { useGT } from "gt-react";
import { Fragment } from "react";

// Source lines stop where VS Code's do.
export const MAX_SHOWN_LINE_CHARS = 10_000;

export function useMoreCharactersLabel() {
  const gt = useGT();
  return (count: number) =>
    gt("{count, plural, one {{formatted} more character} other {{formatted} more characters}}", {
      count,
      formatted: count.toLocaleString(),
    });
}

export function CellText(props: { text: string; limit: number; more: (count: number) => string }) {
  const end = shownLength(props.text, props.limit);
  if (end === props.text.length) return props.text;
  return (
    <>
      {props.text.slice(0, end)}
      <span className="text-muted-foreground select-none">
        {cutSuffix(props.text.length - end, props.more)}
      </span>
    </>
  );
}

// A notebook cell or output lays out two source lines' worth, however many lines it has.
const MAX_SHOWN_TEXT_CHARS = 2 * MAX_SHOWN_LINE_CHARS;

type ShownPart = { readonly at: number; readonly text: string; readonly cut: number };

/** `text` as runs to lay out, each followed by how many characters were left out after it. */
function cappedParts(text: string): ShownPart[] {
  if (text.length <= MAX_SHOWN_LINE_CHARS) return [{ at: 0, text, cut: 0 }];
  const parts: ShownPart[] = [];
  let from = 0;
  for (let start = 0; start <= text.length;) {
    const newline = text.indexOf("\n", start);
    const end = newline < 0 ? text.length : newline;
    if (end - start > MAX_SHOWN_LINE_CHARS) {
      const shownEnd = start + shownLength(text.slice(start, end), MAX_SHOWN_LINE_CHARS);
      parts.push({ at: from, text: text.slice(from, shownEnd), cut: end - shownEnd });
      from = end;
    }
    start = end + 1;
  }
  parts.push({ at: from, text: text.slice(from), cut: 0 });
  let room = MAX_SHOWN_TEXT_CHARS;
  for (const [index, part] of parts.entries()) {
    if (part.text.length <= room) {
      room -= part.text.length;
      continue;
    }
    const kept = shownLength(part.text, room);
    const cut = parts.slice(index).reduce((sum, next) => sum + next.text.length + next.cut, -kept);
    return [...parts.slice(0, index), { at: part.at, text: part.text.slice(0, kept), cut }];
  }
  return parts;
}

/** Multi-line text with each line cut like a source line, and the whole cut short. */
export function LineCappedText(props: { text: string; more: (count: number) => string }) {
  return cappedParts(props.text).map((part) => (
    <Fragment key={part.at}>
      {part.text}
      {part.cut > 0 ? (
        <span className="text-muted-foreground select-none">{cutSuffix(part.cut, props.more)}</span>
      ) : null}
    </Fragment>
  ));
}

/** `text` cut like `LineCappedText`, for text rendered as markdown. */
export function lineCappedText(text: string, more: (count: number) => string): string {
  return cappedParts(text)
    .map((part) => (part.cut > 0 ? part.text + cutSuffix(part.cut, more) : part.text))
    .join("");
}

/** The length of a cell's text the preview lays out, never splitting a surrogate pair. */
export function shownLength(text: string, limit: number): number {
  if (text.length <= limit) return text.length;
  const before = text.charCodeAt(limit - 1);
  const after = text.charCodeAt(limit);
  return before >= 0xd800 && before <= 0xdbff && after >= 0xdc00 && after <= 0xdfff
    ? limit - 1
    : limit;
}

function cutSuffix(count: number, more: (count: number) => string): string {
  return `… ${more(count)}`;
}

/** The text a cell lays out: at most `limit` characters, then how many were left out. */
export function shownText(text: string, limit: number, more: (count: number) => string): string {
  const end = shownLength(text, limit);
  return end === text.length ? text : text.slice(0, end) + cutSuffix(text.length - end, more);
}
