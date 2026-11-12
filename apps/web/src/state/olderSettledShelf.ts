import { useAtomValue } from "@effect/atom-react";
import type { EnvironmentThreadShell } from "@onicode/client-runtime/state/models";
import {
  isListedSettledThread,
  settledShelfFrontier,
  sortOlderSettledScopes,
  unloadedOlderSettledCount,
  wantedOlderSettledPages,
  type OlderSettledScope,
} from "@onicode/client-runtime/state/threads";
import type { EnvironmentId, ProjectId } from "@onicode/contracts";
import { useEffect, useState } from "react";

import { appAtomRegistry } from "../rpc/atomRegistry";
import { olderSettledThreads, threadFamilies } from "./threads";

const NO_PAGES: ReadonlyMap<EnvironmentId, number> = new Map();

/**
 * The Settled shelf for one scope when shells leave older settled threads out: which rows it may
 * list, how many threads it holds in all, and the pages read below each environment's cut, whose
 * rows merge into the thread list. `useOlderSettledShelfGrowth` reads the next pages.
 */
export function useOlderSettledShelf(input: {
  readonly scopes: ReadonlyArray<OlderSettledScope>;
  /** A collapsed shelf mounts no pages, so a reconnect reads none; expanding restores them. */
  readonly expanded: boolean;
  /** Rows the shelf shows; pages are read until it lists that many. */
  readonly visibleCount: number;
}) {
  // The compiler caches what a hook derives from its arguments, which it knows are never mutated,
  // but not from a list sorted between hook calls. So the sorted scopes arrive as an argument.
  return useSortedOlderSettledShelf(
    sortOlderSettledScopes(input.scopes),
    input.expanded,
    input.visibleCount,
  );
}

function useSortedOlderSettledShelf(
  scopes: ReadonlyArray<OlderSettledScope>,
  expanded: boolean,
  visibleCount: number,
) {
  const scopeKey = JSON.stringify(scopes);
  const [requested, setRequested] = useState({ scopeKey, pages: NO_PAGES });
  const pages = requested.scopeKey === scopeKey && expanded ? requested.pages : NO_PAGES;
  const shelfScopes = scopes.map((scope) => ({
    scope,
    pages: pages.get(scope.environmentId) ?? 0,
  }));
  const shelfKey = JSON.stringify(shelfScopes);
  const shelf = useAtomValue(olderSettledThreads.shelfAtom(shelfScopes));
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- mounts the requested older settled pages into the thread family registry while shown
  useEffect(() => {
    const releases = (JSON.parse(shelfKey) as typeof shelfScopes).flatMap(({ scope, pages }) =>
      pages === 0
        ? []
        : [
            threadFamilies.mountRows(
              appAtomRegistry,
              scope.environmentId,
              olderSettledThreads.rowsKey(scope, pages),
              olderSettledThreads.pageRowsAtom(scope, pages),
            ),
          ],
    );
    return () => {
      for (const release of releases) release();
    };
  }, [shelfKey]);

  const olderOf = (environmentId: EnvironmentId) => shelf.get(environmentId)?.older ?? null;
  const pagesOf = (environmentId: EnvironmentId) => shelf.get(environmentId)?.pages;
  const frontier = settledShelfFrontier([...shelf.values()]);
  const loaded = scopes.flatMap((scope) => {
    const count = pages.get(scope.environmentId) ?? 0;
    return count > 0 ? [{ scope, pages: count }] : [];
  });
  return {
    environments: shelf,
    visibleCount: expanded ? visibleCount : 0,
    pages,
    grow: (next: ReadonlyMap<EnvironmentId, number>) => setRequested({ scopeKey, pages: next }),
    isListed: (thread: EnvironmentThreadShell) => isListedSettledThread(thread.source, frontier),
    /** Older settled threads in scope that are not local; add it to the local Settled rows. */
    unloadedCount: (threads: ReadonlyArray<EnvironmentThreadShell>) =>
      unloadedOlderSettledCount({ olderSettled: olderOf, scopes, threads }),
    loading: loaded.some(({ scope }) => pagesOf(scope.environmentId)?.status === "pending"),
    failed: loaded.some(({ scope }) => pagesOf(scope.environmentId)?.status === "failed"),
    retry: () => {
      for (const { scope, pages } of loaded) {
        olderSettledThreads.retryPages(appAtomRegistry, scope, pages);
      }
    },
  };
}

/** Reads the next pages while the shelf lists fewer rows than it shows. */
export function useOlderSettledShelfGrowth(
  shelf: ReturnType<typeof useOlderSettledShelf>,
  listedSettledThreads: ReadonlyArray<EnvironmentThreadShell>,
) {
  const wanted = wantedOlderSettledPages({
    requested: shelf.pages,
    environments: shelf.environments,
    listedCount: listedSettledThreads.length,
    visibleCount: shelf.visibleCount,
  });
  const { grow, pages } = shelf;
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- requests more older settled pages when the listed threads cannot fill the visible count
  useEffect(() => {
    if (wanted !== pages) grow(wanted);
  }, [wanted, pages, grow]);
}

/**
 * Threads of these projects that this client does not hold (older settled threads), for counts
 * shown outside the shelf, such as a removal confirm.
 */
export function readUnloadedThreadCount(
  members: ReadonlyArray<{ readonly environmentId: EnvironmentId; readonly id: ProjectId }>,
  threads: ReadonlyArray<EnvironmentThreadShell>,
): number {
  return unloadedOlderSettledCount({
    olderSettled: (environmentId) =>
      appAtomRegistry.get(olderSettledThreads.olderSettledAtom(environmentId)),
    scopes: [...new Set(members.map((member) => member.environmentId))].map((environmentId) => ({
      environmentId,
      projectIds: members.flatMap((member) =>
        member.environmentId === environmentId ? [member.id] : [],
      ),
    })),
    threads: threads.filter((thread) =>
      members.some(
        (member) => member.environmentId === thread.environmentId && member.id === thread.projectId,
      ),
    ),
  });
}
