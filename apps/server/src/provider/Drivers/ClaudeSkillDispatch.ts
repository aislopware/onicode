import type { ServerProviderSkill } from "@onicode/contracts";

/**
 * Claude expands one skill per message only when `/name` starts the text block.
 * Everything after the name, including newlines or a second `/x`, becomes `ARGUMENTS`.
 * https://github.com/anthropics/claude-code/issues/87113
 */

/** Match packages/shared/src/composerInlineTokens.ts so displayed chips and dispatched skills accept the same tokens. */
const SKILL_MENTION_PATTERN =
  /(^|\s)\p{Sc}(?![0-9][0-9_]*(?:[kKmMbBtT]|[eE][0-9]+)?(?:\s|$))(?=[a-zA-Z0-9:_-]*[a-zA-Z])([a-zA-Z0-9][a-zA-Z0-9:_-]*)(?=\s|$)/gu;

export const FRONTMATTER_PATTERN = /^---\r?\n([\s\S]*?)\r?\n---(?:\r?\n|$)/;

// In UTF-16 units: a zip member may hold 1 MiB, and several must not crowd out the request.
const INLINE_INSTRUCTIONS_BUDGET = 64 * 1024;

export type ClaudeSkillTarget = Pick<ServerProviderSkill, "path" | "userInvocationOnly"> & {
  /** The SKILL.md of a skill with no readable file, such as one inside a plugin zip. */
  readonly instructions?: string;
  /** The plugin zip that holds it. */
  readonly archive?: string;
};

export interface ClaudeSkillDispatch {
  /** The whole prompt with mentions as `/name`, or `undefined` when the mention opens it. */
  readonly leadingText: string | undefined;
  /** `/name` plus the trailing text, ready to be the message's last text block. */
  readonly commandText: string;
}

/** Files a zipped skill names beside its SKILL.md stay in the archive. */
function archivedFilesNote({ archive, path }: ClaudeSkillTarget): string {
  if (archive === undefined || !path.startsWith(archive)) return "";
  // Zip members always use `/`, whatever the host's separator.
  const member = path.slice(archive.length + 1).replaceAll("\\", "/");
  const folder = member.slice(0, member.lastIndexOf("/") + 1);
  return folder
    ? `\nIts other files are in ${folder} inside the archive ${archive}.`
    : `\nIts other files are inside the archive ${archive}.`;
}

export function planClaudeSkillDispatch(
  prompt: string,
  skills: ReadonlyMap<string, ClaudeSkillTarget>,
  /** Mentions before this offset, such as inline handoff history, stay plain text. */
  mentionsFrom = 0,
): ClaudeSkillDispatch | undefined {
  const mentions = [...prompt.matchAll(SKILL_MENTION_PATTERN)].flatMap((match) => {
    const name = match[2] ?? "";
    const skill = skills.get(name);
    const start = (match.index ?? 0) + (match[1]?.length ?? 0);
    if (!skill || start < mentionsFrom) return [];
    return [{ name, skill, start, end: (match.index ?? 0) + match[0].length }];
  });
  const last = mentions.at(-1);
  if (!last) {
    return undefined;
  }

  // Only the final skill gets native expansion. Earlier user-only skills need
  // a fallback to their raw instructions because Claude's Skill tool refuses them.
  const annotated = new Set([last.name]);
  let inlineBudget = INLINE_INSTRUCTIONS_BUDGET;
  let promptWithInlineSlashes = "";
  let cursor = 0;
  for (const mention of mentions) {
    promptWithInlineSlashes += `${prompt.slice(cursor, mention.start)}/${mention.name}`;
    if (mention.skill.userInvocationOnly === true && !annotated.has(mention.name)) {
      const { instructions, path } = mention.skill;
      // A skill inside a zip has no file Claude can read, so only its body can go along.
      const body = instructions?.replace(FRONTMATTER_PATTERN, "").trim();
      if (instructions === undefined) {
        promptWithInlineSlashes += ` [The user invoked this skill. Follow the instructions at: ${path}]`;
      } else if (body && inlineBudget === 0) {
        promptWithInlineSlashes += " [The user invoked this skill; its instructions did not fit.]";
      } else if (body) {
        const kept =
          body.length > inlineBudget
            ? `${body.slice(0, inlineBudget).replace(/[\uD800-\uDBFF]$/, "")}\n[Truncated]`
            : body;
        inlineBudget = Math.max(0, inlineBudget - body.length);
        promptWithInlineSlashes += ` [The user invoked this skill. Follow its instructions:\n<skill name="${mention.name}">\n${kept}\n</skill>${archivedFilesNote(mention.skill)}]`;
      }
      annotated.add(mention.name);
    }
    cursor = mention.end;
  }
  promptWithInlineSlashes = `${promptWithInlineSlashes}${prompt.slice(cursor)}`.trimEnd();

  return {
    leadingText:
      prompt.slice(0, last.start).trim().length > 0 ? promptWithInlineSlashes : undefined,
    commandText: `/${last.name}${prompt.slice(last.end)}`.trimEnd(),
  };
}
