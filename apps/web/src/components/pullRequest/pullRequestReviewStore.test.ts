import { beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { ProjectId } from "@onicode/contracts";

import {
  type PendingReviewComment,
  pullRequestReviewKey,
  usePullRequestReviewStore,
} from "./pullRequestReviewStore";
import { createMemoryStorage } from "../../lib/storage";

function comment(id: string, body = id): PendingReviewComment {
  return { id, body, path: "src/app.ts", position: { kind: "added", newLine: 1 } };
}

describe("pull request review drafts", () => {
  beforeEach(() => {
    usePullRequestReviewStore.setState({ drafts: {}, summaries: {} });
  });

  it("removes only the line comments included in a submitted snapshot", () => {
    const store = usePullRequestReviewStore.getState();
    store.addComment("review-a", comment("submitted"));
    const submittedIds =
      usePullRequestReviewStore.getState().drafts["review-a"]?.map((entry) => entry.id) ?? [];

    usePullRequestReviewStore.getState().addComment("review-a", comment("added-in-flight"));
    usePullRequestReviewStore.getState().removeComments("review-a", submittedIds);

    expect(usePullRequestReviewStore.getState().drafts["review-a"]).toEqual([
      comment("added-in-flight"),
    ]);
  });

  it("keeps drafts on different hosts separate when a thread reviews the same repository and number", () => {
    const reference = {
      projectId: ProjectId.make("project-a"),
      repository: "owner/repo",
      number: 7,
    };
    const publicKey = pullRequestReviewKey({ ...reference, host: "github.com" });
    const enterpriseKey = pullRequestReviewKey({ ...reference, host: "github.example.com" });
    const store = usePullRequestReviewStore.getState();
    store.addComment(publicKey, comment("public"));
    store.setSummary(publicKey, "Public review");

    expect(usePullRequestReviewStore.getState().drafts[enterpriseKey]).toBeUndefined();
    expect(usePullRequestReviewStore.getState().summaries[enterpriseKey]).toBeUndefined();

    store.addComment(enterpriseKey, comment("enterprise"));
    store.setSummary(enterpriseKey, "Enterprise review");
    store.clear(enterpriseKey);
    store.clearSummary(enterpriseKey, "Enterprise review");

    expect(usePullRequestReviewStore.getState().drafts[publicKey]).toEqual([comment("public")]);
    expect(usePullRequestReviewStore.getState().summaries[publicKey]).toBe("Public review");
  });

  it("does not clear a summary revised while submission is in flight", () => {
    const store = usePullRequestReviewStore.getState();
    store.setSummary("review-a", "Submitted body");
    usePullRequestReviewStore.getState().setSummary("review-a", "Revised body");
    usePullRequestReviewStore.getState().clearSummary("review-a", "Submitted body");

    expect(usePullRequestReviewStore.getState().summaries["review-a"]).toBe("Revised body");
  });

  it("forgets a summary erased to empty without writing when nothing changed", () => {
    const listener = vi.fn();
    const unsubscribe = usePullRequestReviewStore.subscribe(listener);
    try {
      usePullRequestReviewStore.getState().setSummary("review-a", "");
      expect(listener).not.toHaveBeenCalled();

      usePullRequestReviewStore.getState().setSummary("review-a", "Draft");
      expect(listener).toHaveBeenCalledTimes(1);

      usePullRequestReviewStore.getState().setSummary("review-a", "Draft");
      expect(listener).toHaveBeenCalledTimes(1);

      usePullRequestReviewStore.getState().setSummary("review-a", "");
      expect(listener).toHaveBeenCalledTimes(2);
      expect(usePullRequestReviewStore.getState().summaries).toEqual({});

      usePullRequestReviewStore.getState().setSummary("review-a", "");
      expect(listener).toHaveBeenCalledTimes(2);
    } finally {
      unsubscribe();
    }
  });

  it("restores only well-formed drafts and summaries", async () => {
    const storage = createMemoryStorage();
    storage.setItem(
      "onicode:pull-request-review-drafts:v1",
      JSON.stringify({
        state: {
          drafts: [[comment("from-array")]],
          summaries: { "review-a": "Kept", "review-b": "", "review-c": 3 },
        },
        version: 0,
      }),
    );
    vi.stubGlobal("localStorage", storage);
    vi.resetModules();
    try {
      const restored = await import("./pullRequestReviewStore");
      expect(restored.usePullRequestReviewStore.getState().drafts).toEqual({});
      expect(restored.usePullRequestReviewStore.getState().summaries).toEqual({
        "review-a": "Kept",
      });

      storage.setItem(
        "onicode:pull-request-review-drafts:v1",
        JSON.stringify({ state: { drafts: {}, summaries: "hi" }, version: 0 }),
      );
      await restored.usePullRequestReviewStore.persist.rehydrate();
      expect(restored.usePullRequestReviewStore.getState().summaries).toEqual({});
    } finally {
      vi.unstubAllGlobals();
      vi.resetModules();
    }
  });

  // regression: #42078 line comments and the summary were lost when the window reloaded
  it("keeps unsubmitted line comments and the summary across a reload", async () => {
    vi.stubGlobal("localStorage", createMemoryStorage());
    vi.useFakeTimers();
    vi.resetModules();
    try {
      const before = await import("./pullRequestReviewStore");
      before.usePullRequestReviewStore.getState().addComment("review-a", comment("drafted"));
      before.usePullRequestReviewStore.getState().setSummary("review-a", "Drafted summary");
      await vi.runAllTimersAsync();

      vi.resetModules();
      const after = await import("./pullRequestReviewStore");
      await vi.runAllTimersAsync();

      expect(after.usePullRequestReviewStore.getState().drafts["review-a"]).toEqual([
        comment("drafted"),
      ]);
      expect(after.usePullRequestReviewStore.getState().summaries["review-a"]).toBe(
        "Drafted summary",
      );
    } finally {
      vi.useRealTimers();
      vi.unstubAllGlobals();
      vi.resetModules();
    }
  });
});
