import { GitCommandError } from "@onicode/contracts";
import { assert, it } from "@effect/vitest";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";

import * as NodeCrypto from "@effect/platform-node/NodeCrypto";
import * as NodeServices from "@effect/platform-node/NodeServices";
import * as FileSystem from "effect/FileSystem";
import * as Path from "effect/Path";
import { ChildProcessSpawner } from "effect/process";

import * as ServerConfig from "../config.ts";
import * as VcsProcess from "../vcs/VcsProcess.ts";
import * as GitManager from "./GitManager.ts";
import * as GitWorkflowService from "./GitWorkflowService.ts";
import * as SharedCheckoutGuard from "./sharedCheckoutGuard.ts";
import * as GitVcsDriver from "../vcs/GitVcsDriver.ts";
import * as VcsDriverRegistry from "../vcs/VcsDriverRegistry.ts";
import {
  withCheckoutSwitchReservation,
  withCheckoutSwitchReservationOrElse,
  withTurnStartReservation,
} from "../workspace/workspaceLease.ts";

// Synchronous, so a forked workflow runs up to its first wait before the test goes on.
const syncFileSystem = FileSystem.makeNoop({ realPath: (target) => Effect.succeed(target) });

// Overlaps a ref switch of /repo but not a turn start in /repo/packages/app.
const probeSwitchReservation = (path: Path.Path) =>
  withCheckoutSwitchReservationOrElse(
    syncFileSystem,
    path,
    "/repo/docs",
    Effect.succeed("free"),
    () => Effect.succeed("reserved"),
  );

interface CheckoutHarness {
  /** Checkout moves, in order. */
  readonly events: Array<string>;
  /** Guard checks, each with whether a ref switch held the checkout's reservation then. */
  readonly guards: Array<string>;
  working: boolean;
  onGuard: Effect.Effect<void>;
}

const makeCheckoutHarness = (): CheckoutHarness => ({
  events: [],
  guards: [],
  working: false,
  onGuard: Effect.void,
});

/** A workflow over /repo on branch main whose guard refuses while `harness.working`. */
const layerCheckoutWorkflow = (harness: CheckoutHarness) =>
  GitWorkflowService.layer.pipe(
    Layer.provide(Layer.succeed(FileSystem.FileSystem, syncFileSystem)),
    Layer.provide(
      Layer.mock(VcsDriverRegistry.VcsDriverRegistry)({
        resolve: () =>
          Effect.succeed({
            kind: "git",
            repository: { rootPath: "/repo" },
          } as VcsDriverRegistry.VcsDriverHandle),
        detect: () =>
          Effect.succeed({
            kind: "git",
            repository: { rootPath: "/repo" },
          } as VcsDriverRegistry.VcsDriverHandle),
      }),
    ),
    Layer.provide(
      Layer.unwrap(
        Effect.gen(function* () {
          const path = yield* Path.Path;
          return Layer.mock(GitManager.GitManager)({
            runStackedAction: (input, options) =>
              Effect.gen(function* () {
                if (!input.featureBranch) {
                  harness.events.push("commit");
                  return {} as never;
                }
                const branch = Effect.sync(() => harness.events.push("branch"));
                const guardCheckoutMove = options?.guardCheckoutMove;
                yield* guardCheckoutMove === undefined
                  ? branch
                  : guardCheckoutMove(input.cwd, branch);
                // Committing, pushing and opening the PR happen after the branch is made.
                harness.events.push(`pushing-${yield* probeSwitchReservation(path)}`);
                return {} as never;
              }),
            preparePullRequestThread: (input, options) =>
              Effect.gen(function* () {
                const move = probeSwitchReservation(path).pipe(
                  Effect.map((reservation) => {
                    harness.events.push(`pull-request-${input.mode}-${reservation}`);
                  }),
                );
                const guardCheckoutMove = options?.guardCheckoutMove;
                yield* input.mode === "local" || guardCheckoutMove === undefined
                  ? move
                  : // A refused refresh leaves the reused worktree where it was, as GitManager does.
                    guardCheckoutMove(input.cwd, move).pipe(Effect.ignore);
                return {} as never;
              }),
          });
        }),
      ),
    ),
    Layer.provide(
      Layer.mock(GitVcsDriver.GitVcsDriver)({
        execute: () =>
          Effect.succeed({
            exitCode: ChildProcessSpawner.ExitCode(0),
            stdout: "refs/heads/main\n",
            stderr: "",
            stdoutTruncated: false,
            stderrTruncated: false,
          }),
        switchRef: (input) =>
          Effect.sync(() => harness.events.push(`switch-${input.refName}`)).pipe(
            Effect.as({ refName: input.refName }),
          ),
        createRef: (input) =>
          Effect.sync(() => harness.events.push(`create-${input.switchRef === true}`)).pipe(
            Effect.as({ refName: input.refName }),
          ),
        pullCurrentBranch: () =>
          Effect.sync(() => harness.events.push("pull")).pipe(
            Effect.as({ status: "pulled" as const, refName: "main", upstreamRef: "origin/main" }),
          ),
      }),
    ),
    Layer.provide(
      Layer.effect(
        SharedCheckoutGuard.SharedCheckoutGuard,
        Effect.gen(function* () {
          const path = yield* Path.Path;
          return SharedCheckoutGuard.SharedCheckoutGuard.of({
            assertIdle: ({ operation, cwd }) =>
              Effect.gen(function* () {
                harness.guards.push(`guard-${yield* probeSwitchReservation(path)}`);
                yield* harness.onGuard;
                if (harness.working) {
                  return yield* new GitCommandError({
                    operation,
                    command: "git checkout",
                    cwd,
                    detail: "busy",
                  });
                }
              }),
          });
        }),
      ),
    ),
  );

