import { act, useLayoutEffect } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vite-plus/test";
import {
  nativeImagePasteKey,
  nativeImagePastePending,
  onNativeImagePastesSettled,
} from "./composerDraftOperations";
import { useComposerAttachmentActions } from "./use-composer-attachment-actions";

const mocks = vi.hoisted(() => ({
  pickComposerMedia: vi.fn(),
  append: vi.fn(() => 0),
  alert: vi.fn(),
}));

vi.mock("../lib/composerImages", () => ({
  convertPastedImagesToAttachments: vi.fn(),
  createPastedTextComposerAttachment: vi.fn(),
  pickComposerFiles: vi.fn(),
  pickComposerMedia: mocks.pickComposerMedia,
  removePersistedComposerAttachmentFile: vi.fn(),
}));

vi.mock("./use-composer-drafts", () => ({
  appendComposerDraftAttachments: mocks.append,
  joinNativeImagesToDraft: <T,>(insert: () => T) => insert(),
  captureComposerDraftInsertion: () => ({ text: "", start: 0, end: 0 }),
  countComposerDraftAttachmentsAfterSelection: () => 0,
  getComposerDraftSnapshot: () => ({ text: "", attachments: [] }),
}));

vi.mock("../components/ui/Dialog", () => ({ alert: mocks.alert }));

function setup() {
  const root = createRoot(document.createElement("div"));
  let actions: ReturnType<typeof useComposerAttachmentActions>;
  function Probe() {
    const current = useComposerAttachmentActions({ draftKey: "draft", serverConfig: null });
    useLayoutEffect(() => {
      actions = current;
    });
    return null;
  }
  act(() => root.render(<Probe />));
  const settled = vi.fn();
  const stop = onNativeImagePastesSettled(settled);
  return {
    get actions() {
      return actions!;
    },
    settled,
    [Symbol.dispose]() {
      stop();
      act(() => root.unmount());
    },
  };
}

it("holds a send while a picked photo lands, and drops it when the pick fails", async () => {
  using state = setup();
  const key = nativeImagePasteKey("draft");
  const photo = { id: "photo", fileUri: "file:///photo.jpg" };
  const picked = Promise.withResolvers<{ attachments: unknown[]; error: string | null }>();
  mocks.pickComposerMedia.mockReturnValueOnce(picked.promise);

  const pick = state.actions.onPickDraftMedia();
  // The picker has closed and is still copying the photo; Send must wait for it.
  expect(nativeImagePastePending(key)).toBe(true);
  picked.resolve({ attachments: [photo], error: null });
  await pick;
  expect(mocks.append).toHaveBeenCalledWith("draft", [photo], expect.anything());
  expect(nativeImagePastePending(key)).toBe(false);
  expect(state.settled).toHaveBeenLastCalledWith(key, false);

  mocks.pickComposerMedia.mockResolvedValueOnce({ attachments: [], error: "Too large" });
  await state.actions.onPickDraftMedia();
  expect(state.settled).toHaveBeenLastCalledWith(key, true);
  expect(mocks.alert).toHaveBeenCalledOnce();
});
