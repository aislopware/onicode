import { EnvironmentId, ThreadId, type ScopedThreadRef } from "@onicode/contracts";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";

export const THREAD_LINK_PROTOCOL = "oni-thread";
const V1_PREFIX = `${THREAD_LINK_PROTOCOL}://v1/`;
const V2_PREFIX = `${THREAD_LINK_PROTOCOL}://v2/`;
const THREAD_HREF = String.raw`oni-thread:\/\/v[12]\/`;
// A label stops at any bracket, so a run of unclosed brackets is scanned once, not once per bracket.
const LABEL = String.raw`[^[\]\n]*`;
const LIST_MARKERS = String.raw`(?:(?:[-*+]|\d{1,9}[.)])[ \t]+)*`;
// CommonMark fences start on their own line (optional blockquotes, indentation, or list markers)
// and close on a line with at least as many of the same marker; backtick info strings hold no `.
// Quote markers are compared from the first `>`, so lines may indent them differently.
// Space after the markers is matched only after a `>`: two adjacent `[ \t]*` backtrack quadratically.
const AFTER_QUOTE = String.raw`(?:(?<=>)[ \t]*)?`;
const fencedCode = (char: "`" | "~", name: string) => {
  const bq = `${name}Bq`;
  const info = char === "`" ? "[^`\\r\\n]*" : "[^\\r\\n]*";
  const closer = String.raw`[ \t]*\k<${bq}>${AFTER_QUOTE}\k<${name}>${char}*(?!${char})[ \t]*\r?$`;
  return String.raw`^[ \t]*${LIST_MARKERS}(?<${bq}>(?:>(?:[ \t]*>)*)?)(?:(?<=>)[ \t]*${LIST_MARKERS})?(?<${name}>${char}{3,}(?!${char}))${info}(?=\r?$)(?:\r?\n(?!${closer})[ \t]*\k<${bq}>[^\r\n]*)*(?:\r?\n${closer})?`;
};
// A code span may wrap lines but ends where its paragraph does: a blank line, heading, list item, or fence.
const SPAN_LINE_BREAK = String.raw`\n(?![ \t>]*(?:\r?$|#{1,6}(?:[ \t]|\r?$)|(?:[-*+]|\d{1,9}[.)])[ \t]|\`{3,}[^\`\r\n]*\r?$|~{3,}))`;
// Earlier alternatives claim text so later ones cannot match inside it: an escaped character opens
// nothing, code is literal, and a link to anywhere else keeps a thread URL in its label or href.
const THREAD_LINK_OUTSIDE_CODE = new RegExp(
  [
    String.raw`\\[!-/:-@[-\`{-~]`,
    fencedCode("`", "bt"),
    fencedCode("~", "tl"),
    String.raw`(?<span>(?<spanTicks>\`+)(?!\`)(?:[^\n]|${SPAN_LINE_BREAK})*?(?<!\`)\k<spanTicks>(?!\`))`,
    // An unmatched backtick run is literal as a whole, so no span opens partway through it.
    String.raw`\`+`,
    // A labeled link's destination may be angle-bracketed, as CommonMark allows: [label](<href>).
    String.raw`\[(?<label>${LABEL})\]\((?:<(?<angleHref>${THREAD_HREF}[^\s<>]+)>|(?<href>${THREAD_HREF}[^\s>)]+))\)`,
    String.raw`\[${LABEL}\]\((?:<[^<>\n]*>|[^\s()]*(?:\([^\s()]*\)[^\s()]*)*)(?:[ \t]+(?:"[^"\n]*"|'[^'\n]*'))?\)`,
    String.raw`<(?<autolink>${THREAD_HREF}[^\s<>]+)>`,
  ].join("|"),
  "gm",
);

const decodeThreadId = Schema.decodeUnknownOption(ThreadId);
const decodeEnvironmentId = Schema.decodeUnknownOption(EnvironmentId);

/** A v1 link names no environment; the message's own, or a unique match, stands in. */
export interface ThreadLink {
  readonly environmentId: EnvironmentId | null;
  readonly threadId: ThreadId;
}

/** The ids as written. Thread ids can hold percent escapes of their own, so they are not decoded. */
export function parseThreadLinkHref(href: string): ThreadLink | null {
  if (href.startsWith(V1_PREFIX)) {
    const threadId = Option.getOrNull(decodeThreadId(href.slice(V1_PREFIX.length)));
    return threadId === null ? null : { environmentId: null, threadId };
  }
  if (!href.startsWith(V2_PREFIX)) return null;
  const path = href.slice(V2_PREFIX.length);
  const slash = path.indexOf("/");
  if (slash < 0) return null;
  const environmentId = Option.getOrNull(decodeEnvironmentId(path.slice(0, slash)));
  const threadId = Option.getOrNull(decodeThreadId(path.slice(slash + 1)));
  return environmentId === null || threadId === null ? null : { environmentId, threadId };
}

