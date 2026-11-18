import { act, useLayoutEffect } from "react";
import { createRoot } from "react-dom/client";
import { RuntimeRequestId } from "@onicode/contracts";
import { expect, it, vi } from "vite-plus/test";
import {
  abortComposerDraftOperations,
  startComposerDraftOperation,
} from "../../state/composerDraftOperations";
import { useSubmitAfterNativeImages } from "./useSubmitAfterNativeImages";

function setup() {
  const root = createRoot(document.createElement("div"));
  let queue: ReturnType<typeof useSubmitAfterNativeImages>;
  let generation = 0;
  let blocked = false;
  let draftKey = "draft";
  let approvalIds: ReadonlyArray<RuntimeRequestId> = [];
  const onSubmit = vi.fn();
  const preparations: Array<(success: boolean) => void> = [];
  // Like the real paste, a draft operation that discarding the draft aborts.
  const onPasteImages = () => {
    const operation = startComposerDraftOperation(draftKey);
    return new Promise<boolean>((resolve) => preparations.push(resolve)).finally(operation.finish);
  };
  function Probe() {
    const current = useSubmitAfterNativeImages({
      draftKey,
      readResetGeneration: () => generation,
      blocked,
      approvalIds,
      onPasteImages,
      onSubmit: (action, shownApprovalIds) => {
        if (!blocked) onSubmit(action, shownApprovalIds);
      },
    });
    useLayoutEffect(() => {
      queue = current;
    });
    return null;
  }
  const render = () => act(() => root.render(<Probe />));
  render();
  return {
    get queue() {
      return queue!;
    },
    onSubmit,
    setBlocked(value: boolean) {
      blocked = value;
      render();
    },
    /** A clear after a send: the draft resets, but work still landing in it carries on. */
    reset() {
      generation += 1;
      render();
    },
    discard() {
      abortComposerDraftOperations(draftKey);
      generation += 1;
      render();
    },
    abortPastes() {
      abortComposerDraftOperations(draftKey);
    },
    setDraftKey(value: string) {
      draftKey = value;
      render();
    },
    setApprovalIds(value: ReadonlyArray<string>) {
      approvalIds = value.map((id) => RuntimeRequestId.make(id));
      render();
    },
    async finish(success: boolean) {
      await act(async () => {
        preparations.shift()!(success);
      });
    },
    [Symbol.dispose]() {
      act(() => root.unmount());
    },
  };
}

it("does not send when conversion finishes without adding an image", async () => {
  using state = setup();
  const paste = state.queue.pasteImages(["image"]);
  expect(state.queue.deferSend()).toBe(true);
  await state.finish(false);
  await paste;
  expect(state.onSubmit).not.toHaveBeenCalled();

  const first = state.queue.pasteImages(["failed"]);
  const second = state.queue.pasteImages(["successful"]);
  state.queue.deferSend();
  await state.finish(false);
  await state.finish(true);
  await Promise.all([first, second]);
  expect(state.onSubmit).not.toHaveBeenCalled();

  const retry = state.queue.pasteImages(["retry"]);
  state.queue.deferSend();
  await state.finish(true);
  await retry;
  expect(state.onSubmit).toHaveBeenCalledOnce();
});

it("does not send a replacement draft after the original draft was discarded", async () => {
  using state = setup();
  const paste = state.queue.pasteImages(["image"]);
  state.queue.deferSend();
  state.discard();
  // The discarded draft's paste is still converting, but nothing in the new draft waits on it.
  expect(state.queue.deferSend()).toBe(false);
  await state.finish(true);
  await paste;
  expect(state.onSubmit).not.toHaveBeenCalled();

  const next = state.queue.pasteImages(["next"]);
  state.queue.deferSend();
  state.setBlocked(true);
  await state.finish(true);
  await next;
  state.reset();
  state.setBlocked(false);
  expect(state.onSubmit).not.toHaveBeenCalled();
});

it("waits for other attachment preparation and sends once after all images join the draft", async () => {
  using state = setup();
  const first = state.queue.pasteImages(["first"]);
  const second = state.queue.pasteImages(["second"]);
  state.queue.deferSend("queue");
  state.queue.deferSend("steer");
  await state.finish(true);
  expect(state.onSubmit).not.toHaveBeenCalled();
  state.setBlocked(true);
  await state.finish(true);
  await Promise.all([first, second]);
  expect(state.onSubmit).not.toHaveBeenCalled();
  state.setBlocked(false);
  expect(state.onSubmit).toHaveBeenCalledExactlyOnceWith("steer", []);
  state.setBlocked(true);
  state.setBlocked(false);
  expect(state.onSubmit).toHaveBeenCalledOnce();
});

it("holds a released send again for an image pasted while it waited on another blocker", async () => {
  using state = setup();
  const first = state.queue.pasteImages(["first"]);
  state.queue.deferSend();
  state.setBlocked(true);
  await state.finish(true);
  await first;
  const second = state.queue.pasteImages(["second"]);
  state.setApprovalIds(["arrived"]);
  state.setBlocked(false);
  expect(state.onSubmit).not.toHaveBeenCalled();
  await state.finish(true);
  await second;
  expect(state.onSubmit).toHaveBeenCalledExactlyOnceWith(undefined, []);
});

it("sends with the approvals shown when Send was pressed, not those shown since", async () => {
  using state = setup();
  state.setApprovalIds(["seen"]);
  const paste = state.queue.pasteImages(["image"]);
  state.queue.deferSend("steer");
  state.setApprovalIds(["seen", "unseen"]);
  await state.finish(true);
  await paste;
  expect(state.onSubmit).toHaveBeenCalledExactlyOnceWith("steer", ["seen"]);
});

it("does not hold a send for a paste into the draft the composer left", async () => {
  using state = setup();
  const paste = state.queue.pasteImages(["thread draft"]);
  state.abortPastes();
  state.setDraftKey("queued edit");
  expect(state.queue.deferSend()).toBe(false);

  const editPaste = state.queue.pasteImages(["queued edit"]);
  expect(state.queue.deferSend()).toBe(true);
  await state.finish(false);
  await paste;
  await state.finish(true);
  await editPaste;
  expect(state.onSubmit).toHaveBeenCalledOnce();
});

it("waits for a paste that still lands in its draft after the draft was cleared", async () => {
  using state = setup();
  const paste = state.queue.pasteImages(["image"]);
  state.reset();
  expect(state.queue.deferSend()).toBe(true);
  await state.finish(true);
  await paste;
  expect(state.onSubmit).toHaveBeenCalledOnce();
});

it("drops a send whose paste was aborted, even if a later paste lands", async () => {
  using state = setup();
  const aborted = state.queue.pasteImages(["aborted"]);
  state.queue.deferSend();
  // A queued edit that opened and closed at once aborts the paste without a draft switch.
  state.abortPastes();
  await state.finish(false);
  await aborted;
  const later = state.queue.pasteImages(["later"]);
  await state.finish(true);
  await later;
  expect(state.onSubmit).not.toHaveBeenCalled();
});
