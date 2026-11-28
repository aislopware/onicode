// @effect-diagnostics nodeBuiltinImport:off
import * as NodeFS from "node:fs";
import * as NodeFSP from "node:fs/promises";
import * as NodeOS from "node:os";

import * as NodePath from "@effect/platform-node/NodePath";
import { describe, expect, it } from "@effect/vitest";
import * as HostProcess from "@onicode/shared/HostProcess";
import { symlinksSupported } from "@onicode/shared/testing/symlinks";
import * as Effect from "effect/Effect";
import * as Path from "effect/Path";
import { vi } from "vite-plus/test";

import { networkPathReachedBy } from "./networkLinks.ts";
import { ATTACKER_SHARE, fakeWindowsTree } from "./networkLinks.testkit.ts";

vi.mock("node:fs/promises", async (importOriginal) => {
  const actual = await importOriginal<typeof import("node:fs/promises")>();
  return { ...actual, readlink: vi.fn(actual.readlink), realpath: vi.fn(actual.realpath) };
});

const SHARE = ATTACKER_SHARE;

type Reached = (
  filePath: string,
  options?: Parameters<typeof networkPathReachedBy>[2],
) => Promise<string | null>;

// Every row but the last runs a win32 path service over an in-memory tree, on any host.
const walk = (
  tree: Parameters<typeof fakeWindowsTree>[0],
  run: (reached: Reached) => Effect.Effect<void>,
) =>
  Effect.gen(function* () {
    const path = yield* Path.Path;
    const fake = fakeWindowsTree(tree);
    const readlink = vi.mocked(NodeFSP.readlink).mockClear().mockImplementation(fake.readlink);
    vi.mocked(NodeFSP.realpath).mockImplementation(fake.realpath);
    yield* run((filePath, options) => networkPathReachedBy(path, filePath, options));
    expect(fake.reachedShare()).toEqual([]);
    return readlink.mock.calls.map(([target]) => String(target));
  }).pipe(Effect.provide(NodePath.layerWin32));

