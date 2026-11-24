export function truncate(text: string, maxLength = 50): string {
  const trimmed = text.trim();
  const characters = Array.from(trimmed);
  if (characters.length <= maxLength) {
    return trimmed;
  }

  return `${characters.slice(0, maxLength).join("")}...`;
}

const isHighSurrogate = (code: number) => code >= 0xd800 && code <= 0xdbff;
const isLowSurrogate = (code: number) => code >= 0xdc00 && code <= 0xdfff;

/**
 * Where to end the start of `text` kept within `limit` UTF-16 units: at its last space, unless
 * that keeps less than half (text such as Chinese has no spaces to cut at), else at the limit,
 * never inside a surrogate pair.
 */
export function wordCutEnd(text: string, limit: number): number {
  const space = text.lastIndexOf(" ", limit);
  if (space > limit / 2) return space;
  return isHighSurrogate(text.charCodeAt(limit - 1)) ? limit - 1 : limit;
}

/** Where to start the end of `text` kept within `limit` UTF-16 units; `wordCutEnd` mirrored. */
export function wordCutStart(text: string, limit: number): number {
  const from = Math.max(0, text.length - limit);
  if (from === 0 || text[from - 1] === " ") return from;
  const space = text.indexOf(" ", from);
  if (space !== -1 && space < from + limit / 2) return space + 1;
  return isLowSurrogate(text.charCodeAt(from)) ? from + 1 : from;
}
