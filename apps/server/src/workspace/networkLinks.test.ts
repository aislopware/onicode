// @effect-diagnostics nodeBuiltinImport:off
import * as NodeChildProcess from "node:child_process";
import * as NodeFS from "node:fs";
import * as NodeFSP from "node:fs/promises";
import * as NodeOS from "node:os";

import * as NodeServices from "@effect/platform-node/NodeServices";
import { describe, expect, it } from "@effect/vitest";
import * as HostProcess from "@onicode/shared/HostProcess";
import { symlinksSupported } from "@onicode/shared/testing/symlinks";
import * as Effect from "effect/Effect";
import * as Path from "effect/Path";

import { networkPathReachedBy } from "./networkLinks.ts";

const windows = HostProcess.Platform.defaultValue() === "win32";
// `.invalid` never resolves, and a typed link is made without reading its target.
const SHARE = "\\\\attacker.invalid\\share";
const adminShare = (localPath: string) => `\\\\localhost\\${localPath[0]}$${localPath.slice(2)}`;
const adminSharesReachable =
  windows && NodeFS.existsSync(adminShare(NodeFS.realpathSync(NodeOS.tmpdir())));

const fixture = Effect.acquireRelease(
  Effect.promise(async () => {
    const directory = await NodeFSP.mkdtemp(`${NodeFS.realpathSync(NodeOS.tmpdir())}\\oni-net-`);
    await NodeFSP.mkdir(`${directory}\\workspace`);
    return directory;
  }),
  (directory) => Effect.promise(() => NodeFSP.rm(directory, { recursive: true, force: true })),
);
const link = (target: string, at: string) =>
  Effect.promise(() => NodeFSP.symlink(target, at, "dir"));
// Node makes every absolute target drive-absolute, so these come from mklink as typed.
const rawLink = (target: string, at: string) =>
  Effect.sync(() => NodeChildProcess.execFileSync("cmd", ["/c", "mklink", "/D", at, target]));

describe.runIf(windows && symlinksSupported)("networkPathReachedBy", () => {
  it.layer(NodeServices.layer)((it) => {
    it.effect("names the share a workspace link leads to, without following it", () =>
      Effect.scoped(
        Effect.gen(function* () {
          const path = yield* Path.Path;
          const directory = yield* fixture;
          const workspace = `${directory}\\workspace`;
          yield* link(SHARE, `${workspace}\\nas`);

          const reached = (filePath: string) =>
            Effect.promise(() =>
              networkPathReachedBy(path, filePath, { from: workspace, allowedRoot: workspace }),
            );
          expect(yield* reached("nas\\notes\\a.md")).toBe(`${SHARE}\\notes\\a.md`);
          expect(yield* reached("sub\\..\\nas")).toBe(SHARE);
          expect(
            yield* Effect.promise(() =>
              networkPathReachedBy(path, `${workspace}\\nas\\a.md`, { allowedRoot: workspace }),
            ),
          ).toBe(`${SHARE}\\a.md`);
          expect(yield* reached("missing\\a.md")).toBeNull();
        }),
      ),
    );

    it.effect("keeps a root-relative target on the link's own drive", () =>
      Effect.scoped(
        Effect.gen(function* () {
          const path = yield* Path.Path;
          const directory = yield* fixture;
          const workspace = `${directory}\\workspace`;
          yield* Effect.promise(() => NodeFSP.mkdir(`${directory}\\other`));
          yield* link(SHARE, `${directory}\\other\\nas`);
          // `\Users\...\other\nas`: the drive comes from where the link sits.
          yield* rawLink(`${directory.slice(2)}\\other\\nas`, `${workspace}\\rooted`);

          expect(
            yield* Effect.promise(() =>
              networkPathReachedBy(path, "rooted\\a.md", {
                from: workspace,
                allowedRoot: workspace,
              }),
            ),
          ).toBe(`${SHARE}\\a.md`);
        }),
      ),
    );

    it.effect("refuses links it cannot place: drive-relative targets and long chains", () =>
      Effect.scoped(
        Effect.gen(function* () {
          const path = yield* Path.Path;
          const directory = yield* fixture;
          const workspace = `${directory}\\workspace`;
          yield* rawLink("C:elsewhere", `${workspace}\\drive-relative`);
          for (let index = 0; index < 45; index += 1) {
            yield* link(`${workspace}\\chain-${index + 1}`, `${workspace}\\chain-${index}`);
          }
          yield* Effect.promise(() => NodeFSP.mkdir(`${workspace}\\chain-45`));

          const reached = (filePath: string) =>
            Effect.promise(() =>
              networkPathReachedBy(path, filePath, { from: workspace, allowedRoot: workspace }),
            );
          expect(yield* reached("drive-relative\\a.md")).not.toBeNull();
          expect(yield* reached("chain-0\\a.md")).not.toBeNull();
          expect(yield* reached("chain-30\\a.md")).toBeNull();
        }),
      ),
    );

    it.effect.runIf(adminSharesReachable)(
      "allows the share a workspace root links to, and walks on from it",
      () =>
        Effect.scoped(
          Effect.gen(function* () {
            const path = yield* Path.Path;
            const directory = yield* fixture;
            const workspace = `${directory}\\workspace`;
            yield* link(adminShare(workspace), `${directory}\\root-link`);
            yield* link(adminShare(workspace), `${workspace}\\self`);
            yield* link(SHARE, `${workspace}\\nas`);
            const root = `${directory}\\root-link`;

            const reached = (filePath: string) =>
              Effect.promise(() => networkPathReachedBy(path, filePath, { allowedRoot: root }));
            expect(yield* reached(`${root}\\self\\a.md`)).toBeNull();
            expect(yield* reached(`${root}\\self\\nas\\a.md`)).toBe(`${SHARE}\\a.md`);
          }),
        ),
    );
  });
});
