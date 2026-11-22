import { scopedThreadKey, scopeThreadRef } from "@onicode/client-runtime/environment";
import { EnvironmentId, ThreadId } from "@onicode/contracts";
import { useImperativeHandle, type RefObject } from "react";
import { expect, it } from "vite-plus/test";

import { act, render } from "../../../test/render";
import { makeThreadFixture } from "../../test-fixtures";
import type { Thread } from "../../types";
import { useLocalDispatchState } from "./useLocalDispatchState";

type HookResult = ReturnType<typeof useLocalDispatchState>;

const ENV = EnvironmentId.make("env-1");
const THREAD_A = makeThreadFixture({ id: ThreadId.make("thread-a"), environmentId: ENV });
const THREAD_B = makeThreadFixture({ id: ThreadId.make("thread-b"), environmentId: ENV });
// Another environment can hold a thread with the same id.
const REMOTE_THREAD_A = makeThreadFixture({
  id: THREAD_A.id,
  environmentId: EnvironmentId.make("env-2"),
});

function Harness(props: {
  readonly thread: Thread;
  readonly hookRef: RefObject<HookResult | null>;
}) {
  const result = useLocalDispatchState({
    threadKey: scopedThreadKey(scopeThreadRef(props.thread.environmentId, props.thread.id)),
    environmentId: props.thread.environmentId,
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
  expect(requireHook(hook).isSendBusy).toBe(true);

  await screen.rerender(<Harness thread={THREAD_A} hookRef={hook} />);
  expect(requireHook(hook).isSendBusy).toBe(true);
  await screen.rerender(<Harness thread={REMOTE_THREAD_A} hookRef={hook} />);
  expect(requireHook(hook).isSendBusy).toBe(false);

  await act(() => detached.resetLocalDispatch());
  await screen.rerender(<Harness thread={THREAD_A} hookRef={hook} />);
  expect(requireHook(hook).isSendBusy).toBe(false);
  await screen.rerender(<Harness thread={THREAD_B} hookRef={hook} />);
  expect(requireHook(hook).isSendBusy).toBe(true);
});
