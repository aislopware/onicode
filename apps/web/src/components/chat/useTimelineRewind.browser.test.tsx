import { scopedThreadKey, scopeThreadRef } from "@onicode/client-runtime/environment";
import { EnvironmentId, RunId, ThreadId } from "@onicode/contracts";
import { expect, it, vi } from "vite-plus/test";

const state = vi.hoisted(() => ({
  fork: (async () => undefined) as (input: unknown) => Promise<unknown>,
  forked: Promise.resolve(),
}));

vi.mock(import("../../state/use-atom-command"), async (importOriginal) => ({
  ...(await importOriginal()),
  useAtomCommand: (() => (input: unknown) => state.fork(input)) as never,
}));

vi.mock(import("../../state/use-orchestration-command"), async (importOriginal) => ({
  ...(await importOriginal()),
  useOrchestrationCommand: (() => async () => undefined) as never,
}));

vi.mock(import("../../state/use-atom-query-runner"), async (importOriginal) => ({
  ...(await importOriginal()),
  useAtomQueryRunner: (() => async () => undefined) as never,
}));

vi.mock(import("../../state/entities"), async (importOriginal) => ({
  ...(await importOriginal()),
  waitForThreadShell: (async () => true) as never,
}));

import { render } from "../../../test/render";
import { useTimelineRewind } from "./useTimelineRewind";

const environmentId = EnvironmentId.make("environment:test");
const parentRef = scopeThreadRef(environmentId, ThreadId.make("parent"));

/** Answers each fork request once `created` resolves, and reports the fork's thread path. */
function setup(created: Promise<void>) {
  const paths: string[] = [];
  state.fork = async (input) => {
    const { targetThreadId } = (input as { input: { targetThreadId: string } }).input;
    paths.push(`/${encodeURIComponent(environmentId)}/${targetThreadId}`);
    await created;
    return { _tag: "Success", value: undefined };
  };
  state.forked = Promise.resolve();
  return paths;
}

it("opens the fork only when the user is still where they pressed it", async () => {
  const paths = setup(Promise.resolve());
  await using screen = await render(<ForkHarness />, { router: "/environment:test/parent" });
  await screen.getByRole("button", { name: "Fork" }).click();
  await state.forked;
  expect(screen.router?.state.location.pathname).toBe(paths[0]);

  const created = Promise.withResolvers<void>();
  setup(created.promise);
  await screen.getByRole("button", { name: "Fork" }).click();
  await screen.router?.navigate({ to: "/settings/general" });
  created.resolve();
  await state.forked;
  expect(screen.router?.state.location.pathname).toBe("/settings/general");
});

function ForkHarness() {
  const rewind = useTimelineRewind({
    environmentId,
    routeThreadRef: parentRef,
    routeThreadKey: scopedThreadKey(parentRef),
    currentRouteThreadKeyRef: { current: scopedThreadKey(parentRef) },
    serverProjection: null,
    activeThread: { id: parentRef.threadId, title: "Parent" } as never,
    timelineMessages: [],
    phase: "ready",
    isSendBusy: false,
    isRevertingCheckpoint: false,
    supportsConversationRollback: true,
    activeEnvironmentUnavailable: false,
    activeEnvironmentUnavailableLabel: null,
    promptRef: { current: "" },
    composerRef: { current: null } as never,
    composerDraftTarget: { kind: "thread", ref: parentRef } as never,
    setThreadError: () => undefined,
  });
  return (
    <button
      type="button"
      onClick={() => {
        state.forked = rewind.onForkFromRunStable({
          sourceThreadId: parentRef.threadId,
          runId: RunId.make("run-1"),
        });
      }}
    >
      Fork
    </button>
  );
}
