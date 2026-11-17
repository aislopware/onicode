import type { PullRequestChangeBreakdown } from "@onicode/contracts";
import { summarizeChangedFiles } from "@onicode/shared/changedFileKind";
import type { ProviderFileStats } from "@onicode/source-control-core/server/PullRequestProvider";

export function toChangeBreakdown(stats: ProviderFileStats): PullRequestChangeBreakdown {
  if (stats.truncated) return null;
  return summarizeChangedFiles(stats.files);
}