describe("networkPathReachedBy", () => {
  it.effect("names the share a workspace link leads to, collapsing `..` first", () =>
    walk(
      {
        // Windows reads a directory link to a share back with a trailing separator.
        links: { "D:\\ws\\nas": `${SHARE}\\`, "D:\\ws\\up": "missing\\..\\nas" },
        files: { "D:\\ws\\a.md": "" },
      },
      (reached) =>
        Effect.promise(async () => {
          expect(await reached("D:\\ws\\nas\\notes\\a.md", { allowedRoot: "D:\\ws" })).toBe(
            `${SHARE}\\notes\\a.md`,
          );
          const below = { from: "D:\\ws", allowedRoot: "D:\\ws" };
          // Neither `missing` exists: Windows drops it with the `..` before it reads a link.
          expect(await reached("missing\\..\\nas", below)).toBe(SHARE);
          expect(await reached("up\\a.md", below)).toBe(`${SHARE}\\a.md`);
          expect(await reached("a.md", below)).toBeNull();
        }),
    ).pipe(Effect.asVoid),
  );

  it.effect("keeps a root-relative target on the link's own drive", () =>
    Effect.gen(function* () {
      const read = yield* walk(
        { links: { "D:\\ws\\rooted": "\\other\\nas", "D:\\other\\nas": SHARE } },
        (reached) =>
          Effect.promise(async () => {
            expect(await reached("D:\\ws\\rooted\\a.md", { allowedRoot: "D:\\ws" })).toBe(
              `${SHARE}\\a.md`,
            );
          }),
      );
      // Not the drive of the process's working directory.
      expect(read).toContain("D:\\other\\nas");
    }),
  );

  it.effect("refuses links it cannot place: drive-relative targets and long chains", () =>
    walk(
      {
        links: {
          "D:\\ws\\drive-relative": "C:elsewhere",
          ...Object.fromEntries(
            Array.from({ length: 45 }, (_, index) => [
              `D:\\ws\\chain-${index}`,
              `D:\\ws\\chain-${index + 1}`,
            ]),
          ),
        },
        files: { "D:\\ws\\chain-45\\a.md": "" },
      },
      (reached) =>
        Effect.promise(async () => {
          const options = { allowedRoot: "D:\\ws" };
          expect(await reached("D:\\ws\\drive-relative\\a.md", options)).toBe(
            "D:\\ws\\drive-relative",
          );
          expect(await reached("D:\\ws\\chain-0\\a.md", options)).not.toBeNull();
          expect(await reached("D:\\ws\\chain-30\\a.md", options)).toBeNull();
        }),
    ).pipe(Effect.asVoid),
  );

  it.effect("allows the share a workspace root links to, and walks on from it", () =>
    walk(
      {
        links: {
          "D:\\root-link": "\\\\nas.test\\proj",
          "\\\\nas.test\\proj\\self": "\\\\nas.test\\proj",
          "\\\\nas.test\\proj\\nas": SHARE,
        },
        files: { "\\\\nas.test\\proj\\a.md": "" },
      },
      (reached) =>
        Effect.promise(async () => {
          const options = { allowedRoot: "D:\\root-link" };
          expect(await reached("D:\\root-link\\self\\a.md", options)).toBeNull();
          expect(await reached("D:\\root-link\\self\\nas\\a.md", options)).toBe(`${SHARE}\\a.md`);
        }),
    ).pipe(Effect.asVoid),
  );

  it.effect("reads `\\\\?\\` and `\\??\\` targets as the drive or share they name", () =>
    walk(
      {
        links: {
          "D:\\ws\\globalroot": "\\??\\GLOBALROOT\\Device\\Mup\\attacker.invalid\\share",
          "D:\\ws\\verbatim-unc": "\\\\?\\UNC\\attacker.invalid\\share",
          "D:\\ws\\verbatim-drive": "\\\\?\\D:\\other\\nas",
          "D:\\ws\\nt-drive": "\\??\\D:\\other\\nas",
          "D:\\ws\\verbatim-local": "\\\\?\\D:\\local",
          // Windows leaves a verbatim `..` to the filesystem, which reads it on the share past `nas`.
          "D:\\ws\\verbatim-dots": "\\\\?\\D:\\other\\nas\\..\\..\\local",
          "D:\\other\\nas": SHARE,
        },
        files: { "D:\\local\\a.md": "", "\\\\nas.test\\proj\\a.md": "" },
      },
      (reached) =>
        Effect.promise(async () => {
          const options = { allowedRoot: "D:\\ws" };
          expect(await reached("D:\\ws\\globalroot\\a.md", options)).toBe("D:\\ws\\globalroot");
          expect(await reached("D:\\ws\\verbatim-unc\\a.md", options)).toBe(`${SHARE}\\a.md`);
          expect(await reached("D:\\ws\\verbatim-drive\\a.md", options)).toBe(`${SHARE}\\a.md`);
          expect(await reached("D:\\ws\\nt-drive\\a.md", options)).toBe(`${SHARE}\\a.md`);
          expect(await reached("D:\\ws\\verbatim-local\\a.md", options)).toBeNull();
          expect(await reached("D:\\ws\\verbatim-dots\\a.md", options)).toBe(
            "D:\\ws\\verbatim-dots",
          );
          expect(
            await reached("\\\\?\\UNC\\nas.test\\proj\\a.md", {
              allowedRoot: "\\\\nas.test\\proj",
            }),
          ).toBeNull();
        }),
    ).pipe(Effect.asVoid),
  );

  // Node's readlink on Windows hands back a symlink's target as stored; the rows above assume it.
  it.effect.runIf(HostProcess.Platform.defaultValue() === "win32" && symlinksSupported)(
    "names the share a real workspace link leads to",
    () =>
      Effect.gen(function* () {
        const actual = yield* Effect.promise(() =>
          vi.importActual<typeof NodeFSP>("node:fs/promises"),
        );
        vi.mocked(NodeFSP.readlink).mockImplementation(actual.readlink);
        vi.mocked(NodeFSP.realpath).mockImplementation(actual.realpath);
        const path = yield* Path.Path;
        const directory = yield* Effect.acquireRelease(
          Effect.promise(() => actual.mkdtemp(`${NodeFS.realpathSync(NodeOS.tmpdir())}\\oni-net-`)),
          (created) => Effect.promise(() => actual.rm(created, { recursive: true, force: true })),
        );
        // A typed link is made without reading its target, and `.invalid` never resolves.
        yield* Effect.promise(() => actual.symlink(SHARE, `${directory}\\nas`, "dir"));

        expect(
          yield* Effect.promise(() =>
            networkPathReachedBy(path, `${directory}\\nas\\a.md`, { allowedRoot: directory }),
          ),
        ).toBe(`${SHARE}\\a.md`);
      }).pipe(Effect.scoped, Effect.provide(NodePath.layer)),
  );
});
