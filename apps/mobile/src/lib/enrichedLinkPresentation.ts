import type { ComposerContextRecord, EnvironmentId, ThreadId } from "@onicode/contracts";
import {
  composerChipSizeSuffix,
  contextChipIcon,
  contextChipPresentation,
  contextRecordFileName,
  type ContextChipIconRef,
  UNAVAILABLE_CONTEXT_CHIP_PRESENTATION,
} from "./composerChipPresentation";
import type { MessageResolver } from "../i18n/messages";
import { formatComposerModelMention } from "@onicode/client-runtime/composer-model-mentions";
import type {
  SelectableMarkdownModel,
  SelectableMarkdownSkill,
} from "../native/SelectableMarkdownText.types";
import {
  formatComposerContextHref,
  parseComposerContextHref,
} from "@onicode/shared/composerContextReferences";
import { hasThreadLinks, parseThreadLinkHref } from "@onicode/shared/threadLinks";
import { EXTENSIONLESS_FILE_NAMES } from "@onicode/shared/markdownLinks";

export function enrichedContextLinkPresentation(
  href: string,
  records?: ReadonlyArray<ComposerContextRecord>,
  resolve?: MessageResolver,
): { accent?: string; icon: ContextChipIconRef; label?: string } | undefined {
  const reference = parseComposerContextHref(href);
  if (!reference) return undefined;
  const record = records?.find((candidate) => candidate.contextId === reference.contextId);
  const presentation =
    records && !record
      ? UNAVAILABLE_CONTEXT_CHIP_PRESENTATION
      : contextChipPresentation(reference.kind, record);
  const size = composerChipSizeSuffix(record, resolve);
  return {
    ...(presentation.accent ? { accent: presentation.accent } : {}),
    icon: contextChipIcon(presentation, contextRecordFileName(record)),
    ...(record && size ? { label: `${record.label} · ${size}` } : {}),
  };
}

export function enrichedThreadLinkPresentation(
  href: string,
): { accent?: string; icon: ContextChipIconRef } | undefined {
  if (!parseThreadLinkHref(href)) return undefined;
  const presentation = contextChipPresentation("thread");
  return {
    ...(presentation.accent ? { accent: presentation.accent } : {}),
    icon: contextChipIcon(presentation, ""),
  };
}

export interface ThreadChipTarget {
  readonly href: string;
  readonly environmentId: EnvironmentId;
  readonly threadId: ThreadId;
  readonly link: boolean;
}

const THREAD_HREF = String.raw`oni-thread:\/\/v[123]\/`;
const PUNCTUATION = String.raw`[!-\/:-@[-\x60{-~]`;
const ESCAPED_PUNCTUATION = new RegExp(String.raw`\\(${PUNCTUATION})`, "g");
const IS_PUNCTUATION = new RegExp(`^${PUNCTUATION}$`);
// Keys are the destinations the renderer reports, angle brackets and escapes dropped: an inline
// link's or a reference definition's, in a quote or list too. Bare ones are read by `bareDestination`.
const THREAD_CHIP_HREF = new RegExp(
  String.raw`(?:\]\([ \t]*(?:\r?\n[ \t>]*)?|^(?:[ \t>]|[-*+](?=[ \t])|\d{1,9}[.)](?=[ \t]))*\[[^\]\n]+\]:[ \t]*(?:\r?\n[ \t>]*)?)(?:<(${THREAD_HREF}[^\s<>]+)>|(?=${THREAD_HREF}))`,
  "gm",
);
// md4c's limit on nested parentheses in a bare destination.
const MAX_PARENTHESIS_DEPTH = 32;

/** A bare destination from `start`: no space or control character, parentheses balanced or escaped. */
function bareDestination(text: string, start: number): { href: string; end: number } | null {
  let depth = 0;
  let index = start;
  for (; index < text.length; index += 1) {
    const char = text[index]!;
    if (char === "\\" && IS_PUNCTUATION.test(text[index + 1] ?? "")) index += 1;
    else if (char <= " " || char === "\x7f") break;
    else if (char === "(") {
      depth += 1;
      if (depth > MAX_PARENTHESIS_DEPTH) return null;
    } else if (char === ")") {
      if (depth === 0) break;
      depth -= 1;
    }
  }
  return depth === 0 ? { href: text.slice(start, index), end: index } : null;
}

export function threadChipTargets(
  environmentId: EnvironmentId,
  markdown: string,
  records?: ReadonlyArray<ComposerContextRecord>,
): ReadonlyArray<ThreadChipTarget> {
  const targets = new Map<string, ThreadChipTarget>();
  if (hasThreadLinks(markdown)) {
    for (const match of markdown.matchAll(THREAD_CHIP_HREF)) {
      const [whole, angled] = match;
      let destination = angled;
      if (destination === undefined) {
        const bare = bareDestination(markdown, match.index + whole.length);
        // One that stops short of a space, or an inline link's `)`, never parsed.
        const after = bare === null ? "" : markdown[bare.end];
        const ends =
          after === undefined || /\s/.test(after) || (after === ")" && whole.startsWith("]"));
        if (bare === null || !ends) continue;
        destination = bare.href;
      }
      const href = destination.replace(ESCAPED_PUNCTUATION, "$1");
      const link = parseThreadLinkHref(href);
      if (link) {
        targets.set(href, {
          href,
          environmentId: link.environmentId ?? environmentId,
          threadId: link.threadId,
          link: true,
        });
      }
    }
  }
  for (const record of records ?? []) {
    if (record.kind !== "thread" || !("threadId" in record)) continue;
    const href = formatComposerContextHref("thread", record.contextId);
    targets.set(href, {
      href,
      environmentId: record.environmentId,
      threadId: record.threadId,
      link: false,
    });
  }
  return [...targets.values()];
}

