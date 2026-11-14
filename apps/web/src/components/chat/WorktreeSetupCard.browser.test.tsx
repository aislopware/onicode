import { recordServerClock } from "@onicode/client-runtime/clock";
import { EnvironmentId, ThreadId, type WorktreeSetupSnapshot } from "@onicode/contracts";
import { expect, it, vi } from "vite-plus/test";

import { act, fakeTimers, render } from "../../../test/render";
import { WorktreeSetupCard } from "./WorktreeSetupCard";

const NOW = new Date("2026-10-26T12:00:10.000Z");

function snapshot(overrides: Partial<WorktreeSetupSnapshot>): WorktreeSetupSnapshot {
  return {
    threadId: ThreadId.make("thread-1"),
    phase: "running",
    startedAt: "2026-10-26T12:00:00.000Z",
    endedAt: null,
    branch: "onicode/feature",
    baseRef: "main",
    worktreePath: "/repo/.onicode/worktrees/feature",
    setupScript: null,
    stages: [
      {
        id: "fetch",
        status: "done",
        startedAt: "2026-10-26T12:00:00.000Z",
        endedAt: "2026-10-26T12:00:03.000Z",
        percent: null,
        detail: null,
        tail: [],
      },
      {
        id: "checkout",
        status: "running",
        startedAt: "2026-10-26T12:00:03.000Z",
        endedAt: null,
        percent: null,
        detail: null,
        tail: [],
      },
    ],
    error: null,
    sequence: 1,
    ...overrides,
  };
}

const card = (value: WorktreeSetupSnapshot, environmentId = "environment-1") => (
  <WorktreeSetupCard
    environmentId={EnvironmentId.make(environmentId)}
    snapshot={value}
    onCancel={null}
    onWorkLocally={null}
    onOpenTerminal={null}
  />
);

it("ticks the running stage while finished stages keep their duration", async () => {
  using _timers = fakeTimers();
  vi.setSystemTime(NOW);
  await using screen = await render(card(snapshot({})));
  // Assert synchronously: expect.element polls on timers these tests fake.
  const section = screen.getByRole("region", { name: "Worktree setup" });

  expect(section.element().textContent).toContain("Fetch base branch3s");
  expect(section.element().textContent).toContain("Check out files7s");

  await act(async () => {
    await vi.advanceTimersByTimeAsync(5_000);
  });
  expect(section.element().textContent).toContain("Fetch base branch3s");
  expect(section.element().textContent).toContain("Check out files12s");
});

it("counts a running stage on the host's clock", async () => {
  using _timers = fakeTimers();
  vi.setSystemTime(NOW);
  const now = NOW.getTime();
  // The host runs a minute ahead of this client.
  recordServerClock(EnvironmentId.make("environment-ahead"), {
    serverTimeMs: now + 60_000,
    requestedAtMs: now,
    receivedAtMs: now,
  });
  const started = "2026-10-26T12:01:03.000Z";
  await using screen = await render(
    card(
      snapshot({
        startedAt: started,
        stages: [
          {
            id: "checkout",
            status: "running",
            startedAt: started,
            endedAt: null,
            percent: null,
            detail: null,
            tail: [],
          },
        ],
      }),
      "environment-ahead",
    ),
  );

  expect(screen.getByRole("region", { name: "Worktree setup" }).element().textContent).toContain(
    "Check out files7s",
  );
});

it("stops a stage interrupted by the setup ending at the setup's end", async () => {
  using _timers = fakeTimers();
  vi.setSystemTime(NOW);
  const failed = snapshot({ phase: "failed", endedAt: "2026-10-26T12:00:05.000Z" });
  await using screen = await render(card(failed));
  const section = screen.getByRole("region", { name: "Worktree setup" });

  expect(section.element().textContent).toContain("Check out files2s");
  await act(async () => {
    await vi.advanceTimersByTimeAsync(5_000);
  });
  expect(section.element().textContent).toContain("Check out files2s");
});
