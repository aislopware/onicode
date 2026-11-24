import type { OrchestrationV2AppThread, OrchestrationV2GeneratedTitle } from "@onicode/contracts";
import * as DateTime from "effect/DateTime";

// A subject that three runs did not reveal is not worth a generation after every run.
const MAX_SUBJECT_REFINEMENTS = 3;

type GeneratedTitleThread = OrchestrationV2AppThread & {
  readonly generatedTitle: OrchestrationV2GeneratedTitle;
};

/** The generator still owns the title, and no generation is running on this live thread. */
export function canRefreshGeneratedTitle(
  thread: OrchestrationV2AppThread,
): thread is GeneratedTitleThread {
  return (
    thread.generatedTitle != null &&
    thread.titleRegeneration == null &&
    thread.archivedAt === null &&
    thread.deletedAt === null
  );
}

/** The title does not name the subject yet, so each finished run is a chance to learn it. */
export function awaitsTitleSubject(thread: GeneratedTitleThread): boolean {
  return (
    thread.generatedTitle.needsRefinement &&
    thread.generatedTitle.refinements < MAX_SUBJECT_REFINEMENTS
  );
}

/**
 * Whether the thread holds something the title generator has not read. An unknown subject can
 * surface in any finished run; a known one moves only when someone asks the thread for more.
 */
export function generatedTitleIsStale(
  thread: GeneratedTitleThread,
  activity: {
    readonly latestRunCompletedAt: DateTime.Utc | null;
    readonly latestResumingMessageAt: DateTime.Utc | null;
  },
): boolean {
  if (activity.latestRunCompletedAt === null) return false;
  const coveredAt = thread.generatedTitle.coveredAt;
  if (coveredAt === null) return true;
  const latest = awaitsTitleSubject(thread)
    ? activity.latestRunCompletedAt
    : activity.latestResumingMessageAt;
  return latest !== null && DateTime.isGreaterThan(latest, coveredAt);
}
