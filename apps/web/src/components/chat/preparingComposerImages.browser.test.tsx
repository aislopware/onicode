import { useEffect } from "react";
import { expect, it, vi } from "vite-plus/test";
import { createStore, type StoreApi, useStore } from "zustand";

import { act, render } from "../../../test/render";
import {
  addPreparingComposerImages,
  cancelPreparingComposerImages,
  removePreparingComposerImages,
  sendAfterPreparingImages,
  useSubmitAfterPreparingImages,
} from "./preparingComposerImages";

type QueueSubmit = ReturnType<typeof useSubmitAfterPreparingImages>;

type DraftImagesStore = StoreApi<{ readonly ids: ReadonlyArray<string> }>;

function createDraftImagesStore(): DraftImagesStore {
  return createStore(() => ({ ids: [] }));
}

// Mirrors ChatComposer: the send reads draft images that a later effect syncs from the store.
function Probe(props: {
  targetKey: string;
  draftImagesStore: DraftImagesStore;
  onReady: (queue: QueueSubmit) => void;
  onCommit?: (images: ReadonlyArray<string>) => void;
  blocked?: boolean;
}) {
  const queue = useSubmitAfterPreparingImages(props.targetKey, props.blocked);
  const draftImages = useStore(props.draftImagesStore, (state) => state.ids);
  const { onCommit } = props;
  useEffect(() => {
    onCommit?.(draftImages);
  }, [onCommit, draftImages]);
  props.onReady(queue);
  return null;
}

function preparingImage(id: string) {
  return { id, name: `${id}.png`, file: new File(["png"], `${id}.png`), hidden: false };
}

it("sends after pasted images finish compressing, with the compressed image in the draft", async () => {
  const target = "submit-after-preparing";
  using _store = { [Symbol.dispose]: () => cancelPreparingComposerImages(target) };
  const draftImagesStore = createDraftImagesStore();
  let queueSubmit: QueueSubmit | undefined;
  let committedImages: ReadonlyArray<string> = [];
  const onCommit = (images: ReadonlyArray<string>) => {
    committedImages = images;
  };
  const onReady = (queue: QueueSubmit) => {
    queueSubmit = queue;
  };
  await using screen = await render(
    <Probe
      targetKey={target}
      draftImagesStore={draftImagesStore}
      onCommit={onCommit}
      onReady={onReady}
    />,
    // A bare hook probe; nothing here navigates.
    { router: false },
  );
  const submit = vi.fn(() => committedImages);

  expect(queueSubmit?.(submit)).toBe(false);

  addPreparingComposerImages(target, [preparingImage("first"), preparingImage("second")]);
  expect(queueSubmit?.(submit)).toBe(true);

  await act(() => removePreparingComposerImages(target, new Set(["first"])));
  expect(submit).not.toHaveBeenCalled();

  // Intake drops the placeholder and adds the compressed image in the same tick.
  await act(() => {
    removePreparingComposerImages(target, new Set(["second"]));
    draftImagesStore.setState({ ids: ["second"] });
  });
  expect(submit).toHaveBeenCalledOnce();
  expect(submit.mock.results[0]?.value).toEqual(["second"]);

  submit.mockClear();
  await act(() => addPreparingComposerImages(target, [preparingImage("uploading")]));
  expect(queueSubmit?.(submit)).toBe(true);
  await screen.rerender(
    <Probe
      targetKey={target}
      draftImagesStore={draftImagesStore}
      onCommit={onCommit}
      onReady={onReady}
      blocked
    />,
  );
  await act(() => removePreparingComposerImages(target, new Set(["uploading"])));
  expect(submit).not.toHaveBeenCalled();
  await screen.rerender(
    <Probe
      targetKey={target}
      draftImagesStore={draftImagesStore}
      onCommit={onCommit}
      onReady={onReady}
    />,
  );
  expect(submit).toHaveBeenCalledOnce();
  expect(submit.mock.results[0]?.value).toEqual(["second"]);
});

it("drops a waiting send when the composer moves to another draft or discards it", async () => {
  const target = "submit-after-preparing-old";
  const otherTarget = "submit-after-preparing-new";
  using _store = {
    [Symbol.dispose]: () => {
      cancelPreparingComposerImages(target);
      cancelPreparingComposerImages(otherTarget);
    },
  };
  const draftImagesStore = createDraftImagesStore();
  let queueSubmit: QueueSubmit | undefined;
  const onReady = (queue: QueueSubmit) => {
    queueSubmit = queue;
  };
  await using screen = await render(
    <Probe targetKey={target} draftImagesStore={draftImagesStore} onReady={onReady} />,
    {
      // A bare hook probe; nothing here navigates.
      router: false,
    },
  );
  const submit = vi.fn();
  addPreparingComposerImages(target, [preparingImage("pasted")]);
  expect(queueSubmit?.(submit)).toBe(true);

  // The other draft is still compressing too, so the switch alone must drop the send.
  addPreparingComposerImages(otherTarget, [preparingImage("other")]);
  await screen.rerender(
    <Probe targetKey={otherTarget} draftImagesStore={draftImagesStore} onReady={onReady} />,
  );
  await act(() => removePreparingComposerImages(target, new Set(["pasted"])));
  await screen.rerender(
    <Probe targetKey={target} draftImagesStore={draftImagesStore} onReady={onReady} />,
  );
  expect(submit).not.toHaveBeenCalled();

  // Discarding the draft cancels its compression, which must not release the held send.
  await act(() => addPreparingComposerImages(target, [preparingImage("discarded")]));
  expect(queueSubmit?.(submit)).toBe(true);
  await act(() => cancelPreparingComposerImages(target));
  expect(submit).not.toHaveBeenCalled();
});

