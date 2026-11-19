import type { EnvironmentId, OrchestrationV2ThreadShell } from "@onicode/contracts";

/**
 * The threads a pull request or an issue can be linked to: the environment's unarchived ones
 * whose `searchText` holds the query, newest first. Side chats are left out, as linked-thread
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
): T[] {
  const search = query.trim().toLocaleLowerCase();
  return threads
    .filter(
      (thread) =>
        thread.environmentId === environmentId &&
        thread.archivedAt === null &&
        thread.lineage.sideChat !== true &&
        searchText(thread).toLocaleLowerCase().includes(search),
    )
    .toSorted((a, b) => b.updatedAt.localeCompare(a.updatedAt));
}
