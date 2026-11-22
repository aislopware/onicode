import { act, useLayoutEffect } from "react";
import { createRoot } from "react-dom/client";
import { ProviderInstanceId, RuntimeRequestId } from "@onicode/contracts";
import type { ActiveTurnComposerAction } from "@onicode/client-runtime/state/composer-dispatch";
import { expect, it, vi } from "vite-plus/test";
import {
  abortComposerDraftOperations,
  startComposerDraftOperation,
  startNativeImagePaste,
} from "../../state/composerDraftOperations";
import type { ComposerDraftSendSettings } from "../../state/use-composer-drafts";
import {
  useNativeImagesConverting,
  useSubmitAfterNativeImages,
} from "./useSubmitAfterNativeImages";

const SHOWN: ComposerDraftSendSettings = {
  runtimeMode: "full-access",
  modelSelection: { instanceId: ProviderInstanceId.make("codex"), model: "gpt-5.5" },
};

function setup() {
  let root = createRoot(document.createElement("div"));
  let queue: ReturnType<typeof useSubmitAfterNativeImages>;
  let converting = false;
  let generation = 0;
  let blocked = false;
  let draftKey = "draft";
  let approvalIds: ReadonlyArray<RuntimeRequestId> = [];
  const onSubmit = vi.fn();
  let draftEdited = () => {};
  const unwatchDraft = vi.fn();
  const watchDraft = vi.fn((onEdit: () => void) => {
    draftEdited = onEdit;
    return () => {
      unwatchDraft();
      if (draftEdited === onEdit) draftEdited = () => {};
    };
  });
  let flushing = Promise.resolve();
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
      watchDraft,
      flushInput: () => flushing,
      onSubmit: (action, shownApprovalIds, settings) => {
        if (!blocked) onSubmit(action, shownApprovalIds, settings);
      },
    });
    const currentConverting = useNativeImagesConverting(draftKey);
    useLayoutEffect(() => {
      queue = current;
      converting = currentConverting;
    });
    return null;
  }
  const render = () => act(() => root.render(<Probe />));
  render();
  return {
    get queue() {
      return {
        ...queue!,
        deferSend: (followUp?: ActiveTurnComposerAction, settings = SHOWN) =>
          queue!.deferSend(followUp, settings),
      };
    },
    get converting() {
      return converting;
    },
    onSubmit,
    watchDraft,
    unwatchDraft,
    editDraft() {
      act(() => draftEdited());
    },
    /** Holds the editor's flush, as iOS accepting an autocorrection after the press does. */
    holdFlush() {
      const flushed = Promise.withResolvers<void>();
      flushing = flushed.promise;
      return async () => {
        flushing = Promise.resolve();
        await act(async () => flushed.resolve());
      };
    },
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
    /** Leaving the thread screen unmounts the composer. */
    unmount() {
      act(() => root.unmount());
    },
    /** Coming back mounts a new composer on the same draft. */
    remount() {
      root = createRoot(document.createElement("div"));
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
  expect(state.onSubmit).toHaveBeenCalledExactlyOnceWith("steer", [], SHOWN);
  state.setBlocked(true);
  state.setBlocked(false);
  expect(state.onSubmit).toHaveBeenCalledOnce();
});

it("shows a held send until it goes, and drops it on Cancel, an edit, or another paste", async () => {
  using state = setup();
  const first = state.queue.pasteImages(["first", "second"]);
  expect(state.queue.held).toBeNull();
  act(() => void state.queue.deferSend());
  expect(state.queue.held).toEqual({ images: 2 });
  act(() => state.queue.cancel());
  expect(state.queue.held).toBeNull();
  await state.finish(true);
  await first;
  expect(state.onSubmit).not.toHaveBeenCalled();

  const edited = state.queue.pasteImages(["edited"]);
  await act(async () => void state.queue.deferSend());
  state.editDraft();
  expect(state.queue.held).toBeNull();
  await state.finish(true);
  await edited;
  expect(state.onSubmit).not.toHaveBeenCalled();

  // An image pasted while the released send waits on another blocker is an edit too.
  const held = state.queue.pasteImages(["held"]);
  act(() => void state.queue.deferSend());
  state.setBlocked(true);
  await state.finish(true);
  await held;
  expect(state.queue.held).toEqual({ images: 0 });
  let later = Promise.resolve();
  act(() => {
    later = state.queue.pasteImages(["later"]);
  });
  expect(state.queue.held).toBeNull();
  state.setBlocked(false);
  await state.finish(true);
  await later;
  expect(state.onSubmit).not.toHaveBeenCalled();

  // An autocorrection iOS accepts for the press lands after it, and is not an edit.
  const autocorrected = state.holdFlush();
  const sent = state.queue.pasteImages(["sent"]);
  act(() => void state.queue.deferSend());
  expect(state.queue.held).toEqual({ images: 1 });
  state.editDraft();
  await autocorrected();
  await state.finish(true);
  await sent;
  expect(state.onSubmit).toHaveBeenCalledOnce();
  expect(state.queue.held).toBeNull();
  expect(state.watchDraft).toHaveBeenCalledTimes(3);
});

