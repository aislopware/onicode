export function truncate(text: string, maxLength = 50): string {
  const trimmed = text.trim();
  const characters = Array.from(trimmed);
  if (characters.length <= maxLength) {
    return trimmed;
  }

  return `${characters.slice(0, maxLength).join("").trimEnd()}...`;
}

const isHighSurrogate = (code: number) => code >= 0xd800 && code <= 0xdbff;
const isLowSurrogate = (code: number) => code >= 0xdc00 && code <= 0xdfff;

const isWhitespace = (text: string, at: number) => /\s/.test(text.charAt(at));

/**
 * Where to end the start of `text` kept within `limit` UTF-16 units: before its last whitespace
 * run, unless that keeps half or less (text such as Chinese has no spaces to cut at), else at the
 * limit, never inside a surrogate pair.
 */
export function wordCutEnd(text: string, limit: number): number {
  if (text.length <= limit) return text.length;
  let end = limit;
  while (end > 0 && !isWhitespace(text, end)) end--;
  while (end > 0 && isWhitespace(text, end - 1)) end--;
  if (end > limit / 2) return end;
  end = isHighSurrogate(text.charCodeAt(limit - 1)) ? limit - 1 : limit;
  while (end > 1 && isWhitespace(text, end - 1)) end--;
  return end;
}

/** Where to start the end of `text` kept within `limit` UTF-16 units; `wordCutEnd` mirrored. */
export function wordCutStart(text: string, limit: number): number {
  const from = Math.max(0, text.length - limit);
  if (from === 0) return 0;
  let start = from;
  if (!isWhitespace(text, from - 1)) {
    while (start < text.length && !isWhitespace(text, start)) start++;
  }
  while (start < text.length && isWhitespace(text, start)) start++;
  if (start < from + limit / 2) return start;
  start = isLowSurrogate(text.charCodeAt(from)) ? from + 1 : from;
  while (start < text.length - 1 && isWhitespace(text, start)) start++;
  return start;
}
