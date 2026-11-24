import {
  pullRequestConversationThreads,
  type PullRequestActivity,
  type PullRequestDetail,
} from "@onicode/contracts";
import type { ReviewGuideGenerationInput } from "@onicode/provider-core/server/textGeneration";

const SETTLED_REMARK_CHARS = 200;

function oneLine(text: string): string {
  const flat = text
    .replace(/<!--[\s\S]*?-->/g, "")
    .replace(/\s+/g, " ")
    .trim();
  return flat.length > SETTLED_REMARK_CHARS ? `${flat.slice(0, SETTLED_REMARK_CHARS)}…` : flat;
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