export function enrichedLinkVariantPattern(url: string) {
  return `^${url.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}$`;
}

const EXTENSIONLESS_FILE_NAME_PATTERN = [...EXTENSIONLESS_FILE_NAMES]
  .map((name) => name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"))
  .join("|");

// Folder and file names take the shared parser's routing punctuation (`(group)/[id]/+page.svelte`).
export const ENRICHED_INLINE_FILE_LINK_REGEX = new RegExp(
  String.raw`^(?!\S*:\/\/)(?:(?:\.{1,2}[\\/]|~[\\/]|\/(?:Users|home|tmp|var|etc|opt|mnt|Volumes|private|root|usr|bin|sbin|lib|lib64|srv|dev|proc|sys|run|boot|media|workspace|workspaces)\/|[A-Za-z]:[\\/])[^\s\x60]+|(?:[\p{L}\p{M}\p{N}._$+@()[\]-]+[\\/])+(?:[\p{L}\p{M}\p{N}._$+@()[\]-]+\.[A-Za-z0-9_-]+|(?:${EXTENSIONLESS_FILE_NAME_PATTERN}))(?::\d+(?::\d+|-\d+)?)?|[\p{L}\p{M}\p{N}._$+()[\]-]+\.[A-Za-z0-9_-]+:\d+(?::\d+|-\d+)?)$`,
  "u",
);

export function enrichedSkillLinkRegex(skills?: ReadonlyArray<SelectableMarkdownSkill>) {
  const names = [...new Set(skills?.map((skill) => skill.name) ?? [])].filter(Boolean);
  if (names.length === 0) return null;
  names.sort((a, b) => b.length - a.length);
  const alternatives = names.map((name) => name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"));
  return new RegExp(
    `(?<![A-Za-z0-9_-])(?<!\\p{Sc})\\p{Sc}(?:${alternatives.join("|")})(?![A-Za-z0-9_-])`,
    "u",
  );
}

export function enrichedSkillReference(
  url: string,
  skills?: ReadonlyArray<SelectableMarkdownSkill>,
) {
  const match = /^\p{Sc}(.+)$/u.exec(url);
  return match ? skills?.find((skill) => skill.name === match[1]) : undefined;
}

export function buildEnrichedModelReferences(
  providers: ReadonlyArray<{
    readonly instanceId: string;
    readonly driver: string;
    readonly displayName?: string | undefined;
    readonly iconUrl?: string | undefined;
    readonly models: ReadonlyArray<{
      readonly slug: string;
      readonly name: string;
      readonly subProvider?: string | undefined;
    }>;
  }>,
  sources?: ReadonlySet<string>,
): ReadonlyArray<SelectableMarkdownModel> {
  return providers.flatMap((provider) =>
    provider.models
      .filter(
        (model) =>
          !sources || sources.has(formatComposerModelMention(provider.instanceId, model.slug)),
      )
      .map((model) => {
        const qualifier = model.subProvider?.trim();
        const label = qualifier
          ? model.name
              .replace(
                new RegExp(
                  `^${qualifier.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}(?:\\s*[.:/-]\\s*|\\s+)`,
                  "iu",
                ),
                "",
              )
              .trim() || model.name
          : model.name;
        return {
          source: formatComposerModelMention(provider.instanceId, model.slug),
          label,
          providerLabel: qualifier || provider.displayName || provider.instanceId,
          driver: provider.driver,
          iconUrl: provider.iconUrl,
        };
      }),
  );
}

export function enrichedModelLinkRegex(models?: ReadonlyArray<SelectableMarkdownModel>) {
  if (!models?.length) return null;
  const alternatives = models
    .map((model) => model.source)
    .sort((a, b) => b.length - a.length)
    .map((source) => source.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"));
  return new RegExp(
    `(?<![^\\s,(\\["'])(?:${alternatives.join("|")})(?=$|[\\s,"']|[.;:!?\\)\\]]+(?=$|[\\s,"']))`,
    "u",
  );
}

export function enrichedModelReference(
  source: string,
  models?: ReadonlyArray<SelectableMarkdownModel>,
) {
  return models?.find((model) => model.source === source);
}

export function enrichedModelDetails(model: SelectableMarkdownModel) {
  return `${model.providerLabel}\n${model.source}`;
}

export function enrichedTextLinkRegex(
  skills?: ReadonlyArray<SelectableMarkdownSkill>,
  models?: ReadonlyArray<SelectableMarkdownModel>,
) {
  const patterns = [enrichedSkillLinkRegex(skills), enrichedModelLinkRegex(models)]
    .filter((pattern) => pattern !== null)
    .map((pattern) => `(?:${pattern.source})`);
  return patterns.length ? new RegExp(patterns.join("|"), "u") : null;
}