const checkoutWorkflow = (harness: CheckoutHarness) =>
  Effect.service(GitWorkflowService.GitWorkflowService).pipe(
    Effect.provide(layerCheckoutWorkflow(harness)),
  );

const failureDetail = <A, E extends { readonly _tag: string }, R>(effect: Effect.Effect<A, E, R>) =>
  effect.pipe(
    Effect.flip,
    Effect.map((error) => ("detail" in error ? String(error.detail) : error._tag)),
    Effect.option,
    Effect.map(Option.getOrNull),
  );

it.layer(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer))("GitWorkflowService", (it) => {
  it.effect("finds a name among the refs of every namespace", () =>
    Effect.gen(function* () {
      const fileSystem = yield* FileSystem.FileSystem;
      const cwd = yield* fileSystem.makeTempDirectoryScoped({ prefix: "oni-ref-named-" });
      const workflow = yield* GitWorkflowService.GitWorkflowService;
      const driver = yield* GitVcsDriver.GitVcsDriver;
      for (const args of [
        ["init", "-b", "main"],
        ["-c", "user.email=t@t", "-c", "user.name=t", "commit", "--allow-empty", "-m", "init"],
        ["update-ref", "refs/remotes/upstream/feature/topic", "HEAD"],
        ["symbolic-ref", "refs/remotes/origin/alias", "refs/heads/main"],
        ["update-ref", "refs/mirror/origin/mirrored", "HEAD"],
      ]) {
        yield* driver.execute({ operation: "test.setupRepo", cwd, args });
      }
      const found = yield* Effect.forEach(
        ["main", "feature/topic", "alias", "mirrored", "onicode/renamed", "feature/*"],
        (refName) => workflow.hasRefNamed({ cwd, refName }),
      );
      assert.deepEqual(found, [true, true, true, true, false, false]);
    }).pipe(
      Effect.scoped,
      Effect.provide(
        GitWorkflowService.layer.pipe(
          Layer.provide(
            Layer.mock(VcsDriverRegistry.VcsDriverRegistry)({
              resolve: () => Effect.succeed({ kind: "git" } as VcsDriverRegistry.VcsDriverHandle),
            }),
          ),
          Layer.provide(Layer.mock(GitManager.GitManager)({})),
          Layer.provide(Layer.mock(SharedCheckoutGuard.SharedCheckoutGuard)({})),
          Layer.provideMerge(GitVcsDriver.layer),
          Layer.provideMerge(VcsProcess.layer),
          Layer.provideMerge(
            ServerConfig.layerTest(process.cwd(), { prefix: "oni-ref-named-" }).pipe(
              Layer.provide(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer)),
            ),
          ),
          Layer.provideMerge(Layer.mergeAll(NodeServices.layer, NodeCrypto.layer)),
        ),
      ),
    ),
  );

  it.effect("reads no HEAD path from a removed worktree without spawning Git", () =>
    Effect.gen(function* () {
      const fileSystem = yield* FileSystem.FileSystem;
      const parent = yield* fileSystem.makeTempDirectoryScoped({ prefix: "oni-removed-wt-" });
      const removed = `${parent}/removed`;
      const workflow = yield* GitWorkflowService.GitWorkflowService;
      assert.strictEqual(yield* workflow.headPath(removed), null);
    }).pipe(
      Effect.scoped,
      Effect.provide(
        GitWorkflowService.layer.pipe(
          Layer.provide(Layer.mock(VcsDriverRegistry.VcsDriverRegistry)({})),
          Layer.provide(Layer.mock(GitManager.GitManager)({})),
          Layer.provide(Layer.mock(SharedCheckoutGuard.SharedCheckoutGuard)({})),
          Layer.provide(
            Layer.mock(GitVcsDriver.GitVcsDriver)({
              execute: (input) => Effect.die(`spawned Git in ${input.cwd}`),
            }),
          ),
        ),
      ),
    ),
  );

  it.effect.each([
    ["switching to another branch", "busy", []],
    ["switching to the current branch", null, []],
    ["creating and switching to a branch", "busy", []],
    ["creating a branch", null, ["create-false"]],
    ["committing on a new branch", "busy", []],
    ["committing", null, ["commit"]],
    ["checking out a pull request locally", "busy", []],
    ["checking out a pull request in a worktree", null, []],
    ["pulling", "busy", []],
    // An automatic pull skips the checkout without a refusal to show.
    ["pulling automatically", null, []],
  ] as const)(
    "while a thread works in the checkout: %s (refusal: %s, moves: %j)",
    ([operation, refusal, moves]) =>
      Effect.gen(function* () {
        const harness = makeCheckoutHarness();
        harness.working = true;
        const workflow = yield* checkoutWorkflow(harness);
        const cwd = "/repo";
        const run: Effect.Effect<unknown, { readonly _tag: string }> =
          operation === "switching to another branch"
            ? workflow.switchRef({ cwd, refName: "feature" })
            : operation === "switching to the current branch"
              ? workflow.switchRef({ cwd, refName: "main" })
              : operation === "creating and switching to a branch"
                ? workflow.createRef({ cwd, refName: "feature", switchRef: true })
                : operation === "creating a branch"
                  ? workflow.createRef({ cwd, refName: "feature" })
                  : operation === "pulling"
                    ? workflow.pullCurrentBranch(cwd)
                    : operation === "pulling automatically"
                      ? workflow.pullCurrentBranchWhenIdle(cwd)
                      : operation === "committing on a new branch" || operation === "committing"
                        ? workflow.runStackedAction({
                            cwd,
                            actionId: "commit",
                            action: "commit",
                            featureBranch: operation === "committing on a new branch",
                          })
                        : workflow.preparePullRequestThread({
                            cwd,
                            reference: "91",
                            mode:
                              operation === "checking out a pull request locally"
                                ? "local"
                                : "worktree",
                          });
        assert.deepEqual(
          { refusal: yield* failureDetail(run), moves: harness.events },
          { refusal, moves: [...moves] },
        );
      }),
  );

  it.effect("a ref switch checks the checkout only after a turn start there has started", () =>
    Effect.gen(function* () {
      const path = yield* Path.Path;
      const harness = makeCheckoutHarness();
      const workflow = yield* checkoutWorkflow(harness);
      const reserved = yield* Deferred.make<void>();
      const release = yield* Deferred.make<void>();
      const guardsWhileStarting = yield* Deferred.make<ReadonlyArray<string>>();
      // A turn start in a project nested in the checkout.
      const turnStart = yield* withTurnStartReservation(
        syncFileSystem,
        path,
        "/repo/packages/app",
        Effect.gen(function* () {
          yield* Deferred.succeed(reserved, undefined);
          yield* Deferred.await(release);
          // Its run is visible by the time startup releases the reservation.
          harness.working = true;
          yield* Deferred.succeed(guardsWhileStarting, [...harness.guards]);
        }),
      ).pipe(Effect.forkChild({ startImmediately: true }));
      yield* Deferred.await(reserved);
      const switching = yield* failureDetail(
        workflow.switchRef({ cwd: "/repo", refName: "feature" }),
      ).pipe(Effect.forkChild({ startImmediately: true }));

      yield* Deferred.succeed(release, undefined);
      yield* Fiber.join(turnStart);
      assert.deepEqual(
        {
          guardsWhileStarting: yield* Deferred.await(guardsWhileStarting),
          refusal: yield* Fiber.join(switching),
          guards: harness.guards,
          moves: harness.events,
        },
        { guardsWhileStarting: [], refusal: "busy", guards: ["guard-reserved"], moves: [] },
      );
    }),
  );

  it.effect("a turn start in the checkout waits for a ref switch that checked it first", () =>
    Effect.gen(function* () {
      const path = yield* Path.Path;
      const harness = makeCheckoutHarness();
      const workflow = yield* checkoutWorkflow(harness);
      const guardEntered = yield* Deferred.make<void>();
      const guardRelease = yield* Deferred.make<void>();
      harness.onGuard = Deferred.succeed(guardEntered, undefined).pipe(
        Effect.andThen(Deferred.await(guardRelease)),
      );
      const switching = yield* workflow
        .switchRef({ cwd: "/repo", refName: "feature" })
        .pipe(Effect.forkChild({ startImmediately: true }));
      yield* Effect.raceFirst(
        Deferred.await(guardEntered),
        Fiber.await(switching).pipe(
          Effect.andThen(Effect.die("switched without checking the checkout")),
        ),
      );
      const turnStart = yield* withTurnStartReservation(
        syncFileSystem,
        path,
        "/repo/packages/app",
        Effect.sync(() => harness.events.push("turn-start")),
      ).pipe(Effect.forkChild({ startImmediately: true }));

      yield* Deferred.succeed(guardRelease, undefined);
      yield* Fiber.join(switching);
      yield* Fiber.join(turnStart);
      assert.deepEqual(
        { guards: harness.guards, moves: harness.events },
        { guards: ["guard-reserved"], moves: ["switch-feature", "turn-start"] },
      );
    }),
  );

  it.effect(
    "a new branch for a commit holds the checkout's reservation only while it is made",
    () =>
      Effect.gen(function* () {
        const harness = makeCheckoutHarness();
        const workflow = yield* checkoutWorkflow(harness);
        yield* workflow.runStackedAction({
          cwd: "/repo",
          actionId: "commit",
          action: "commit_push_pr",
          featureBranch: true,
        });
        assert.deepEqual(
          { guards: harness.guards, moves: harness.events },
          // Checked before the branch name is generated, then again while the branch is made.
          { guards: ["guard-free", "guard-reserved"], moves: ["branch", "pushing-free"] },
        );
      }),
  );

  it.effect.each([
    [
      "a turn starts",
      withTurnStartReservation,
      "A thread is starting or restoring files in this checkout. Wait for it to finish, or use a worktree for a separate branch.",
    ],
    [
      "another ref switch waits",
      withCheckoutSwitchReservation,
      "Another branch change is in progress in this checkout. Try again when it finishes.",
    ],
  ] as const)(
    "a new branch for a commit is refused while %s in the checkout",
    ([_holder, withReservation, expectedRefusal]) =>
      Effect.gen(function* () {
        const path = yield* Path.Path;
        const harness = makeCheckoutHarness();
        const workflow = yield* checkoutWorkflow(harness);
        const reserved = yield* Deferred.make<void>();
        const release = yield* Deferred.make<void>();
        const holder = yield* withReservation(
          syncFileSystem,
          path,
          "/repo/packages/app",
          Deferred.succeed(reserved, undefined).pipe(Effect.andThen(Deferred.await(release))),
        ).pipe(Effect.forkChild({ startImmediately: true }));
        yield* Deferred.await(reserved);
        // Runs to the end while the reservation is held: the step does not wait.
        const refusal = yield* failureDetail(
          workflow.runStackedAction({
            cwd: "/repo",
            actionId: "commit",
            action: "commit",
            featureBranch: true,
          }),
        );
        yield* Deferred.succeed(release, undefined);
        yield* Fiber.join(holder);
        assert.deepEqual(
          { refusal, moves: harness.events },
          { refusal: expectedRefusal, moves: [] },
        );
      }),
  );

  it.effect("refreshing a reused pull request worktree holds its checkout's reservation", () =>
    Effect.gen(function* () {
      const harness = makeCheckoutHarness();
      const workflow = yield* checkoutWorkflow(harness);
      yield* workflow.preparePullRequestThread({ cwd: "/repo", reference: "91", mode: "worktree" });
      assert.deepEqual(
        { guards: harness.guards, moves: harness.events },
        { guards: ["guard-reserved"], moves: ["pull-request-worktree-reserved"] },
      );
    }),
  );
});
