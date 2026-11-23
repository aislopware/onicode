import { EnvironmentId, type ProjectReadFileResult } from "@onicode/contracts";
import { AsyncResult } from "effect/reactivity";
import { expect, it, vi } from "vite-plus/test";

const testState = await vi.hoisted(async () => {
  const { AsyncResult, Atom, AtomRegistry } = await import("effect/reactivity");
  return {
    registry: AtomRegistry.make(),
    projectFile: Atom.make<AsyncResult.AsyncResult<ProjectReadFileResult | null>>(
      AsyncResult.initial(),
    ),
  };
});
vi.mock("~/rpc/atomRegistry", () => ({ appAtomRegistry: testState.registry }));
vi.mock("~/components/files/projectFilesQueryState", () => ({
  getProjectFileQueryAtom: () => testState.projectFile,
  resolveProjectFileQueryData: (
    _environmentId: unknown,
    _workspaceRoot: unknown,
    _relativePath: unknown,
    data: ProjectReadFileResult | null,
  ) => data,
}));
vi.mock("@onicode/client-runtime/state/runtime", () => ({ executeAtomQuery: vi.fn() }));

import { peekOniProjectFile } from "./oniProjectFileDefaults";

const environmentId = EnvironmentId.make("environment-project-file");
const worktreeFile: ProjectReadFileResult = {
  relativePath: "onicode.json",
  contents: JSON.stringify({ defaultThreadEnvMode: "worktree" }),
  byteLength: 34,
  truncated: false,
};

it("peeks a loaded onicode.json as current only while it is not refetching", () => {
  expect(peekOniProjectFile(environmentId, "/repo")).toBeUndefined();

  testState.registry.set(testState.projectFile, AsyncResult.success(worktreeFile));
  expect(peekOniProjectFile(environmentId, "/repo")).toEqual({
    projectFile: { defaultThreadEnvMode: "worktree" },
    current: true,
  });

  // An edit to the file refetches it; the old copy is only a guess until the read lands.
  testState.registry.set(
    testState.projectFile,
    AsyncResult.success(worktreeFile, { waiting: true }),
  );
  expect(peekOniProjectFile(environmentId, "/repo")).toEqual({
    projectFile: { defaultThreadEnvMode: "worktree" },
    current: false,
  });
});