it("joins a closed question's image to a released send, and drops a send the composer left", async () => {
  using state = setup();
  const held = state.queue.pasteImages(["held"]);
  act(() => void state.queue.deferSend());
  state.setBlocked(true);
  await state.finish(true);
  await held;
  const question = startNativeImagePaste("draft");
  state.setBlocked(false);
  expect(state.onSubmit).not.toHaveBeenCalled();
  await act(async () => question(true));
  expect(state.onSubmit).toHaveBeenCalledOnce();

  const left = state.queue.pasteImages(["left"]);
  await act(async () => void state.queue.deferSend());
  state.unwatchDraft.mockClear();
  state.setDraftKey("queued edit");
  expect(state.unwatchDraft).toHaveBeenCalledOnce();
  expect(state.queue.held).toBeNull();
  await state.finish(true);
  await left;
  expect(state.onSubmit).toHaveBeenCalledOnce();
});

it("sends with the approvals, mode and model of the press, not those shown since", async () => {
  using state = setup();
  state.setApprovalIds(["seen"]);
  const paste = state.queue.pasteImages(["image"]);
  const settings = { ...SHOWN, runtimeMode: "approval-required" as const };
  state.queue.deferSend("steer", settings);
  state.setApprovalIds(["seen", "unseen"]);
  await state.finish(true);
  await paste;
  expect(state.onSubmit).toHaveBeenCalledExactlyOnceWith("steer", ["seen"], settings);
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

it("waits for an image this composer did not paste, such as a closed question's", async () => {
  using state = setup();
  const finish = startNativeImagePaste("draft");
  expect(state.queue.deferSend()).toBe(true);
  await act(async () => finish(true));
  expect(state.onSubmit).toHaveBeenCalledOnce();
});

it("drops a send whose paste was aborted, even if a later paste lands", async () => {
  using state = setup();
  const aborted = state.queue.pasteImages(["aborted"]);
  act(() => void state.queue.deferSend());
  expect(state.queue.held).toEqual({ images: 1 });
  // A queued edit that opened and closed at once aborts the paste without a draft switch.
  act(() => state.abortPastes());
  expect(state.queue.held).toBeNull();
  await state.finish(false);
  await aborted;
  const later = state.queue.pasteImages(["later"]);
  await state.finish(true);
  await later;
  expect(state.onSubmit).not.toHaveBeenCalled();
});

it("keeps a held send after the composer unmounts, shows it on return, and drops it on an edit", async () => {
  using state = setup();
  state.setApprovalIds(["seen"]);
  const paste = state.queue.pasteImages(["image"]);
  const settings = { ...SHOWN, runtimeMode: "approval-required" as const };
  await act(async () => void state.queue.deferSend("steer", settings));
  state.unmount();
  state.remount();
  expect(state.queue.held).toEqual({ images: 1 });
  await state.finish(true);
  await paste;
  expect(state.onSubmit).toHaveBeenCalledExactlyOnceWith("steer", ["seen"], settings);
  expect(state.queue.held).toBeNull();

  const edited = state.queue.pasteImages(["edited"]);
  await act(async () => void state.queue.deferSend());
  state.unmount();
  state.editDraft();
  state.remount();
  expect(state.queue.held).toBeNull();
  await state.finish(true);
  await edited;

  const cancelled = state.queue.pasteImages(["cancelled"]);
  await act(async () => void state.queue.deferSend());
  state.unmount();
  state.remount();
  act(() => state.queue.cancel());
  expect(state.queue.held).toBeNull();
  await state.finish(true);
  await cancelled;
  expect(state.onSubmit).toHaveBeenCalledOnce();
});

it("holds a send from the start of an iOS pasteboard read until its image lands", async () => {
  using state = setup();
  let landRead: (uris: ReadonlyArray<string>) => void = () => {};
  act(() => {
    landRead = state.queue.readPastedImages(1);
  });
  act(() => void state.queue.deferSend());
  expect(state.queue.held).toEqual({ images: 1 });
  act(() => landRead(["image"]));
  expect(state.queue.held).toEqual({ images: 1 });
  await state.finish(true);
  expect(state.onSubmit).toHaveBeenCalledOnce();

  // A read that found no image drops the send rather than sending without it.
  act(() => {
    landRead = state.queue.readPastedImages(1);
  });
  act(() => void state.queue.deferSend());
  act(() => landRead([]));
  expect(state.queue.held).toBeNull();
  expect(state.onSubmit).toHaveBeenCalledOnce();
});

it("counts an image as content to send from its read until it joins the draft", async () => {
  using state = setup();
  expect(state.converting).toBe(false);
  let landRead: (uris: ReadonlyArray<string>) => void = () => {};
  act(() => {
    landRead = state.queue.readPastedImages(1);
  });
  expect(state.converting).toBe(true);
  act(() => landRead(["image"]));
  expect(state.converting).toBe(true);
  await state.finish(true);
  expect(state.converting).toBe(false);
});
