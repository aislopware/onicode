import { describe, expect, it } from "vite-plus/test";

import { composerFolderMatches } from "./composerFolderMentions.ts";

const entry = (fullPath: string) => ({
  name: fullPath.split(/[\\/]/).at(-1) ?? fullPath,
  fullPath,
});

describe("composerFolderMatches", () => {
  it("drops the workspace and folders inside it, keeping its siblings", () => {
    const entries = [
      entry("/home/u/work/onicode"),
      entry("/home/u/work/onicode/apps"),
      entry("/home/u/work/onicode-rust"),
      entry("/home/u/notes"),
    ];
    expect(composerFolderMatches(entries, "/home/u/work/onicode/").map((e) => e.fullPath)).toEqual([
      "/home/u/work/onicode-rust",
      "/home/u/notes",
    ]);
  });

  it("compares Windows paths without regard to case or separator", () => {
    const entries = [entry("c:\\Users\\u\\Repo\\src"), entry("C:\\Users\\u\\Other")];
    expect(composerFolderMatches(entries, "C:/Users/u/repo").map((e) => e.fullPath)).toEqual([
      "C:\\Users\\u\\Other",
    ]);
  });
});
