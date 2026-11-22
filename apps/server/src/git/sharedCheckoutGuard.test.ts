import { ProjectId, ThreadId } from "@onicode/contracts";
import * as NodePath from "@effect/platform-node/NodePath";
import * as NodeServices from "@effect/platform-node/NodeServices";
import { describe, expect, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Layer from "effect/Layer";
import * as Path from "effect/Path";

import * as ProjectionStore from "../orchestration-v2/ProjectionStore.ts";
import * as ProjectStore from "../orchestration-v2/ProjectStore.ts";
import * as VcsDriverRegistry from "../vcs/VcsDriverRegistry.ts";
import * as SharedCheckoutGuard from "./sharedCheckoutGuard.ts";

const projectId = ProjectId.make("project-a");
const otherProjectId = ProjectId.make("project-b");
const nestedProjectId = ProjectId.make("project-nested");
const childRepositoryProjectId = ProjectId.make("project-child-repository");

const posixProjects = {
  [projectId]: "/repo",
  [nestedProjectId]: "/repo/packages/app",
  [childRepositoryProjectId]: "/repo/child",
  [otherProjectId]: "/other",
};

const thread = (
  title: string,
  overrides: { readonly projectId?: ProjectId; readonly worktreePath?: string } = {},
) => ({
  id: ThreadId.make(title),
  title,
  projectId: overrides.projectId ?? projectId,
  worktreePath: overrides.worktreePath ?? null,
});

const guardLayer = (input: {
  readonly threads: ReadonlyArray<ReturnType<typeof thread>>;
  readonly projects?: Readonly<Record<string, string>>;
  /** The repository each folder belongs to; "fail" when Git cannot read it. */
  readonly repositoryRoots?: Readonly<Record<string, string | null | "fail">>;
  readonly failThreadLookup?: boolean;
  readonly platform?: Layer.Layer<FileSystem.FileSystem | Path.Path>;
}) =>
  SharedCheckoutGuard.layer.pipe(
    Layer.provide(
      Layer.mergeAll(
        Layer.mock(ProjectionStore.ProjectionStoreV2)({
          listThreadsWithActiveRun: () =>
            input.failThreadLookup
              ? Effect.fail("thread lookup failed" as never)
              : Effect.succeed(input.threads),
        }),
        Layer.mock(ProjectStore.ProjectStoreV2)({
          list: (options) =>
            Effect.succeed(
              Object.entries(input.projects ?? posixProjects)
                .filter(([id]) => options?.projectIds?.includes(ProjectId.make(id)) ?? true)
                .map(([id, workspaceRoot]) => ({ projectId: id, workspaceRoot })) as never,
            ),
        }),
        Layer.mock(VcsDriverRegistry.VcsDriverRegistry)({
          detect: ({ cwd }) => {
            const rootPath = input.repositoryRoots?.[cwd];
            if (rootPath === undefined) return Effect.die(`unexpected repository lookup: ${cwd}`);
            if (rootPath === "fail") return Effect.fail("unreadable repository" as never);
            return Effect.succeed(
              rootPath === null
                ? null
                : ({ kind: "git", repository: { rootPath } } as VcsDriverRegistry.VcsDriverHandle),
            );
          },
        }),
      ),
    ),
    Layer.provide(
      input.platform ??
        Layer.merge(
          Layer.succeed(
            FileSystem.FileSystem,
            FileSystem.makeNoop({ realPath: (target) => Effect.succeed(target) }),
          ),
          NodePath.layerPosix,
        ),
    ),
  );

const assertIdle = (root: string, cwd = root) =>
  Effect.gen(function* () {
    const guard = yield* SharedCheckoutGuard.SharedCheckoutGuard;
    return yield* guard.assertIdle({ operation: "test", cwd, root });
  });

const busyDetail = (titles: ReadonlyArray<string>) =>
  `${titles.map((title) => `'${title}'`).join(" and ")} ${titles.length === 1 ? "is" : "are"} still working in this checkout.`;

describe("SharedCheckoutGuard.assertIdle", () => {
  it.effect.each([
    {
      name: "a subdirectory project, not a nested repository or another project",
      root: "/repo",
      threads: [
        thread("local running"),
        thread("nested running", { projectId: nestedProjectId }),
        thread("child repository running", { projectId: childRepositoryProjectId }),
        thread("other running", { projectId: otherProjectId }),
      ],
      repositoryRoots: { "/repo/packages/app": "/repo", "/repo/child": "/repo/child" },
      busy: ["local running", "nested running"],
    },
    {
      name: "threads sharing a worktree",
      root: "/worktrees/a",
      threads: [
        thread("same worktree", { worktreePath: "/worktrees/a" }),
        thread("other worktree", { worktreePath: "/worktrees/b" }),
        thread("local running"),
      ],
      busy: ["same worktree"],
    },
    {
      name: "a worktree placed inside the checkout",
      root: "/repo",
      threads: [thread("inner worktree", { worktreePath: "/repo/.worktrees/x" })],
      repositoryRoots: { "/repo/.worktrees/x": "/repo/.worktrees/x" },
      busy: [],
    },
    {
      name: "a task folder holding the checkout",
      root: "/tasks/fix/app",
      threads: [
        thread("task running", { worktreePath: "/tasks/fix" }),
        thread("outer repository running"),
      ],
      projects: { [projectId]: "/tasks" },
      repositoryRoots: { "/tasks/fix": null, "/tasks": "/tasks" },
      busy: ["task running"],
    },
    {
      name: "a nested project whose repository cannot be read",
      root: "/repo",
      threads: [thread("nested running", { projectId: nestedProjectId })],
      repositoryRoots: { "/repo/packages/app": "fail" },
      busy: ["nested running"],
    },
    {
      name: "Git's spelling of a Windows root against a saved backslash path",
      root: "C:/repo",
      threads: [
        thread("nested running", { projectId: nestedProjectId }),
        thread("other running", { projectId: otherProjectId }),
      ],
      projects: { [nestedProjectId]: "c:\\repo\\packages\\app", [otherProjectId]: "C:\\other" },
      repositoryRoots: { "c:\\repo\\packages\\app": "C:/repo" },
      platform: Layer.merge(
        Layer.succeed(
          FileSystem.FileSystem,
          FileSystem.makeNoop({ realPath: (target) => Effect.succeed(target) }),
        ),
        NodePath.layerWin32,
      ),
      busy: ["nested running"],
    },
    {
      name: "no thread working",
      root: "/repo",
      threads: [],
      busy: [],
    },
  ])("busy threads with $name", ({ root, busy, ...input }) =>
    Effect.gen(function* () {
      const result = yield* assertIdle(root, `${root}/src`).pipe(
        Effect.flip,
        Effect.option,
        Effect.provide(guardLayer(input)),
      );
      if (busy.length === 0) {
        expect(result._tag).toBe("None");
        return;
      }
      const detail = result._tag === "Some" ? result.value.detail : "";
      expect(detail.slice(0, busyDetail(busy).length)).toBe(busyDetail(busy));
    }),
  );

  it.effect("refuses a switch when the running threads cannot be read", () =>
    Effect.gen(function* () {
      const error = yield* assertIdle("/repo").pipe(
        Effect.flip,
        Effect.provide(guardLayer({ threads: [], failThreadLookup: true })),
      );
      expect(error.detail).toBe("Could not check whether a thread is working in this checkout.");
    }),
  );

  it.effect("refuses a switch while a thread works in the checkout through a symlink", () =>
    Effect.gen(function* () {
      const fileSystem = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tmp = yield* fileSystem.makeTempDirectoryScoped({ prefix: "onicode-guard-" });
      const real = path.join(tmp, "real");
      yield* fileSystem.makeDirectory(real);
      yield* fileSystem.symlink(real, path.join(tmp, "alias"));
      // Git reports the resolved path; the project keeps the one the user picked.
      const root = yield* fileSystem.realPath(real);
      const error = yield* assertIdle(root).pipe(
        Effect.flip,
        Effect.provide(
          guardLayer({
            threads: [thread("local running")],
            projects: { [projectId]: path.join(tmp, "alias") },
            platform: NodeServices.layer,
          }),
        ),
      );
      expect(error.detail).toMatch(/^'local running' is still working/);
    }).pipe(Effect.scoped, Effect.provide(NodeServices.layer)),
  );
});
