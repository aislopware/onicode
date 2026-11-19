import { EnvironmentId } from "@onicode/contracts";
import { describe, expect, it } from "vite-plus/test";

import { threadLinkCandidates } from "./ThreadLinkPicker.logic";

const here = EnvironmentId.make("here");

function thread(
  id: string,
  overrides: Partial<Parameters<typeof threadLinkCandidates>[0][number]>,
) {
  return {
    id,
    environmentId: here,
    archivedAt: null,
    updatedAt: "2026-11-01T00:00:00.000Z",
    lineage: { sideChat: false },
    ...overrides,
  };
}

describe("threadLinkCandidates", () => {
  it("offers the environment's unarchived threads the query matches, newest first", () => {
    const threads = [
      thread("older", { updatedAt: "2026-11-01T00:00:00.000Z" }),
      thread("newer", { updatedAt: "2026-11-02T00:00:00.000Z" }),
      thread("archived", { archivedAt: "2026-11-03T00:00:00.000Z" }),
      thread("elsewhere", { environmentId: EnvironmentId.make("elsewhere") }),
      // Linked-thread answers leave side chats out, so linking one would show nothing.
      thread("side-chat", { lineage: { sideChat: true } }),
      thread("unmatched", {}),
    ];
    const labels = new Map([["unmatched", "Release notes"]]);
    expect(
      threadLinkCandidates(
        threads,
        here,
        " FIX ",
        (candidate) => labels.get(candidate.id) ?? `Fix ${candidate.id}`,
      ).map((candidate) => candidate.id),
    ).toEqual(["newer", "older"]);
  });
});
