import * as NodeServices from "@effect/platform-node/NodeServices";
import { assert, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Path from "effect/Path";
import { vi } from "vite-plus/test";

import { acquireServiceSelectionLock } from "./serviceSelectionLock.ts";

// Errors the next matching lock-path call throws instead of running, oldest first.
const faults = vi.hoisted(() => ({ queued: [] as Array<{ call: string; code: string }> }));
vi.mock("node:fs", async (importOriginal) => {
  const actual = await importOriginal<typeof import("node:fs")>();
  const faulty =
    <A extends ReadonlyArray<unknown>, R>(call: string, run: (...args: A) => R) =>
    (...args: A): R => {
      const index = faults.queued.findIndex((fault) => fault.call === call);
      if (index !== -1 && String(args[0]).includes(".selection-lock")) {
        const { code } = faults.queued.splice(index, 1)[0]!;
        throw Object.assign(new Error(`${code}: injected ${call}`), { code });
      }
      return run(...args);
    };
  return {
    ...actual,
    renameSync: faulty("renameSync", actual.renameSync),
    readdirSync: faulty("readdirSync", actual.readdirSync),
    unlinkSync: faulty("unlinkSync", actual.unlinkSync),
    rmSync: faulty("rmSync", actual.rmSync),
  };
});

const injectFaults = (...queued: Array<{ call: string; code: string }>) =>
  Effect.acquireRelease(
    Effect.sync(() => faults.queued.splice(0, faults.queued.length, ...queued)),
    () => Effect.sync(() => faults.queued.splice(0)),
  );

it.layer(NodeServices.layer)("service selection lock", (it) => {
  it.effect("reclaims a lease from an earlier boot even when its PID is live again", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const root = yield* fs.makeTempDirectoryScoped({ prefix: "oni-service-selection-lock-" });
      const selectionPath = path.join(root, "service-state.json");
      const lock = `${selectionPath}.selection-lock`;
      const stale = `${process.pid}.1.00000000-0000-4000-8000-000000000000`;
      yield* fs.makeDirectory(lock, { recursive: true });
      yield* fs.writeFileString(path.join(lock, stale), "");

      const release = yield* Effect.promise(() => acquireServiceSelectionLock(selectionPath));
      const owners = yield* fs.readDirectory(lock);
      assert.equal(owners.length, 1);
      assert.notEqual(owners[0], stale);
      assert.isTrue(owners[0]!.startsWith(`${process.pid}.`));
      // Another process reclaimed this lease while its holder looked dead; release stays quiet.
      yield* fs.remove(path.join(lock, owners[0]!));
      release();
      assert.isFalse(yield* fs.exists(lock));
    }),
  );

  it.effect("on Windows, retries a lock that another process is still deleting", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const root = yield* fs.makeTempDirectoryScoped({ prefix: "oni-service-selection-lock-" });
      const selectionPath = path.join(root, "service-state.json");
      const lock = `${selectionPath}.selection-lock`;
      const stale = `${process.pid}.1.00000000-0000-4000-8000-000000000000`;
      yield* fs.makeDirectory(lock, { recursive: true });
      yield* fs.writeFileString(path.join(lock, stale), "");
      yield* injectFaults(
        { call: "renameSync", code: "EBUSY" },
        { call: "readdirSync", code: "EPERM" },
        { call: "unlinkSync", code: "EBUSY" },
      );

      const release = yield* Effect.promise(() =>
        acquireServiceSelectionLock(selectionPath, {
          platform: "win32",
          waitForRetry: () => Promise.resolve(),
        }),
      );
      assert.deepEqual(faults.queued, []);
      const owners = yield* fs.readDirectory(lock);
      assert.equal(owners.length, 1);
      assert.notEqual(owners[0], stale);
      release();
      assert.isFalse(yield* fs.exists(lock));
    }),
  );

  it.effect("reports why acquisition failed even when cleaning up its candidate fails", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const root = yield* fs.makeTempDirectoryScoped({ prefix: "oni-service-selection-lock-" });
      const selectionPath = path.join(root, "service-state.json");
      yield* injectFaults({ call: "renameSync", code: "EIO" }, { call: "rmSync", code: "EBUSY" });

      const error = yield* Effect.promise(() =>
        acquireServiceSelectionLock(selectionPath).then(
          () => undefined,
          (cause: unknown) => cause,
        ),
      );
      assert.deepEqual(faults.queued, []);
      assert.equal((error as NodeJS.ErrnoException | undefined)?.code, "EIO");
    }),
  );

  it.effect("reclaims its own lease when a Windows release could not remove it", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const root = yield* fs.makeTempDirectoryScoped({ prefix: "oni-service-selection-lock-" });
      const selectionPath = path.join(root, "service-state.json");
      const lock = `${selectionPath}.selection-lock`;

      const releaseFirst = yield* Effect.promise(() =>
        acquireServiceSelectionLock(selectionPath, { platform: "win32" }),
      );
      // rmSync retries EBUSY and EPERM, but not EACCES.
      yield* injectFaults({ call: "rmSync", code: "EACCES" });
      releaseFirst();
      assert.lengthOf(yield* fs.readDirectory(lock), 1);

      const releaseSecond = yield* Effect.promise(() =>
        acquireServiceSelectionLock(selectionPath, {
          platform: "win32",
          waitForRetry: () => Promise.reject(new Error("waited on this process's own lease")),
        }),
      );
      releaseSecond();
      assert.isFalse(yield* fs.exists(lock));
    }),
  );

  it.effect("waits on a contended live lease until the first holder releases it", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const root = yield* fs.makeTempDirectoryScoped({ prefix: "oni-service-selection-lock-" });
      const selectionPath = path.join(root, "service-state.json");
      const lock = `${selectionPath}.selection-lock`;

      const releaseFirst = yield* Effect.promise(() => acquireServiceSelectionLock(selectionPath));
      let signalRetryStarted!: () => void;
      const retryStarted = new Promise<void>((resolve) => {
        signalRetryStarted = resolve;
      });
      let releaseRetryGate!: () => void;
      const retryGate = new Promise<void>((resolve) => {
        releaseRetryGate = resolve;
      });
      let secondAcquired = false;
      const second = acquireServiceSelectionLock(selectionPath, {
        waitForRetry: () => {
          signalRetryStarted();
          return retryGate;
        },
      }).then((release) => {
        secondAcquired = true;
        return release;
      });
      const firstOutcome = yield* Effect.promise(() =>
        Promise.race([
          retryStarted.then(() => "retrying" as const),
          second.then(() => "acquired" as const),
        ]),
      );
      assert.equal(firstOutcome, "retrying");
      assert.isFalse(secondAcquired);
      releaseFirst();
      releaseRetryGate();
      const releaseSecond = yield* Effect.promise(() => second);
      assert.isTrue(secondAcquired);
      releaseSecond();
      assert.isFalse(yield* fs.exists(lock));
    }),
  );
});
