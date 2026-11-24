// @effect-diagnostics nodeBuiltinImport:off -- Shared with the standalone launcher, which has no Effect runtime.
// @effect-diagnostics globalTimers:off -- Bounded cross-process lease acquisition in the standalone launcher.
// @effect-diagnostics globalDate:off -- The standalone launcher has no Effect clock for its acquisition deadline.
import * as NodeCrypto from "node:crypto";
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";

// Coarse boot time: a lease from an earlier boot is dead even when its PID is live again.
const bootEpochSeconds = () => Math.round(Date.now() / 1000 - NodeOS.uptime());

// Leases this process released but could not delete; other processes wait for it to exit.
const abandonedOwners = new Set<string>();

/** Serialize durable selection and unit writes, never manager stop/start commands. */
export async function acquireServiceSelectionLock(
  selectionPath: string,
  options?: {
    readonly waitForRetry?: () => Promise<void>;
    readonly platform?: NodeJS.Platform;
  },
): Promise<() => void> {
  const waitForRetry =
    options?.waitForRetry ?? (() => new Promise<void>((resolve) => setTimeout(resolve, 50)));
  const directory = NodePath.dirname(selectionPath);
  NodeFS.mkdirSync(directory, { recursive: true, mode: 0o700 });
  const lock = selectionPath + ".selection-lock";
  const owner = `${process.pid}.${bootEpochSeconds()}.${NodeCrypto.randomUUID()}`;
  const candidate = `${lock}.${owner}`;
  const deadline = Date.now() + 30_000;
  // oxlint-disable-next-line onicode/no-global-process-runtime -- Standalone launcher has no Effect runtime.
  const windows = (options?.platform ?? NodeOS.platform()) === "win32";
  // A lock another process is deleting stays DELETE_PENDING on Windows until its last handle
  // closes, and every operation on it fails with one of these until then.
  const contendedOnWindows = (error: unknown) =>
    windows && ["EBUSY", "EPERM", "EACCES"].includes((error as NodeJS.ErrnoException).code ?? "");
  const removeEmptyLock = () => {
    try {
      NodeFS.rmdirSync(lock);
    } catch (error) {
      const code = (error as NodeJS.ErrnoException).code ?? "";
      if (!["ENOENT", "ENOTEMPTY", "EEXIST"].includes(code) && !contendedOnWindows(error))
        throw error;
    }
  };
  NodeFS.mkdirSync(candidate, { mode: 0o700 });
  try {
    NodeFS.writeFileSync(NodePath.join(candidate, owner), "", { mode: 0o600 });
    while (true) {
      try {
        // Publish the owner atomically; rename cannot replace another populated directory.
        NodeFS.renameSync(candidate, lock);
        return () => {
          // The guarded write already happened; a release failure must not undo its success.
          try {
            NodeFS.rmSync(NodePath.join(lock, owner), { force: true, maxRetries: 5 });
          } catch {
            abandonedOwners.add(owner);
          }
          try {
            removeEmptyLock();
          } catch {}
        };
      } catch (error) {
        const code = (error as NodeJS.ErrnoException).code ?? "";
        // Windows MoveFileExW fails with EPERM/EACCES when the target directory already exists.
        if (code !== "EEXIST" && code !== "ENOTEMPTY" && !contendedOnWindows(error)) throw error;
      }
      let reclaimed = false;
      let owners: string[] = [];
      try {
        owners = NodeFS.readdirSync(lock);
      } catch (error) {
        // A Windows rename can fail with EPERM while no lock exists; keep the deadline and delay.
        if ((error as NodeJS.ErrnoException).code !== "ENOENT" && !contendedOnWindows(error))
          throw error;
      }
      if (owners.length === 1) {
        const previous = owners[0]!;
        const [pidText, bootText] = previous.split(".");
        const pid = Number(pidText);
        if (!Number.isSafeInteger(pid) || pid <= 0)
          throw new Error(`Invalid service selection lock owner at ${lock}.`);
        const ownerBoot = Number(bootText);
        const fromEarlierBoot =
          Number.isSafeInteger(ownerBoot) && Math.abs(ownerBoot - bootEpochSeconds()) > 60;
        let dead = fromEarlierBoot || abandonedOwners.has(previous);
        if (!dead) {
          try {
            process.kill(pid, 0);
          } catch (error) {
            const code = (error as NodeJS.ErrnoException).code;
            if (code !== "ESRCH" && code !== "EPERM") throw error;
            dead = code === "ESRCH";
          }
        }
        if (dead) {
          // Remove only this dead owner's unique file, never a replacement owner's.
          try {
            NodeFS.unlinkSync(NodePath.join(lock, previous));
            abandonedOwners.delete(previous);
            reclaimed = true;
          } catch (error) {
            if ((error as NodeJS.ErrnoException).code !== "ENOENT" && !contendedOnWindows(error))
              throw error;
          }
        }
      }
      removeEmptyLock();
      if (reclaimed) continue;
      if (Date.now() >= deadline)
        throw new Error(
          `Service selection is locked at ${lock}. Retry when the other operation finishes.`,
        );
      await waitForRetry();
    }
  } finally {
    // Gone after a successful rename; a leftover must not replace the acquisition's own error.
    try {
      NodeFS.rmSync(candidate, { recursive: true, force: true, maxRetries: 5 });
    } catch {}
  }
}
