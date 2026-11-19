import { describe, expect, it } from "vite-plus/test";

import { findProjectByPath, isScratchProject } from "./projects.ts";

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
