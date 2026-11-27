import {
  pullRequestConversationThreads,
  type PullRequestActivity,
  type PullRequestDetail,
} from "@onicode/contracts";
import type { ReviewGuideGenerationInput } from "@onicode/provider-core/server/textGeneration";
import { splitsSurrogatePair } from "@onicode/shared/assistantCitations";

const SETTLED_REMARK_CHARS = 200;

function oneLine(text: string): string {
  const flat = text
    .replace(/<!--[\s\S]*?-->/g, "")
    .replace(/\s+/g, " ")
    .trim();
  if (flat.length <= SETTLED_REMARK_CHARS) return flat;
  const end = splitsSurrogatePair(flat, SETTLED_REMARK_CHARS)
    ? SETTLED_REMARK_CHARS - 1
    : SETTLED_REMARK_CHARS;
  return `${flat.slice(0, end)}…`;
}

function settledReviewSummary(activity: PullRequestActivity): string {
  return pullRequestConversationThreads(activity)
    .filter((thread) => thread.isResolved || thread.isOutdated)
    .flatMap((thread) => {
      const opener = thread.comments[0];
      if (opener === undefined) return [];
      const who = opener.author?.login ?? "ghost";
      const state = thread.isResolved ? "resolved" : "outdated";
      return [`- ${thread.path ?? "PR overview"} (${state}, ${who}): ${oneLine(opener.body)}`];
    })
    .join("\n");
}

export function reviewGuideInputFromPullRequest(
  detail: PullRequestDetail,
  activity: PullRequestActivity,
  baseCommit: string,
): Omit<ReviewGuideGenerationInput, "cwd" | "projectCwd" | "modelSelection"> {
  return {
    number: detail.number,
    title: detail.title,
    description: detail.body,
    baseBranch: detail.baseBranch,
    baseCommit,
    settledReview: settledReviewSummary(activity),
  };
}
