import { EnvironmentId, ThreadId } from "@onicode/contracts";
import { useImperativeHandle, useLayoutEffect, useRef, type RefObject } from "react";
import { expect, it } from "vite-plus/test";

import { act, render } from "../../../test/render";
import { makeThreadFixture } from "../../test-fixtures";
import type { Thread } from "../../types";
import { useLocalDispatchState } from "./useLocalDispatchState";

type HookResult = ReturnType<typeof useLocalDispatchState>;

const ENV = EnvironmentId.make("env-1");
const OTHER_ENV = EnvironmentId.make("env-2");
const THREAD_A = makeThreadFixture({ id: ThreadId.make("thread-a"), environmentId: ENV });
const THREAD_B = makeThreadFixture({ id: ThreadId.make("thread-b"), environmentId: ENV });
const THREAD_B_ELSEWHERE = makeThreadFixture({
  id: ThreadId.make("thread-b"),
  environmentId: OTHER_ENV,
});

function Harness(props: {
  readonly thread: Thread;
  readonly hookRef: RefObject<HookResult | null>;
}) {
  const routeThreadKey = `${props.thread.environmentId}:${props.thread.id}`;
  const routeRef = useRef<string | null>(null);
  useLayoutEffect(() => {
    routeRef.current = routeThreadKey;
  });
  const result = useLocalDispatchState({
    environmentId: props.thread.environmentId,
    routeThreadKey,
    currentRouteThreadKeyRef: routeRef,
    activeThread: props.thread,
    activeLatestRun: props.thread.latestRun,
    latestUserMessageId: null,
    phase: "ready",
    activePendingApproval: null,
    activePendingUserInput: null,
    threadError: null,
  });
  useImperativeHandle(props.hookRef, () => result);
  return null;
}

function requireHook(hook: { current: HookResult | null }): HookResult {
  if (hook.current === null) throw new Error("hook did not render");
  return hook.current;
}

it("keeps a detached send's dispatch state on its own thread", async () => {
  const hook = { current: null as HookResult | null };
  await using screen = await render(<Harness thread={THREAD_A} hookRef={hook} />);
  // A dictated send keeps the setters of the thread it was dictated on.
  const detached = requireHook(hook);

  // The chat view stays mounted across threads, so the hook sees thread B in place.
  await screen.rerender(<Harness thread={THREAD_B} hookRef={hook} />);
  await act(() => detached.beginLocalDispatch());
  expect(requireHook(hook).isSendBusy).toBe(false);

  await act(() => requireHook(hook).beginLocalDispatch());
  await act(() => detached.beginLocalDispatch());
  await act(() => detached.resetLocalDispatch());
  expect(requireHook(hook).isSendBusy).toBe(true);

  // Another environment's thread with the same id is a different thread.
  await screen.rerender(<Harness thread={THREAD_B_ELSEWHERE} hookRef={hook} />);
  expect(requireHook(hook).isSendBusy).toBe(false);
});
