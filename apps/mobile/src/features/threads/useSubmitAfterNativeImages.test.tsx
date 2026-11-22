import { act, StrictMode, useLayoutEffect } from "react";
import { createRoot } from "react-dom/client";
import { ProviderInstanceId, RuntimeRequestId } from "@onicode/contracts";
import type { ActiveTurnComposerAction } from "@onicode/client-runtime/state/composer-dispatch";
import { expect, it, vi } from "vite-plus/test";
import {
  abortComposerDraftOperations,
  onNativeImagePastesSettled,
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
  const roots = [root];
  // Unmounts the newest composer; the one beside it, if any, takes the harness's renders.
  const unmountNewest = () => {
    roots.pop()?.unmount();
    root = roots.at(-1) ?? root;
  };
  let queue: ReturnType<typeof useSubmitAfterNativeImages>;
  let converting = false;
  let generation = 0;
  let blocked = false;
  let draftKey = "draft";
  let approvalIds: ReadonlyArray<RuntimeRequestId> = [];
  const onSubmit = vi.fn();
  const discardImages = vi.fn();
  let strict = false;
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
  let conversionsStarted = 0;
  // Like the real paste, a draft operation that discarding the draft aborts.
  const onPasteImages = () => {
    conversionsStarted += 1;
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
      discardImages,
      watchDraft,
      flushInput: () => flushing,
      // Like the thread screen: a blocked send is turned down, one that goes clears the draft.
      onSubmit: (action, shownApprovalIds, settings, away) => {
        if (blocked) return;
        if (away === undefined) onSubmit(action, shownApprovalIds, settings);
        else onSubmit(action, shownApprovalIds, settings, away);
        draftEdited();
      },
    });
    const currentConverting = useNativeImagesConverting(draftKey);
    useLayoutEffect(() => {
      queue = current;
      converting = currentConverting;
    });
    return null;
  }
  const render = () =>
    act(() =>
      root.render(
        strict ? (
          <StrictMode>
            <Probe />
          </StrictMode>
        ) : (
          <Probe />
        ),
      ),
    );
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
    discardImages,
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
    /** Pass `render: false` while unmounted; the next mount reads it. */
    setBlocked(value: boolean, { render: rerender = true } = {}) {
      blocked = value;
      if (rerender) render();
    },
    get conversionsStarted() {
      return conversionsStarted;
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
      act(unmountNewest);
    },
    /**
     * Coming back mounts a new composer, on the same draft unless another thread is opened.
     * Without an unmount first, it mounts beside the current one, as a Files route over a thread.
     */
    remount(value = draftKey, options: { strict?: boolean } = {}) {
      draftKey = value;
      strict = options.strict ?? false;
      root = createRoot(document.createElement("div"));
      roots.push(root);
      render();
    },
    /** The pastes settle, and the composer unmounts before React renders what that queued. */
    async finishAndUnmount(success: boolean) {
      await act(async () => {
        const settled = Promise.withResolvers<void>();
        using _settled = { [Symbol.dispose]: onNativeImagePastesSettled(() => settled.resolve()) };
        preparations.shift()!(success);
        await settled.promise;
        unmountNewest();
      });
    },
    async finish(success: boolean) {
      await act(async () => {
        preparations.shift()!(success);
      });
    },
    [Symbol.dispose]() {
      act(() => {
        while (roots.length > 0) unmountNewest();
      });
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

it("sends a send left while away once its images land, unless the thread screen must send it", async () => {
  using state = setup();
  const paste = state.queue.pasteImages(["image"]);
  const settings = { ...SHOWN, runtimeMode: "approval-required" as const };
  await act(async () => void state.queue.deferSend("steer", settings));
  state.unmount();
  // Split view opens another thread in its place; that composer leaves it alone.
  state.remount("other");
  expect(state.queue.held).toBeNull();
  await state.finish(true);
  await paste;
  expect(state.onSubmit).toHaveBeenCalledExactlyOnceWith("steer", [], settings, true);
  state.unmount();
  state.remount("draft");
  expect(state.queue.held).toBeNull();
  expect(state.onSubmit).toHaveBeenCalledOnce();

  // Its images joined while another blocker held it.
  state.setBlocked(true);
  const blocked = state.queue.pasteImages(["blocked"]);
  await act(async () => void state.queue.deferSend());
  await state.finish(true);
  await blocked;
  state.unmount();
  state.setBlocked(false, { render: false });
  state.remount();
  expect(state.onSubmit).toHaveBeenCalledTimes(2);

  // Before the press's own input landed, an edit could go unseen, so it is not kept.
  const release = state.holdFlush();
  const early = state.queue.pasteImages(["early"]);
  act(() => void state.queue.deferSend());
  state.unmount();
  await release();
  state.remount();
  expect(state.queue.held).toBeNull();
  await state.finish(true);
  await early;
  expect(state.onSubmit).toHaveBeenCalledTimes(2);

  // A blocker when it left, or an approval it answers, waits for the composer to come back.
  state.setBlocked(true);
  const waiting = state.queue.pasteImages(["waiting"]);
  await act(async () => void state.queue.deferSend());
  state.unmount();
  await state.finish(true);
  await waiting;
  expect(state.onSubmit).toHaveBeenCalledTimes(2);
  state.setBlocked(false, { render: false });
  state.remount();
  expect(state.onSubmit).toHaveBeenCalledTimes(3);
  state.setApprovalIds(["seen"]);
  const redirect = state.queue.pasteImages(["redirect"]);
  await act(async () => void state.queue.deferSend());
  state.unmount();
  await state.finish(true);
  await redirect;
  expect(state.onSubmit).toHaveBeenCalledTimes(3);
  state.remount();
  expect(state.onSubmit).toHaveBeenLastCalledWith(undefined, ["seen"], SHOWN);
  expect(state.onSubmit).toHaveBeenCalledTimes(4);

  // Turned down away, as while its thread is still being created, it waits for the composer too.
  state.setApprovalIds([]);
  const declined = state.queue.pasteImages(["declined"]);
  await act(async () => void state.queue.deferSend());
  state.unmount();
  state.setBlocked(true, { render: false });
  await state.finish(true);
  await declined;
  expect(state.onSubmit).toHaveBeenCalledTimes(4);
  state.setBlocked(false, { render: false });
  state.remount();
  expect(state.onSubmit).toHaveBeenCalledTimes(5);
});

it("hands a left send to a composer still showing its draft", async () => {
  using state = setup();
  // A Files route's composer over the thread sends and leaves; the thread's composer takes over.
  state.remount();
  const paste = state.queue.pasteImages(["image"]);
  await act(async () => void state.queue.deferSend());
  state.unmount();
  expect(state.queue.held).toEqual({ images: 1 });
  await state.finish(true);
  await paste;
  expect(state.onSubmit).toHaveBeenCalledOnce();
});

it("sends a send whose images land in the batch that unmounts its composer", async () => {
  using state = setup();
  const paste = state.queue.pasteImages(["image"]);
  await act(async () => void state.queue.deferSend());
  await state.finishAndUnmount(true);
  await paste;
  expect(state.onSubmit).toHaveBeenCalledOnce();
  state.remount();
  expect(state.onSubmit).toHaveBeenCalledOnce();
});

it("keeps the edit watch on an adopted send when Strict Mode replays its effects", async () => {
  using state = setup();
  state.setBlocked(true);
  const paste = state.queue.pasteImages(["image"]);
  await act(async () => void state.queue.deferSend());
  await state.finish(true);
  await paste;
  state.unmount();
  state.remount("draft", { strict: true });
  expect(state.queue.held).toEqual({ images: 0 });
  state.editDraft();
  state.setBlocked(false);
  expect(state.onSubmit).not.toHaveBeenCalled();
});

it("drops its own held send and takes the left one when a composer turns to that draft", async () => {
  using state = setup();
  const left = state.queue.pasteImages(["left"]);
  await act(async () => void state.queue.deferSend());
  state.unmount();
  state.remount("other");
  const other = state.queue.pasteImages(["other"]);
  await act(async () => void state.queue.deferSend());
  state.setDraftKey("draft");
  expect(state.queue.held).toEqual({ images: 1 });
  await state.finish(true);
  await left;
  expect(state.onSubmit).toHaveBeenCalledOnce();
  await state.finish(true);
  await other;
  expect(state.onSubmit).toHaveBeenCalledOnce();
});

it("drops an iOS image read whose draft was discarded or left before it landed", async () => {
  using state = setup();
  let landRead: (uris: ReadonlyArray<string>) => void = () => {};
  act(() => {
    landRead = state.queue.readPastedImages(1);
  });
  act(() => void state.queue.deferSend());
  state.discard();
  expect(state.queue.held).toBeNull();
  act(() => landRead(["image"]));
  expect(state.converting).toBe(false);

  act(() => {
    landRead = state.queue.readPastedImages(1);
  });
  state.setDraftKey("queued-edit");
  act(() => landRead(["image"]));
  expect(state.conversionsStarted).toBe(0);
  expect(state.discardImages).toHaveBeenCalledTimes(2);

  // Leaving during the read ends it with the editor, so the send has no image and is dropped.
  state.setDraftKey("draft");
  act(() => {
    landRead = state.queue.readPastedImages(1);
  });
  act(() => void state.queue.deferSend());
  state.unmount();
  act(() => landRead([]));
  state.remount();
  expect(state.queue.held).toBeNull();
  expect(state.onSubmit).not.toHaveBeenCalled();
  expect(state.onSubmit).not.toHaveBeenCalled();
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
