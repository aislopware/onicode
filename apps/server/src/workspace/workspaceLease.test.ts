import * as NodeServices from "@effect/platform-node/NodeServices";
import { assert, it } from "@effect/vitest";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as FileSystem from "effect/FileSystem";
import * as Path from "effect/Path";

import { withWorkspaceLease } from "./workspaceLease.ts";

it.effect("a checkout named through a symlink shares the lease of its real path", () =>
  Effect.gen(function* () {
    const fileSystem = yield* FileSystem.FileSystem;
    const path = yield* Path.Path;
    const tmp = yield* fileSystem.makeTempDirectoryScoped({ prefix: "onicode-lease-" });
    const real = path.join(tmp, "worktrees", "feature");
    yield* fileSystem.makeDirectory(real, { recursive: true });
    yield* fileSystem.symlink(path.join(tmp, "worktrees"), path.join(tmp, "alias"));
    const held = yield* Deferred.make<void>();
    const release = yield* Deferred.make<void>();
    const calls: string[] = [];

    const holder = yield* withWorkspaceLease(
      fileSystem,
      path,
      path.join(tmp, "alias", "feature"),
      Deferred.succeed(held, undefined).pipe(Effect.andThen(Deferred.await(release))),
    ).pipe(Effect.forkChild({ startImmediately: true }));
    yield* Deferred.await(held);
    // Resolved up front, so a synchronous realPath leaves the contender waiting by the assertion.
    const contender = yield* withWorkspaceLease(
      FileSystem.makeNoop({ realPath: (target) => Effect.succeed(target) }),
      path,
      yield* fileSystem.realPath(real),
      Effect.sync(() => calls.push("git action")),
    ).pipe(Effect.forkChild({ startImmediately: true }));
    assert.deepEqual(calls, []);

    yield* Deferred.succeed(release, undefined);
    yield* Fiber.join(holder);
    yield* Fiber.join(contender);
    assert.deepEqual(calls, ["git action"]);
  }).pipe(Effect.scoped, Effect.provide(NodeServices.layer)),
);
