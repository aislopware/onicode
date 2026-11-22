import { GitCommandError, type ProjectId } from "@onicode/contracts";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Layer from "effect/Layer";
import * as Path from "effect/Path";

import * as ProjectionStore from "../orchestration-v2/ProjectionStore.ts";
import * as ProjectStore from "../orchestration-v2/ProjectStore.ts";
import * as VcsDriverRegistry from "../vcs/VcsDriverRegistry.ts";
import { canonicalize, contains } from "../workspace/workspaceLease.ts";

function describeBusyCheckout(titles: ReadonlyArray<string>): string {
  const quoted = titles.map((title) => `'${title}'`);
  const names =
    quoted.length <= 2
      ? quoted.join(" and ")
      : `${quoted.slice(0, 2).join(", ")} and ${quoted.length - 2} more`;
  return `${names} ${quoted.length === 1 ? "is" : "are"} still working in this checkout. Wait for ${quoted.length === 1 ? "it" : "them"} to finish, or use a worktree for a separate branch.`;
}

/** Switching a shared checkout's ref moves every thread working in it, mid-turn included. */
export class SharedCheckoutGuard extends Context.Service<
  SharedCheckoutGuard,
  {
    /** Fails while a thread with a run in progress works in the checkout at `root`. */
    readonly assertIdle: (input: {
      readonly operation: string;
      readonly cwd: string;
      readonly root: string;
    }) => Effect.Effect<void, GitCommandError>;
  }
>()("onicode/git/sharedCheckoutGuard") {}

const make = Effect.gen(function* () {
  const projections = yield* ProjectionStore.ProjectionStoreV2;
  const projects = yield* ProjectStore.ProjectStoreV2;
  const registry = yield* VcsDriverRegistry.VcsDriverRegistry;
  const fileSystem = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;

  const repositoryRoot = (cwd: string) =>
    registry
      .detect({ cwd })
      .pipe(
        Effect.flatMap((handle) =>
          handle?.kind === "git"
            ? canonicalize(fileSystem, path, handle.repository.rootPath)
            : Effect.succeed(null),
        ),
      );

  const assertIdle: SharedCheckoutGuard["Service"]["assertIdle"] = Effect.fn(
    "SharedCheckoutGuard.assertIdle",
  )(function* ({ operation, cwd, root }) {
    const unverified = (cause: unknown) =>
      new GitCommandError({
        operation,
        command: "git checkout",
        cwd,
        detail: "Could not check whether a thread is working in this checkout.",
        cause,
      });
    const threads = yield* projections.listThreadsWithActiveRun().pipe(Effect.mapError(unverified));
    if (threads.length === 0) return;
    const localProjectIds = [
      ...new Set(
        threads.flatMap((thread) => (thread.worktreePath === null ? [thread.projectId] : [])),
      ),
    ];
    const projectRoots = new Map<ProjectId, string>(
      localProjectIds.length === 0
        ? []
        : (yield* projects
            .list({ projectIds: localProjectIds, includeDeleted: true })
            .pipe(Effect.mapError(unverified))).map((project) => [
            project.projectId,
            project.workspaceRoot,
          ]),
    );
    const checkout = yield* canonicalize(fileSystem, path, root);
    // An unreadable repository counts as this one: the switch must not pass on a guess.
    const sameRepository = (folder: string) =>
      repositoryRoot(folder).pipe(
        Effect.map(
          (folderRoot) => folderRoot === null || path.relative(folderRoot, checkout) === "",
        ),
        Effect.orElseSucceed(() => true),
      );
    const busy = yield* Effect.filter(
      threads,
      (thread) =>
        Effect.gen(function* () {
          const own = thread.worktreePath ?? projectRoots.get(thread.projectId);
          if (own === undefined) return false;
          const folder = yield* canonicalize(fileSystem, path, path.resolve(own));
          // A project in a subdirectory shares the checkout, unless it is a repository of its own.
          if (contains(path, checkout, folder)) {
            return path.relative(checkout, folder) === "" || (yield* sameRepository(folder));
          }
          // A task folder or a folder of repositories reaches the checkouts inside it.
          return (
            contains(path, folder, checkout) &&
            (yield* repositoryRoot(folder).pipe(
              Effect.map((folderRoot) => folderRoot === null),
              Effect.orElseSucceed(() => true),
            ))
          );
        }),
      { concurrency: "unbounded" },
    );
    if (busy.length === 0) return;
    return yield* new GitCommandError({
      operation,
      command: "git checkout",
      cwd,
      detail: describeBusyCheckout(busy.map((thread) => thread.title)),
    });
  });

  return SharedCheckoutGuard.of({ assertIdle });
});

export const layer = Layer.effect(SharedCheckoutGuard, make);