function percentDecodedThreadLinkId(threadId: ThreadId): ThreadId | null {
  try {
    const decoded = decodeURIComponent(threadId);
    return decoded === threadId ? null : Option.getOrNull(decodeThreadId(decoded));
  } catch {
    return null;
  }
}

/** The threads a link can name, in its own environment or else the message's: as written, then decoded. */
export function threadLinkCandidates(
  link: ThreadLink,
  messageEnvironmentId: EnvironmentId,
): ReadonlyArray<ScopedThreadRef> {
  const environmentId = link.environmentId ?? messageEnvironmentId;
  const decodedId = percentDecodedThreadLinkId(link.threadId);
  const written = { environmentId, threadId: link.threadId };
  return decodedId === null ? [written] : [written, { environmentId, threadId: decodedId }];
}

/** A v1 link its message's environment lacks opens a unique match elsewhere; a v2 link never does. */
export function guessThreadLinkRef(
  link: ThreadLink,
  refs: ReadonlyArray<ScopedThreadRef>,
): ScopedThreadRef | null {
  if (link.environmentId !== null) return null;
  const decodedId = percentDecodedThreadLinkId(link.threadId);
  for (const threadId of decodedId === null ? [link.threadId] : [link.threadId, decodedId]) {
    let match: ScopedThreadRef | null = null;
    for (const ref of refs) {
      if (ref.threadId !== threadId) continue;
      if (match !== null) return null;
      match = ref;
    }
    if (match !== null) return match;
  }
  return null;
}

function threadLinkHref(link: ThreadLink): string {
  return link.environmentId === null
    ? `${V1_PREFIX}${link.threadId}`
    : `${V2_PREFIX}${link.environmentId}/${link.threadId}`;
}

const linkLabel = (label: string | undefined) =>
  label
    ?.replace(/[[\]\\\r\n]/g, " ")
    .replace(/\s+/g, " ")
    .trim();

/** Without a label, an autolink: clients show the thread's current title. */
export function formatThreadLink(ref: ThreadLink, label?: string): string {
  const cleaned = linkLabel(label);
  const href = threadLinkHref(ref);
  const destination = /[()]/.test(href) ? `<${href}>` : href;
  return cleaned ? `[${cleaned}](${destination})` : `<${href}>`;
}

export function hasThreadLinks(markdown: string): boolean {
  return markdown.includes(`${THREAD_LINK_PROTOCOL}://v`);
}

/** Every thread the markdown's links can name, so their titles are keyed reads. */
export function threadLinkRefs(
  markdown: string,
  messageEnvironmentId: EnvironmentId,
): ReadonlyArray<ScopedThreadRef> {
  const refs = new Map<string, ScopedThreadRef>();
  // Hermes' matchAll yields matches without `groups`; exec keeps them.
  const pattern = new RegExp(THREAD_LINK_OUTSIDE_CODE);
  for (let match = pattern.exec(markdown); match !== null; match = pattern.exec(markdown)) {
    const href = match.groups?.angleHref ?? match.groups?.href ?? match.groups?.autolink;
    const link = href === undefined ? null : parseThreadLinkHref(href);
    if (link === null) continue;
    for (const ref of threadLinkCandidates(link, messageEnvironmentId)) {
      refs.set(`${ref.environmentId}/${ref.threadId}`, ref);
    }
  }
  return [...refs.values()];
}

/** Rewrites each link outside code `replace` answers for. `label` is a Markdown link's own text. */
export function replaceThreadLinks(
  markdown: string,
  replace: (link: ThreadLink, label: string | undefined) => string | undefined,
): string {
  if (!hasThreadLinks(markdown)) return markdown;
  return markdown.replace(THREAD_LINK_OUTSIDE_CODE, (source, ...args) => {
    const groups = args.at(-1) as Partial<
      Record<"angleHref" | "href" | "autolink" | "label", string>
    >;
    const href = groups.angleHref ?? groups.href ?? groups.autolink;
    const link = href === undefined ? null : parseThreadLinkHref(href);
    return (link && replace(link, groups.label)) ?? source;
  });
}
