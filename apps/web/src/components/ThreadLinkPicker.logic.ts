import type { EnvironmentId, OrchestrationV2ThreadShell } from "@onicode/contracts";

import { normalizeSearchText } from "~/lib/utils";

export function threadLinkMatcher(query: string, locale?: string) {
  const localQuery = query.trim().toLocaleLowerCase(locale);
  const foldedQuery = normalizeSearchText(query);
  return (text: string) =>
    text.toLocaleLowerCase(locale).includes(localQuery) ||
    // A query of combining marks alone folds to nothing, which every text includes.
    (foldedQuery !== "" && normalizeSearchText(text).includes(foldedQuery));
}

/**
 * The threads a pull request or an issue can be linked to: the environment's unarchived ones
 * whose `searchText` matches the query, newest first. Side chats are left out, as linked-thread
 * answers leave them out.
 */
export function threadLinkCandidates<
  T extends {
    readonly environmentId: EnvironmentId;
    readonly archivedAt: string | null;
    readonly updatedAt: string;
    readonly lineage: Pick<OrchestrationV2ThreadShell["lineage"], "sideChat">;
  },
>(
  threads: ReadonlyArray<T>,
  environmentId: EnvironmentId,
  query: string,
  searchText: (thread: T) => string,
  locale?: string,
): T[] {
  const matches = threadLinkMatcher(query, locale);
  return threads
    .filter(
      (thread) =>
        thread.environmentId === environmentId &&
        thread.archivedAt === null &&
        thread.lineage.sideChat !== true &&
        matches(searchText(thread)),
    )
    .toSorted((a, b) => b.updatedAt.localeCompare(a.updatedAt));
}
