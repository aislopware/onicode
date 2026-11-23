import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as FileSystem from "effect/FileSystem";
import * as Path from "effect/Path";

import {
  GitManagerError,
  GitCommandError,
  type VcsSwitchRefInput,
  type VcsSwitchRefResult,
  type VcsCreateRefInput,
  type VcsCreateRefResult,
  type VcsCreateWorktreeInput,
  type VcsCreateWorktreeResult,
  type VcsListRefsInput,
  type VcsListRefsResult,
  type GitManagerServiceError,
  type GitPreparePullRequestThreadInput,
  type GitPreparePullRequestThreadResult,
  type GitPullRequestRefInput,
  type VcsPullResult,
  type VcsRemoveWorktreeInput,
  type GitResolvePullRequestResult,
  type GitRunStackedActionInput,
  type GitRunStackedActionResult,
  type VcsStatusInput,
  type VcsStatusLocalResult,
  type VcsStatusResult,
} from "@onicode/contracts";

import { trimAsciiWhitespace } from "@onicode/contracts";
import * as GitManager from "./GitManager.ts";
import * as SharedCheckoutGuard from "./sharedCheckoutGuard.ts";
import * as GitVcsDriver from "../vcs/GitVcsDriver.ts";
import * as VcsDriverRegistry from "../vcs/VcsDriverRegistry.ts";
import {
  withCheckoutSwitchReservation,
  withCheckoutSwitchReservationOrElse,
  withWorkspaceLease,
  withWorkspaceLeaseOrElse,
} from "../workspace/workspaceLease.ts";

