import { describe, expect, it } from "vite-plus/test";

import {
  findProjectByPath,
  isScratchProject,
  listedBrowseDirectoryPath,
  resolveProjectPathForDispatch,
} from "./projects.ts";

describe("isScratchProject", () => {
  it.each([null, undefined])(
    "recognises a loaded scratch project when its environment offers no available root (%s)",
    (scratchWorkspaceRoot) => {
      const project = { workspaceRoot: "/remote/scratch", isScratch: true as const };

      expect(isScratchProject(project, scratchWorkspaceRoot)).toBe(true);
    },
  );
});

describe("findProjectByPath", () => {
  it("leaves a folder whose name ends in a space to itself, not its neighbour", () => {
    const projects = [{ workspaceRoot: "/code/foo" }];

    expect(findProjectByPath(projects, "/code/foo/")).toBe(projects[0]);
    expect(findProjectByPath(projects, "/code/foo ")).toBeUndefined();
  });
});

describe("listedBrowseDirectoryPath", () => {
  it("browses into a listed folder whose name ends in a space, not its neighbour", () => {
    expect(listedBrowseDirectoryPath("/code/foo ")).toBe("/code/foo /");
  });
});

describe("resolveProjectPathForDispatch", () => {
  it("keeps literal backslashes in relative POSIX project paths", () => {
    expect(resolveProjectPathForDispatch("./docs\\archive", "/workspace/app")).toBe(
      "/workspace/app/docs\\archive",
    );
    expect(resolveProjectPathForDispatch("./docs\\archive/../other", "/workspace/app")).toBe(
      "/workspace/app/other",
    );
  });

  it("splits relative paths on both slashes and backslashes for Windows bases", () => {
    expect(resolveProjectPathForDispatch("..\\sibling\\sub", "C:\\workspace\\app")).toBe(
      "C:\\workspace\\sibling\\sub",
    );
    expect(resolveProjectPathForDispatch("./docs/archive", "\\\\server\\share\\app")).toBe(
      "\\\\server\\share\\app\\docs\\archive",
    );
  });
});
