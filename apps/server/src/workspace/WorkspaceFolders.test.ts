// @effect-diagnostics nodeBuiltinImport:off
import * as NodeFSP from "node:fs/promises";
import * as NodeServices from "@effect/platform-node/NodeServices";
import { assert, it } from "@effect/vitest";
import { ProjectId, type ProjectTrackedFolder } from "@onicode/contracts";
import * as Deferred from "effect/Deferred";
import * as HostProcess from "@onicode/shared/HostProcess";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as FileSystem from "effect/FileSystem";
import * as Layer from "effect/Layer";
import * as Path from "effect/Path";
import * as Queue from "effect/Queue";
import * as TestClock from "effect/testing/TestClock";
import { vi } from "vite-plus/test";

import * as ProjectStore from "../orchestration-v2/ProjectStore.ts";
import * as SqlitePersistence from "../persistence/Sqlite.ts";
import { resolveLinkTarget } from "./linkTargets.ts";
import * as TaskFolderStore from "./TaskFolderStore.ts";
import * as WorkspaceFolders from "./WorkspaceFolders.ts";

vi.mock("node:fs/promises", async (importOriginal) => {
  const actual = await importOriginal<typeof import("node:fs/promises")>();
  return { ...actual, stat: vi.fn(actual.stat) };
});

const projectId = ProjectId.make("project:workspace-folders");

const layerFor = (project?: {
  readonly workspaceRoot: string;
  readonly trackedFolders: ReadonlyArray<ProjectTrackedFolder>;
}) =>
  WorkspaceFolders.layer.pipe(
    Layer.provide(
      Layer.mock(ProjectStore.ProjectStoreV2)({
        get: () =>
          project === undefined
            ? Effect.succeedNone
            : Effect.succeedSome({
                projectId,
                title: "Project",
                hasCustomTitle: false,
                defaultModelSelection: null,
                defaultThreadEnvMode: null,
                autoPull: false,
                faviconPath: null,
                projectIcon: null,
                scripts: [],
                pinnedAt: null,
                archivedAt: null,
                createdAt: "2026-06-21T00:00:00.000Z",
                updatedAt: "2026-06-21T00:00:00.000Z",
                deletedAt: null,
                ...project,
              }),
      }),
    ),
    Layer.provideMerge(TaskFolderStore.layer.pipe(Layer.provide(SqlitePersistence.layerMemory))),
  );

// Resolved, so a symlinked temp directory does not show up as a second spelling of each path.
const makeTempDir = Effect.gen(function* () {
  const fileSystem = yield* FileSystem.FileSystem;
  return yield* fileSystem.realPath(
    yield* fileSystem.makeTempDirectoryScoped({ prefix: "onicode-workspace-folders-" }),
  );
});

it("matches reopened submodule sections by their decoded, case-sensitive names", () => {
  const gitmodules = String.raw`
[submodule.Legacy] path = stale
[SUBMODULE "le\gacy"] path = real
[submodule "Legacy"] path = another
`;
  assert.deepEqual(WorkspaceFolders.parseGitmodulesPaths(gitmodules), ["real", "another"]);
});