export class GitWorkflowService extends Context.Service<
  GitWorkflowService,
  {
    readonly repositoryRoot: (cwd: string) => Effect.Effect<string | null, GitManagerServiceError>;
    readonly hasCommit: (input: {
      readonly cwd: string;
      readonly refName: string;
    }) => Effect.Effect<boolean, GitCommandError>;
    readonly hasRefNamed: (input: {
      readonly cwd: string;
      readonly refName: string;
    }) => Effect.Effect<boolean, GitCommandError>;
    readonly status: (
      input: VcsStatusInput,
    ) => Effect.Effect<VcsStatusResult, GitManagerServiceError>;
    readonly localStatus: (
      input: VcsStatusInput,
      options?: GitManager.GitLocalStatusOptions,
    ) => Effect.Effect<VcsStatusLocalResult, GitManagerServiceError>;
    readonly currentBranch: (cwd: string) => Effect.Effect<string | null, GitManagerServiceError>;
    readonly upstreamDivergence: (cwd: string) => Effect.Effect<
      {
        readonly refName: string;
        readonly aheadCount: number;
        readonly behindCount: number;
        readonly details: GitVcsDriver.GitRemoteStatusDetails;
      } | null,
      GitManagerServiceError
    >;
    readonly remoteStatus: (
      input: VcsStatusInput,
      options?: GitManager.GitRemoteStatusOptions,
    ) => Effect.Effect<GitManager.GitRemoteStatusRead | null, GitManagerServiceError>;
    readonly invalidateLocalStatus: (cwd: string) => Effect.Effect<void, never>;
    readonly invalidateRemoteStatus: (cwd: string) => Effect.Effect<void, never>;
    readonly readLocalStatusRefSnapshot: GitVcsDriver.GitVcsDriver["Service"]["readLocalStatusRefSnapshot"];
    readonly readRefSnapshot: GitVcsDriver.GitVcsDriver["Service"]["readRefSnapshot"];
    readonly invalidateStatus: (cwd: string) => Effect.Effect<void, never>;
    /** The checkout's own HEAD file, inside a linked worktree's admin directory when there is one. */
    readonly headPath: (cwd: string) => Effect.Effect<string | null, never>;
    readonly pullCurrentBranch: (cwd: string) => Effect.Effect<VcsPullResult, GitCommandError>;
    /**
     * For automatic pulls: null, without pulling, while the checkout is busy or no longer a clean
     * `branch` without local commits.
     */
    readonly pullCurrentBranchWhenIdle: (
      cwd: string,
      branch: string,
    ) => Effect.Effect<VcsPullResult | null, GitCommandError>;
    readonly runStackedAction: (
      input: GitRunStackedActionInput,
      options?: GitManager.GitRunStackedActionOptions,
    ) => Effect.Effect<GitRunStackedActionResult, GitManagerServiceError>;
    readonly resolvePullRequest: (
      input: GitPullRequestRefInput,
    ) => Effect.Effect<GitResolvePullRequestResult, GitManagerServiceError>;
    readonly preparePullRequestThread: (
      input: GitPreparePullRequestThreadInput,
    ) => Effect.Effect<GitPreparePullRequestThreadResult, GitManagerServiceError>;
    readonly listRefs: (
      input: VcsListRefsInput,
    ) => Effect.Effect<VcsListRefsResult, GitCommandError>;
    readonly createWorktree: (
      input: VcsCreateWorktreeInput,
      options?: GitVcsDriver.CreateWorktreeOptions,
    ) => Effect.Effect<VcsCreateWorktreeResult, GitCommandError>;
    readonly listLocalBranchNames: (cwd: string) => Effect.Effect<string[], GitCommandError>;
    readonly fetchRemote: (input: {
      readonly cwd: string;
      readonly remoteName: string;
      readonly refName?: string;
    }) => Effect.Effect<void, GitCommandError>;
    readonly remoteExists: (input: {
      readonly cwd: string;
      readonly remoteName: string;
    }) => Effect.Effect<boolean, GitCommandError>;
    readonly remoteBranchExists: (input: {
      readonly cwd: string;
      readonly remoteName: string;
      readonly refName: string;
    }) => Effect.Effect<boolean, GitCommandError>;
    readonly resolvePrimaryRemoteName: (cwd: string) => Effect.Effect<string, GitCommandError>;
    readonly resolveRemoteTrackingCommit: (input: {
      readonly cwd: string;
      readonly refName: string;
      readonly fallbackRemoteName: string;
    }) => Effect.Effect<
      { readonly commitSha: string; readonly remoteRefName: string },
      GitCommandError
    >;
    readonly resolveWorktreeBaseRemote: GitVcsDriver.GitVcsDriver["Service"]["resolveWorktreeBaseRemote"];
    readonly fetchWorktreeBaseCommit: (input: {
      readonly cwd: string;
      readonly refName: string;
      readonly remote?: GitVcsDriver.GitWorktreeBaseRemote;
    }) => Effect.Effect<
      { readonly commitSha: string; readonly remoteRefName: string } | null,
      GitCommandError
    >;
    readonly resolveDefaultBranchName: (
      cwd: string,
      remoteName: string,
    ) => Effect.Effect<string | null, GitCommandError>;
    readonly removeWorktree: (
      input: VcsRemoveWorktreeInput,
    ) => Effect.Effect<void, GitCommandError>;
    readonly pruneWorktrees: (input: {
      readonly cwd: string;
    }) => Effect.Effect<void, GitCommandError>;
    readonly deleteLocalBranch: (
      input: GitVcsDriver.GitDeleteLocalBranchInput,
    ) => Effect.Effect<void, GitCommandError>;
    readonly createRef: (
      input: VcsCreateRefInput,
    ) => Effect.Effect<VcsCreateRefResult, GitCommandError>;
    readonly switchRef: (
      input: VcsSwitchRefInput,
    ) => Effect.Effect<VcsSwitchRefResult, GitCommandError>;
    readonly renameBranch: (input: {
      readonly exactName?: boolean;
      readonly cwd: string;
      readonly oldBranch: string;
      readonly newBranch: string;
    }) => Effect.Effect<{ readonly branch: string }, GitManagerServiceError>;
  }
>()("onicode/git/GitWorkflowService") {}

function nonRepositoryLocalStatus(): VcsStatusLocalResult {
  return {
    isRepo: false,
    hasPrimaryRemote: false,
    isDefaultRef: false,
    refName: null,
    hasWorkingTreeChanges: false,
    workingTree: {
      files: [],
      insertions: 0,
      deletions: 0,
    },
  };
}

function nonRepositoryStatus(): VcsStatusResult {
  return {
    ...nonRepositoryLocalStatus(),
    hasUpstream: false,
    aheadCount: 0,
    behindCount: 0,
    aheadOfDefaultCount: 0,
    pr: null,
  };
}

function nonRepositoryListRefs(): VcsListRefsResult {
  return {
    refs: [],
    isRepo: false,
    hasPrimaryRemote: false,
    nextCursor: null,
    totalCount: 0,
  };
}

