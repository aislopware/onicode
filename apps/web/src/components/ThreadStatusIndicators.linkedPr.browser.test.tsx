import { EnvironmentId, type ThreadPullRequestLink } from "@onicode/contracts";
import { useState } from "react";
import { expect, it, vi } from "vite-plus/test";
import { cdp, page, userEvent } from "vite-plus/test/browser";

import { render } from "../../test/render";
import { AppAtomRegistryProvider } from "../rpc/atomRegistry";
import { makeThreadFixture } from "../test-fixtures";
import { ThreadLinkBadge, ThreadRowLeadingStatus } from "./ThreadStatusIndicators";
import { SharedTooltipProvider } from "./ui/shared-tooltip";

const environmentId = EnvironmentId.make("environment-test");

vi.mock(import("../state/entities"), async (original) => ({
  ...(await original()),
  useProject: () => null,
  useServerConfigs: (() =>
    new Map([
      [environmentId, { environment: { capabilities: { threadPullRequests: true } } }],
    ])) as never,
}));

it("updates palette PR status from linked snapshots without a legacy link or branch PR", async () => {
  const link: ThreadPullRequestLink = {
    host: "github.com",
    repository: "example/repo",
    number: 7,
    url: "https://github.com/example/repo/pull/7",
    source: "manual",
    linkedAt: "2026-01-01T00:00:00Z",
    stack: null,
    snapshot: {
      state: "merged",
      title: "Linked change",
      headBranch: "deleted-branch",
      baseBranch: "main",
      isDraft: false,
      updatedAt: null,
      syncedAt: "2026-01-03T00:00:00Z",
    },
  };
  const row = (pullRequest: ThreadPullRequestLink) => (
    <AppAtomRegistryProvider>
      <ThreadRowLeadingStatus
        thread={makeThreadFixture({
          environmentId,
          linkedPullRequest: null,
          branchPullRequest: null,
          pullRequests: [pullRequest],
        })}
      />
    </AppAtomRegistryProvider>
  );
  await using screen = await render(row(link));
  await expect.element(screen.getByLabelText("PR #7 - Merged: Linked change")).toBeVisible();
  await expect.element(screen.getByLabelText("PR #7, status pending")).not.toBeInTheDocument();

  await screen.rerender(row({ ...link, snapshot: null }));
  await expect.element(screen.getByLabelText("PR #7, status pending")).toBeVisible();
  await expect
    .element(screen.getByLabelText("PR #7 - Merged: Linked change"))
    .not.toBeInTheDocument();

  await screen.rerender(
    row({ ...link, snapshot: { ...link.snapshot!, state: "open", isDraft: true } }),
  );
  await expect.element(screen.getByLabelText("PR #7 - Draft: Linked change")).toBeVisible();
  await expect.element(screen.getByLabelText("PR #7, status pending")).not.toBeInTheDocument();
});

function GrowingList() {
  const [expanded, setExpanded] = useState(false);
  return (
    <>
      {Array.from({ length: expanded ? 12 : 3 }, (_, index) => (
        <div key={index} className="h-7">
          Row {index + 1}
        </div>
      ))}
      <button type="button" onClick={() => setExpanded(true)}>
        Show more
      </button>
      <button type="button">Last action</button>
    </>
  );
}

it("a badge card that grows under the pointer keeps the pointer inside it", async () => {
  await page.viewport(800, 600);
  await using screen = await render(
    <SharedTooltipProvider>
      {/* Low enough that the grown card would fit by flipping to end above the badge. */}
      <div style={{ position: "fixed", top: 440, left: 20 }}>
        <ThreadLinkBadge
          control={<button type="button" aria-label="12 linked issues" />}
          Icon={() => null}
          toneClassName=""
          text={12}
          side="right"
        >
          <GrowingList />
        </ThreadLinkBadge>
      </div>
    </SharedTooltipProvider>,
  );
  // Without the slide, no frame between the two placements can pass for the settled one.
  const session = cdp();
  await session.send("Emulation.setEmulatedMedia", {
    features: [{ name: "prefers-reduced-motion", value: "reduce" }],
  });
  await using _motion = {
    [Symbol.asyncDispose]: async () => {
      await session.send("Emulation.setEmulatedMedia", { features: [] });
    },
  };
  await userEvent.hover(screen.getByRole("button", { name: "12 linked issues" }));
  const showMore = page.getByRole("button", { name: "Show more" });
  await expect.element(showMore).toBeVisible();
  const box = showMore.element().getBoundingClientRect();
  const pointer = box.top + box.height / 2;
  await userEvent.click(showMore);
  await expect.element(page.getByText("Row 12")).toBeInTheDocument();
  // A card that moved off the pointer closes on the next pointer event, under the user's hand.
  await vi.waitFor(() => {
    const card = document.querySelector("[data-slot='tooltip-popup']")!.getBoundingClientRect();
    expect(card.bottom).toBeLessThanOrEqual(window.innerHeight);
    expect([card.top <= pointer, card.bottom >= pointer]).toEqual([true, true]);
  });
});