it("keeps the draft unsent when image preparation fails", async () => {
  const target = "submit-after-failed-preparation";
  using _store = { [Symbol.dispose]: () => cancelPreparingComposerImages(target) };
  const draftImagesStore = createDraftImagesStore();
  let queueSubmit: QueueSubmit | undefined;
  await using _screen = await render(
    <Probe
      targetKey={target}
      draftImagesStore={draftImagesStore}
      onReady={(queue) => {
        queueSubmit = queue;
      }}
    />,
    // A bare hook probe; nothing here navigates.
    { router: false },
  );
  const submit = vi.fn();
  await act(() => addPreparingComposerImages(target, [preparingImage("failed")]));
  expect(queueSubmit?.(submit)).toBe(true);
  await act(() =>
    removePreparingComposerImages(target, new Set(["failed"]), { cancelSubmission: true }),
  );
  expect(submit).not.toHaveBeenCalled();
});

it("hands a held answer to its away send when the composer leaves before the images land", async () => {
  const target = "submit-after-preparing-away";
  const otherTarget = "submit-after-preparing-away-other";
  using _store = {
    [Symbol.dispose]: () => {
      cancelPreparingComposerImages(target);
      cancelPreparingComposerImages(otherTarget);
    },
  };
  const draftImagesStore = createDraftImagesStore();
  let queueSubmit: QueueSubmit | undefined;
  const onReady = (queue: QueueSubmit) => {
    queueSubmit = queue;
  };
  await using screen = await render(
    <Probe targetKey={target} draftImagesStore={draftImagesStore} onReady={onReady} />,
    // A bare hook probe; nothing here navigates.
    { router: false },
  );
  const submit = vi.fn();
  const sendAway = vi.fn();

  // The composer moves on to another thread's question.
  addPreparingComposerImages(target, [preparingImage("switched")]);
  expect(queueSubmit?.(submit, sendAway)).toBe(true);
  await screen.rerender(
    <Probe targetKey={otherTarget} draftImagesStore={draftImagesStore} onReady={onReady} />,
  );
  expect(sendAway).not.toHaveBeenCalled();
  await act(() => removePreparingComposerImages(target, new Set(["switched"])));
  expect(sendAway).toHaveBeenCalledOnce();

  // A failed image still drops the hold its composer left behind.
  await act(() => addPreparingComposerImages(otherTarget, [preparingImage("failed")]));
  expect(queueSubmit?.(submit, sendAway)).toBe(true);
  await screen.rerender(
    <Probe targetKey={target} draftImagesStore={draftImagesStore} onReady={onReady} />,
  );
  await act(() =>
    removePreparingComposerImages(otherTarget, new Set(["failed"]), { cancelSubmission: true }),
  );
  expect(sendAway).toHaveBeenCalledOnce();

  // The thread view closes.
  await act(() => addPreparingComposerImages(target, [preparingImage("unmounted")]));
  expect(queueSubmit?.(submit, sendAway)).toBe(true);
  await screen.unmount();
  await act(() => removePreparingComposerImages(target, new Set(["unmounted"])));
  expect(sendAway).toHaveBeenCalledTimes(2);
  expect(submit).not.toHaveBeenCalled();
});

it("keeps an away send dropped when an image fails after the press", async () => {
  const target = "send-after-preparing-pinned";
  using _store = { [Symbol.dispose]: () => cancelPreparingComposerImages(target) };
  addPreparingComposerImages(target, [preparingImage("failed")]);
  // Pressed while the image prepared; the transcript that sends arrives after it failed.
  const sendWhenPrepared = sendAfterPreparingImages(target);
  removePreparingComposerImages(target, new Set(["failed"]), { cancelSubmission: true });
  const send = vi.fn();
  sendWhenPrepared(send);
  await act(() => Promise.resolve());
  expect(send).not.toHaveBeenCalled();

  const sendLater = sendAfterPreparingImages(target);
  sendLater(send);
  await vi.waitFor(() => expect(send).toHaveBeenCalledOnce());
});