it.layer(NodeServices.layer)("WorkspaceFolders", (it) => {
  it.effect("scans child repositories and the folders a code-workspace file lists", () =>
    Effect.gen(function* () {
      const fileSystem = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tmp = yield* makeTempDir;
      const root = path.join(tmp, "wrapper");
      const outside = path.join(tmp, "outside");
      // Labels are cut at 128 UTF-16 units; an emoji across the cut is left out whole.
      const cut = `${"a".repeat(127)}🚀`;
      const fits = `${"b".repeat(126)}🚀`;
      const padded = `${" ".repeat(127)}🚀`;
      for (const folder of [
        ".git",
        `${cut}/.git`,
        `${fits}/.git`,
        `${padded}/.git`,
        "api/.git",
        "web/.git",
        "unrelated/.git",
        "sub/.git",
        "libs/hash/.git",
        "libs/semi/.git",
        "libs/a#b/.git",
        "libs/cont/.git",
        "libs/inline/.git",
        "extra/repo/.git",
        "extra/joined/.git",
        " notes",
        "notes",
        "src/svc/.git",
        "tree",
        ".hidden/.git",
        "node_modules/.git",
        "../outside/.git",
      ]) {
        yield* fileSystem.makeDirectory(path.join(root, folder), { recursive: true });
      }
      if (!(yield* HostProcess.isWindows)) {
        yield* fileSystem.makeDirectory(path.join(root, "trailing "), { recursive: true });
      }
      yield* fileSystem.symlink(outside, path.join(root, "linked"));
      yield* fileSystem.symlink(root, path.join(root, "self"));
      // A worktree of `api`: its history is that repository's.
      yield* fileSystem.writeFileString(
        path.join(root, "tree", ".git"),
        `gitdir: ${path.join(root, "api", ".git", "worktrees", "tree")}\n`,
      );
      // Read as Git reads it: comments, quotes, escapes and keys outside `submodule` sections.
      yield* fileSystem.writeFileString(
        path.join(root, ".gitmodules"),
        [
          "[include]",
          "\tpath = extra/repo",
          '[submodule "sub"]',
          "\tpath = unrelated",
          "\tpath = sub",
          "\turl = https://example.test/sub.git",
          '[submodule "hash"]',
          "\tpath = libs/hash # pinned",
          '[submodule "semi"]',
          "\tpath = libs/semi ; pinned",
          '[submodule "quoted"]',
          '\tpath = "libs/a#b"',
          '[submodule "cont"]',
          "\tpath = libs/\\",
          "cont",
          '[submodule "inline"] path = libs/inline',
          '[submodule "joined"]',
          "\turl = https://example.test/joined.git\\",
          "\tpath = extra/joined",
          "",
        ].join("\n"),
      );
      yield* fileSystem.writeFileString(
        path.join(root, "team.code-workspace"),
        `{
          // Editors write comments and trailing commas here.
          "folders": [
            // Entries that lead nowhere take none of the room the real ones need.
            ${Array.from({ length: 130 }, (_, index) => `{ "path": "gone-${index}" },`).join("")}
            { "path": "api", "name": "API" },
            { "path": " notes", "name": "Intended" },
            { "path": "notes ", "name": "TrailingSpace" },
            { "path": "notes" },
            { "path": "trailing /", "name": "TrailingSlash" },
            { "path": "../outside" },
            { "path": "gone" },
            { "path": "." },
          ],
        }`,
      );

      const workspaceFolders = yield* WorkspaceFolders.WorkspaceFolders;
      assert.deepEqual(yield* workspaceFolders.scan({ workspaceRoot: `${root}${path.sep}` }), {
        workspaceRoot: root,
        isGitRepository: true,
        codeWorkspaceFile: path.join(root, "team.code-workspace"),
        folders: [
          {
            path: path.join(root, "api"),
            name: "API",
            isGitRepository: true,
            source: "code-workspace",
          },
          {
            path: path.join(root, " notes"),
            name: "Intended",
            isGitRepository: false,
            source: "code-workspace",
          },
          {
            path: path.join(root, "notes"),
            name: "notes",
            isGitRepository: false,
            source: "code-workspace",
          },
          { path: outside, name: "outside", isGitRepository: true, source: "code-workspace" },
          { path: path.join(root, padded), name: "🚀", isGitRepository: true, source: "child" },
          {
            path: path.join(root, cut),
            name: "a".repeat(127),
            isGitRepository: true,
            source: "child",
          },
          { path: path.join(root, fits), name: fits, isGitRepository: true, source: "child" },
          // One level below a plain folder is looked into.
          {
            path: path.join(root, "extra", "joined"),
            name: "joined",
            isGitRepository: true,
            source: "child",
          },
          {
            path: path.join(root, "extra", "repo"),
            name: "repo",
            isGitRepository: true,
            source: "child",
          },
          {
            path: path.join(root, "src", "svc"),
            name: "svc",
            isGitRepository: true,
            source: "child",
          },
          {
            path: path.join(root, "unrelated"),
            name: "unrelated",
            isGitRepository: true,
            source: "child",
          },
          { path: path.join(root, "web"), name: "web", isGitRepository: true, source: "child" },
        ],
      });
      assert.equal(
        (yield* workspaceFolders.scan({ workspaceRoot: path.join(root, "gone") }).pipe(Effect.flip))
          ._tag,
        "WorkspaceFoldersError",
      );
    }).pipe(Effect.provide(layerFor())),
  );

  it.effect("finds the repositories inside a project folder from disk each time", () =>
    Effect.gen(function* () {
      const fileSystem = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tmp = yield* makeTempDir;
      const workspaceRoot = path.join(tmp, "wrapper");
      const outside = path.join(tmp, "outside");
      const repository = (name: string) => path.join(workspaceRoot, "src", name);
      for (const folder of ["src/api/.git", "src/notes", "docs/.git", "vendor/big/.git", "tree"]) {
        yield* fileSystem.makeDirectory(path.join(workspaceRoot, folder), { recursive: true });
      }
      // A worktree of `docs` follows that repository, even when a workspace file lists it.
      yield* fileSystem.writeFileString(
        path.join(workspaceRoot, "tree", ".git"),
        `gitdir: ${path.join(workspaceRoot, "docs", ".git", "worktrees", "tree")}\n`,
      );
      yield* fileSystem.writeFileString(
        path.join(workspaceRoot, "team.code-workspace"),
        JSON.stringify({ folders: [{ path: "tree" }] }),
      );
      yield* fileSystem.makeDirectory(path.join(outside, ".git"), { recursive: true });
      // A link to a repository elsewhere is not inside the project.
      yield* fileSystem.symlink(outside, path.join(workspaceRoot, "linked"));
      yield* fileSystem.writeFileString(
        path.join(workspaceRoot, "onicode.json"),
        JSON.stringify({ ignoreRepositories: ["vendor/big"] }),
      );
      // The stored list is only trusted for what someone added.
      const trackedFolders = [
        { path: path.join(workspaceRoot, "gone"), name: "gone", discovered: true },
        { path: outside, name: "Outside" },
      ];

      yield* Effect.gen(function* () {
        const workspaceFolders = yield* WorkspaceFolders.WorkspaceFolders;
        const listed = Effect.map(workspaceFolders.list({ projectId }), ({ folders }) =>
          folders.map((folder) => [folder.name, folder.path]),
        );
        assert.deepEqual(yield* listed, [
          ["docs", path.join(workspaceRoot, "docs")],
          ["api", repository("api")],
          ["Outside", outside],
        ]);
        yield* fileSystem.makeDirectory(path.join(repository("web"), ".git"), { recursive: true });
        yield* fileSystem.remove(repository("api"), { recursive: true });
        assert.deepEqual(yield* listed, [
          ["docs", path.join(workspaceRoot, "docs")],
          ["web", repository("web")],
          ["Outside", outside],
        ]);
        // A folder that cannot be read says nothing about the repositories inside it.
        assert.isNull(yield* WorkspaceFolders.discoverFolders(path.join(tmp, "unmounted")));
        // Server work keeps the repositories found before, not none of them.
        const stored = [
          { path: repository("web"), name: "web", discovered: true },
          { path: outside, name: "Outside" },
        ];
        assert.deepEqual(
          yield* WorkspaceFolders.projectFolders({
            workspaceRoot: path.join(tmp, "unmounted"),
            trackedFolders: stored,
          }),
          stored,
        );
      }).pipe(Effect.provide(layerFor({ workspaceRoot, trackedFolders })));
    }),
  );

  it.effect("does not walk a project's folders when onicode.json ignores the whole project", () =>
    Effect.gen(function* () {
      const fileSystem = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const workspaceRoot = yield* makeTempDir;
      for (const folder of ["api/.git", "src/web/.git", "notes"]) {
        yield* fileSystem.makeDirectory(path.join(workspaceRoot, folder), { recursive: true });
      }
      yield* fileSystem.writeFileString(
        path.join(workspaceRoot, "onicode.json"),
        JSON.stringify({ ignoreRepositories: ["."] }),
      );
      const probed: Array<string> = [];
      const counting = FileSystem.FileSystem.of({
        ...fileSystem,
        readDirectory: (target, options) => {
          probed.push(target);
          return fileSystem.readDirectory(target, options);
        },
        readDirectoryEntries: (target) => {
          probed.push(target);
          return fileSystem.readDirectoryEntries(target);
        },
        stat: (target) => {
          probed.push(target);
          return fileSystem.stat(target);
        },
      });

      const found = yield* WorkspaceFolders.discoverFolders(workspaceRoot).pipe(
        Effect.provideService(FileSystem.FileSystem, counting),
      );

      assert.deepEqual(found, []);
      assert.deepEqual(
        probed.filter((target) => target !== workspaceRoot),
        [],
      );
    }),
  );

  it.effect("finds the repositories beside links whose targets hang on a stalled mount", () =>
    Effect.gen(function* () {
      const fileSystem = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tmp = yield* makeTempDir;
      const workspaceRoot = path.join(tmp, "wrapper");
      const mount = path.join(tmp, "mount", "share");
      for (const folder of ["api/.git", "src/web/.git", "../mount/share/.git"]) {
        yield* fileSystem.makeDirectory(path.join(workspaceRoot, folder), { recursive: true });
      }
      // One link at the top and one inside a plain folder, which the scan looks into.
      const links = [
        path.join(workspaceRoot, "stalled"),
        path.join(workspaceRoot, "src", "stalled"),
      ];
      for (const link of links) yield* fileSystem.symlink(mount, link);
      // A workspace file can list the mount by path too.
      yield* fileSystem.writeFileString(
        path.join(workspaceRoot, "team.code-workspace"),
        JSON.stringify({ folders: [{ path: "../mount/share" }] }),
      );
      const hanging = [...links, mount];

      // Probes of links into one share are one probe of its target.
      const stalled = yield* Deferred.make<void>();
      const releases: Array<() => void> = [];
      const stat = vi.mocked(NodeFSP.stat);
      const realStat = stat.getMockImplementation()!;
      stat.mockImplementation(((target: string) => {
        if (!hanging.includes(target)) return realStat(target);
        Deferred.doneUnsafe(stalled, Effect.void);
        return new Promise((resolve) => releases.push(() => resolve(realStat(target))));
      }) as typeof NodeFSP.stat);
      yield* Effect.addFinalizer(() =>
        Effect.sync(() => {
          for (const release of releases) release();
          stat.mockImplementation(realStat);
        }),
      );
      // Anything else that reaches the mount fails the test instead of hanging it.
      const onMount = (target: string) =>
        hanging.some((link) => target === link || target.startsWith(`${link}${path.sep}`));
      const refuse = (target: string) => {
        const probed = Effect.die(new Error(`Probed ${target} on the stalled mount.`));
        Deferred.doneUnsafe(stalled, probed);
        return probed;
      };
      const guarded = FileSystem.FileSystem.of({
        ...fileSystem,
        exists: (target) => (onMount(target) ? refuse(target) : fileSystem.exists(target)),
        readDirectory: (target, options) =>
          onMount(target) ? refuse(target) : fileSystem.readDirectory(target, options),
        readDirectoryEntries: (target) =>
          onMount(target) ? refuse(target) : fileSystem.readDirectoryEntries(target),
        realPath: (target) => (onMount(target) ? refuse(target) : fileSystem.realPath(target)),
        stat: (target) => (onMount(target) ? refuse(target) : fileSystem.stat(target)),
      });

      const discovery = yield* WorkspaceFolders.discoverFolders(workspaceRoot).pipe(
        Effect.provideService(FileSystem.FileSystem, guarded),
        Effect.forkChild,
      );
      yield* Deferred.await(stalled);
      yield* TestClock.adjust("1 second");
      assert.deepEqual(yield* Fiber.join(discovery), [
        { path: path.join(workspaceRoot, "api"), name: "api", discovered: true },
        { path: path.join(workspaceRoot, "src", "web"), name: "web", discovered: true },
      ]);
    }).pipe(Effect.provide(TestClock.layer())),
  );

  it.effect("keeps the folders someone added when a project holds more than fit", () =>
    Effect.gen(function* () {
      const fileSystem = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tmp = yield* makeTempDir;
      const workspaceRoot = path.join(tmp, "wrapper");
      const outside = path.join(tmp, "outside");
      const longName = "r".repeat(200);
      yield* Effect.forEach(
        [longName, ...Array.from({ length: 130 }, (_, index) => `s${index}`)],
        (name) =>
          fileSystem.makeDirectory(path.join(workspaceRoot, "src", name, ".git"), {
            recursive: true,
          }),
        { discard: true },
      );
      yield* fileSystem.makeDirectory(outside, { recursive: true });

      yield* Effect.gen(function* () {
        const workspaceFolders = yield* WorkspaceFolders.WorkspaceFolders;
        const { folders } = yield* workspaceFolders.list({ projectId });
        assert.equal(folders.length, 128);
        assert.deepEqual(
          [folders[0]!.name, folders[0]!.path],
          ["r".repeat(128), path.join(workspaceRoot, "src", longName)],
        );
        assert.deepEqual([folders.at(-1)!.name, folders.at(-1)!.path], ["Outside", outside]);
      }).pipe(
        Effect.provide(
          layerFor({ workspaceRoot, trackedFolders: [{ path: outside, name: "Outside" }] }),
        ),
      );
    }),
  );

  it.effect("reports each tracked folder's state and grants a thread the ones outside", () =>
    Effect.gen(function* () {
      const fileSystem = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tmp = yield* makeTempDir;
      const workspaceRoot = path.join(tmp, "app");
      const folder = (name: string, ...segments: ReadonlyArray<string>) => ({
        name,
        path: path.join(tmp, ...segments),
      });
      const inside = folder("web", "app", "packages", "web");
      const repository = folder("api", "api");
      const missing = folder("gone", "gone");
      const linked = folder("shared", "app", "linked");
      const shared = path.join(tmp, "shared");
      const worktree = path.join(tmp, "tree");
      // `lib` is checked out with a submodule of its own; `docs` was never initialized. A path
      // listed twice is one submodule. Git reads config keys in any case, so `PATH` declares one.
      // `ext` is a symlink out of the repository, so what it leads to is no submodule of it.
      const gitmodules =
        '[submodule "lib"]\n\tpath = vendor/lib\n[submodule "docs"]\n\tpath = docs\n[submodule "again"]\n\tpath = vendor/lib\n[submodule "out"]\n\tpath = ext/x\n';
      for (const root of [workspaceRoot, worktree]) {
        yield* fileSystem.makeDirectory(path.join(root, "vendor", "lib", "deep", ".git"), {
          recursive: true,
        });
        yield* fileSystem.makeDirectory(path.join(root, "docs"), { recursive: true });
        yield* fileSystem.writeFileString(path.join(root, "vendor", "lib", ".git"), "gitdir: x\n");
        yield* fileSystem.writeFileString(path.join(root, ".gitmodules"), gitmodules);
        yield* fileSystem.writeFileString(
          path.join(root, "vendor", "lib", ".gitmodules"),
          '[submodule "deep"]\n\tPATH = deep\n',
        );
      }
      for (const directory of [inside.path, path.join(repository.path, ".git"), shared]) {
        yield* fileSystem.makeDirectory(directory, { recursive: true });
      }
      yield* fileSystem.symlink(shared, linked.path);
      yield* fileSystem.makeDirectory(path.join(tmp, "elsewhere", "x", ".git"), {
        recursive: true,
      });
      yield* fileSystem.symlink(path.join(tmp, "elsewhere"), path.join(workspaceRoot, "ext"));
      // An added folder on a stalled mount counts as missing for a call it does not answer, as
      // does a link to it inside the project folder, deeper than discovery looks.
      const stalled = folder("share", "mount", "share");
      const stalledLink = folder("mounted", "app", "packages", "web", "mounted");
      yield* fileSystem.makeDirectory(path.join(stalled.path, ".git"), { recursive: true });
      yield* fileSystem.symlink(stalled.path, stalledLink.path);
      const hanging = [stalled.path, stalledLink.path];
      const stalledStarted = yield* Deferred.make<void>();
      const stalledCalls: string[] = [];
      const releases: Array<() => void> = [];
      const stat = vi.mocked(NodeFSP.stat);
      const realStat = stat.getMockImplementation()!;
      stat.mockImplementation(((target: string) => {
        if (!hanging.includes(target)) return realStat(target);
        stalledCalls.push(target);
        if (stalledCalls.length === hanging.length)
          Deferred.doneUnsafe(stalledStarted, Effect.void);
        return new Promise((resolve) => releases.push(() => resolve(realStat(target))));
      }) as typeof NodeFSP.stat);
      yield* Effect.addFinalizer(() =>
        Effect.sync(() => {
          for (const release of releases) release();
          stat.mockImplementation(realStat);
        }),
      );
      const onMount = (target: string) =>
        hanging.some((mounted) => target === mounted || target.startsWith(`${mounted}${path.sep}`));
      const refuse = (target: string) => {
        const probed = Effect.die(new Error(`Probed ${target} on the stalled mount.`));
        Deferred.doneUnsafe(stalledStarted, probed);
        return probed;
      };
      const guarded = FileSystem.FileSystem.of({
        ...fileSystem,
        exists: (target) => (onMount(target) ? refuse(target) : fileSystem.exists(target)),
        realPath: (target) => (onMount(target) ? refuse(target) : fileSystem.realPath(target)),
        stat: (target) => (onMount(target) ? refuse(target) : fileSystem.stat(target)),
      });

      yield* Effect.gen(function* () {
        const workspaceFolders = yield* WorkspaceFolders.WorkspaceFolders;
        assert.deepEqual(yield* workspaceFolders.list({ projectId }), {
          folders: [
            { ...inside, state: "plain" },
            { ...repository, state: "git" },
            { ...missing, state: "missing" },
            { ...linked, state: "plain" },
          ],
        });
        assert.deepEqual(yield* workspaceFolders.forThread({ projectId, worktreePath: null }), {
          folders: [
            { ...inside, isGitRepository: false },
            { ...repository, isGitRepository: true },
            { ...linked, isGitRepository: false },
          ],
          // A symlink inside the project folder still leads outside it.
          extraRoots: [repository.path, linked.path, shared],
        });
        assert.deepEqual(yield* workspaceFolders.forThread({ projectId, worktreePath: worktree }), {
          folders: [],
          extraRoots: [],
        });
        const { folders } = yield* workspaceFolders.forThread({ projectId, worktreePath: null });
        assert.deepEqual(
          yield* workspaceFolders.submodules({ projectId, worktreePath: null, folders }),
          [
            { name: "lib", path: path.join(workspaceRoot, "vendor", "lib"), isGitRepository: true },
            {
              name: "deep",
              path: path.join(workspaceRoot, "vendor", "lib", "deep"),
              isGitRepository: true,
            },
          ],
        );
        // A worktree reaches its own submodules, named by where the project folder has them.
        assert.deepEqual(
          yield* workspaceFolders.submodules({ projectId, worktreePath: worktree, folders: [] }),
          [
            {
              name: "lib",
              path: path.join(worktree, "vendor", "lib"),
              isGitRepository: true,
              repositoryPath: path.join(workspaceRoot, "vendor", "lib"),
            },
            {
              name: "deep",
              path: path.join(worktree, "vendor", "lib", "deep"),
              isGitRepository: true,
              repositoryPath: path.join(workspaceRoot, "vendor", "lib", "deep"),
            },
          ],
        );
      }).pipe(
        Effect.provide(
          layerFor({ workspaceRoot, trackedFolders: [inside, repository, missing, linked] }),
        ),
        Effect.provideService(FileSystem.FileSystem, guarded),
      );

      // Only folders on the mount, so moving the clock times out no healthy folder's probe.
      yield* Effect.gen(function* () {
        const workspaceFolders = yield* WorkspaceFolders.WorkspaceFolders;
        const listing = yield* Effect.forkChild(workspaceFolders.list({ projectId }));
        yield* Deferred.await(stalledStarted);
        yield* TestClock.adjust("1 second");
        assert.deepEqual(yield* Fiber.join(listing), {
          folders: [
            { ...stalled, state: "missing" },
            { ...stalledLink, state: "missing" },
          ],
        });
        assert.deepEqual(yield* workspaceFolders.forThread({ projectId, worktreePath: null }), {
          folders: [],
          extraRoots: [],
        });
      }).pipe(
        Effect.provide(layerFor({ workspaceRoot, trackedFolders: [stalled, stalledLink] })),
        Effect.provideService(FileSystem.FileSystem, guarded),
      );
    }).pipe(Effect.provide(TestClock.layer())),
  );

  it.effect("reports a folder that first timed out waiting behind busy probes", () =>
    Effect.gen(function* () {
      const fileSystem = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tmp = yield* makeTempDir;
      const workspaceRoot = path.join(tmp, "app");
      const healthy = { name: "api", path: path.join(tmp, "api") };
      const busy = ["a", "b", "c"].map((name) => path.join(tmp, "busy", name));
      for (const folder of [workspaceRoot, path.join(healthy.path, ".git"), ...busy]) {
        yield* fileSystem.makeDirectory(folder, { recursive: true });
      }
      const busyStarted = yield* Deferred.make<void>();
      const releases: Array<() => void> = [];
      const releaseAll = () => {
        for (const release of releases.splice(0)) release();
      };
      const stat = vi.mocked(NodeFSP.stat);
      const realStat = stat.getMockImplementation()!;
      stat.mockImplementation(((target: string) => {
        if (!busy.includes(target)) return realStat(target);
        return new Promise((resolve) => {
          releases.push(() => resolve(realStat(target)));
          if (releases.length === busy.length) Deferred.doneUnsafe(busyStarted, Effect.void);
        });
      }) as typeof NodeFSP.stat);
      yield* Effect.addFinalizer(() =>
        Effect.sync(() => {
          releaseAll();
          stat.mockImplementation(realStat);
        }),
      );
      // Probes hold every slot: one outlived its caller's timeout, the others' callers moved on.
      const [timedOut, ...movedOn] = yield* Effect.forEach(busy, (folder) =>
        Effect.forkChild(resolveLinkTarget(folder)),
      );
      yield* Deferred.await(busyStarted);
      yield* Fiber.interruptAll(movedOn);
      yield* TestClock.adjust("1 second");
      assert.equal(yield* Fiber.join(timedOut!), undefined);

      yield* Effect.gen(function* () {
        const workspaceFolders = yield* WorkspaceFolders.WorkspaceFolders;
        // The pool queues a caller with a fresh `Promise.withResolvers`; nothing else here makes one.
        const queued = yield* Queue.unbounded<void>();
        const withResolvers = Promise.withResolvers;
        const queueSpy = vi.spyOn(Promise, "withResolvers").mockImplementation((() => {
          Queue.offerUnsafe(queued, undefined);
          return withResolvers.call(Promise);
        }) as typeof Promise.withResolvers);
        yield* Effect.addFinalizer(() => Effect.sync(() => queueSpy.mockRestore()));

        // A pool that stays busy answers after three tries.
        const crowded = yield* Effect.forkChild(workspaceFolders.list({ projectId }));
        for (let attempt = 0; attempt < 3; attempt += 1) {
          yield* Queue.take(queued);
          yield* TestClock.adjust("1 second");
        }
        assert.deepEqual(yield* Fiber.join(crowded), {
          folders: [{ ...healthy, state: "missing" }],
        });

        const listing = yield* Effect.forkChild(workspaceFolders.list({ projectId }));
        yield* Queue.take(queued);
        yield* TestClock.adjust("1 second");
        releaseAll();
        assert.deepEqual(yield* Fiber.join(listing), {
          folders: [{ ...healthy, state: "git" }],
        });
      }).pipe(Effect.provide(layerFor({ workspaceRoot, trackedFolders: [healthy] })));
    }).pipe(Effect.provide(TestClock.layer())),
  );

  it.effect("gives a task folder thread its worktrees, never the checkouts they came from", () =>
    Effect.gen(function* () {
      const fileSystem = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tmp = yield* makeTempDir;
      const ctx = path.join(tmp, "x", "ctx");
      const app = path.join(tmp, "x", "app");
      const lib = path.join(ctx, "vendor", "lib");
      // The worktrees directory is reached through a symlink, as on a machine that moved it.
      const realWorktrees = path.join(tmp, "disk", "worktrees");
      const worktrees = path.join(tmp, "worktrees");
      const taskRoot = path.join(worktrees, "x", "feature-0123");
      for (const directory of [
        lib,
        app,
        path.join(realWorktrees, "x", "feature-0123", "ctx", "vendor", "lib"),
        path.join(realWorktrees, "x", "feature-0123", "app"),
        path.join(realWorktrees, "x", "single"),
      ]) {
        yield* fileSystem.makeDirectory(directory, { recursive: true });
      }
      yield* fileSystem.symlink(realWorktrees, worktrees);
      const trackedFolders = [
        { name: "app", path: app },
        { name: "lib", path: lib },
      ];

      yield* Effect.gen(function* () {
        const taskFolders = yield* TaskFolderStore.TaskFolderStore;
        const workspaceFolders = yield* WorkspaceFolders.WorkspaceFolders;
        const taskFolder = {
          root: taskRoot,
          projectId,
          projectRoot: ctx,
          anchor: path.join(tmp, "x"),
          projectPath: path.join(taskRoot, "ctx"),
          projectIsWorktree: true,
          repositories: [
            { repositoryPath: app, worktreePath: path.join(taskRoot, "app"), name: "app" },
            {
              repositoryPath: lib,
              worktreePath: path.join(taskRoot, "ctx", "vendor", "lib"),
              name: "lib",
            },
          ],
        };
        yield* taskFolders.put(taskFolder);
        const expected = {
          folders: [
            {
              name: "app",
              path: path.join(taskRoot, "app"),
              isGitRepository: true,
              repositoryPath: app,
            },
            {
              name: "lib",
              path: path.join(taskRoot, "ctx", "vendor", "lib"),
              isGitRepository: true,
              repositoryPath: lib,
            },
          ],
          // Only the sibling lies outside the thread's folder, under both of its spellings.
          extraRoots: [
            path.join(taskRoot, "app"),
            path.join(realWorktrees, "x", "feature-0123", "app"),
          ],
        };
        // The thread stores the path as named; a sandbox or Git may hand back the resolved one.
        for (const worktreePath of [
          path.join(taskRoot, "ctx"),
          path.join(realWorktrees, "x", "feature-0123", "ctx"),
        ]) {
          assert.deepEqual(
            yield* workspaceFolders.forThread({ projectId, worktreePath }),
            expected,
          );
        }
        assert.deepEqual(
          yield* taskFolders.findContaining(
            path.join(realWorktrees, "x", "feature-0123", "app", "src"),
          ),
          taskFolder,
        );
        // A single worktree beside it is no task folder.
        assert.deepEqual(
          yield* workspaceFolders.forThread({
            projectId,
            worktreePath: path.join(worktrees, "x", "single"),
          }),
          { folders: [], extraRoots: [] },
        );
      }).pipe(Effect.provide(layerFor({ workspaceRoot: ctx, trackedFolders })));
    }),
  );
  it.effect("gives a task folder thread the tracked repositories checked out in it later", () =>
    Effect.gen(function* () {
      const fileSystem = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tmp = yield* makeTempDir;
      const project = path.join(tmp, "project");
      const taskRoot = path.join(tmp, "worktrees", "feature");
      for (const name of ["web", "backend", "mobile"]) {
        yield* fileSystem.makeDirectory(path.join(project, name, ".git"), { recursive: true });
      }
      yield* fileSystem.makeDirectory(path.join(taskRoot, "web"), { recursive: true });
      yield* fileSystem.makeDirectory(path.join(taskRoot, "mobile"), { recursive: true });
      // A linked worktree keeps `.git` as a file; `mobile` was never checked out.
      yield* fileSystem.writeFileString(path.join(taskRoot, "web", ".git"), "gitdir: elsewhere\n");

      yield* Effect.gen(function* () {
        const taskFolders = yield* TaskFolderStore.TaskFolderStore;
        const workspaceFolders = yield* WorkspaceFolders.WorkspaceFolders;
        yield* taskFolders.put({
          root: taskRoot,
          projectId,
          projectRoot: project,
          anchor: project,
          projectPath: taskRoot,
          projectIsWorktree: true,
          repositories: [],
        });
        assert.deepEqual(yield* workspaceFolders.forThread({ projectId, worktreePath: taskRoot }), {
          folders: [
            {
              name: "web",
              path: path.join(taskRoot, "web"),
              isGitRepository: true,
              repositoryPath: path.join(project, "web"),
            },
          ],
          extraRoots: [],
        });
      }).pipe(Effect.provide(layerFor({ workspaceRoot: project, trackedFolders: [] })));
    }),
  );
});