/** @public */
export const make = Effect.gen(function* () {
  const registry = yield* VcsDriverRegistry.VcsDriverRegistry;
  const git = yield* GitVcsDriver.GitVcsDriver;
  const gitManager = yield* GitManager.GitManager;
  const checkoutGuard = yield* SharedCheckoutGuard.SharedCheckoutGuard;
  const fileSystem = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;

  // Nested projects and path aliases must share the checkout's branch-mutation lease.
  const withCheckoutLease = <A, E, R>(rootPath: string, effect: Effect.Effect<A, E, R>) =>
    withWorkspaceLease(fileSystem, path, rootPath, effect);

  const headRef = (cwd: string) =>
    git
      .execute({
        operation: "GitWorkflowService.headRef",
        cwd,
        args: ["symbolic-ref", "--quiet", "HEAD"],
        allowNonZeroExit: true,
        timeoutMs: 5_000,
      })
      .pipe(
        Effect.map((result) => (result.exitCode === 0 ? result.stdout.replace(/\n$/, "") : null)),
        Effect.orElseSucceed(() => null),
      );

  // Turn starts in the checkout wait on the reservation, so none can start between the
  // check and the checkout.
  const switchingCheckout = <A, E, R>(
    input: { readonly operation: string; readonly cwd: string; readonly rootPath: string },
    effect: Effect.Effect<A, E, R>,
  ) =>
    withCheckoutSwitchReservation(
      fileSystem,
      path,
      input.rootPath,
      withCheckoutLease(
        input.rootPath,
        checkoutGuard
          .assertIdle({ operation: input.operation, cwd: input.cwd, root: input.rootPath })
          .pipe(Effect.andThen(effect)),
      ),
    );

  const busyCheckout = (operation: string, cwd: string, detail: string) =>
    new GitCommandError({ operation, command: "git checkout", cwd, detail });

  // A step of a longer Git action that holds `leasedRoot`'s lease reserves only for itself,
  // and without waiting: waiting would deadlock with a restore, which reserves before leasing.
  // Without a lease held, the step takes both as a ref switch does.
  const guardCheckoutMove =
    (operation: string, options: { readonly leasedRoot?: string }): GitManager.CheckoutMoveGuard =>
    (checkout, effect) => {
      const leasedRoot = options.leasedRoot;
      if (leasedRoot === undefined) {
        return registry.detect({ cwd: checkout }).pipe(
          Effect.map((handle) => handle?.repository.rootPath ?? checkout),
          Effect.orElseSucceed(() => checkout),
          Effect.flatMap((rootPath) =>
            switchingCheckout({ operation, cwd: checkout, rootPath }, effect),
          ),
        );
      }
      const idle = checkoutGuard.assertIdle({ operation, cwd: checkout, root: leasedRoot });
      return withCheckoutSwitchReservationOrElse(
        fileSystem,
        path,
        leasedRoot,
        idle.pipe(Effect.andThen(effect)),
        (held) =>
          held === "checkout-switch"
            ? Effect.fail(
                busyCheckout(
                  operation,
                  checkout,
                  "Another branch change is in progress in this checkout. Try again when it finishes.",
                ),
              )
            : // A turn starting or a restore; the guard names a run it can already see.
              idle.pipe(
                Effect.andThen(
                  Effect.fail(
                    busyCheckout(
                      operation,
                      checkout,
                      "A thread is starting or restoring files in this checkout. Wait for it to finish, or use a worktree for a separate branch.",
                    ),
                  ),
                ),
              ),
      );
    };

  const ensureGit = Effect.fn("GitWorkflowService.ensureGit")(function* (
    operation: string,
    cwd: string,
  ) {
    const handle = yield* registry.resolve({ cwd }).pipe(
      Effect.mapError(
        (cause) =>
          new GitManagerError({
            operation,
            cwd,
            detail: "Failed to resolve the VCS driver for this Git workflow.",
            cause,
          }),
      ),
    );
    return handle;
  });

  const ensureGitCommand = Effect.fn("GitWorkflowService.ensureGitCommand")(function* (
    operation: string,
    cwd: string,
  ) {
    const handle = yield* registry.resolve({ cwd }).pipe(
      Effect.mapError(
        (cause) =>
          new GitCommandError({
            operation,
            command: "vcs-route",
            cwd,
            detail: "Failed to resolve the VCS driver for this Git command.",
            cause,
          }),
      ),
    );
    return handle;
  });

  const detectGitRepositoryForStatus = Effect.fn("GitWorkflowService.detectGitRepositoryForStatus")(
    function* (operation: string, cwd: string) {
      const handle = yield* registry.detect({ cwd }).pipe(
        Effect.mapError(
          (cause) =>
            new GitManagerError({
              operation,
              cwd,
              detail: "Failed to detect a VCS repository for this Git workflow.",
              cause,
            }),
        ),
      );
      if (!handle) {
        return false;
      }
      return true;
    },
  );

  const detectGitRepositoryForCommand = Effect.fn(
    "GitWorkflowService.detectGitRepositoryForCommand",
  )(function* (operation: string, cwd: string) {
    const handle = yield* registry.detect({ cwd }).pipe(
      Effect.mapError(
        (cause) =>
          new GitCommandError({
            operation,
            command: "vcs-route",
            cwd,
            detail: "Failed to detect a VCS repository for this Git command.",
            cause,
          }),
      ),
    );
    if (!handle) {
      return false;
    }
    return true;
  });

  const routeGitManager =
    <Input extends { readonly cwd: string }, Output>(
      operation: string,
      run: (input: Input) => Effect.Effect<Output, GitManagerServiceError>,
    ) =>
    (input: Input) =>
      ensureGit(operation, input.cwd).pipe(Effect.andThen(run(input)));

  return GitWorkflowService.of({
    repositoryRoot: Effect.fn("GitWorkflowService.repositoryRoot")(function (cwd) {
      return registry.detect({ cwd }).pipe(
        Effect.map((handle) => (handle?.kind === "git" ? handle.repository.rootPath : null)),
        Effect.mapError(
          (cause) =>
            new GitManagerError({
              operation: "GitWorkflowService.repositoryRoot",
              cwd,
              detail: "Failed to detect a VCS repository for this Git workflow.",
              cause,
            }),
        ),
      );
    }),
    hasCommit: Effect.fn("GitWorkflowService.hasCommit")(function (input) {
      return ensureGitCommand("GitWorkflowService.hasCommit", input.cwd).pipe(
        Effect.andThen(
          git.execute({
            operation: "GitWorkflowService.hasCommit",
            cwd: input.cwd,
            args: ["rev-parse", "--verify", `${input.refName}^{commit}`],
            allowNonZeroExit: true,
          }),
        ),
        Effect.map((result) => result.exitCode === 0),
      );
    }),
    hasRefNamed: Effect.fn("GitWorkflowService.hasRefNamed")(function (input) {
      // Pattern characters would widen the ref match.
      return /[*?[\\]/.test(input.refName)
        ? Effect.succeed(false)
        : ensureGitCommand("GitWorkflowService.hasRefNamed", input.cwd).pipe(
            Effect.andThen(
              git.execute({
                operation: "GitWorkflowService.hasRefNamed",
                cwd: input.cwd,
                args: [
                  "for-each-ref",
                  "--count=1",
                  "--format=%(refname)",
                  `refs/**/${input.refName}`,
                ],
              }),
            ),
            Effect.map((result) => result.stdout.trim().length > 0),
          );
    }),
    status: Effect.fn("GitWorkflowService.status")(function (input) {
      return detectGitRepositoryForStatus("GitWorkflowService.status", input.cwd).pipe(
        Effect.flatMap((isGitRepository) =>
          isGitRepository ? gitManager.status(input) : Effect.succeed(nonRepositoryStatus()),
        ),
      );
    }),
    localStatus: Effect.fn("GitWorkflowService.localStatus")(function (input, options) {
      return detectGitRepositoryForStatus("GitWorkflowService.localStatus", input.cwd).pipe(
        Effect.flatMap((isGitRepository) =>
          isGitRepository
            ? gitManager.localStatus(input, options)
            : Effect.succeed(nonRepositoryLocalStatus()),
        ),
      );
    }),
    currentBranch: Effect.fn("GitWorkflowService.currentBranch")(function* (cwd) {
      if (!(yield* detectGitRepositoryForStatus("GitWorkflowService.currentBranch", cwd))) {
        return null;
      }
      const result = yield* git.execute({
        operation: "GitWorkflowService.currentBranch",
        cwd,
        args: ["symbolic-ref", "--quiet", "HEAD"],
        allowNonZeroExit: true,
        timeoutMs: 5_000,
      });
      if (result.exitCode === 0) {
        return trimAsciiWhitespace(result.stdout).replace(/^refs\/heads\//, "") || null;
      }
      if (result.exitCode === 1) return null;
      return yield* new GitCommandError({
        operation: "GitWorkflowService.currentBranch",
        command: "git symbolic-ref",
        cwd,
        argumentCount: 3,
        ...(result.exitCode === null ? {} : { exitCode: result.exitCode }),
        stdoutLength: result.stdout.length,
        stderrLength: result.stderr.length,
        detail: "Failed to read the checked-out branch.",
      });
    }),
    upstreamDivergence: Effect.fn("GitWorkflowService.upstreamDivergence")(function* (cwd) {
      const details = yield* git.statusDetailsRemote(cwd, { refreshUpstream: false });
      return details.hasUpstream && details.branch !== null && details.divergenceUnknown !== true
        ? {
            refName: details.branch,
            aheadCount: details.aheadCount,
            behindCount: details.behindCount,
            details,
          }
        : null;
    }),
    remoteStatus: Effect.fn("GitWorkflowService.remoteStatus")(function (input, options) {
      return detectGitRepositoryForStatus("GitWorkflowService.remoteStatus", input.cwd).pipe(
        Effect.flatMap((isGitRepository) =>
          isGitRepository ? gitManager.remoteStatus(input, options) : Effect.succeed(null),
        ),
      );
    }),
    invalidateLocalStatus: gitManager.invalidateLocalStatus,
    invalidateRemoteStatus: gitManager.invalidateRemoteStatus,
    readLocalStatusRefSnapshot: git.readLocalStatusRefSnapshot,
    readRefSnapshot: git.readRefSnapshot,
    invalidateStatus: gitManager.invalidateStatus,
    headPath: Effect.fn("GitWorkflowService.headPath")(function* (cwd) {
      // Clients keep watching threads whose worktree was removed; spawning there fails noisily.
      if (!(yield* fileSystem.exists(cwd).pipe(Effect.orElseSucceed(() => true)))) return null;
      return yield* git
        .execute({
          operation: "GitWorkflowService.headPath",
          cwd,
          args: ["rev-parse", "--path-format=absolute", "--git-path", "HEAD"],
          allowNonZeroExit: true,
        })
        .pipe(
          Effect.map((result) => (result.exitCode === 0 ? result.stdout.trim() || null : null)),
          Effect.orElseSucceed(() => null),
        );
    }),
    pullCurrentBranch: Effect.fn("GitWorkflowService.pullCurrentBranch")(function (cwd) {
      return ensureGitCommand("GitWorkflowService.pullCurrentBranch", cwd).pipe(
        Effect.flatMap((handle) =>
          switchingCheckout(
            {
              operation: "GitWorkflowService.pullCurrentBranch",
              cwd,
              rootPath: handle.repository.rootPath,
            },
            git.pullCurrentBranch(cwd),
          ),
        ),
      );
    }),
    pullCurrentBranchWhenIdle: Effect.fn("GitWorkflowService.pullCurrentBranchWhenIdle")(
      function (cwd, branch) {
        // The caller checked before taking the checkout, and another Git action may have moved it since.
        const stillEligible = git
          .statusDetailsLocal(cwd)
          .pipe(
            Effect.map(
              (status) =>
                status.branch === branch &&
                !status.hasWorkingTreeChanges &&
                status.aheadCount === 0,
            ),
          );
        return ensureGitCommand("GitWorkflowService.pullCurrentBranchWhenIdle", cwd).pipe(
          Effect.flatMap((handle) => {
            const rootPath = handle.repository.rootPath;
            // Skips instead of waiting on another Git action, which would stall a status refresh.
            return withWorkspaceLeaseOrElse(
              fileSystem,
              path,
              rootPath,
              withCheckoutSwitchReservationOrElse(
                fileSystem,
                path,
                rootPath,
                // Busy skips this pull; a check that could not run fails, so the caller logs it
                // instead of skipping quietly forever.
                checkoutGuard
                  .busyThreads({
                    operation: "GitWorkflowService.pullCurrentBranchWhenIdle",
                    cwd,
                    root: rootPath,
                  })
                  .pipe(
                    Effect.flatMap((busy) =>
                      busy.length > 0 ? Effect.succeed(null) : stillEligible,
                    ),
                    Effect.flatMap((eligible) =>
                      eligible === true ? git.pullCurrentBranch(cwd) : Effect.succeed(null),
                    ),
                  ),
                () => Effect.succeed(null),
              ),
              () => Effect.succeed(null),
            );
          }),
        );
      },
    ),
    runStackedAction: Effect.fn("GitWorkflowService.runStackedAction")(function (input, options) {
      return ensureGit("GitWorkflowService.runStackedAction", input.cwd).pipe(
        Effect.flatMap((handle) =>
          // A feature branch is refused before its name is generated, and again when it is made.
          withCheckoutLease(
            handle.repository.rootPath,
            input.featureBranch
              ? checkoutGuard
                  .assertIdle({
                    operation: "GitWorkflowService.runStackedAction",
                    cwd: input.cwd,
                    root: handle.repository.rootPath,
                  })
                  .pipe(
                    Effect.andThen(
                      gitManager.runStackedAction(input, {
                        ...options,
                        guardCheckoutMove: guardCheckoutMove(
                          "GitWorkflowService.runStackedAction",
                          { leasedRoot: handle.repository.rootPath },
                        ),
                      }),
                    ),
                  )
              : gitManager.runStackedAction(input, options),
          ),
        ),
      );
    }),
    resolvePullRequest: routeGitManager(
      "GitWorkflowService.resolvePullRequest",
      gitManager.resolvePullRequest,
    ),
    preparePullRequestThread: Effect.fn("GitWorkflowService.preparePullRequestThread")(
      function (input) {
        return ensureGit("GitWorkflowService.preparePullRequestThread", input.cwd).pipe(
          Effect.flatMap((handle) =>
            input.mode === "local"
              ? switchingCheckout(
                  {
                    operation: "GitWorkflowService.preparePullRequestThread",
                    cwd: input.cwd,
                    rootPath: handle.repository.rootPath,
                  },
                  gitManager.preparePullRequestThread(input),
                )
              : gitManager.preparePullRequestThread(input, {
                  guardCheckoutMove: guardCheckoutMove(
                    "GitWorkflowService.preparePullRequestThread",
                    {},
                  ),
                }),
          ),
        );
      },
    ),
    listRefs: Effect.fn("GitWorkflowService.listRefs")(function (input) {
      return detectGitRepositoryForCommand("GitWorkflowService.listRefs", input.cwd).pipe(
        Effect.flatMap((isGitRepository) =>
          isGitRepository ? git.listRefs(input) : Effect.succeed(nonRepositoryListRefs()),
        ),
      );
    }),
    createWorktree: Effect.fn("GitWorkflowService.createWorktree")(function (input, options) {
      return ensureGitCommand("GitWorkflowService.createWorktree", input.cwd).pipe(
        Effect.andThen(gitManager.createWorktree(input, options)),
      );
    }),
    listLocalBranchNames: Effect.fn("GitWorkflowService.listLocalBranchNames")(function (cwd) {
      return ensureGitCommand("GitWorkflowService.listLocalBranchNames", cwd).pipe(
        Effect.andThen(git.listLocalBranchNames(cwd)),
      );
    }),
    fetchRemote: Effect.fn("GitWorkflowService.fetchRemote")(function (input) {
      return ensureGitCommand("GitWorkflowService.fetchRemote", input.cwd).pipe(
        Effect.andThen(git.fetchRemote(input)),
      );
    }),
    remoteExists: Effect.fn("GitWorkflowService.remoteExists")(function (input) {
      return ensureGitCommand("GitWorkflowService.remoteExists", input.cwd).pipe(
        Effect.andThen(git.remoteExists(input)),
      );
    }),
    remoteBranchExists: Effect.fn("GitWorkflowService.remoteBranchExists")(function (input) {
      return ensureGitCommand("GitWorkflowService.remoteBranchExists", input.cwd).pipe(
        Effect.andThen(git.remoteBranchExists(input)),
      );
    }),
    resolvePrimaryRemoteName: Effect.fn("GitWorkflowService.resolvePrimaryRemoteName")(
      function (cwd) {
        return ensureGitCommand("GitWorkflowService.resolvePrimaryRemoteName", cwd).pipe(
          Effect.andThen(git.resolvePrimaryRemoteName(cwd)),
        );
      },
    ),
    resolveRemoteTrackingCommit: Effect.fn("GitWorkflowService.resolveRemoteTrackingCommit")(
      function (input) {
        return ensureGitCommand("GitWorkflowService.resolveRemoteTrackingCommit", input.cwd).pipe(
          Effect.andThen(git.resolveRemoteTrackingCommit(input)),
        );
      },
    ),
    resolveWorktreeBaseRemote: Effect.fn("GitWorkflowService.resolveWorktreeBaseRemote")(
      function (input) {
        return ensureGitCommand("GitWorkflowService.resolveWorktreeBaseRemote", input.cwd).pipe(
          Effect.andThen(git.resolveWorktreeBaseRemote(input)),
        );
      },
    ),
    fetchWorktreeBaseCommit: Effect.fn("GitWorkflowService.fetchWorktreeBaseCommit")(
      function (input) {
        return ensureGitCommand("GitWorkflowService.fetchWorktreeBaseCommit", input.cwd).pipe(
          Effect.andThen(git.fetchWorktreeBaseCommit(input)),
        );
      },
    ),
    resolveDefaultBranchName: Effect.fn("GitWorkflowService.resolveDefaultBranchName")(
      function (cwd, remoteName) {
        return ensureGitCommand("GitWorkflowService.resolveDefaultBranchName", cwd).pipe(
          Effect.andThen(git.resolveDefaultBranchName(cwd, remoteName)),
        );
      },
    ),
    removeWorktree: Effect.fn("GitWorkflowService.removeWorktree")(function (input) {
      return ensureGitCommand("GitWorkflowService.removeWorktree", input.cwd).pipe(
        Effect.andThen(git.removeWorktree(input)),
      );
    }),
    pruneWorktrees: Effect.fn("GitWorkflowService.pruneWorktrees")(function (input) {
      return ensureGitCommand("GitWorkflowService.pruneWorktrees", input.cwd).pipe(
        Effect.andThen(git.pruneWorktrees(input)),
      );
    }),
    deleteLocalBranch: Effect.fn("GitWorkflowService.deleteLocalBranch")(function (input) {
      return ensureGitCommand("GitWorkflowService.deleteLocalBranch", input.cwd).pipe(
        Effect.andThen(git.deleteLocalBranch(input)),
      );
    }),
    createRef: Effect.fn("GitWorkflowService.createRef")(function (input) {
      return ensureGitCommand("GitWorkflowService.createRef", input.cwd).pipe(
        Effect.flatMap((handle) =>
          input.switchRef
            ? switchingCheckout(
                {
                  operation: "GitWorkflowService.createRef",
                  cwd: input.cwd,
                  rootPath: handle.repository.rootPath,
                },
                git.createRef(input),
              )
            : git.createRef(input),
        ),
      );
    }),
    switchRef: Effect.fn("GitWorkflowService.switchRef")(function (input) {
      return ensureGitCommand("GitWorkflowService.switchRef", input.cwd).pipe(
        Effect.flatMap((handle) =>
          withCheckoutSwitchReservation(
            fileSystem,
            path,
            handle.repository.rootPath,
            withCheckoutLease(
              handle.repository.rootPath,
              Effect.gen(function* () {
                // Checking out the current branch again moves nothing, so it is neither
                // guarded nor run, and its post-checkout hook stays quiet.
                if ((yield* headRef(input.cwd)) === `refs/heads/${input.refName}`) {
                  return { refName: input.refName };
                }
                yield* checkoutGuard.assertIdle({
                  operation: "GitWorkflowService.switchRef",
                  cwd: input.cwd,
                  root: handle.repository.rootPath,
                });
                return yield* Effect.scoped(git.switchRef(input));
              }),
            ),
          ),
        ),
      );
    }),
    renameBranch: Effect.fn("GitWorkflowService.renameBranch")(function (input) {
      return ensureGit("GitWorkflowService.renameBranch", input.cwd).pipe(
        Effect.andThen(git.renameBranch(input)),
      );
    }),
  });
});

export const layer = Layer.effect(GitWorkflowService, make);
